#!/usr/bin/env bash
# Two things 26- could not settle.
#
# (a) The PR's exact quoted failure — the *supervisor* missing while the harness
#     is present. 26- removed both from PATH, so the harness check answered
#     first and the supervisor path was never reached.
#
# (b) The circuit breaker. BREAK_AFTER_FAILURES = 5 and backoff is 2^n minutes,
#     so five real failures are 30 minutes apart. The failures below are real
#     spawn failures; only the *clock* is advanced, by pulling next_fire_at_ms
#     back between ticks. Nothing about the outcome is faked.
set -uo pipefail
AREA=breaker
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

# `claude` is present here; `jod-run` is not.
#
# A *copy* of `jod`, not a symlink. `discovery::find_binary` looks next to
# `current_exe()` before it looks at PATH, and `current_exe()` resolves
# symlinks — so a symlinked `jod` finds the `jod-run` sitting beside its target
# and the supervisor is never actually missing.
HARNESS_DIR="$(dirname "$(command -v claude)")"
NOSUP=/tmp/jod-e2e/no-supervisor
rm -rf "$NOSUP"; mkdir -p "$NOSUP"
cp "$BIN/jod" "$NOSUP/jod"
NOSUP_PATH="$NOSUP:$HARNESS_DIR:/usr/bin:/bin"
unset JOD_SUPERVISOR_BIN

due_now() {
  python3 - "$DB" <<'PY'
import sqlite3, sys, time
con = sqlite3.connect(sys.argv[1])
con.execute("UPDATE schedules SET next_fire_at_ms = ?",
            (int(time.time() * 1000) - 1000,))
con.commit()
PY
}

section "0. a PATH with the harness but no supervisor"
runsh "PATH='$NOSUP_PATH' command -v claude"
runsh "PATH='$NOSUP_PATH' command -v jod-run || echo 'jod-run: absent, as intended'"
runsh "PATH='$NOSUP_PATH' JOD_HOME='$JOD_HOME' '$NOSUP/jod' harnesses"

section "1. the PR's quoted failure: supervisor missing"
run jod schedule add tick-demo "say OK and stop" --cron "* * * * *"
due_now
runsh "PATH='$NOSUP_PATH' JOD_HOME='$JOD_HOME' '$NOSUP/jod' daemon --once"
runsh "PATH='$NOSUP_PATH' JOD_HOME='$JOD_HOME' '$NOSUP/jod' schedule log tick-demo"
runsh "q \"SELECT outcome, detail FROM schedule_fires ORDER BY id\""

section "2. drive five real failures and watch the breaker"
for i in 2 3 4 5 6 7; do
  due_now
  echo "--- failure attempt $i ---"
  runsh "PATH='$NOSUP_PATH' JOD_HOME='$JOD_HOME' '$NOSUP/jod' daemon --once"
  runsh "q \"SELECT name, state, consecutive_failures FROM schedules\""
done

section "3. a broken schedule in ls and log"
runsh "PATH='$NOSUP_PATH' JOD_HOME='$JOD_HOME' '$NOSUP/jod' schedule ls"
runsh "PATH='$NOSUP_PATH' JOD_HOME='$JOD_HOME' '$NOSUP/jod' schedule log tick-demo -l 20"

section "4. a broken schedule must not be claimed again"
due_now
runsh "PATH='$NOSUP_PATH' JOD_HOME='$JOD_HOME' '$NOSUP/jod' daemon --once"
runsh "q \"SELECT name, state, consecutive_failures FROM schedules\""

section "5. resume must revive a broken schedule and clear the count"
run jod schedule resume tick-demo
runsh "q \"SELECT name, state, consecutive_failures FROM schedules\""
run jod schedule ls

section "6. backoff curve, for the record"
runsh "q \"SELECT id, due_at_ms, outcome FROM schedule_fires ORDER BY id\""
