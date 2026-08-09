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
                eprintln!("{}", paint(YELLOW, &format!("[jod] dropped {n} events — output fell behind")));
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
        if let AgentEvent::Finished { is_error, exit_code, .. } = &envelope.event {
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
        AgentEvent::Finished { is_error, usage, .. } => {
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
}
