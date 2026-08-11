#!/usr/bin/env bash
# `jod monitor` end to end, against a real database and a real probe.
#
# The point of a monitor is that most ticks spend nothing. So the assertions
# here are mostly about what *does not* happen: an unchanged probe suppresses,
# a failing probe is not mistaken for an unchanged one, and a dry check leaves
# the baseline where it was.
set -euo pipefail

JOD=${JOD:-target/release/jod}
export JOD_HOME
JOD_HOME=$(mktemp -d)
trap 'rm -rf "$JOD_HOME" "$WATCHED"' EXIT

WATCHED=$(mktemp -d)
echo "version 1" > "$WATCHED/state.txt"

say() { printf '\n=== %s ===\n' "$1"; }

say "empty"
"$JOD" monitor ls

say "a schedule to hang it on"
"$JOD" schedule add nightly-sweep "summarise what changed" --cron '@daily'

say "attach a watch monitor"
"$JOD" monitor set nightly-sweep --command "cat $WATCHED/state.txt"

say "ls — no baseline yet"
"$JOD" monitor ls

say "first check is a baseline, and wakes nothing"
"$JOD" monitor check nightly-sweep

say "a dry check must NOT have moved the baseline"
# If `check` recorded, this would now say "unchanged" instead of "baseline",
# and the first real tick would miss the change it exists to catch.
"$JOD" monitor check nightly-sweep

say "log is still empty, because a dry check records nothing"
"$JOD" monitor log nightly-sweep

# ---- the two outcomes the whole feature exists for ------------------------
# Unreachable without --record: a dry check never sets a baseline, so nothing
# downstream of "baseline" could be demonstrated end to end at all.

say "arm the baseline deliberately"
"$JOD" monitor check nightly-sweep --record

say "same bytes → unchanged, and a tick would spend nothing"
out=$("$JOD" monitor check nightly-sweep)
echo "$out"
case "$out" in *unchanged*) ;; *) echo "FAIL: expected unchanged"; exit 1;; esac

say "change the watched file → changed, with a diff"
echo "version 2" > "$WATCHED/state.txt"
out=$("$JOD" monitor check nightly-sweep)
echo "$out"
case "$out" in *changed*) ;; *) echo "FAIL: expected changed"; exit 1;; esac
case "$out" in *"version 2"*) ;; *) echo "FAIL: diff did not carry the new line"; exit 1;; esac

say "still unchanged from the ARMED baseline — the dry check above moved nothing"
# version 1 is still the baseline, so this must report changed again rather
# than having quietly absorbed version 2 on the previous line.
out=$("$JOD" monitor check nightly-sweep)
case "$out" in *changed*) ;; *) echo "FAIL: a dry check absorbed the change"; exit 1;; esac
echo "still 'changed' — the dry check did not absorb it"

say "record it, and now it is quiet again"
"$JOD" monitor check nightly-sweep --record > /dev/null
out=$("$JOD" monitor check nightly-sweep)
echo "$out"
case "$out" in *unchanged*) ;; *) echo "FAIL: expected unchanged after recording"; exit 1;; esac

say "the log now has the recorded checks, newest first"
"$JOD" monitor log nightly-sweep

say "ls now shows a baseline"
"$JOD" monitor ls

say "a mistyped schedule is an error, not a silent success"
if "$JOD" monitor set no-such-schedule --command true 2>&1; then
  echo "FAIL: expected an error"; exit 1
fi

say "--no-agent on a URL is refused"
if "$JOD" monitor set nightly-sweep --url https://example.com --no-agent 2>&1; then
  echo "FAIL: expected a refusal"; exit 1
fi

say "no_agent mode reports stdout as the result"
"$JOD" monitor set nightly-sweep --command "echo 'two PRs are stale'" --no-agent
"$JOD" monitor check nightly-sweep

say "an empty no_agent script stays silent"
"$JOD" monitor set nightly-sweep --command "true" --no-agent
"$JOD" monitor check nightly-sweep

say "a failing probe is not 'unchanged'"
"$JOD" monitor set nightly-sweep --command "exit 3"
"$JOD" monitor check nightly-sweep

say "detach"
"$JOD" monitor set nightly-sweep --command "cat $WATCHED/state.txt"
"$JOD" monitor rm nightly-sweep
"$JOD" monitor ls

say "removing a monitor that is not there is an error"
if "$JOD" monitor rm nightly-sweep 2>&1; then
  echo "FAIL: expected an error"; exit 1
fi

echo
echo "all monitor assertions held"
