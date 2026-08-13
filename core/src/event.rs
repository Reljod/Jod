//! The one event vocabulary every harness is normalised into.
//!
//! Claude Code and OpenCode emit completely different JSONL. Everything above
//! this module — the desktop UI, a future iOS client, Jod's own orchestration —
//! only ever sees `AgentEvent`, so adding a third harness never reaches them.

use serde::{Deserialize, Serialize};

/// Token/cost accounting for one agent turn. Fields are optional because the
/// harnesses report different subsets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        *self == Usage::default()
    }
}

/// A single normalised thing that happened inside an agent run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The harness booted and told us which session/model it is using.
    Started {
        session_id: Option<String>,
        model: Option<String>,
    },
    /// Reasoning/thinking text, when the harness surfaces it.
    Thinking { text: String },
    /// The harness is mid-turn and has nothing renderable yet.
    ///
    /// Not decoration. A turn that reasons for nine minutes before its next
    /// tool call emits *nothing else* — no text, no thinking block, no tool —
    /// and a UI with nothing to draw is indistinguishable from a UI watching a
    /// process that died. Observed exactly that way: a `jod tui` transcript
    /// froze on a tool result from second 7 while the status bar counted to
    /// `working 4m49s` on a bare spinner. This is the only thing on the wire in
    /// that window, so it is the only thing that can say "still working".
    ///
    /// Deliberately carries no text. It is a tick, not content: it belongs in a
    /// status line rather than the transcript, and
    /// [`crate::conversation::NewMessage::from_event`] drops it so replaying a
    /// thread into another harness does not replay a heartbeat.
    Progress {
        /// Reasoning tokens produced so far this turn, when the harness counts
        /// them. Optional so a harness that can only say "still here" — or a
        /// future build that renames its counter — still produces the tick
        /// rather than falling silent again.
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking_tokens: Option<u64>,
    },
    /// A fragment of a content block the harness has not finished emitting.
    ///
    /// `--include-partial-messages` is what puts this on the wire at all.
    /// Without it, a block that takes a while to finish produces *nothing*
    /// until it is complete — and "a while" is not always reasoning.
    /// [`Progress`](AgentEvent::Progress) covers a long think; this covers a
    /// long *write*. Observed live: a `jod tui` transcript froze for six
    /// minutes, and what had actually happened in that window, recovered
    /// afterwards, was one assistant turn carrying seven `Write` tool calls in
    /// a row — each one's `content` argument a whole file, streamed as
    /// `input_json_delta`. `thinking_tokens` cannot tick through that: the
    /// model was not reasoning, it was emitting. This is the only thing on the
    /// wire during that window.
    ///
    /// Carries the raw fragment — the incremental piece, not the running
    /// total — whether it came from prose (`text_delta`) or a tool call's
    /// arguments building up (`input_json_delta`). Both duplicate content that
    /// reappears complete in the `Message`/`ToolCall` fired once the block
    /// finishes, so this is deliberately not a substitute for either: a
    /// consumer that only wants the finished form can ignore it, and
    /// [`crate::conversation::NewMessage::from_event`] does exactly that — a
    /// thread replayed into another harness must not replay every fragment a
    /// second time.
    Delta { text: String },
    /// Assistant prose addressed to the caller.
    Message { text: String },
    /// The agent invoked a tool.
    ToolCall {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
    /// A tool came back.
    ToolResult {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        is_error: bool,
    },
    /// The run ended. `text` is the final answer when the harness gives one.
    Finished {
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        is_error: bool,
        usage: Usage,
    },
    /// Something the harness said that we could not classify. Kept rather than
    /// dropped so a harness upgrade degrades to "shown verbatim", never to
    /// "silently swallowed".
    Raw { line: String },
    /// The runner itself failed (spawn error, unreadable stream, killed).
    Error { message: String },
}

/// An `AgentEvent` stamped with who emitted it and when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEnvelope {
    pub agent_id: String,
    /// Milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// Monotonic per-agent sequence number, so a late-joining UI can resume.
    pub seq: u64,
    #[serde(flatten)]
    pub event: AgentEvent,
}

/// Truncate long tool payloads so the UI stream stays readable and the event
/// log does not balloon with whole file contents.
pub(crate) fn summarize(value: &serde_json::Value, max: usize) -> String {
    let raw = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let trimmed = raw.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{head}… (+{} chars)", trimmed.chars().count() - max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_passes_short_strings_through() {
        let v = serde_json::json!("hello");
        assert_eq!(summarize(&v, 10), "hello");
    }

    #[test]
    fn summarize_truncates_and_reports_remainder() {
        let v = serde_json::json!("abcdefghij");
        assert_eq!(summarize(&v, 4), "abcd… (+6 chars)");
    }

    #[test]
    fn summarize_renders_non_strings_as_json() {
        let v = serde_json::json!({"a": 1});
        assert_eq!(summarize(&v, 100), "{\"a\":1}");
    }

    /// The tick has to survive the wire, because the clients that most need it
    /// are the ones on the far side of it — `api/src/sse.rs` serialises these
    /// straight to the desktop, iOS and web apps, and a liveness signal that
    /// does not serialise is a spinner again.
    #[test]
    fn a_progress_tick_survives_the_wire() {
        let e = AgentEvent::Progress {
            thinking_tokens: Some(1408),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(s, r#"{"kind":"progress","thinking_tokens":1408}"#);
        assert_eq!(serde_json::from_str::<AgentEvent>(&s).unwrap(), e);

        // And without a count, which is what a harness that only says "still
        // here" sends. The field disappears rather than becoming a null.
        let bare = AgentEvent::Progress {
            thinking_tokens: None,
        };
        let s = serde_json::to_string(&bare).unwrap();
        assert_eq!(s, r#"{"kind":"progress"}"#);
        assert_eq!(serde_json::from_str::<AgentEvent>(&s).unwrap(), bare);
    }

    /// The same wire requirement as the progress tick, for the same reason:
    /// `api/src/sse.rs` serialises this straight to every client, so a
    /// streaming fragment that does not survive `serde_json` is silence again
    /// by the time it would reach a UI.
    #[test]
    fn a_delta_fragment_survives_the_wire() {
        let e = AgentEvent::Delta {
            text: "Cr".into(),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(s, r#"{"kind":"delta","text":"Cr"}"#);
        assert_eq!(serde_json::from_str::<AgentEvent>(&s).unwrap(), e);
    }

    #[test]
    fn event_round_trips_through_json() {
        let e = AgentEvent::ToolCall {
            name: "Bash".into(),
            input: Some(serde_json::json!({"command": "ls"})),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&s).unwrap(), e);
    }
}
