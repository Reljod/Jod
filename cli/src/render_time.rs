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

/// Webhook rules: what fires, on what, and whether it is armed.
///
/// The match is on the first line because it is what a reader is checking — a
/// rule that never fires is nearly always matching something narrower than its
/// author meant. Conditions go underneath, and only when there are any.
pub fn webhook_rules(list: &[jod_core::webhook::Rule]) {
    for r in list {
        let (mark, colour) = if r.enabled { ("●", GREEN) } else { ("○", DIM) };
        let event = match &r.action {
            Some(a) => format!("{}.{a}", r.event),
            None => r.event.clone(),
        };
        println!(
            "{colour}{mark}{RESET} {BOLD}{:<20}{RESET} {DIM}{:<22}{RESET} {}",
            r.name, event, r.repo
        );
        let c = &r.conditions;
        let mut narrowing = Vec::new();
        if !c.labels.is_empty() {
            // "all of" and not a bare list: requiring every label is the
            // conservative reading the matcher implements, and a comma-separated
            // list reads like any-of to everyone who has used a search box.
            narrowing.push(format!("all of [{}]", c.labels.join(", ")));
        }
        if let Some(b) = &c.branch {
            narrowing.push(format!("branch {b}"));
        }
        if let Some(a) = &c.author {
            narrowing.push(format!("author {a}"));
        }
        if let Some(d) = c.draft {
            narrowing.push(if d { "drafts only".into() } else { "not drafts".into() });
        }
        if !narrowing.is_empty() {
            println!("  {DIM}when {}{RESET}", narrowing.join(" · "));
        }
    }
}

/// What has arrived on the webhook endpoint.
///
/// The status is the point — a delivery that arrived and matched nothing is the
/// single most common thing to be confused about, so it says `no match` rather
/// than being absent from a list of runs.
pub fn deliveries(list: &[jod_core::webhook::Delivery], now_ms: i64) {
    for d in list {
        let status = d.status.as_str();
        let colour = match d.status {
            jod_core::webhook::DeliveryStatus::Accepted => GREEN,
            jod_core::webhook::DeliveryStatus::Rejected => RED,
            _ => DIM,
        };
        let what = match (&d.repo, &d.action) {
            (Some(repo), Some(action)) => format!("{} {}.{action}", repo, d.event),
            (Some(repo), None) => format!("{} {}", repo, d.event),
            (None, _) => d.event.clone(),
        };
        println!(
            "{DIM}{}{RESET} {colour}{:<9}{RESET} {}",
            when(d.received_at_ms, now_ms),
            status,
            what
        );
    }
}

/// Every monitor, against the schedule whose spending it decides.
///
/// Named by its schedule rather than keyed by its id: a monitor is only ever
/// talked about as "the monitor on nightly-sweep", and printing an id here
/// would make every read of this list start with a lookup somewhere else.
pub fn monitors(list: &[(jod_core::monitor::Monitor, String)], now_ms: i64) {
    for (m, name) in list {
        // Whether a baseline exists, not whether the schedule is armed — the
        // schedule list already answers that one. A monitor with no digest
        // suppresses nothing and wakes nothing on its next tick, and "why did
        // my monitor not fire" is answered here more often than anywhere else.
        let (mark, colour) = match m.last_digest {
            Some(_) => ("●", GREEN),
            None => ("○", DIM),
        };
        println!(
            "{colour}{mark}{RESET} {BOLD}{:<20}{RESET} {DIM}{:<9}{:<8}{RESET} {}",
            name,
            m.mode.as_str(),
            m.probe.kind(),
            m.probe.target()
        );
        let mut facts = vec![match m.last_checked_at_ms {
            Some(at) => format!("checked {}", when(at, now_ms)),
            None => "never checked".to_string(),
        }];
        if let Some(at) = m.last_changed_at_ms {
            facts.push(format!("changed {}", when(at, now_ms)));
        }
        if m.last_digest.is_none() {
            facts.push("no baseline — the first check sets one and wakes nothing".into());
        }
        println!("  {DIM}{}{RESET}", facts.join(" · "));
    }
}

/// What a monitor has seen, newest first.
///
/// `unchanged` is the outcome this whole feature exists to produce, so it is
/// dimmed rather than hidden: a column of quiet ticks is the evidence that a
/// watchdog is working, and a list that showed only the exciting rows would
/// look identical to one that had not run at all.
pub fn checks(list: &[jod_core::monitor::Check], now_ms: i64) {
    for c in list {
        let colour = match c.outcome.as_str() {
            "changed" | "reported" => GREEN,
            "failed" => RED,
            "baseline" => YELLOW,
            _ => DIM,
        };
        let detail = match &c.detail {
            Some(d) => format!(" {}", one_line(d)),
            None => String::new(),
        };
        println!(
            "{DIM}{}{RESET} {colour}{:<9}{RESET}{detail}",
            when(c.at_ms, now_ms),
            c.outcome
        );
    }
}

/// A detail collapsed onto the one line a list row has for it.
///
/// A failing probe's detail is often a multi-line stderr, and letting it wrap
/// freely turns a ten-row history into a screenful where the timestamps no
/// longer line up — which is the only thing the list is read for.
fn one_line(s: &str) -> String {
    const WIDTH: usize = 72;
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > WIDTH {
        format!("{}…", flat.chars().take(WIDTH - 1).collect::<String>())
    } else {
        flat
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

// ---- the delivery ledger --------------------------------------------------

/// A glyph and a colour per delivery state, arranged so that **settled and
/// unsettled cannot be confused at a glance** — which is the whole product.
///
/// The glyphs are graded by how much is still owed rather than by how the news
/// feels: hollow `○` is owed and untouched, half-filled `◐` is owed and in
/// flight, filled `●` is done. `✗` is the odd one out on purpose — a failure is
/// settled, but settling it is not an outcome anybody wanted, and giving it a
/// filled circle would file it beside `delivered` in a column somebody is
/// scanning for trouble.
///
/// Colour is never the only carrier, for the reason the rest of this file gives:
/// `NO_COLOR`, eight-colour terminals and colour-blind readers all have to get
/// the same answer.
fn delivery_mark(state: jod_core::ledger::DeliveryState) -> (&'static str, &'static str) {
    use jod_core::ledger::DeliveryState;
    match state {
        DeliveryState::Pending => ("○", YELLOW),
        DeliveryState::Attempting => ("◐", YELLOW),
        DeliveryState::Delivered => ("●", GREEN),
        DeliveryState::Failed => ("✗", RED),
    }
}

/// The ledger as a list, newest first.
///
/// Unsettled rows are bold and settled ones are dim, on top of the glyph. That
/// is deliberate redundancy: this list is read for one question — *is anybody
/// still owed anything?* — and the answer has to survive being skimmed.
pub fn obligations(list: &[jod_core::ledger::Obligation], now_ms: i64) {
    for o in list {
        for line in obligation_lines(o, now_ms) {
            println!("{line}");
        }
    }
}

/// One list row, as the lines it prints.
///
/// Pure and returning strings rather than printing, so the two properties this
/// view exists for — that settled and unsettled differ, and that a possible
/// duplicate says so — are assertions in a test rather than something somebody
/// has to eyeball on a real database.
fn obligation_lines(o: &jod_core::ledger::Obligation, now_ms: i64) -> Vec<String> {
    let (mark, colour) = delivery_mark(o.state);
    // `is_settled` drives the emphasis rather than a second match on the state,
    // so a fifth state added later is loud by default rather than quietly
    // formatted as though it were finished.
    let emphasis = if o.state.is_settled() { DIM } else { BOLD };
    let mut lines = vec![format!(
        "{colour}{mark}{RESET} {DIM}{:<9}{RESET} {emphasis}{}{RESET}",
        o.state.as_str(),
        one_line(&o.body)
    )];

    let mut facts = vec![
        format!("{}→{}", o.channel, o.target),
        when(o.updated_at_ms, now_ms),
    ];
    if o.attempts > 0 {
        facts.push(format!(
            "{} attempt{}",
            o.attempts,
            if o.attempts == 1 { "" } else { "s" }
        ));
    }
    facts.push(o.message_key.clone());
    lines.push(format!("  {DIM}{}{RESET}", facts.join(" · ")));

    // The reason belongs in the *list*, not only in `show`. `jod ledger failed`
    // is read to answer "why did these not go", and a list that made you open
    // each row to find out would send you back to SQLite by another route —
    // which is the thing this command exists to stop.
    if let Some(detail) = &o.detail {
        lines.push(format!("  {RED}{}{RESET}", one_line(detail)));
    }
    if let Some(note) = duplicate_warning(o) {
        lines.push(format!("  {YELLOW}{note}{RESET}"));
    }
    lines
}

/// One obligation in full: what was owed, to whom, and what became of it.
pub fn obligation(o: &jod_core::ledger::Obligation, now_ms: i64) {
    let (mark, colour) = delivery_mark(o.state);
    println!(
        "{colour}{mark} {BOLD}{}{RESET}  {DIM}{}{RESET}",
        o.state.as_str(),
        o.message_key
    );
    println!("  {DIM}to{RESET}        {}→{}", o.channel, o.target);
    println!("  {DIM}owed{RESET}      {}", when(o.created_at_ms, now_ms));
    println!("  {DIM}last{RESET}      {}", when(o.updated_at_ms, now_ms));
    println!(
        "  {DIM}attempts{RESET}  {} of {}",
        o.attempts,
        jod_core::ledger::MAX_ATTEMPTS
    );
    // The process, because "why has this not gone" is answered by it more often
    // than by anything else: a row held by a machine that is not this one is
    // waiting for that box to come back, not for anything here.
    println!(
        "  {DIM}held by{RESET}   {} pid {}",
        o.owner.machine, o.owner.pid
    );
    if let Some(run) = &o.run_id {
        println!("  {DIM}run{RESET}       {run}");
    }
    if let Some(detail) = &o.detail {
        println!("  {DIM}why{RESET}       {RED}{detail}{RESET}");
    }
    if let Some(note) = duplicate_warning(o) {
        println!("  {YELLOW}{note}{RESET}");
    }
    println!("\n{}", o.body);
}

/// What to say about a message that may reach its recipient twice.
///
/// Only an `attempting` row can say it, and that is a limit of the record
/// rather than a choice: `Obligation::may_be_a_duplicate` reads the *current*
/// state, and once a recovered message lands the row is `delivered` like any
/// other. Nothing in the schema remembers that `RECOVERED_MARKER` was ever
/// prefixed. So this warns about the duplicates that are still ahead and cannot
/// speak for the ones already sent — which is worth knowing when reading it.
fn duplicate_warning(o: &jod_core::ledger::Obligation) -> Option<String> {
    o.may_be_a_duplicate().then(|| {
        "in flight — if the process holding it died, this is resent labelled as a possible \
         duplicate"
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    mod delivery_ledger {
        use super::*;
        use jod_core::ledger::{DeliveryState, Obligation, Owner};

        fn row(state: DeliveryState, attempts: i64, detail: Option<&str>) -> Obligation {
            Obligation {
                id: 1,
                message_key: "telegram:7:42".into(),
                channel: "telegram".into(),
                target: "7".into(),
                body: "Deploy finished.".into(),
                state,
                attempts,
                owner: Owner::new("jod-cloud", 4821),
                run_id: Some("run-b2".into()),
                detail: detail.map(str::to_string),
                created_at_ms: 1_000,
                updated_at_ms: 2_000,
            }
        }

        /// The distinction the whole command exists to draw. Asserted on the
        /// glyphs alone, with no colour, because that is what a `NO_COLOR`
        /// terminal and a colour-blind reader are left with — and if the answer
        /// only survives in the escape codes it is not an answer.
        #[test]
        fn a_settled_row_and_an_owed_one_never_share_a_glyph() {
            let owed: Vec<&str> = [DeliveryState::Pending, DeliveryState::Attempting]
                .iter()
                .map(|s| delivery_mark(*s).0)
                .collect();
            let done: Vec<&str> = [DeliveryState::Delivered, DeliveryState::Failed]
                .iter()
                .map(|s| delivery_mark(*s).0)
                .collect();

            for a in &owed {
                assert!(
                    !done.contains(a),
                    "`{a}` marks both an owed row and a settled one"
                );
            }
            // And all four differ from each other, so the state is readable
            // without the word beside it.
            let mut all: Vec<&str> = owed.into_iter().chain(done).collect();
            all.sort_unstable();
            all.dedup();
            assert_eq!(all.len(), 4, "two states share a glyph");
        }

        /// Emphasis carries the same distinction a second time, on purpose:
        /// this list is skimmed, and one signal is one signal to miss.
        #[test]
        fn an_owed_row_is_emphasised_and_a_settled_one_is_dimmed() {
            let owed = obligation_lines(&row(DeliveryState::Pending, 0, None), 3_000);
            let done = obligation_lines(&row(DeliveryState::Delivered, 1, None), 3_000);
            assert!(owed[0].contains(BOLD), "an owed row is not emphasised");
            assert!(!done[0].contains(BOLD), "a settled row is emphasised");
        }

        /// `RECOVERED_MARKER` is the module's stated ethic — ambiguity labelled
        /// rather than hidden — and a reader that quietly dropped it would undo
        /// the honesty the sender paid for.
        #[test]
        fn only_a_message_still_in_flight_is_called_a_possible_duplicate() {
            let in_flight = obligation_lines(&row(DeliveryState::Attempting, 1, None), 3_000);
            assert!(
                in_flight.iter().any(|l| l.contains("possible duplicate")),
                "an in-flight message does not warn: {in_flight:?}"
            );

            for quiet in [
                DeliveryState::Pending,
                DeliveryState::Delivered,
                DeliveryState::Failed,
            ] {
                let lines = obligation_lines(&row(quiet, 1, None), 3_000);
                assert!(
                    !lines.iter().any(|l| l.contains("duplicate")),
                    "{quiet:?} claims it may be a duplicate: {lines:?}"
                );
            }
        }

        /// `jod ledger failed` is read to find out *why*. A list that made you
        /// open each row would send you back to SQLite by another route.
        #[test]
        fn a_row_that_failed_says_why_in_the_list_itself() {
            let lines = obligation_lines(
                &row(
                    DeliveryState::Failed,
                    3,
                    Some("Unauthorized: bot was removed from the chat"),
                ),
                3_000,
            );
            assert!(
                lines.iter().any(|l| l.contains("bot was removed")),
                "the reason is not in the list: {lines:?}"
            );
        }

        /// A row nobody has tried yet says nothing about attempts rather than
        /// "0 attempts", which reads as a failure count.
        #[test]
        fn an_untried_message_does_not_advertise_a_count_of_nothing() {
            let fresh = obligation_lines(&row(DeliveryState::Pending, 0, None), 3_000);
            assert!(!fresh[1].contains("attempt"), "{}", fresh[1]);
            let tried = obligation_lines(&row(DeliveryState::Pending, 1, None), 3_000);
            assert!(tried[1].contains("1 attempt ·"), "{}", tried[1]);
        }
    }

    /// The strings below are copied out of a real `jod main` run's `messages`
    /// rows, not invented, because the bug was that plausible-looking rendering
    /// code had never been shown a real payload.
    mod transcript_turns {
        use super::*;
        use jod_core::conversation::Role;

        /// The `text` column here is deliberately the *cut* form the store
        /// actually wrote, so a renderer that reached for it instead of
        /// `tool_input` fails this test rather than passing on a tidy fake.
        #[test]
        fn a_tool_call_reads_as_the_verb_and_its_subject() {
            let whole = serde_json::json!({
                "cron": "0 8 * * 1-5",
                "cwd": "/home/reljod",
                "misfire": "fire_once",
                "name": "weekday-pr-sweep",
                "overlap": "skip",
                "prompt": "Sweep the open PRs and report only what needs him.",
            });
            let cut = r#"{"cron":"0 8 * * 1-5","cwd":"/home/reljod","misfire":"fire_once","name":"weekday-pr-swe… (+926 chars)"#;
            let line = turn_line(
                Role::ToolCall,
                Some("mcp__jod__schedule_create"),
                cut,
                Some(&whole),
            );
            assert_eq!(line, "schedule_create weekday-pr-sweep");
        }

        /// `name` is preferred over `prompt` deliberately: both are present on
        /// the call above, and the prompt would have filled the line with the
        /// briefing text while saying nothing about which schedule this is.
        #[test]
        fn a_call_with_no_nameable_subject_still_names_the_tool() {
            let line = turn_line(
                Role::ToolCall,
                Some("mcp__jod__schedule_list"),
                "{}",
                Some(&serde_json::json!({})),
            );
            assert_eq!(line, "schedule_list");
        }

        #[test]
        fn a_result_sheds_its_content_envelope() {
            let stored = r#"[{"text":"{\n  \"name\": \"weekday-pr-sweep\",\n  \"state\": \"armed\"\n}","type":"text"}]"#;
            let line = turn_line(Role::ToolResult, Some("schedule_create"), stored, None);
            assert!(!line.contains("\"type\""), "envelope survived: {line}");
            assert!(line.contains("weekday-pr-sweep"), "{line}");
        }

        /// The escapes are the point: an earlier draft searched the raw JSON for
        /// the decoded string, which cannot be found once it contains a newline.
        #[test]
        fn a_result_containing_newlines_is_still_unwrapped() {
            let stored = r#"[{"text":"line one\nline two","type":"text"}]"#;
            let line = turn_line(Role::ToolResult, None, stored, None);
            assert_eq!(line, "line one line two");
        }

        /// Copied from `list_agents`, whose result was long enough that the
        /// store cut it mid-string. No parser accepts this, and it is what the
        /// chat actually holds — so the readable path cannot depend on parsing.
        #[test]
        fn a_result_cut_mid_string_is_still_readable() {
            let stored = r#"[{"text":"[\n  {\n    \"run_id\": \"1f0fc870\",\n    \"name\": \"main\",\n    \"harn… (+16 chars)"#;
            assert!(
                serde_json::from_str::<serde_json::Value>(stored).is_err(),
                "the fixture must be the unparseable form, or it proves nothing"
            );
            let line = turn_line(Role::ToolResult, Some("mcp__jod__list_agents"), stored, None);
            assert!(!line.contains(r#"\""#), "escapes survived: {line}");
            assert!(!line.starts_with("[{"), "envelope survived: {line}");
            assert!(line.contains("1f0fc870"), "{line}");
        }

        #[test]
        fn a_result_that_is_not_an_envelope_is_left_alone() {
            let line = turn_line(Role::ToolResult, None, "plain text", None);
            assert_eq!(line, "plain text");
        }

        /// One per tool call, and every one of them was a bullet on a blank
        /// line. `thread` drops what this returns empty.
        #[test]
        fn an_empty_thinking_turn_renders_to_nothing() {
            assert!(turn_line(Role::Thinking, None, "", None).is_empty());
        }
    }

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
        let body = turn_line(m.role, m.tool_name.as_deref(), &m.text, m.tool_input.as_ref());
        // A thinking turn with nothing in it is a blank line with a bullet on
        // it. The harness emits one per tool call, so the transcript was one
        // third punctuation.
        if body.is_empty() {
            continue;
        }
        // The message id is shown because every other verb takes one: reverting
        // or forking means naming a message, and hunting for it in the database
        // is not a user interface.
        println!("{DIM}{:>5}{RESET} {colour}{mark} {body}{RESET}", m.id);
    }
}

/// One transcript turn, as a person would want to read it.
///
/// Split out and pure because the interesting part is the tool turns, and they
/// were being printed as their raw JSON: a `schedule_create` showed as
/// `{"cron":"0 8 * * 1-5","cwd":"/home/reljod","misfire":…` and its result as an
/// escaped content array. Both are *stored* whole on purpose — replay needs
/// them — but the whole of a payload is not a line of a chat.
fn turn_line(
    role: jod_core::conversation::Role,
    tool: Option<&str>,
    text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    use jod_core::conversation::Role;
    match role {
        // `input` and not `text`: for a tool call `text` is a *summary*, cut to
        // length with a `… (+926 chars)` marker glued on the end, so it is not
        // JSON and no amount of parsing will make it one. The arguments live
        // whole in `tool_input`, which is the column that exists for this.
        Role::ToolCall => {
            let name = short_tool(tool.unwrap_or("tool"));
            match input.and_then(subject_of) {
                Some(s) => format!("{name} {s}"),
                None => name.to_string(),
            }
        }
        // A result is worth a line to show the call returned, and rarely worth
        // more: what it *did* shows up in the reply, and the payload is one
        // `jod conv show` away.
        Role::ToolResult => {
            let flat = first_line(&unwrap_content(text));
            if flat.is_empty() {
                "ok".into()
            } else {
                flat
            }
        }
        _ => first_line(text),
    }
}

/// The one field of a tool's arguments a reader actually wants.
///
/// Tools disagree about what to call it, so this tries the names in the order
/// that answers "which thing?" best: an explicit name beats a free-text query,
/// and a prompt is the last resort because it is the longest.
fn subject_of(input: &serde_json::Value) -> Option<String> {
    for key in ["name", "id", "query", "goal", "prompt", "text"] {
        if let Some(s) = input.get(key).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return Some(first_line(s));
            }
        }
    }
    None
}

/// `mcp__jod__schedule_create` is Jod calling itself, and saying so four times a
/// screen is noise. The server prefix is dropped; anything else is left as the
/// harness named it, because for a non-Jod tool the full name is the fact.
fn short_tool(name: &str) -> &str {
    name.strip_prefix("mcp__jod__").unwrap_or(name)
}

/// Pull the text out of a harness content array, leaving anything else alone.
///
/// MCP results arrive as `[{"type":"text","text":"…"}]`, so without this every
/// result line is spent on the envelope rather than the answer.
///
/// Owned rather than borrowed, and the first draft got this wrong: the inner
/// string comes back with its escapes *resolved*, so looking for it inside the
/// raw JSON fails on any result containing a newline — which is nearly all of
/// them — and the "fallback" would have been the envelope, in exactly the case
/// this function exists to handle.
fn unwrap_content(text: &str) -> String {
    if let Some(inner) = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .as_ref()
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|first| first.get("text"))
        .and_then(|t| t.as_str())
    {
        return inner.to_string();
    }
    // A result long enough to be worth summarising is stored *cut*, mid-string,
    // with a marker after it — so the envelope is real but no parser will take
    // it. Peeling the opening by hand is not elegant and is the difference
    // between a readable line and a screen of `\"run_id\": \"1f0f…`.
    match text.strip_prefix(r#"[{"text":""#) {
        Some(rest) => unescape(rest),
        None => text.to_string(),
    }
}

/// Undo JSON string escaping on a fragment that is not parseable JSON.
///
/// Only the escapes that actually appear in a cut-off tool result: a lone
/// backslash at the very end is dropped rather than guessed at, since the
/// character it was escaping is exactly what the truncation removed.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
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
