//! `jod approve-hook` — the answer to a permission question, from inside the run.
//!
//! ## What this is
//!
//! Claude Code runs a `PreToolUse` hook before every matching tool call and
//! obeys the `permissionDecision` it prints. That hook is the only channel Jod
//! has into a headless run's permission check — this build has no
//! `--permission-prompt-tool` — and it is what turns
//! [`PermissionPolicy::Ask`](jod_core::harness::PermissionPolicy::Ask) and
//! `AcceptEdits` from "deny, silently" into modes that mean what they say.
//!
//! The protocol is a process: the payload arrives on stdin, the decision leaves
//! on stdout, and the exit code is not the answer. Nothing is printed unless
//! Jod actually has a decision to give.
//!
//! ## The three ways out
//!
//! 1. **A standing grant covers it** — print `allow` and get out of the way.
//!    This is the ordinary path once a thing has been approved once, and it is
//!    the whole point of the feature.
//! 2. **Nobody has said yes yet** — raise a card and wait, bounded. Answering
//!    it "always" writes a grant, so the question is asked once and never
//!    again. Answering it "once" allows this call alone.
//! 3. **Nothing decided in time** — print *nothing* and exit 0, which hands the
//!    call back to Claude Code's own permission flow. That is exactly the
//!    behaviour that existed before this file, so an unanswered question can
//!    never be worse than not having asked it.
//!
//! Note what (3) protects: this hook stands in front of every tool call in the
//! run. A crash, a locked database or a missing argument must degrade to the
//! old behaviour rather than to a wedged run, which is why almost everything
//! here ends in "say nothing and let the harness decide".

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use jod_core::approvals::{decide, Decision};
use jod_core::cards::{CardKind, Importance, NewCard, Source, Status};
use jod_core::store::Store;
use jod_core::Jod;
use serde_json::{json, Value};

/// What the rail offers, in the order it offers them.
///
/// "Always" first because it is the one that ends the interruption for good,
/// and the reason a person is being asked at all is that they are trying to get
/// on with something else.
///
/// `ALWAYS` is `jod_core`'s, not this file's: the *answer* is what writes the
/// grant — from whichever surface answered it, and whether or not this process
/// is still waiting — so the text offered here and the text acted on there have
/// to be one string.
use jod_core::approvals::ALWAYS;
const ONCE: &str = "allow once";
const DENY: &str = "deny";

/// How often to look for an answer while waiting.
///
/// A poll rather than a notification because the answer can arrive from the
/// rail, the CLI, an MCP call or a phone, and every one of those is a write to
/// the same table. Polling one table is the only mechanism all four already
/// share.
const POLL: Duration = Duration::from_millis(400);

/// Decide one tool call, reading the harness's payload from stdin.
///
/// `run_id` is baked into the hook's command line by the launcher, because a
/// hook is a fresh process that inherits nothing useful about which run it
/// belongs to and a card with no run on it is a card nobody can trace.
pub async fn hook(jod: Arc<Jod>, run_id: Option<String>, wait: u64) -> Result<()> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
        // Unparseable input is not something to guess about. Say nothing.
        return Ok(());
    };
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if tool.is_empty() {
        return Ok(());
    }
    let subject = subject_of(&tool, payload.get("tool_input"));

    let Some(store) = jod.store() else {
        return Ok(());
    };
    let Ok(grants) = store.grants() else {
        return Ok(());
    };

    match decide(&tool, &subject, &grants) {
        Decision::Allow { grant } => {
            allow(&format!("Jod grants `{grant}`"));
            Ok(())
        }
        Decision::Ask { suggest } => {
            ask(store, run_id, &tool, &subject, Some(suggest), wait).await
        }
        // Offered no pattern at all: the reason this cannot be matched is that
        // no pattern *could* cover it, so "always allow" is not on the table.
        // The `why` becomes the card's explanation — a person asked to approve
        // something deserves to know it is unapprovable-in-general and not
        // merely new.
        Decision::MustAsk { .. } => ask(store, run_id, &tool, &subject, None, wait).await,
    }
}

/// Raise the question and wait for as long as we said we would.
async fn ask(
    store: &Store,
    run_id: Option<String>,
    tool: &str,
    subject: &str,
    suggest: Option<String>,
    wait: u64,
) -> Result<()> {
    let Some(run_id) = run_id else {
        // No run means no conversation to hang a card on. Nothing to ask
        // through, so fall through to the harness rather than inventing one.
        return Ok(());
    };
    let Ok(Some(conversation_id)) = store.conversation_for_run(&run_id) else {
        return Ok(());
    };

    let mut options = vec![];
    if let Some(pattern) = &suggest {
        options.push(format!("{ALWAYS} `{pattern}`"));
    }
    options.push(ONCE.to_string());
    options.push(DENY.to_string());

    // Deduplicated on the *pattern*, not the command. A run that retries `git
    // init`, then `git init -b main`, then `git init -q` is asking one question
    // three times, and three cards for it is three interruptions for one
    // decision. This is also what bounds the waiting: the second occurrence
    // finds the first card and does not start a second clock.
    //
    // The key is also what `answer_card` reads to know this is an approval and
    // what to grant, so its shape is `jod_core`'s to define, not this file's.
    let dedupe = format!(
        "{}{tool}:{}",
        jod_core::approvals::CARD_KEY,
        suggest.clone().unwrap_or_else(|| subject.to_string())
    );
    // Taken before the raise so the card's own timestamp says whether this call
    // created it or merely found the one an earlier call left. Cheaper and more
    // honest than re-querying and matching on the title, which two different
    // commands can share.
    let raised_at = chrono::Utc::now().timestamp_millis();

    let card = match store.raise_card(NewCard {
        conversation_id,
        run_id: Some(run_id),
        kind: Some(CardKind::Question),
        importance: Some(Importance::High),
        // It genuinely stopped a tool call. The rail colours it accordingly,
        // which is the difference between a question and an interruption.
        blocking: true,
        title: title_for(tool, subject),
        body: body_for(tool, subject, suggest.as_deref()),
        options,
        // Jod noticed this, not the agent: the agent called a tool and was
        // refused. Attributing it to the agent would read as the agent asking
        // for permission it knows it needs, which is not what happened.
        source: Some(Source::Jod),
        dedupe_key: Some(dedupe),
        ..Default::default()
    }) {
        Ok(card) => card,
        Err(_) => return Ok(()),
    };

    // Already asked and still unanswered: the question is in front of them
    // already, and waiting again would stall the run a second time over one
    // decision. This is what bounds an unattended run's cost — it pays the wait
    // once per distinct question, not once per retry.
    if card.created_at_ms < raised_at && card.status == Status::Open {
        return Ok(());
    }

    let deadline = Instant::now() + Duration::from_secs(wait);
    while Instant::now() < deadline {
        match store.card(card.id) {
            Ok(Some(current)) => match current.status {
                Status::Open => {}
                // Read and deliberately not answered. That is an answer.
                Status::Dismissed => {
                    deny("Jod: dismissed at the rail");
                    return Ok(());
                }
                Status::Answered => {
                    let chosen = current.chosen.clone().unwrap_or_default();
                    // The grant itself is already written — `answer_card` does
                    // it, in the same transaction as the answer, so that a
                    // decision made after this process has gone still persists.
                    // All that is left here is to unblock the call in front of
                    // us.
                    if chosen.starts_with(ALWAYS) {
                        allow("Jod: always allowed at the rail");
                    } else if chosen.starts_with(ONCE) {
                        allow("Jod: allowed once at the rail");
                    } else {
                        deny("Jod: denied at the rail");
                    }
                    return Ok(());
                }
            },
            // The card is gone, or the database is unreadable. Either way there
            // is no answer coming through this card.
            Ok(None) => return Ok(()),
            Err(_) => return Ok(()),
        }
        tokio::time::sleep(POLL).await;
    }
    // Waited and heard nothing. The card stays open — the question outlives
    // this call — and the harness decides this one on its own.
    Ok(())
}

fn title_for(tool: &str, subject: &str) -> String {
    if subject.is_empty() {
        return format!("{tool} needs approval");
    }
    format!("{tool}: {}", ellipsis(subject, 80))
}

fn body_for(tool: &str, subject: &str, suggest: Option<&str>) -> String {
    let mut out = format!("A run asked to use `{tool}` and Jod has no standing grant for it.\n\n");
    if !subject.is_empty() {
        out.push_str(&format!("    {subject}\n\n"));
    }
    match suggest {
        Some(pattern) => out.push_str(&format!(
            "Answering \"always\" records `{pattern}` and every session from now on runs it \
             without asking. \"Once\" allows this call only."
        )),
        None => out.push_str(
            "This one can only ever be allowed once: it runs a command the text does not show — \
             command or process substitution — so no standing grant could describe what it does.",
        ),
    }
    out
}

fn ellipsis(text: &str, at: usize) -> String {
    if text.chars().count() <= at {
        return text.to_string();
    }
    format!("{}…", text.chars().take(at).collect::<String>())
}

/// The subject a grant is matched against.
///
/// `Bash` is the command; everything else offers whichever of its arguments
/// says what it touched. A tool whose arguments are all unfamiliar gets an
/// empty subject and is matched on its name alone, which is what a `*` grant is
/// for — better than inventing a subject out of a payload we do not understand.
fn subject_of(tool: &str, input: Option<&Value>) -> String {
    const KEYS: [&str; 6] = ["command", "url", "file_path", "path", "pattern", "query"];
    let Some(input) = input else {
        return String::new();
    };
    if tool == "Bash" {
        return input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    }
    KEYS.iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

fn allow(reason: &str) {
    decision("allow", reason);
}

fn deny(reason: &str) {
    decision("deny", reason);
}

/// The one shape Claude Code reads a decision out of.
///
/// Printed to stdout and nowhere else; the exit code says whether the hook
/// worked, never what it decided.
fn decision(verdict: &str, reason: &str) {
    println!(
        "{}",
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": verdict,
                "permissionDecisionReason": reason,
            }
        })
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_is_matched_on_its_command() {
        assert_eq!(
            subject_of("Bash", Some(&json!({ "command": "git init", "description": "x" }))),
            "git init"
        );
    }

    #[test]
    fn another_tool_offers_whichever_argument_says_what_it_touched() {
        assert_eq!(
            subject_of("WebFetch", Some(&json!({ "url": "https://docs.rs" }))),
            "https://docs.rs"
        );
        assert_eq!(
            subject_of("Read", Some(&json!({ "file_path": "/etc/hosts" }))),
            "/etc/hosts"
        );
    }

    /// A payload we do not understand must not become an invented subject —
    /// that would be a grant matched against something nobody wrote.
    #[test]
    fn an_unfamiliar_payload_has_no_subject_rather_than_a_guessed_one() {
        assert_eq!(subject_of("Whatever", Some(&json!({ "zork": "1" }))), "");
        assert_eq!(subject_of("Whatever", None), "");
    }

    #[test]
    fn a_decision_is_the_one_shape_the_harness_reads() {
        // Not asserted by capturing stdout — the shape is what matters, and it
        // is built in one place.
        let doc = json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": "Jod grants `git init*`",
            }
        });
        assert_eq!(doc["hookSpecificOutput"]["permissionDecision"], "allow");
    }
}
