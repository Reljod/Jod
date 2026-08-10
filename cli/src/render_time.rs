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

pub fn fires(list: &[Fire], now_ms: i64) {
    for f in list {
        let (mark, colour) = match f.outcome {
            FireOutcome::Ran => ("✓", GREEN),
            FireOutcome::Replaced => ("↻", YELLOW),
            FireOutcome::SkippedOverlap | FireOutcome::SkippedMisfire => ("○", DIM),
            FireOutcome::SpawnFailed => ("✗", RED),
            FireOutcome::Abandoned => ("■", RED),
        };
        let detail = f.detail.clone().unwrap_or_default();
        println!(
            "{colour}{mark}{RESET} {:<30} {DIM}{:<16}{RESET} {detail}",
            when(f.fired_at_ms, now_ms),
            f.outcome.as_str()
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
}
