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
//!
//! ## The fourth way out: refusing to wait
//!
//! A run that hands work to other agents must never sit and watch for the
//! answer. It arrives on its own, as a card. `--refuse-waiting` turns on one
//! extra check in front of everything above: a `Bash` call that sleeps or polls
//! is denied outright, and the refusal says why so the model reads a rule
//! rather than a malfunction.
//!
//! **This is Claude Code only, and that is a limit rather than an oversight.**
//! `Bash` belongs to the harness, not to Jod, so Jod's MCP server never sees
//! the call and has nothing to refuse. A `PreToolUse` hook is the only place in
//! the run where Jod gets a say about it. OpenCode and AGY have no equivalent
//! hook, so for those two the same rule is preamble wording and nothing more.
//! Main runs on Claude Code in practice, which is why the narrow fix is still
//! worth having.

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
/// All three are `jod_core`'s, not this file's: the *answer* is what acts —
/// `ALWAYS` writes the grant from whichever surface answered it, whether or not
/// this process is still waiting, and the rail's quick answer picks `ONCE` by
/// name. So the text offered here and the text acted on there have to be one
/// string.
use jod_core::approvals::{ALWAYS, DENY, ONCE};

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
///
/// `refuse_waiting` and `never_ask` are baked in for the same reason: which
/// access level the run holds and which permission mode it is under are the
/// launcher's knowledge, and this process has no way back to either.
pub async fn hook(
    jod: Arc<Jod>,
    run_id: Option<String>,
    wait: u64,
    refuse_waiting: bool,
    never_ask: bool,
) -> Result<()> {
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

    // Ahead of the grants, deliberately. A standing grant is somebody having
    // said "yes, run that" once; it is not permission to stop the turn. If the
    // order were the other way round, one `Bash(sleep:*)` grant would switch
    // this rule off for every run that ever holds it.
    //
    // Ahead of the store, too, so a refusal that needs no database still lands
    // when the database is unreadable.
    if refuse_waiting && tool == "Bash" && waits(&subject) {
        deny(WAITING_REFUSAL);
        return Ok(());
    }
    // Nobody to ask. The run is in a mode that never stops for a question, and
    // this hook is installed only for the refusal above — raising a card here
    // would put the block back exactly where it was taken out, one tool call at
    // a time.
    if never_ask {
        return Ok(());
    }

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

/// What the model is told when a waiting command is refused.
///
/// It names the reason, because a refusal the model cannot explain is one it
/// works around. Told only "denied", a model retries the same idea in a
/// different shell; told that the answer is coming to it, it returns and reads
/// the answer. The last sentence is the instruction — everything before it is
/// why that instruction is not a punishment.
const WAITING_REFUSAL: &str = "Jod refuses waiting commands for a run that can hand work to \
                               other agents. `sleep`, `until` and `while` hold this turn open \
                               watching for something that arrives on its own: anything you \
                               delegated comes back later as a card on the rail, not from a \
                               shell, and nothing you can poll here will tell you sooner. Hand \
                               the work over and return now — say what you started, and stop. If \
                               you were waiting on something local instead, run the check once \
                               and report what it said.";

/// The words that mean "stop here and watch".
///
/// `sleep` is the whole of it in practice. `until` and `while` are how the same
/// poll gets written when a bare sleep feels too blunt — the recorded instance
/// this rule exists for is
/// `until [ "$(jod agents --json | grep -c <id>)" = "0" ]; do sleep 5; done`,
/// forty-two seconds and thirty-nine cents spent to learn nothing.
const WAITING_WORDS: [&str; 3] = ["sleep", "until", "while"];

/// Shell words that introduce another command rather than being one.
///
/// After `do` comes the body of a loop, after `then` the body of a branch, and
/// the word that follows is in command position just as much as the first word
/// of the line. Without this, `for f in *; do sleep 1; done` reads as a `for`
/// and nothing else.
const CONTINUES: [&str; 6] = ["do", "then", "else", "elif", "if", "!"];

/// Whether this `Bash` command waits.
///
/// **The matcher is the whole decision, so here is the reasoning.** It matches
/// a word only in *command position* — the first word of the command line, and
/// the first word after every operator that starts another command (`;`, `&`,
/// `|`, a newline, an opening parenthesis) or after one of [`CONTINUES`]. It
/// compares the last path segment of that word, so `/bin/sleep` counts and
/// `./sleep_test.py` does not.
///
/// The alternative was matching the word anywhere in the string, and it is the
/// worse failure. Too narrow a matcher leaves the poll loop running, which is
/// the bug staying unfixed. Too broad a matcher denies `grep sleep log.txt`,
/// `python sleep_test.py` and `cat notes/sleep.md` — ordinary work, refused for
/// containing five letters — and a rule that fires on those does not read as a
/// rule. It reads as Jod being broken, and the model's correct response to a
/// broken tool is to route around it. So the matcher is the narrow one, and it
/// covers every shape of the loop that was actually observed.
///
/// Quotes are honoured while scanning, so a separator inside `echo "a; sleep"`
/// is text rather than the start of a command. The word itself is compared with
/// its quotes stripped, because `"sleep" 5` does sleep.
///
/// It does not chase evasion. A command that reaches a wait through `eval`, a
/// variable or a script written a line earlier gets through, and that is
/// deliberate: at that point the model is not tripping over the rule, it is
/// working around one it has read, and the answer to that is the preamble and
/// the transcript rather than a longer regex.
fn waits(command: &str) -> bool {
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    // The next word starts a command. True at the beginning of the line and
    // after every operator that ends one.
    let mut at_command = true;

    /// End the word being read, and say whether it was one that waits.
    fn close(word: &mut String, at_command: &mut bool) -> bool {
        let done = std::mem::take(word);
        if done.is_empty() || !*at_command {
            return false;
        }
        let name = done.rsplit('/').next().unwrap_or(&done);
        if WAITING_WORDS.contains(&name) {
            return true;
        }
        // `TZ=UTC sleep 5` is a sleep. An assignment in command position is a
        // prefix to the command, not the command, so the next word is still the
        // one that runs. Recognised by shell's own rule: a name, then `=`,
        // before any slash.
        let assignment = done
            .split_once('=')
            .is_some_and(|(name, _)| !name.is_empty() && !name.contains('/'));
        if !assignment && !CONTINUES.contains(&done.as_str()) {
            *at_command = false;
        }
        false
    }

    for c in command.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
            continue;
        }
        match c {
            '\\' => escaped = true,
            '\'' | '"' => quote = Some(c),
            ';' | '&' | '|' | '\n' | '(' | ')' => {
                if close(&mut word, &mut at_command) {
                    return true;
                }
                at_command = true;
            }
            c if c.is_whitespace() => {
                if close(&mut word, &mut at_command) {
                    return true;
                }
            }
            c => word.push(c),
        }
    }
    close(&mut word, &mut at_command)
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

    /// The two commands that were actually recorded, from one turn of main's
    /// that spent forty-two seconds and thirty-nine cents learning nothing.
    #[test]
    fn the_loop_that_blocked_main_is_refused() {
        assert!(waits("sleep 45; echo waited"));
        assert!(waits(
            r#"until [ "$(jod agents --json | grep -c a2e4f620)" = "0" ]; do sleep 5; done"#
        ));
    }

    #[test]
    fn a_poll_written_as_a_while_loop_is_refused_too() {
        assert!(waits("while true; do jod agents; sleep 2; done"));
        // The wait need not be first. Anything after `&&`, `;` or `|` starts a
        // command of its own.
        assert!(waits("jod agents && sleep 10"));
        assert!(waits("echo starting; sleep 10"));
        // A loop body reached through `do`, whose head is not itself a wait.
        assert!(waits("for i in 1 2 3; do sleep 1; done"));
        // A wait inside a substitution still runs.
        assert!(waits("echo $(sleep 5)"));
        // A path to the same program is the same program.
        assert!(waits("/bin/sleep 5"));
        // An environment assignment is a prefix, not the command.
        assert!(waits("TZ=UTC sleep 5"));
    }

    /// **The false-positive guard, and it is the half that costs more to get
    /// wrong.** Five letters in a filename, a pattern or a path are not a wait,
    /// and an agent refused those cannot do its job — which reads as Jod being
    /// broken rather than as a rule, and the right response to a broken tool is
    /// to route around it.
    #[test]
    fn an_ordinary_command_that_merely_says_sleep_is_allowed() {
        assert!(!waits("ls"));
        assert!(!waits("grep -rn sleep core/src"));
        assert!(!waits("python sleep_test.py"));
        assert!(!waits("cat docs/sleep.md"));
        assert!(!waits("./scripts/sleep_test.sh --all"));
        assert!(!waits("cargo test --workspace -- sleep"));
        // A separator inside quotes is text, not the start of a command.
        assert!(!waits(r#"echo "first; sleep later""#));
        assert!(!waits(r#"git commit -m "stop the sleep loop""#));
        // `while` and `until` as arguments, not as the shape of the command.
        assert!(!waits("grep -c while build.log"));
        assert!(!waits("rg 'until' docs/"));
        // The escaped semicolon `find` needs is part of the argument.
        assert!(!waits(r"find . -name '*.log' -exec grep -l sleep {} \;"));
    }

    /// A denial the model cannot explain is one it works around. This asserts
    /// the refusal carries the reason and the instruction, not just a verdict.
    #[test]
    fn the_refusal_says_why_and_what_to_do_instead() {
        assert!(WAITING_REFUSAL.contains("card"));
        assert!(WAITING_REFUSAL.contains("delegated"));
        assert!(WAITING_REFUSAL.contains("return now"));
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
