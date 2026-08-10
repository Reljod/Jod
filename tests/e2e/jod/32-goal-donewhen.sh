#!/usr/bin/env bash
# Two documented goal controls, checked by running them.
#
# `--done-when` is documented "A command that decides done. Deterministic, and
# consulted before anything is asked to judge progress", and the schema calls it
# "run before anything is asked to judge progress so a pass is evidence rather
# than an opinion".
#
# `stall_after` is documented as catching "a loop completing iterations while
# nothing changes".
#
# So: a goal whose done-when passes trivially, and whose every iteration
# completes while changing nothing at all. It should finish. Watch whether it
# does.
set -uo pipefail
AREA=donewhen
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
        print(f'settled after {i*5}s'); sys.exit(0)
    time.sleep(5)
print('did not settle in 180s')
PY
  echo
}

# Bring the goal's next fire forward so iterations do not wait on the cron.
due_now() {
  python3 - "$DB" <<'PY'
import sqlite3, sys, time
con = sqlite3.connect(sys.argv[1])
con.execute("UPDATE goals SET next_fire_at_ms = ?", (int(time.time()*1000) - 1000,))
con.commit()
PY
}

section "1. a goal that is already done, by its own deterministic check"
run jod goal add alreadydone "say OK and stop" --done-when "true" --stall-after 2
runsh "q \"SELECT name, state, done_when, stall_after FROM goals\""

section "2. four iterations. Each completes; the check passes every time."
for i in 1 2 3 4; do
  echo "########## iteration $i ##########"
  due_now
  run jod daemon --once
  settle
  runsh "q \"SELECT name, state, iteration, spent_usd, no_progress FROM goals\""
done

section "3. where did it end up?"
run jod goal ls
run jod goal log alreadydone
runsh "q \"SELECT name, state, iteration, spent_usd, no_progress, stall_after FROM goals\""
run jod report

section "4. what each iteration recorded"
runsh "q \"SELECT id, subject, predicate, object FROM facts ORDER BY id\""

section "5. did Jod ever run the done-when command itself?"
echo "-- a done-when with an observable side effect: if Jod runs it, the file appears --"
run jod goal add sideeffect "say OK and stop" --done-when "touch /tmp/jod-e2e/donewhen/RAN_BY_JOD"
run rm -f /tmp/jod-e2e/donewhen/RAN_BY_JOD
due_now
run jod daemon --once
settle
runsh "ls -la /tmp/jod-e2e/donewhen/RAN_BY_JOD 2>&1 || echo 'the file does not exist: Jod never executed the check'"
runsh "q \"SELECT name, state, iteration FROM goals ORDER BY name\""

section "6. is GoalState::Satisfied reachable at all?"
runsh "q \"SELECT DISTINCT state FROM goals\""
echo "-- every state any goal in this suite has ever held --"
