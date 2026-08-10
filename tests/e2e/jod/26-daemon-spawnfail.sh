#!/usr/bin/env bash
# The PR quotes a specific failure as evidence that a fire that did not happen
# says so:
#
#   $ jod daemon --once
#   claimed 1 · started 0 · held 0 · failed 1
#   $ jod schedule log tick-demo
#   ✗ spawn_failed   the `jod-run` supervisor was not found
#
# The first attempt at this ticked before the schedule was due and so proved
# nothing. Here the tick happens *after* the due instant, with `jod-run`
# genuinely absent from PATH.
set -uo pipefail
AREA=spawnfail
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

# A directory holding `jod` but deliberately *not* `jod-run`.
ONLY=/tmp/jod-e2e/only-jod
rm -rf "$ONLY"; mkdir -p "$ONLY"
ln -s "$BIN/jod" "$ONLY/jod"

section "1. arm a schedule and let it come due"
run jod schedule add tick-demo "say OK and stop" --cron "* * * * *"
runsh "python3 -c \"
import time
w = 62 - (time.time() % 60)
print(f'sleeping {w:.1f}s so the instant is genuinely past')
time.sleep(w)\""

section "2. tick with jod-run absent from PATH"
runsh "PATH='$ONLY:/usr/bin:/bin' JOD_HOME='$JOD_HOME' '$ONLY/jod' daemon --once"
runsh "PATH='$ONLY:/usr/bin:/bin' JOD_HOME='$JOD_HOME' '$ONLY/jod' schedule log tick-demo"
runsh "q \"SELECT id, outcome, detail, run_id FROM schedule_fires ORDER BY id\""

section "3. the failure must be counted against the schedule"
runsh "q \"SELECT name, state, consecutive_failures, last_fire_at_ms, next_fire_at_ms FROM schedules\""

section "4. no orphan run row for a spawn that never happened"
runsh "q \"SELECT id, name, status FROM runs\""
run jod ls

section "5. repeated failures: does the breaker the PR mentions trip?"
for i in 1 2 3 4 5 6; do
  runsh "python3 -c \"
import time
time.sleep(max(0, 62 - (time.time() % 60)))\""
  echo "--- tick $i ---"
  runsh "PATH='$ONLY:/usr/bin:/bin' JOD_HOME='$JOD_HOME' '$ONLY/jod' daemon --once"
  runsh "q \"SELECT name, state, consecutive_failures FROM schedules\""
done

section "6. what the log shows after repeated failure"
runsh "PATH='$ONLY:/usr/bin:/bin' JOD_HOME='$JOD_HOME' '$ONLY/jod' schedule log tick-demo -l 20"
runsh "q \"SELECT id, due_at_ms, outcome, detail FROM schedule_fires ORDER BY id\""

section "7. resume clears the failure count, as the help claims"
run jod schedule resume tick-demo
runsh "q \"SELECT name, state, consecutive_failures FROM schedules\""
