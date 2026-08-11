# Monitors — end-to-end proof

```sh
cargo build --release --bin jod
bash tests/e2e/monitor/probe.sh
```

## Why this needed building at all

The change detector, the diff renderer, the suppression fold in the tick
(`ticker::plan`), the `monitor_checks` history and the storage layer were all
built and tested. `Store::set_monitor` and `Store::delete_monitor` had **no
production caller** — only test functions.

So on every real machine the `monitors` table was empty, every schedule fired
its cron unconditionally, and the feature whose entire purpose is *not spending
money* was unreachable while every test was green. This is the same defect as
the webhook rules table, one table over, found by the same audit once
`core/src/webhook.rs` stopped hiding a NUL byte from grep.

`jod monitor set` is the missing verb.

## The outcomes, each demonstrated

| what the probe did | verdict | what a tick would spend |
|---|---|---|
| first sighting | `baseline` | nothing |
| identical bytes | `unchanged` | **nothing** — the branch the module exists for |
| different bytes | `changed` | one run, with the diff in front of the prompt |
| `no_agent`, stdout | `reported` | nothing — the stdout *is* the result |
| `no_agent`, silence | `silent` | nothing |
| exit 3 | `failed` | nothing, and it is not filed as "unchanged" |

The last row is the one worth keeping honest. A watchdog broken for a week and
a watchdog dutifully reporting all-is-well both produce no runs; only the
recorded outcome distinguishes them.

```
=== change the watched file → changed, with a diff ===
changed — a tick now would run the schedule with:
MONITOR CHANGE DETECTED
@@ line 1 @@
- version 1
+ version 2
```

## `--record`, and the hole that found it

`jod monitor check` is a dry run: it probes, reports, and records nothing, so
testing a monitor cannot consume the very change the next real tick exists to
notice.

Writing the e2e exposed the cost of that being the *only* behaviour: with no
way to set a baseline from the CLI, `unchanged` and `changed` were unreachable
end to end — the two outcomes the whole feature is for. An operator had the same
problem in a different shape: no way to say "start watching from here", only
waiting for the daemon's first tick.

`--record` closes both. Dry stays the default, so the safe thing needs no flag.

The script asserts the distinction rather than assuming it:

```
=== still unchanged from the ARMED baseline — the dry check above moved nothing ===
still 'changed' — the dry check did not absorb it
```

Without that assertion, a `check` that quietly recorded would look identical in
every other line of output — and the failure it caused would surface a day
later as "the monitor never fired", which is the least debuggable ending
available.

## One combination refused

```
=== --no-agent on a URL is refused ===
Error: --no-agent reports the probe's whole output every tick, which for a URL
is the entire page — drop --no-agent to be told only when it changes
```

`no_agent` means "stdout is the result". For a URL that is the whole page,
delivered in full on every tick — a notification firehose rather than a
watchdog. `Mode` is documented as two modes rather than two flags for the same
reason: the combination has no honest reading.

## Defaults worth knowing

- A monitor's `cwd` defaults to **the schedule's own** directory, so the probe
  and the run it gates look at the same tree. A monitor watching `git log` in
  some other checkout is a bug that reads as a working monitor.
- Re-pointing a monitor does **not** carry the digest over. A monitor aimed at
  something new has seen nothing, and its next check is a baseline.
- `jod monitor ls` marks a monitor with no baseline `○`, and says so, because
  "why did my monitor not fire" is answered there more often than anywhere else.
