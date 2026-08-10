#!/usr/bin/env bash
# A goal iteration that actually happens.
#
# 30- got no iteration at all: `jod goal run` is documented "Run one iteration
# now" but only brings the next fire forward — the work needs a `jod daemon`
# tick behind it. Here the tick is supplied explicitly, so the iteration is
# real and the claims about what a goal records can be checked.
set -uo pipefail
AREA=goalsreal
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

settle() {
  echo "\$ (waiting for every run to reach a terminal status)"
  python3 - <<'PY'
import sqlite3, time, os, sys
db = os.environ['JOD_HOME'] + '/jod.db'
for i in range(36):
    con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
    runs = con.execute('SELECT id, status FROM runs').fetchall()
    con.close()
    if runs and all(r[1] not in ('running', 'starting', 'queued') for r in runs):
        print(f'settled after {i*5}s: '
              + ', '.join(f'{r[0][:8]}={r[1]}' for r in runs))
        sys.exit(0)
    time.sleep(5)
print('did not settle in 180s')
PY
  echo
}

section "1. does goal run run anything by itself?"
run jod goal add tiny "say OK and stop" --max-iterations 2
run jod goal run tiny
run jod ls
runsh "q \"SELECT name, state, iteration, next_fire_at_ms FROM goals\""
echo "-- nothing has started. The tick is what does the work: --"
run jod daemon --once
run jod ls
runsh "q \"SELECT id, name, status FROM runs\""

section "2. let the iteration finish"
settle
run jod ls
run jod goal ls

section "3. did the iteration count, and did the spend land on the goal?"
runsh "q \"SELECT name, state, iteration, spent_usd, no_progress FROM goals\""
run jod report
echo "-- the PR's stated gap: 'Goal iterations do not yet write their episodic record.' --"
run jod goal log tiny
runsh "q \"SELECT id, scope, subject, predicate, object FROM facts ORDER BY id\""

section "4. a second iteration, to reach max-iterations = 2"
run jod goal run tiny
run jod daemon --once
settle
runsh "q \"SELECT name, state, iteration, spent_usd, no_progress FROM goals\""
run jod goal ls

section "5. max-iterations must now stop it"
run jod goal run tiny
run jod daemon --once
runsh "q \"SELECT name, state, iteration FROM goals\""
run jod goal ls

section "6. the runs a goal produced"
runsh "q \"SELECT id, name, harness, status FROM runs ORDER BY created_at_ms\""
run jod history

section "7. done-when: a deterministic check that passes"
run jod goal add finished "say OK and stop" --done-when "true"
run jod goal run finished
run jod daemon --once
runsh "q \"SELECT name, state, iteration FROM goals WHERE name='finished'\""
run jod goal ls

section "8. done-when that fails, and a budget already exceeded"
run jod goal add broke "say OK and stop" --done-when "false" --budget 0.0
run jod goal run broke
run jod daemon --once
runsh "q \"SELECT name, state, iteration, spent_usd, budget_usd FROM goals WHERE name='broke'\""
run jod goal ls
