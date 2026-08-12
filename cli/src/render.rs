//! Turning the event stream into something readable in a terminal.

use jod_core::service::{AgentStatus, AgentSummary, HarnessInfo, Report};
use jod_core::store::Origin;
use jod_core::{broadcast, AgentEnvelope, AgentEvent};

/// The first `n` *characters* of an id.
///
/// Slicing bytes here panics the moment an id is not pure ASCII, and task ids
/// are whatever the user typed.
fn short(id: &str, n: usize) -> String {
    id.chars().take(n).collect()
}

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

/// Colour only when stdout is a terminal, so piped output stays clean.
fn tty() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

fn paint(colour: &str, text: &str) -> String {
    if tty() {
        format!("{colour}{text}{RESET}")
    } else {
        text.to_string()
    }
}

pub fn harnesses(list: &[HarnessInfo]) {
    for h in list {
        let mark = if h.available {
            paint(GREEN, "✓")
        } else {
            paint(RED, "✗")
        };
        let where_ = h.path.as_deref().unwrap_or("not installed");
        println!("{mark} {:<14} {}", h.label, paint(DIM, where_));
    }
}

pub fn launched(agent: &AgentSummary) {
    println!("{} {}", paint(BOLD, "agent"), agent.id);
    println!("  name    {}", agent.name);
    println!("  harness {}", agent.harness_label);
    println!("  watch   {}", paint(CYAN, &agent.watch_command));
}

/// Say that a run is being watched, and name what would stop it.
///
/// On stderr, so it does not land in the middle of `--json` output — and it
/// names the daemon because a heartbeat with nothing sweeping it is a promise
/// that silently does not hold.
pub fn watching(run_id: &str, stall_ms: i64) {
    eprintln!(
        "{} watching {} — stopped if silent for {} minutes (needs `jod daemon`)",
        paint(DIM, "♥"),
        paint(BOLD, run_id),
        stall_ms / 60_000,
    );
}

pub fn launched_waiting(agent: &AgentSummary) {
    eprintln!(
        "{} {} {} {}",
        paint(DIM, "▸"),
        paint(BOLD, &agent.name),
        paint(DIM, &format!("({})", agent.harness_label)),
        paint(DIM, &format!("· {}", agent.watch_command)),
    );
}

/// Follow one agent until it finishes. Returns the process exit code to use.
pub async fn stream(
    events: broadcast::Receiver<AgentEnvelope>,
    agent_id: &str,
    json: bool,
    show_thinking: bool,
) -> i32 {
    stream_after(events, agent_id, None, json, show_thinking).await
}

/// As [`stream`], skipping anything at or before `last_seen`.
///
/// `jod watch` prints the run's history first and then goes live on the same
/// call; without a cursor the overlap between the two would be printed twice.
pub async fn stream_after(
    mut events: broadcast::Receiver<AgentEnvelope>,
    agent_id: &str,
    last_seen: Option<u64>,
    json: bool,
    show_thinking: bool,
) -> i32 {
    loop {
        let envelope = match events.recv().await {
            Ok(e) => e,
            // The service dropped the sender: nothing more is coming.
            Err(broadcast::error::RecvError::Closed) => return 1,
            // A slow consumer fell behind. Say so rather than pretending the
            // missing events never happened.
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!(
                    "{}",
                    paint(
                        YELLOW,
                        &format!("[jod] dropped {n} events — output fell behind")
                    )
                );
                continue;
            }
        };
        if envelope.agent_id != agent_id {
            continue;
        }
        if last_seen.is_some_and(|seen| envelope.seq <= seen) {
            continue; // already printed from history
        }
        print_envelope(&envelope, json, show_thinking);
        if let AgentEvent::Finished {
            is_error,
            exit_code,
            ..
        } = &envelope.event
        {
            return exit_status(*is_error, *exit_code);
        }
    }
}

/// Render one event, in whichever form was asked for.
pub fn print_envelope(envelope: &AgentEnvelope, json: bool, show_thinking: bool) {
    if json {
        if let Ok(line) = serde_json::to_string(envelope) {
            println!("{line}");
        }
    } else {
        print_event(&envelope.event, show_thinking);
    }
}

/// What `jod run` should exit with.
///
/// A harness can fail while still exiting 0 — AGY does exactly that when a tool
/// is auto-denied in headless mode. Trusting its exit code alone would report
/// success for work that never happened, so a run flagged as an error always
/// exits non-zero.
fn exit_status(is_error: bool, exit_code: Option<i32>) -> i32 {
    match (is_error, exit_code) {
        (true, Some(code)) if code != 0 => code,
        (true, _) => 1,
        (false, Some(code)) => code,
        (false, None) => 0,
    }
}

fn print_event(event: &AgentEvent, show_thinking: bool) {
    match event {
        AgentEvent::Started { model, .. } => {
            if let Some(m) = model {
                eprintln!("{}", paint(DIM, &format!("  model {m}")));
            }
        }
        AgentEvent::Thinking { text } => {
            if show_thinking {
                eprintln!("{}", paint(DIM, &indent(text, "  ")));
            }
        }
        AgentEvent::Message { text } => println!("{text}"),
        AgentEvent::ToolCall { name, .. } => {
            eprintln!("{} {}", paint(CYAN, "⚙"), paint(DIM, name));
        }
        AgentEvent::ToolResult { name, is_error, .. } => {
            if *is_error {
                eprintln!("{} {}", paint(RED, "✗"), paint(DIM, name));
            }
        }
        AgentEvent::Finished {
            is_error, usage, ..
        } => {
            let mut parts = vec![];
            if let Some(c) = usage.cost_usd {
                parts.push(format!("${c:.4}"));
            }
            if let Some(t) = usage.output_tokens {
                parts.push(format!("{t} out"));
            }
            let tail = if parts.is_empty() {
                String::new()
            } else {
                format!(" · {}", parts.join(" · "))
            };
            if *is_error {
                eprintln!("{}{}", paint(RED, "✗ failed"), paint(DIM, &tail));
            } else {
                eprintln!("{}{}", paint(GREEN, "✓ done"), paint(DIM, &tail));
            }
        }
        // An unrecognised harness line is shown, never dropped.
        AgentEvent::Raw { line } => eprintln!("{}", paint(DIM, line)),
        AgentEvent::Error { message } => eprintln!("{} {message}", paint(RED, "error")),
    }
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|l| format!("{prefix}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn agents(list: &[AgentSummary]) {
    if list.is_empty() {
        println!("{}", paint(DIM, "no agents"));
        return;
    }
    for a in list {
        let status = match a.status {
            AgentStatus::Running => paint(YELLOW, "running"),
            AgentStatus::Completed => paint(GREEN, "done"),
            AgentStatus::Failed => paint(RED, "failed"),
            AgentStatus::Killed => paint(DIM, "killed"),
        };
        println!(
            "{:<10} {:<9} {:<12} {}",
            short(&a.id, 8),
            status,
            a.harness_label,
            a.name
        );
    }
}

/// A team: who is on it, then what is on its board.
pub fn team(members: &[jod_core::team::Member], tasks: &[jod_core::team::TeamTask]) {
    use jod_core::team::MemberStatus;

    if members.is_empty() {
        println!("{}", paint(DIM, "no members"));
    }
    for m in members {
        let status = match m.status {
            MemberStatus::Ready => paint(GREEN, "ready"),
            MemberStatus::Busy => paint(YELLOW, "busy"),
            MemberStatus::Error => paint(RED, "error"),
            other => paint(DIM, other.as_str()),
        };
        println!(
            "{:<12} {:<9} {:<13} {}",
            m.name,
            status,
            m.harness.label(),
            m.role
        );
    }

    if tasks.is_empty() {
        return;
    }
    println!();
    for t in tasks {
        // Open / claimed / done, so progress reads at a glance.
        let mark = if t.is_done() {
            paint(GREEN, "done")
        } else if t.is_claimed() {
            paint(YELLOW, "taken")
        } else {
            paint(DIM, "open")
        };
        // The id in full, not a prefix: this board is the only place to learn
        // it, and `jod team claim` needs it exactly. A truncated id looked
        // copyable and was not.
        println!(
            "{:<10} {:<8} {}{}",
            t.id,
            mark,
            t.title,
            t.owner
                .as_ref()
                .map(|o| paint(DIM, &format!("  ({o})")))
                .unwrap_or_default()
        );
    }
}

pub fn history(runs: &[jod_core::store::StoredRun]) {
    if runs.is_empty() {
        println!("{}", paint(DIM, "nothing recorded yet"));
        return;
    }
    for r in runs {
        let when = chrono::DateTime::from_timestamp_millis(r.created_at_ms)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        println!(
            "{:<8} {} {:<10} {}",
            short(&r.id, 8),
            paint(DIM, &when),
            r.status,
            r.name
        );
    }
}

pub fn facts(list: &[jod_core::store::Fact]) {
    if list.is_empty() {
        println!("{}", paint(DIM, "nothing remembered about that"));
        return;
    }
    for f in list {
        // Anything Reljod did not assert himself is labelled. A fact read off a
        // web page reads exactly like one he stated, and storing the difference
        // is worthless if it is invisible at the point of use.
        let origin = match f.origin {
            Origin::Owner => String::new(),
            Origin::Untrusted => format!(" {}", paint(YELLOW, "[untrusted]")),
            Origin::Agent => format!(" {}", paint(DIM, "[agent]")),
            Origin::System => format!(" {}", paint(DIM, "[system]")),
        };
        println!(
            "{} {} {}{}",
            paint(BOLD, &f.subject),
            paint(DIM, &f.predicate),
            f.object,
            origin
        );
        if let Some(src) = &f.source {
            println!("  {}", paint(DIM, &format!("← {src}")));
        }
    }
}

// ---- the rail, the roots, the secrets and the works -----------------------

/// The rail, one card to a line.
///
/// Two facts lead every row because they are the two that decide whether to
/// read it: whether it stopped a run, and what kind of thing it is asking for.
pub fn cards(list: &[jod_core::cards::Card], now_ms: i64) {
    for c in list {
        println!(
            "{:<6} {:<9} {:<9} {:<12} {}{}",
            paint(DIM, &format!("#{}", c.id)),
            kind_of(c),
            importance_of(c),
            paint(DIM, &crate::render_time::when(c.created_at_ms, now_ms)),
            c.title,
            delivery_note(c),
        );
    }
}

/// One card in full: what it asks, what it offers, and who raised it.
pub fn card(c: &jod_core::cards::Card, now_ms: i64) {
    println!(
        "{} {} {} {}",
        paint(BOLD, &format!("#{}", c.id)),
        kind_of(c),
        importance_of(c),
        paint(DIM, &crate::render_time::when(c.created_at_ms, now_ms))
    );
    println!("{}", paint(BOLD, &c.title));
    if !c.body.is_empty() {
        println!("{}", indent(&c.body, "  "));
    }
    if !c.options.is_empty() {
        println!();
        for (i, option) in c.options.iter().enumerate() {
            // One-based, and the same numbering `jod card answer --option` and
            // the rail's digit keys use. Two numbering schemes for one list is
            // an answer landing on the wrong option.
            let mark = if c.chosen.as_deref() == Some(option.as_str()) {
                paint(GREEN, "●")
            } else {
                paint(DIM, "○")
            };
            println!("  {mark} {}. {option}", i + 1);
        }
    }
    if let Some(name) = &c.secret_name {
        // The name and the scope, which is the whole of what a secret card ever
        // shows. There is no value here to print.
        println!(
            "\n  variable {} · {} scope",
            paint(BOLD, name),
            c.secret_scope.as_deref().unwrap_or("work")
        );
    }
    if let Some(answer) = &c.answer {
        println!("\n  {} {answer}", paint(GREEN, "answered:"));
    }
    println!(
        "\n{}",
        paint(
            DIM,
            &format!(
                "{} · {} · raised by {}{}",
                c.status.as_str(),
                c.source.as_str(),
                c.run_id.as_deref().map(|r| short(r, 8)).unwrap_or_else(|| "—".into()),
                c.work_id
                    .as_deref()
                    .map(|w| format!(" · work {}", short(w, 8)))
                    .unwrap_or_default(),
            )
        )
    );
}

fn kind_of(c: &jod_core::cards::Card) -> String {
    use jod_core::cards::CardKind;
    // Blocking outranks the kind in the one column there is for it: the thing
    // that stopped a run is what you need to see from across the room.
    if c.blocking {
        return paint(RED, "blocked");
    }
    match c.kind {
        CardKind::Decision => paint(CYAN, "decision"),
        CardKind::Question => paint(YELLOW, "question"),
        CardKind::Secret => paint(BOLD, "secret"),
    }
}

fn importance_of(c: &jod_core::cards::Card) -> String {
    use jod_core::cards::Importance;
    match c.importance {
        Importance::High => paint(BOLD, "high"),
        Importance::Normal => paint(DIM, "normal"),
        Importance::Low => paint(DIM, "low"),
    }
}

/// Whether the agent has heard about an answer yet.
///
/// Shown because an answer is asynchronous and pretending otherwise would be a
/// lie somebody acts on: answer ten cards during a turn and all ten sit here
/// until it comes up for air.
fn delivery_note(c: &jod_core::cards::Card) -> String {
    use jod_core::cards::Delivery;
    match c.delivery {
        Delivery::None => String::new(),
        Delivery::Queued => paint(DIM, "  (answered, queued)"),
        Delivery::Delivered => paint(DIM, "  (delivered)"),
        Delivery::Undeliverable => paint(YELLOW, "  (never delivered)"),
    }
}

pub fn roots(list: &[jod_core::roots::Root]) {
    for r in list {
        // Writable is the exception and reads as one. A root is read-only until
        // a session claims a worktree, and a listing that made the two look
        // alike would hide the only line that matters here.
        let access = if r.writable {
            paint(GREEN, "writable")
        } else {
            paint(DIM, "read-only")
        };
        println!(
            "{:<10} {:<9} {}",
            access,
            paint(DIM, r.origin.as_str()),
            r.path.display()
        );
    }
}

/// Secrets, by name. There is deliberately nothing here that could reconstruct
/// a value — not the value, not a prefix, not a hash. Length and whether it is
/// long enough to redact, because those are what somebody needs to know.
pub fn secrets(list: &[jod_core::secrets::SecretMeta]) {
    for s in list {
        let redaction = if s.redactable {
            String::new()
        } else {
            paint(YELLOW, "  not redacted (too short)")
        };
        println!(
            "{:<28} {:<13} {:<8} {}{}",
            paint(BOLD, &s.name),
            paint(DIM, s.scope.as_str()),
            paint(DIM, &format!("{} ch", s.length)),
            s.hint,
            redaction
        );
    }
}

pub fn works(list: &[jod_core::works::Work], now_ms: i64) {
    for w in list {
        println!(
            "{:<10} {:<10} {:<12} {}",
            short(&w.id, 8),
            work_state(w.state),
            paint(DIM, &crate::render_time::when(w.updated_at_ms, now_ms)),
            w.title
        );
    }
}

fn work_state(state: jod_core::works::State) -> String {
    use jod_core::works::State;
    match state {
        State::Open => paint(YELLOW, "open"),
        // Not the same as closed, and shown as its own thing: the board is
        // empty but an agent is still mid-turn, and only one of those is safe
        // to act on.
        State::Finishing => paint(CYAN, "finishing"),
        State::Closed => paint(DIM, "closed"),
    }
}

/// One work: what it is, who is on it, what is left, and what it holds on disk.
pub fn work(
    w: &jod_core::works::Work,
    sessions: &[jod_core::works::Session],
    // `(open, blocking)` per session, in the same order.
    cards: &[(usize, usize)],
    tasks: &[jod_core::team::TeamTask],
    leases: &[jod_core::leases::Lease],
    now_ms: i64,
) {
    println!("{} {}", paint(BOLD, &w.title), work_state(w.state));
    if !w.summary.is_empty() {
        println!("{}", paint(DIM, &w.summary));
    }
    println!("{}", paint(DIM, &format!("{} · {}", short(&w.id, 8), w.colour)));

    println!("\n{}", paint(BOLD, "sessions"));
    for (i, s) in sessions.iter().enumerate() {
        let (open, blocking) = cards.get(i).copied().unwrap_or((0, 0));
        // The card count is why this listing exists: the tree is where a
        // question hides, and a work with a blocker in a leaf reads as busy.
        let waiting = match (open, blocking) {
            (0, _) => String::new(),
            (open, 0) => paint(DIM, &format!("  {open} card(s)")),
            (open, blocked) => paint(RED, &format!("  {open} card(s), {blocked} blocked")),
        };
        println!(
            "  {:<10} {:<9} {:<12} {}{}",
            short(&s.conversation_id, 8),
            if s.running {
                paint(YELLOW, "running")
            } else {
                paint(DIM, "idle")
            },
            paint(DIM, s.origin.as_str()),
            if s.name.is_empty() { &s.title } else { &s.name },
            waiting
        );
    }

    if !tasks.is_empty() {
        println!("\n{}", paint(BOLD, "board"));
        for t in tasks {
            let mark = if t.is_done() {
                paint(GREEN, "done")
            } else if t.is_claimed() {
                paint(YELLOW, "taken")
            } else {
                paint(DIM, "open")
            };
            println!("  {:<8} {}", mark, t.title);
        }
    }

    if !leases.is_empty() {
        println!("\n{}", paint(BOLD, "worktrees"));
        for l in leases {
            println!(
                "  {:<9} {:<24} {}",
                paint(DIM, l.state.as_str()),
                l.branch,
                l.worktree_path.display()
            );
        }
    }
    println!(
        "\n{}",
        paint(DIM, &format!("opened {}", crate::render_time::when(w.created_at_ms, now_ms)))
    );
}

/// Every claimed worktree, with what git says about it *now*.
pub fn leases(
    list: &[jod_core::leases::Lease],
    conditions: &[jod_core::leases::Condition],
    now_ms: i64,
) {
    for (i, l) in list.iter().enumerate() {
        let condition = conditions.get(i);
        // The two words that decide whether this is safe to remove, and they
        // are read from git at the moment of asking rather than from the row.
        let state = match condition {
            Some(c) if c.missing => paint(DIM, "gone"),
            Some(c) if c.dirty => paint(RED, "dirty"),
            Some(c) if !c.merged => paint(YELLOW, "unmerged"),
            Some(_) => paint(GREEN, "clean"),
            None => paint(DIM, "—"),
        };
        println!(
            "{:<10} {:<9} {:<24} {}",
            paint(DIM, l.state.as_str()),
            state,
            l.branch,
            l.worktree_path.display()
        );
        println!(
            "{}",
            paint(
                DIM,
                &format!(
                    "           {} · {} · claimed {}",
                    // An orphaned lease still says what it was for, which is
                    // what makes it safe for somebody to act on.
                    l.work_id
                        .as_deref()
                        .map(|w| short(w, 8))
                        .unwrap_or_else(|| "work deleted".into()),
                    l.work_title,
                    crate::render_time::when(l.created_at_ms, now_ms)
                )
            )
        );
    }
}

pub fn report(r: &Report) {
    println!("running   {}", r.running);
    println!("completed {}", r.completed);
    println!("failed    {}", r.failed);
    println!("killed    {}", r.killed);
    // Rust's f64 sum uses -0.0 as its identity, so an empty total would print
    // as "$-0.0000". Adding 0.0 turns -0.0 back into 0.0 and leaves every
    // other value alone.
    println!("spend     ${:.4}", r.total_cost_usd + 0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indenting_prefixes_every_line_not_just_the_first() {
        assert_eq!(indent("a\nb", "> "), "> a\n> b");
    }

    /// Regression: AGY exits 0 when it auto-denies a tool, so an errored run
    /// was reporting success to the shell.
    #[test]
    fn a_failed_run_never_exits_zero_even_when_the_harness_did() {
        assert_eq!(exit_status(true, Some(0)), 1);
        assert_eq!(exit_status(true, None), 1);
    }

    #[test]
    fn a_failing_harness_keeps_its_own_exit_code() {
        assert_eq!(exit_status(true, Some(42)), 42);
    }

    #[test]
    fn a_clean_run_exits_zero() {
        assert_eq!(exit_status(false, Some(0)), 0);
        assert_eq!(exit_status(false, None), 0);
    }

    #[test]
    fn colour_is_omitted_when_output_is_not_a_terminal() {
        // The test harness captures stdout, so `tty()` is false here.
        assert_eq!(paint(RED, "plain"), "plain");
    }
}
