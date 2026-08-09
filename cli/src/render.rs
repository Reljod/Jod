//! Turning the event stream into something readable in a terminal.

use jod_core::service::{AgentStatus, AgentSummary, HarnessInfo, Report};
use jod_core::store::Origin;
use jod_core::{broadcast, AgentEnvelope, AgentEvent};

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
    println!("  watch   {}", paint(CYAN, &agent.attach_command));
}

pub fn launched_waiting(agent: &AgentSummary) {
    eprintln!(
        "{} {} {} {}",
        paint(DIM, "▸"),
        paint(BOLD, &agent.name),
        paint(DIM, &format!("({})", agent.harness_label)),
        paint(DIM, &format!("· {}", agent.attach_command)),
    );
}

/// Follow one agent until it finishes. Returns the process exit code to use.
pub async fn stream(
    mut events: broadcast::Receiver<AgentEnvelope>,
    agent_id: &str,
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
        if json {
            if let Ok(line) = serde_json::to_string(&envelope) {
                println!("{line}");
            }
        } else {
            print_event(&envelope.event, show_thinking);
        }
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
            &a.id[..a.id.len().min(8)],
            status,
            a.harness_label,
            a.name
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
            &r.id[..r.id.len().min(8)],
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

    // --- following a run -------------------------------------------------

    use jod_core::event::Usage;
    use jod_core::harness::HarnessKind;
    use jod_core::service::AgentStatus;

    fn env(agent_id: &str, seq: u64, event: AgentEvent) -> AgentEnvelope {
        AgentEnvelope {
            agent_id: agent_id.into(),
            at_ms: 0,
            seq,
            event,
        }
    }

    fn finished(is_error: bool, exit_code: Option<i32>) -> AgentEvent {
        AgentEvent::Finished {
            text: None,
            exit_code,
            is_error,
            usage: Usage::default(),
        }
    }

    #[tokio::test]
    async fn following_a_run_returns_once_it_finishes() {
        let (tx, rx) = broadcast::channel(16);
        tx.send(env("a1", 0, AgentEvent::Message { text: "hi".into() })).unwrap();
        tx.send(env("a1", 1, finished(false, Some(0)))).unwrap();

        assert_eq!(stream(rx, "a1", false, false).await, 0);
    }

    /// One process can be watching one agent while others are running.
    #[tokio::test]
    async fn events_belonging_to_another_agent_are_ignored() {
        let (tx, rx) = broadcast::channel(16);
        tx.send(env("other", 0, finished(true, Some(9)))).unwrap();
        tx.send(env("a1", 1, finished(false, Some(0)))).unwrap();

        assert_eq!(
            stream(rx, "a1", false, false).await,
            0,
            "the other agent's failure must not end this watch"
        );
    }

    #[tokio::test]
    async fn a_failed_run_reports_a_non_zero_exit_even_when_the_harness_exited_zero() {
        let (tx, rx) = broadcast::channel(16);
        tx.send(env("a1", 0, finished(true, Some(0)))).unwrap();

        assert_eq!(stream(rx, "a1", false, false).await, 1);
    }

    /// The service going away without a result is a failure, not a clean exit.
    #[tokio::test]
    async fn a_closed_feed_exits_non_zero() {
        let (tx, rx) = broadcast::channel::<AgentEnvelope>(16);
        drop(tx);

        assert_eq!(stream(rx, "a1", false, false).await, 1);
    }

    /// A consumer that falls behind must be told, and must keep going rather
    /// than treating the gap as the end of the run.
    #[tokio::test]
    async fn falling_behind_is_reported_and_the_watch_continues() {
        let (tx, rx) = broadcast::channel(2);
        for seq in 0..8 {
            tx.send(env("a1", seq, AgentEvent::Message { text: format!("{seq}") })).unwrap();
        }
        tx.send(env("a1", 8, finished(false, Some(3)))).unwrap();

        assert_eq!(stream(rx, "a1", false, false).await, 3);
    }

    #[tokio::test]
    async fn json_mode_still_ends_on_the_finish_event() {
        let (tx, rx) = broadcast::channel(16);
        tx.send(env("a1", 0, AgentEvent::Message { text: "hi".into() })).unwrap();
        tx.send(env("a1", 1, finished(false, Some(7)))).unwrap();

        assert_eq!(stream(rx, "a1", true, false).await, 7);
    }

    /// Every event kind goes through `print_event`; none may panic, and
    /// thinking is only shown when it was asked for.
    #[tokio::test]
    async fn every_event_kind_renders_without_panicking() {
        for show_thinking in [false, true] {
            let (tx, rx) = broadcast::channel(32);
            for (seq, event) in [
                AgentEvent::Started {
                    session_id: Some("s".into()),
                    model: Some("claude-opus-5".into()),
                },
                AgentEvent::Started { session_id: None, model: None },
                AgentEvent::Thinking { text: "line one\nline two".into() },
                AgentEvent::Message { text: "answer".into() },
                AgentEvent::ToolCall { name: "bash".into(), input: None },
                AgentEvent::ToolResult {
                    name: "bash".into(),
                    summary: Some("ok".into()),
                    is_error: false,
                },
                AgentEvent::ToolResult { name: "bash".into(), summary: None, is_error: true },
                AgentEvent::Raw { line: "noise".into() },
                AgentEvent::Error { message: "it broke".into() },
            ]
            .into_iter()
            .enumerate()
            {
                tx.send(env("a1", seq as u64, event)).unwrap();
            }
            tx.send(env("a1", 99, AgentEvent::Finished {
                text: None,
                exit_code: Some(0),
                is_error: false,
                usage: Usage { cost_usd: Some(0.5), output_tokens: Some(12), ..Default::default() },
            }))
            .unwrap();

            assert_eq!(stream(rx, "a1", false, show_thinking).await, 0);
        }
    }

    #[tokio::test]
    async fn a_failed_finish_renders_its_usage_too() {
        let (tx, rx) = broadcast::channel(4);
        tx.send(env("a1", 0, AgentEvent::Finished {
            text: None,
            exit_code: Some(2),
            is_error: true,
            usage: Usage { cost_usd: Some(0.25), ..Default::default() },
        }))
        .unwrap();

        assert_eq!(stream(rx, "a1", false, false).await, 2);
    }

    // --- the list renderers ----------------------------------------------
    //
    // These write to stdout, which `println!` gives no hook to capture, so the
    // assertion is that they render every shape without panicking. That is not
    // a formality: two of them slice an id by byte offset, which panics on a
    // short or non-ASCII id.

    fn summary(id: &str, status: AgentStatus) -> AgentSummary {
        AgentSummary {
            id: id.into(),
            name: "scout".into(),
            harness: HarnessKind::ClaudeCode,
            harness_label: "Claude Code".into(),
            status,
            cwd: "/work".into(),
            model: None,
            permission: Default::default(),
            tmux_session: "jod-x".into(),
            attach_command: "tmux attach -t jod-x".into(),
            switch_command: "tmux switch-client -t jod-x".into(),
            session_closed: false,
            created_at_ms: 0,
            session_id: None,
            usage: Usage::default(),
            event_count: 0,
            last_message: None,
            stream_path: "/runs/x/stream.jsonl".into(),
        }
    }

    #[test]
    fn an_empty_agent_list_says_so_rather_than_printing_nothing() {
        agents(&[]);
    }

    #[test]
    fn every_status_gets_a_label() {
        agents(&[
            summary("aaaaaaaaaaaa", AgentStatus::Running),
            summary("bbbbbbbbbbbb", AgentStatus::Completed),
            summary("cccccccccccc", AgentStatus::Failed),
            summary("dddddddddddd", AgentStatus::Killed),
        ]);
    }

    /// The id column truncates to 8 bytes. An id shorter than that must not
    /// panic on the slice.
    #[test]
    fn a_short_id_is_not_sliced_past_its_end() {
        agents(&[summary("ab", AgentStatus::Running)]);
    }

    #[test]
    fn a_harness_list_renders_installed_and_missing_alike() {
        harnesses(&[
            HarnessInfo {
                id: "claude_code".into(),
                label: "Claude Code".into(),
                available: true,
                path: Some("/usr/local/bin/claude".into()),
            },
            HarnessInfo {
                id: "open_code".into(),
                label: "OpenCode".into(),
                available: false,
                path: None,
            },
        ]);
        harnesses(&[]);
    }

    #[test]
    fn a_launched_agent_is_announced_both_ways() {
        let a = summary("abcdefgh", AgentStatus::Running);
        launched(&a);
        launched_waiting(&a);
    }

    /// Regression: an idle fleet printed "$-0.0000", because Rust's f64 sum
    /// uses -0.0 as its identity.
    #[test]
    fn an_idle_fleet_reports_no_negative_zero_spend() {
        let empty: Vec<AgentSummary> = vec![];
        let total: f64 = empty.iter().filter_map(|a| a.usage.cost_usd).sum();
        assert_eq!(format!("${:.4}", total + 0.0), "$0.0000");

        report(&Report {
            running: 0,
            completed: 0,
            failed: 0,
            killed: 0,
            total_cost_usd: total,
            agents: empty,
        });
    }
}
