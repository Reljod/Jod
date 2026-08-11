#!/usr/bin/env bash
# Goals: add/ls/run/log/pause/resume/rm, and the two stops the PR singles out —
# budget and stall detection. `goal run` starts a real harness.
set -uo pipefail
AREA=goals
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

section "1. set a standing objective"
run jod goal add tidy "keep the scratch directory tidy"
run jod goal ls
run jod goal ls --json

section "2. all the knobs"
run jod goal add capped "a capped goal" --max-iterations 2 --budget 0.50 --stall-after 3 -c "*/5 * * * *" -z Asia/Manila --done-when "test -f /tmp/jod-e2e/goals/DONE"
runsh "q \"SELECT name, cron, timezone, state, iteration, max_iterations, budget_usd, spent_usd, stall_after, no_progress, done_when FROM goals ORDER BY name\""

section "3. bad input at the boundary"
run jod goal add badcron "x" -c "not a cron"
run jod goal add badzone "x" -z "Mars/Olympus"
run jod goal add tidy "a duplicate name"
run jod goal ls

section "4. log before anything has happened"
run jod goal log tidy
run jod goal log ghost

section "5. pause, and what pause means for run"
run jod goal pause tidy
run jod goal ls
run jod goal run tidy
runsh "q \"SELECT name, state FROM goals ORDER BY name\""
echo "-- the daemon must not advance a paused goal --"
run jod daemon --once
runsh "q \"SELECT name, state, iteration FROM goals ORDER BY name\""

section "6. resume"
run jod goal resume tidy
runsh "q \"SELECT name, state, iteration FROM goals ORDER BY name\""

section "7. a REAL iteration"
run jod goal add tiny "say OK and stop" --max-iterations 1
run jod goal run tiny
runsh "q \"SELECT name, state, iteration, spent_usd, no_progress FROM goals WHERE name='tiny'\""
run jod ls

section "8. wait for the iteration to settle"
runsh "python3 - <<'PY'
import sqlite3, time, os
db = os.environ['JOD_HOME'] + '/jod.db'
for i in range(48):
    con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
    runs = con.execute('SELECT id, status FROM runs').fetchall()
    goals = con.execute(\"SELECT name, state, iteration, spent_usd FROM goals WHERE name='tiny'\").fetchall()
    con.close()
    print(f'[{i*5:3d}s] runs=' + ','.join(f'{r[0][:8]}={r[1]}' for r in runs) + f' goal={goals}')
    if runs and all(r[1] not in ('running','starting','queued') for r in runs):
        print('settled'); break
    time.sleep(5)
PY"
run jod ls
run jod goal ls

section "9. what the goal recorded about its own iteration"
echo "-- the PR states plainly: 'Goal iterations do not yet write their episodic record.' --"
run jod goal log tiny
runsh "q \"SELECT id, scope, subject, predicate, object, origin FROM facts ORDER BY id\""
runsh "q \"SELECT name, state, iteration, spent_usd, no_progress FROM goals ORDER BY name\""

section "10. does the goal's spend get attributed back to it?"
run jod report
runsh "q \"SELECT name, spent_usd, budget_usd FROM goals ORDER BY name\""

section "11. max-iterations must stop it"
run jod goal ls --json
runsh "q \"SELECT name, state, iteration, max_iterations FROM goals WHERE name='tiny'\""

section "12. rm"
run jod goal rm capped
run jod goal rm ghost
run jod goal ls
