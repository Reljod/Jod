//! Server-sent events: the live feed, and the part that survives a tunnel.
//!
//! ## Why the connect order matters
//!
//! Subscribing to the live broadcast happens **before** reading history, never
//! after. A stream that reads history first drops every event that lands while
//! it is reading — and a hole is worse than a duplicate: a duplicate is a
//! rendering bug, a hole is a lost tool call. Duplicates are then removed by
//! remembering the highest `seq` emitted. The CLI subscribes before spawning
//! for exactly the same reason.
//!
//! ## Two streams, deliberately different
//!
//! - `/v1/agents/{id}/stream` is **resumable**. `AgentEnvelope.seq` is
//!   monotonic per agent, so it is a valid cursor, and it is sent as the SSE
//!   `id:` field. A reconnecting `EventSource` echoes it back as
//!   `Last-Event-ID` and gets only the tail.
//! - `/v1/events` is **live-only, and sends no `id:` at all**. `seq` is
//!   per-agent, so a single cursor across all agents is ambiguous — `42` means
//!   something different for every agent, and any single-cursor replay would
//!   both double-send and skip. Rather than promise a resume we cannot honour,
//!   the field is omitted, which also stops the browser sending a meaningless
//!   `Last-Event-ID`. After a drop, clients backfill per agent via
//!   `/v1/agents/{id}/events?after_seq=`.
//!
//! Giving `/v1/events` a real global cursor would mean threading the store's
//! rowid — which *is* globally monotonic — out through `AgentEnvelope`. That is
//! a `jod-core` change, and worth making only if a client actually needs it.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Extension, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_core::Stream;
use jod_core::AgentEnvelope;
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use crate::auth::Scope;
use crate::error::ApiResult;
use crate::{AppState, Identity};

/// Long enough not to be chatty, short enough to keep an idle connection alive
/// through a NAT or proxy that reaps quiet sockets.
const KEEPALIVE: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    /// Explicit cursor, for a client that persisted its position across a cold
    /// start and so has no `EventSource` state to reconnect with.
    pub after_seq: Option<u64>,
}

/// One agent, live, resumable.
pub async fn agent_stream(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    identity.require(Scope::Read)?;

    // Fail before opening the stream if the agent is unknown, so the client
    // gets a 404 it can act on rather than a stream that never yields.
    state.jod.agent(&id).await?;

    // `Last-Event-ID` wins over `?after_seq=`: it is the one the runtime sets
    // and the one that reflects what was actually received. Absent from both
    // means "everything" — *not* `0`, since `seq` starts at 0 and the first
    // event of a run is `started`.
    let after_seq = last_event_id(&headers).or(q.after_seq);

    // Subscribe *before* reading history. See the module note.
    let mut live = state.jod.subscribe();
    let history = state.jod.events_since(&id, after_seq).await?;

    let stream = async_stream::stream! {
        // `None` means nothing has been sent yet — which is why this is an
        // Option and not a 0, since `seq` 0 is a real event.
        let mut high: Option<u64> = after_seq;
        for envelope in history {
            high = advance(high, envelope.seq);
            yield Ok(frame(&envelope, true));
        }

        loop {
            match live.recv().await {
                Ok(envelope) => {
                    if envelope.agent_id != id || already_sent(high, envelope.seq) {
                        continue;
                    }
                    high = advance(high, envelope.seq);
                    yield Ok(frame(&envelope, true));
                }
                // The subscriber fell behind and the channel dropped messages.
                // Rather than silently lose them, re-read and carry on — this
                // is why `high` is tracked rather than assumed.
                Err(RecvError::Lagged(_)) => {
                    if let Ok(missed) = state.jod.events_since(&id, high).await {
                        for envelope in missed {
                            if already_sent(high, envelope.seq) {
                                continue;
                            }
                            high = advance(high, envelope.seq);
                            yield Ok(frame(&envelope, true));
                        }
                    }
                }
                Err(RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE)))
}

/// Every agent, live only. No `id:` — see the module note.
pub async fn all_agents_stream(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    identity.require(Scope::Read)?;
    let mut live = state.jod.subscribe();

    let stream = async_stream::stream! {
        loop {
            match live.recv().await {
                Ok(envelope) => yield Ok(frame(&envelope, false)),
                // Tell the client it has a hole rather than letting it believe
                // it has seen everything. It can backfill per agent.
                Err(RecvError::Lagged(n)) => {
                    yield Ok(Event::default()
                        .event("lagged")
                        .data(format!(r#"{{"missed":{n}}}"#)));
                }
                Err(RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE)))
}

/// Has this `seq` already gone out on this connection?
///
/// `None` means nothing has been sent yet, so nothing has been seen — which is
/// why the cursor is an `Option` and not a `0`. `seq` starts at 0, so a `0`
/// sentinel would suppress the first event of every run.
fn already_sent(high: Option<u64>, seq: u64) -> bool {
    high.is_some_and(|h| seq <= h)
}

fn advance(high: Option<u64>, seq: u64) -> Option<u64> {
    Some(high.map_or(seq, |h| h.max(seq)))
}

/// Render one envelope as an SSE frame.
///
/// `with_id` is false on the all-agents stream, where a per-agent `seq` would
/// be a cursor the server cannot honour on reconnect.
fn frame(envelope: &AgentEnvelope, with_id: bool) -> Event {
    let data = serde_json::to_string(envelope)
        .unwrap_or_else(|e| format!(r#"{{"kind":"error","message":"unserialisable event: {e}"}}"#));
    let event = Event::default().event("agent").data(data);
    if with_id {
        event.id(envelope.seq.to_string())
    } else {
        event
    }
}

/// Parse the reconnect cursor a browser sends automatically.
///
/// A malformed value is ignored rather than rejected: the fallback is replaying
/// from the start, which is correct if wasteful, whereas refusing the
/// connection would leave the client with no stream at all.
fn last_event_id(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::{AgentEvent, Usage};

    fn envelope(seq: u64) -> AgentEnvelope {
        AgentEnvelope {
            agent_id: "a".into(),
            at_ms: 7,
            seq,
            event: AgentEvent::Message {
                text: "hello".into(),
            },
        }
    }

    /// SSE frames are not directly inspectable, so assert on the wire bytes.
    fn wire(event: Event) -> String {
        String::from_utf8(axum::body::Bytes::from(format!("{event:?}")).to_vec())
            .unwrap_or_default()
    }

    #[test]
    fn a_per_agent_frame_carries_its_seq_as_the_sse_id() {
        // The id is what a reconnecting EventSource echoes back, so it must be
        // the seq and nothing else.
        let rendered = wire(frame(&envelope(42), true));
        assert!(
            rendered.contains("42"),
            "frame did not carry the seq: {rendered}"
        );
    }

    #[test]
    fn the_all_agents_frame_omits_the_id() {
        // If this regresses, a browser starts sending a meaningless
        // Last-Event-ID on the all-agents stream and replay silently skews.
        let with = format!("{:?}", frame(&envelope(42), true));
        let without = format!("{:?}", frame(&envelope(42), false));
        assert_ne!(with, without);
        assert!(
            !without.contains("id: 42") && !without.contains("Some(\"42\")"),
            "the all-agents frame carried an id: {without}"
        );
    }

    #[test]
    fn an_envelope_serialises_with_kind_flattened_alongside_its_fields() {
        // The web client branches on `kind` at the top level; if serde ever
        // nests the payload this breaks every consumer.
        let json = serde_json::to_value(envelope(1)).unwrap();
        assert_eq!(json["kind"], "message");
        assert_eq!(json["text"], "hello");
        assert_eq!(json["seq"], 1);
        assert_eq!(json["agent_id"], "a");
    }

    #[test]
    fn a_finished_envelope_carries_usage_for_the_cost_readout() {
        let e = AgentEnvelope {
            agent_id: "a".into(),
            at_ms: 0,
            seq: 9,
            event: AgentEvent::Finished {
                text: Some("done".into()),
                exit_code: Some(0),
                is_error: false,
                usage: Usage {
                    cost_usd: Some(0.25),
                    ..Default::default()
                },
            },
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["kind"], "finished");
        assert_eq!(json["usage"]["cost_usd"], 0.25);
    }

    #[test]
    fn a_fresh_connection_does_not_suppress_seq_zero() {
        // The regression this guards: `seq` starts at 0 and core's cursor is
        // strictly exclusive, so treating "no cursor" as 0 drops the first
        // event of every run — which is `started`, carrying session_id and
        // model. A HUD would show a run that never began.
        assert!(
            !already_sent(None, 0),
            "seq 0 was filtered on a fresh connection"
        );
        assert!(!already_sent(None, 1));
    }

    #[test]
    fn a_resumed_connection_skips_what_it_already_saw() {
        assert!(already_sent(Some(0), 0));
        assert!(already_sent(Some(5), 5));
        assert!(!already_sent(Some(5), 6));
    }

    #[test]
    fn the_cursor_only_ever_moves_forward() {
        assert_eq!(advance(None, 3), Some(3));
        assert_eq!(advance(Some(3), 7), Some(7));
        // Out-of-order delivery must not rewind the cursor and re-send.
        assert_eq!(advance(Some(7), 3), Some(7));
    }

    #[test]
    fn a_valid_last_event_id_is_parsed() {
        let mut h = HeaderMap::new();
        h.insert("last-event-id", "41".parse().unwrap());
        assert_eq!(last_event_id(&h), Some(41));
    }

    #[test]
    fn a_malformed_last_event_id_replays_from_the_start_rather_than_failing() {
        let mut h = HeaderMap::new();
        h.insert("last-event-id", "not-a-number".parse().unwrap());
        assert_eq!(last_event_id(&h), None);
    }

    #[test]
    fn no_last_event_id_header_is_no_cursor() {
        assert_eq!(last_event_id(&HeaderMap::new()), None);
    }
}
