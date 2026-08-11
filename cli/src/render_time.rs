//! Rendering schedules, goals and their history for a plain terminal.
//!
//! Kept beside the TUI's own renderer rather than inside it, because these are
//! answers to a one-shot question — `jod schedule ls` prints and exits — while
//! the TUI redraws the same data four times a second. The shapes agree on
//! purpose: absolute *and* relative time together, a glyph on every state, and
//! the reason a thing stopped shown rather than implied.

use jod_core::schedule::{Fire, FireOutcome, Goal, GoalState, Schedule, ScheduleState};

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// A timestamp as both when it is and how far off.
///
/// `systemctl list-timers` shows the same pair for the same reason: the
/// absolute answers "when", the relative answers "soon?", and neither alone
/// tells you whether a timer is armed correctly. A cron expression on its own
/// tells you neither.
pub fn when(at_ms: i64, now_ms: i64) -> String {
    let stamp = chrono::DateTime::from_timestamp_millis(at_ms)
        .map(|t| t.format("%b %d %H:%M").to_string())
        .unwrap_or_else(|| "—".into());
    let delta = at_ms - now_ms;
    let rel = if delta >= 0 {
        format!("in {}", crate::tui::short_duration(delta))
    } else {
        format!("{} ago", crate::tui::short_duration(-delta))
    };
    format!("{stamp} ({rel})")
}

/// A glyph as well as a colour on every state, so a `NO_COLOR` terminal, an
/// eight-colour terminal and a colour-blind reader all get the same answer.
fn schedule_mark(state: ScheduleState) -> (&'static str, &'static str) {
    match state {
        ScheduleState::Armed => ("●", GREEN),
        ScheduleState::Paused => ("‖", DIM),
        ScheduleState::Broken => ("✗", RED),
    }
}

pub fn schedules(list: &[Schedule], now_ms: i64) {
    for s in list {
        let (mark, colour) = schedule_mark(s.state);
        let next = match s.next_fire_at_ms {
            Some(at) if s.state == ScheduleState::Armed => when(at, now_ms),
            _ => "—".into(),
        };
        println!(
            "{colour}{mark}{RESET} {BOLD}{:<20}{RESET} {DIM}{:<16}{RESET} {next}",
            s.name, s.cron
        );
        // The failure count is what explains a schedule that stopped, so it is
        // shown rather than left to be inferred from the state glyph.
        if s.consecutive_failures > 0 {
            let tail = if s.state == ScheduleState::Broken {
                " — stopped; `jod schedule resume` clears it"
            } else {
                ""
            };
            println!(
                "  {RED}{} consecutive failures{RESET}{tail}",
                s.consecutive_failures
            );
        }
    }
}

pub fn goals(list: &[Goal], now_ms: i64) {
    for g in list {
        let (mark, colour) = match g.state {
            GoalState::Running => ("◎", GREEN),
            GoalState::Paused => ("‖", DIM),
            GoalState::Satisfied => ("✓", GREEN),
            GoalState::Stalled => ("⚠", YELLOW),
            GoalState::Exhausted => ("■", DIM),
            GoalState::Blocked => ("✗", RED),
        };
        let spend = match g.budget_usd {
            Some(cap) => format!("${:.2} of ${cap:.2}", g.spent_usd),
            None => format!("${:.2}", g.spent_usd),
        };
        println!(
            "{colour}{mark}{RESET} {BOLD}{:<20}{RESET} {DIM}iter {:<5}{RESET} {spend}",
            g.name, g.iteration
        );
        println!("  {DIM}{}{RESET}", g.objective);
        // Why it stopped, when it stopped on its own. A goal that went quiet
        // without saying why is the failure these states exist to prevent.
        match g.state {
            GoalState::Stalled => println!(
                "  {YELLOW}stalled — {} iterations changed nothing{RESET}",
                g.no_progress
            ),
            GoalState::Exhausted => {
                println!("  {DIM}exhausted — out of budget or iterations{RESET}")
            }
            GoalState::Running => {
                if let Some(at) = g.next_fire_at_ms {
                    println!("  {DIM}next {}{RESET}", when(at, now_ms));
                }
            }
            _ => {}
        }
    }
}

/// A schedule's history, each fire shown with how its run actually ended.
///
/// The pair matters. `schedule_fires.outcome` is written when a run is
/// *started*, so a run that then failed still says `ran` — and this is the one
/// place a person looks to ask "does this job work". Judged on the fire alone,
/// a schedule whose every run has failed prints a column of green ticks, which
/// is worse than printing nothing.
/// How one fire should read, given how its run actually ended.
///
/// Pure and separate so the judgement is testable rather than only reviewable —
/// a test that re-implemented this match would pass while the real one was
/// wrong, which is the shape of bug this whole file keeps finding.
///
/// **How it ended outranks how it started.** Only `Ran` is reinterpreted: a
/// skip or an abandonment never had a run to disagree with.
fn fire_mark(outcome: FireOutcome, ended: Option<&str>) -> (&'static str, &'static str, &'static str) {
    if outcome == FireOutcome::Ran && ended == Some("failed") {
        return ("✗", RED, "ran, then failed");
    }
    match outcome {
        FireOutcome::Ran => ("✓", GREEN, "ran"),
        FireOutcome::Replaced => ("↻", YELLOW, "replaced"),
        FireOutcome::SkippedOverlap => ("○", DIM, "skipped_overlap"),
        FireOutcome::SkippedMisfire => ("○", DIM, "skipped_misfire"),
        FireOutcome::SpawnFailed => ("✗", RED, "spawn_failed"),
        FireOutcome::Abandoned => ("■", RED, "abandoned"),
        // A watchdog that found nothing is a success, and it is dimmed rather
        // than green: a column of bright ticks for a schedule that has done
        // nothing all week would read as activity.
        FireOutcome::MonitorQuiet => ("·", DIM, "monitor_quiet"),
        // A question mark on purpose. A row this build cannot read must look
        // unreadable rather than borrowing another outcome's glyph.
        FireOutcome::Unknown => ("?", YELLOW, "unknown"),
    }
}

pub fn fires(list: &[(Fire, Option<String>)], now_ms: i64) {
    for (f, ended) in list {
        let (mark, colour, label) = fire_mark(f.outcome, ended.as_deref());
        let detail = f.detail.clone().unwrap_or_default();
        println!(
            "{colour}{mark}{RESET} {:<30} {DIM}{label:<16}{RESET} {detail}",
            when(f.fired_at_ms, now_ms),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A schedule table that showed only the cron expression would be a puzzle;
    /// one that showed only "in 9h" cannot be checked against a clock.
    #[test]
    fn a_time_is_shown_both_absolutely_and_relatively() {
        let now = 1_786_320_000_000;
        let text = when(now + 3_600_000, now);
        assert!(text.contains("in 1h00m"), "{text}");
        assert!(text.contains(':'), "and an absolute stamp: {text}");
    }

    #[test]
    fn a_time_in_the_past_reads_as_ago_rather_than_as_a_negative() {
        let now = 1_786_320_000_000;
        let text = when(now - 7_200_000, now);
        assert!(text.contains("2h00m ago"), "{text}");
        assert!(!text.contains('-'), "no negative durations: {text}");
    }

    #[test]
    fn a_nonsense_timestamp_renders_rather_than_panicking() {
        let text = when(i64::MAX, 0);
        assert!(text.contains('—'), "{text}");
    }

    /// Colour is never the only channel.
    #[test]
    fn every_schedule_state_has_its_own_glyph() {
        let marks: Vec<&str> = [
            ScheduleState::Armed,
            ScheduleState::Paused,
            ScheduleState::Broken,
        ]
        .into_iter()
        .map(|s| schedule_mark(s).0)
        .collect();
        let distinct: std::collections::HashSet<_> = marks.iter().collect();
        assert_eq!(distinct.len(), marks.len(), "two states share a glyph");
    }

    /// The defect this guards: a schedule whose every run fails printed a
    /// column of green ticks, in the one place a person looks to ask whether
    /// the job works. The outcome is written when the run *starts*.
    #[test]
    fn a_fire_whose_run_failed_is_not_shown_as_a_success() {
        let (mark, _, label) = fire_mark(FireOutcome::Ran, Some("failed"));
        assert_eq!(mark, "✗", "a failed run rendered as a tick");
        assert_eq!(label, "ran, then failed", "and it says which half failed");
    }

    #[test]
    fn a_fire_whose_run_succeeded_still_reads_as_success() {
        assert_eq!(fire_mark(FireOutcome::Ran, Some("completed")).0, "✓");
        assert_eq!(fire_mark(FireOutcome::Ran, None).0, "✓");
    }

    /// A skip never had a run to disagree with, so it keeps its own outcome
    /// whatever a stray status says.
    #[test]
    fn an_outcome_with_no_run_is_not_reinterpreted() {
        for outcome in [
            FireOutcome::SkippedOverlap,
            FireOutcome::SkippedMisfire,
            FireOutcome::SpawnFailed,
            FireOutcome::Abandoned,
            FireOutcome::MonitorQuiet,
        ] {
            let alone = fire_mark(outcome, None);
            let with_status = fire_mark(outcome, Some("failed"));
            assert_eq!(alone, with_status, "{outcome:?} was reinterpreted");
        }
    }

    #[test]
    fn a_long_line_is_cut_rather_than_wrapping_the_terminal() {
        let cut = first_line(&"word ".repeat(100));
        assert!(cut.chars().count() <= 91);
        assert!(cut.ends_with('…'));
    }
}

// ---- conversations ------------------------------------------------------

const CYAN: &str = "\x1b[36m";

/// A conversation list, newest first.
///
/// The fork marker is the column that earns its place: a branch and its parent
/// are otherwise two rows with similar titles and nothing saying which came
/// from which.
pub fn conversations(list: &[jod_core::conversation::ConversationSummary], now_ms: i64) {
    for c in list {
        let fork = if c.forked_from.is_some() { "⑂" } else { " " };
        println!(
            "{fork} {DIM}{}{RESET}  {BOLD}{}{RESET}",
            &c.id[..8.min(c.id.len())],
            c.title
        );
        println!(
            "    {DIM}{} · {} msg · {}{RESET}",
            c.harness,
            c.message_count,
            when(c.updated_at_ms, now_ms)
        );
    }
}

/// One conversation, root to head.
pub fn thread(messages: &[jod_core::conversation::Message]) {
    use jod_core::conversation::Role;
    for m in messages {
        let (mark, colour) = match m.role {
            Role::User => ("›", CYAN),
            Role::Assistant => (" ", RESET),
            Role::Thinking => ("·", DIM),
            Role::ToolCall => ("⚙", DIM),
            Role::ToolResult => ("└", DIM),
            Role::System => ("•", YELLOW),
        };
        // The message id is shown because every other verb takes one: reverting
        // or forking means naming a message, and hunting for it in the database
        // is not a user interface.
        println!(
            "{DIM}{:>5}{RESET} {colour}{mark} {}{RESET}",
            m.id,
            first_line(&m.text)
        );
    }
}

/// Search hits, each with the conversation's opening around it.
///
/// The window plus the opening is the shape the Hermes audit measured: it lets
/// you reconstruct what the conversation was for and where the match sits in it
/// without paying for the whole transcript, and with no model call anywhere.
pub fn search(hits: &[jod_core::conversation::SearchHit]) {
    for h in hits {
        println!(
            "{BOLD}{}{RESET} {DIM}{}{RESET}",
            h.title,
            &h.conversation_id[..8.min(h.conversation_id.len())]
        );
        if let Some(opening) = h.bookend_start.first() {
            println!("  {DIM}opened: {}{RESET}", first_line(&opening.text));
        }
        for m in &h.window {
            // Marked, so the hit is findable inside the context around it.
            let marker = if m.id == h.message.id { "▸" } else { " " };
            println!("  {marker} {}", first_line(&m.text));
        }
        println!();
    }
}

/// One line, bounded. A transcript pasted into a list view is not a list.
fn first_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 90 {
        return flat;
    }
    format!("{}…", flat.chars().take(90).collect::<String>())
}
