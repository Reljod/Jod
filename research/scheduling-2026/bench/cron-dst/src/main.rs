//! Does the cron crate get the two days a year that matter right?
//!
//! Every cron library agrees about 08:00 on a Tuesday. They disagree about
//! 02:30 on the morning the clocks move, and that disagreement is the whole
//! reason to pick one over another. This harness asks four crates the same
//! questions and prints what each answered, so the report can quote measured
//! output rather than a README.
//!
//! Run: `cargo run --release`

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::str::FromStr;

use chrono::{DateTime, TimeZone};
use chrono_tz::Tz;

/// One crate's answer, or the reason it did not give one.
type Answer = Result<Vec<String>, String>;

fn fmt<Z: TimeZone>(dt: &DateTime<Z>) -> String
where
    Z::Offset: std::fmt::Display,
{
    dt.format("%Y-%m-%d %H:%M:%S %Z (%:z)").to_string()
}

// ---- adapters -----------------------------------------------------------

/// croner 3: five fields by default, seconds optional, zoned via chrono-tz.
fn croner(pattern: &str, tz: Tz, from: DateTime<Tz>, n: usize) -> Answer {
    catch_unwind(AssertUnwindSafe(|| {
        let cron = croner::Cron::from_str(pattern).map_err(|e| format!("parse: {e}"))?;
        let mut out = Vec::new();
        let mut cursor = from;
        for _ in 0..n {
            let next = cron
                .find_next_occurrence(&cursor, false)
                .map_err(|e| format!("next: {e}"))?;
            out.push(fmt(&next));
            cursor = next;
        }
        let _ = tz;
        Ok(out)
    }))
    .unwrap_or_else(|_| Err("PANIC".into()))
}

/// cron 0.17 (zslayton): six or seven fields, seconds mandatory.
fn zslayton(pattern: &str, tz: Tz, from: DateTime<Tz>, n: usize) -> Answer {
    catch_unwind(AssertUnwindSafe(|| {
        let schedule = cron::Schedule::from_str(pattern).map_err(|e| format!("parse: {e}"))?;
        let out: Vec<String> = schedule.after(&from).take(n).map(|d| fmt(&d)).collect();
        let _ = tz;
        if out.len() < n {
            return Err(format!("only {} of {n} occurrences", out.len()));
        }
        Ok(out)
    }))
    .unwrap_or_else(|_| Err("PANIC".into()))
}

/// saffron 0.1 (Cloudflare): five fields, and the API is `DateTime<Utc>` only.
/// The local time is converted in and out so the comparison is like for like —
/// which is exactly the workaround a caller would have to write themselves.
fn saffron(pattern: &str, tz: Tz, from: DateTime<Tz>, n: usize) -> Answer {
    catch_unwind(AssertUnwindSafe(|| {
        let cron: saffron::Cron = pattern.parse().map_err(|_| "parse: invalid".to_string())?;
        if !cron.any() {
            return Err("never matches".into());
        }
        let mut out = Vec::new();
        let mut cursor = from.with_timezone(&chrono::Utc);
        for _ in 0..n {
            let next = cron.next_after(cursor).ok_or("no next occurrence")?;
            out.push(fmt(&next.with_timezone(&tz)));
            cursor = next;
        }
        Ok(out)
    }))
    .unwrap_or_else(|_| Err("PANIC".into()))
}

/// cronexpr 1.6: five fields plus a mandatory timezone, evaluated with jiff.
fn cronexpr(pattern: &str, tz: Tz, from: DateTime<Tz>, n: usize) -> Answer {
    catch_unwind(AssertUnwindSafe(|| {
        let crontab = cronexpr::parse_crontab(pattern).map_err(|e| format!("parse: {e}"))?;
        let start = jiff::Timestamp::from_millisecond(from.timestamp_millis())
            .map_err(|e| format!("timestamp: {e}"))?;
        let mut out = Vec::new();
        let mut cursor = start;
        for _ in 0..n {
            let next = crontab.find_next(cursor).map_err(|e| format!("next: {e}"))?;
            // Rendered through chrono-tz so every row in the table is printed
            // by the same code and differences are the crates', not the format.
            let ms = next.timestamp().as_millisecond();
            let dt = chrono::Utc
                .timestamp_millis_opt(ms)
                .single()
                .ok_or("unrepresentable")?
                .with_timezone(&tz);
            out.push(fmt(&dt));
            cursor = next.timestamp();
        }
        Ok(out)
    }))
    .unwrap_or_else(|_| Err("PANIC".into()))
}

// ---- cases --------------------------------------------------------------

/// The same schedule written in each crate's dialect. `None` means the crate
/// cannot express it at all, which is itself a result.
struct Case {
    title: &'static str,
    tz: &'static str,
    /// Local wall-clock start, exclusive.
    from: (i32, u32, u32, u32, u32, u32),
    n: usize,
    croner: Option<&'static str>,
    zslayton: Option<&'static str>,
    saffron: Option<&'static str>,
    cronexpr: Option<&'static str>,
    expect: &'static str,
}

const CASES: &[Case] = &[
    Case {
        title: "SPRING GAP, fixed time: 02:30 daily across US spring-forward",
        tz: "America/New_York",
        from: (2026, 3, 7, 12, 0, 0),
        n: 3,
        croner: Some("30 2 * * *"),
        zslayton: Some("0 30 2 * * *"),
        saffron: Some("30 2 * * *"),
        cronexpr: Some("30 2 * * * America/New_York"),
        expect: "2026-03-08 has no 02:30 EST. Acceptable: run at 03:00 EDT \
                 (Vixie/croner) or skip the day. Unacceptable: panic, or drift \
                 the following days.",
    },
    Case {
        title: "FALL-BACK, fixed time: 01:30 daily across US fall-back",
        tz: "America/New_York",
        from: (2026, 10, 31, 12, 0, 0),
        n: 3,
        croner: Some("30 1 * * *"),
        zslayton: Some("0 30 1 * * *"),
        saffron: Some("30 1 * * *"),
        cronexpr: Some("30 1 * * * America/New_York"),
        expect: "2026-11-01 has two 01:30s. A fixed-time job must fire ONCE — \
                 the -04:00 one — then next on 2026-11-02.",
    },
    Case {
        title: "FALL-BACK, the zslayton#48 repro: 01:03 daily, Europe/London",
        tz: "Europe/London",
        from: (2026, 10, 24, 12, 0, 0),
        n: 3,
        croner: Some("3 1 * * *"),
        zslayton: Some("0 3 1 * * *"),
        saffron: Some("3 1 * * *"),
        cronexpr: Some("3 1 * * * Europe/London"),
        expect: "Issue #48 panicked here with 'invalid time'. Must not panic.",
    },
    Case {
        title: "SPRING GAP, interval job: every 30 min across US spring-forward",
        tz: "America/New_York",
        from: (2026, 3, 8, 1, 0, 0),
        n: 4,
        croner: Some("0,30 * * * *"),
        zslayton: Some("0 0,30 * * * *"),
        saffron: Some("0,30 * * * *"),
        cronexpr: Some("0,30 * * * * America/New_York"),
        expect: "01:30 EST then 03:00 EDT — the 02:00 and 02:30 slots do not \
                 exist and must simply not appear.",
    },
    Case {
        title: "WEEKDAYS 08:00 — the schedule Jod actually ships",
        tz: "Asia/Manila",
        from: (2026, 8, 7, 12, 0, 0),
        n: 3,
        croner: Some("0 8 * * 1-5"),
        zslayton: Some("0 0 8 * * 1-5"),
        saffron: Some("0 8 * * 1-5"),
        cronexpr: Some("0 8 * * 1-5 Asia/Manila"),
        expect: "Fri 7th is past 12:00, so: Mon 10th, Tue 11th, Wed 12th. \
                 A crate using Quartz weekday numbering answers Sun-Thu here.",
    },
];

/// Syntax the operator will type. Parse-only: does the dialect accept it?
const SYNTAX: &[(&str, &str, &str, &str, &str)] = &[
    // (label, croner, zslayton, saffron, cronexpr)
    ("@daily", "@daily", "@daily", "@daily", "@daily"),
    (
        "seconds field (*/15 s)",
        "*/15 * * * * *",
        "*/15 * * * * *",
        "*/15 * * * * *",
        "*/15 * * * * * UTC",
    ),
    (
        "L — last day of month",
        "0 12 L * *",
        "0 0 12 L * *",
        "0 12 L * *",
        "0 12 L * * UTC",
    ),
    (
        "# — second Friday",
        "0 12 * * FRI#2",
        "0 0 12 * * FRI#2",
        "0 12 * * FRI#2",
        "0 12 * * FRI#2 UTC",
    ),
    (
        "W — nearest weekday to the 15th",
        "0 12 15W * *",
        "0 0 12 15W * *",
        "0 12 15W * *",
        "0 12 15W * * UTC",
    ),
];

fn parses(which: usize, pattern: &str) -> bool {
    catch_unwind(AssertUnwindSafe(|| match which {
        0 => croner::Cron::from_str(pattern).is_ok(),
        1 => cron::Schedule::from_str(pattern).is_ok(),
        2 => pattern
            .parse::<saffron::Cron>()
            .map(|c| c.any())
            .unwrap_or(false),
        _ => cronexpr::parse_crontab(pattern).is_ok(),
    }))
    .unwrap_or(false)
}

fn main() {
    println!("cron crate DST + syntax comparison");
    println!("croner 3.0.1 · cron 0.17.0 · saffron 0.1.0 · cronexpr 1.6.0");
    println!("chrono-tz {} tzdata\n", chrono_tz::IANA_TZDB_VERSION);

    println!("== SYNTAX ==================================================");
    println!(
        "{:<34} {:>8} {:>8} {:>8} {:>9}",
        "pattern", "croner", "cron", "saffron", "cronexpr"
    );
    for (label, a, b, c, d) in SYNTAX {
        let mark = |ok: bool| if ok { "yes" } else { "NO" };
        println!(
            "{:<34} {:>8} {:>8} {:>8} {:>9}",
            label,
            mark(parses(0, a)),
            mark(parses(1, b)),
            mark(parses(2, c)),
            mark(parses(3, d)),
        );
    }

    for case in CASES {
        let tz: Tz = case.tz.parse().expect("known timezone");
        let (y, mo, d, h, mi, s) = case.from;
        let from = tz
            .with_ymd_and_hms(y, mo, d, h, mi, s)
            .single()
            .expect("unambiguous start");

        println!("\n== {} ==", case.title);
        println!("   tz {}  after {}", case.tz, fmt(&from));
        println!("   expected: {}", case.expect);

        let runs: [(&str, Option<&str>, fn(&str, Tz, DateTime<Tz>, usize) -> Answer); 4] = [
            ("croner  ", case.croner, croner),
            ("cron    ", case.zslayton, zslayton),
            ("saffron ", case.saffron, saffron),
            ("cronexpr", case.cronexpr, cronexpr),
        ];
        for (name, pattern, f) in runs {
            let Some(pattern) = pattern else {
                println!("   {name}  -- cannot express --");
                continue;
            };
            match f(pattern, tz, from.clone(), case.n) {
                Ok(times) => {
                    println!("   {name}  {pattern:<28}");
                    for t in times {
                        println!("             -> {t}");
                    }
                }
                Err(e) => println!("   {name}  {pattern:<28} ERROR: {e}"),
            }
        }
    }

    cost();
}

/// What one next-fire computation costs.
///
/// The tick loop recomputes this for every schedule it advances, so the number
/// decides whether a scheduler can poll cheaply or has to cache.
fn cost() {
    let tz: Tz = "Asia/Manila".parse().unwrap();
    let start = tz.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap();
    const N: u32 = 100_000;

    println!("\n== COST OF ONE NEXT-FIRE COMPUTATION ==");
    for pattern in ["0 8 * * 1-5", "*/5 * * * *", "0 12 L * *", "0 12 * * FRI#2"] {
        let cron = croner::Cron::from_str(pattern).unwrap();
        let t0 = std::time::Instant::now();
        for _ in 0..N {
            // From the same instant every time: this is what a tick does —
            // one next-fire from now, not a walk down the calendar.
            std::hint::black_box(cron.find_next_occurrence(&start, false).unwrap());
        }
        let per = t0.elapsed().as_nanos() as f64 / N as f64;
        println!("   croner {pattern:<18} {per:>8.0} ns/call");
    }

    // Catch-up after an outage: every fire a 5-minute schedule missed while
    // the box was down. The cost of the "fire-all" misfire policy.
    let cron = croner::Cron::from_str("*/5 * * * *").unwrap();
    let t0 = std::time::Instant::now();
    let mut cursor = start.clone();
    let end = start.clone() + chrono::Duration::hours(6);
    let mut missed = 0;
    while cursor < end {
        cursor = cron.find_next_occurrence(&cursor, false).unwrap();
        missed += 1;
    }
    println!(
        "   enumerating a 6-hour outage for */5: {missed} missed fires in {:?}",
        t0.elapsed()
    );
}
