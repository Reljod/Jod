#!/usr/bin/env bash
# Attempts to break `jod daemon --once`, rather than to confirm it.
#
# The PR's headline scheduling claim is that the guarded single-statement claim
# gives "0 duplicates in 5,408 claims" where a read-then-write handed the same
# schedule to two winners 41.26% of the time. That is a concurrency claim, so it
# is tested with concurrency: sixteen daemons launched at once against one due
# schedule. If the claim leaks, more than one real Claude Code run starts — the
# cost of this test is itself the signal.
#
# Then: misfire policy across a gap, a cwd that does not exist, and killing the
# supervisor mid-run.
set -uo pipefail
AREA=dbreak
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
        print(f'settled after {i*5}s: ' + ', '.join(f'{r[0][:8]}={r[1]}' for r in runs))
        sys.exit(0)
    time.sleep(5)
print('did not settle in 180s')
PY
  echo
}

due_now() {
  python3 - "$DB" "${1:-0}" <<'PY'
import sqlite3, sys, time
con = sqlite3.connect(sys.argv[1])
back = int(sys.argv[2])
con.execute("UPDATE schedules SET next_fire_at_ms = ?",
            (int(time.time()*1000) - 1000 - back,))
con.commit()
PY
}

section "1. THE RACE: one due schedule, sixteen daemons at once"
run jod schedule add contended "say OK and stop" --cron "* * * * *"
due_now
echo "-- launching 16 concurrent \`jod daemon --once\` --"
for i in $(seq 1 16); do
  ( JOD_HOME="$JOD_HOME" "$BIN/jod" daemon --once > "$JOD_HOME/tick-$i.log" 2>&1 ) &
done
wait
echo "-- what each daemon reported --"
runsh "grep -h . '$JOD_HOME'/tick-*.log | sort | uniq -c | sort -rn"
echo
echo "-- THE TEST: exactly one fire, and exactly one run --"
runsh "q \"SELECT count(*) AS fires FROM schedule_fires\""
runsh "q \"SELECT count(*) AS runs FROM runs\""
runsh "q \"SELECT id, due_at_ms, outcome, run_id FROM schedule_fires ORDER BY id\""
runsh "q \"SELECT id, name, status FROM runs\""
echo "-- how many real claude processes did that start? --"
runsh "pgrep -fc 'claude -p say OK and stop' || echo 0"
settle
run jod report

section "2. the same race, but the schedule is NOT due"
runsh "python3 - '$DB' <<'PY'
import sqlite3, sys, time
con = sqlite3.connect(sys.argv[1])
con.execute('UPDATE schedules SET next_fire_at_ms = ?', (int(time.time()*1000) + 3600_000,))
con.commit(); print('pushed an hour out')
PY"
for i in $(seq 1 8); do
  ( JOD_HOME="$JOD_HOME" "$BIN/jod" daemon --once > "$JOD_HOME/idle-$i.log" 2>&1 ) &
done
wait
runsh "grep -h 'claimed' '$JOD_HOME'/idle-*.log | sort | uniq -c"
runsh "q \"SELECT count(*) AS fires_total FROM schedule_fires\""

section "3. MISFIRE: instants missed while Jod was down"
echo "-- fire_once must fire once for a long gap, not once per missed instant --"
run jod schedule rm contended
run jod schedule add missed-once "say OK and stop" --cron "* * * * *" --misfire fire_once
echo "-- backdate the next fire by three hours: 180 missed minutes --"
due_now 10800000
runsh "q \"SELECT name, misfire, next_fire_at_ms FROM schedules\""
run jod daemon --once
runsh "q \"SELECT count(*) AS fires FROM schedule_fires\""
runsh "q \"SELECT id, due_at_ms, outcome FROM schedule_fires ORDER BY id\""
echo "-- and the next fire must be back in the future, not still three hours ago --"
runsh "python3 - '$DB' <<'PY'
import sqlite3, sys, time, datetime
con = sqlite3.connect(f'file:{sys.argv[1]}?mode=ro', uri=True)
now = time.time()*1000
for name, nf in con.execute('SELECT name, next_fire_at_ms FROM schedules'):
    d = (nf - now)/1000
    print(f'{name}: next fire in {d:.0f}s ({\"future\" if d > 0 else \"STILL IN THE PAST\"})')
PY"
settle

section "4. a schedule whose cwd does not exist"
run jod schedule add badcwd "say OK and stop" --cron "* * * * *" --cwd /nonexistent/path/xyz
runsh "q \"SELECT name, cwd FROM schedules WHERE name='badcwd'\""
due_now
run jod daemon --once
run jod schedule log badcwd
runsh "q \"SELECT s.name, f.outcome, f.detail FROM schedule_fires f JOIN schedules s ON s.id=f.schedule_id WHERE s.name='badcwd'\""

section "5. kill the supervisor mid-run: does the run stay 'running' for ever?"
run jod schedule rm badcwd
run jod schedule rm missed-once
run jod schedule add killme "count slowly to fifty, one number per line" --cron "* * * * *"
due_now
run jod daemon --once
runsh "q \"SELECT id, name, status, pid, pgid FROM runs ORDER BY created_at_ms DESC LIMIT 1\""
echo "-- kill the supervisor's process group --"
runsh "python3 - '$DB' <<'PY'
import sqlite3, sys, os, signal, time
con = sqlite3.connect(f'file:{sys.argv[1]}?mode=ro', uri=True)
row = con.execute(\"SELECT id, pgid FROM runs WHERE status='running' ORDER BY created_at_ms DESC LIMIT 1\").fetchone()
if not row:
    print('no running run to kill'); sys.exit(0)
rid, pgid = row
print(f'killing pgid {pgid} for run {rid[:8]}')
os.killpg(pgid, signal.SIGKILL)
time.sleep(2)
PY"
runsh "q \"SELECT id, name, status FROM runs ORDER BY created_at_ms DESC LIMIT 1\""
echo "-- a later tick must notice the corpse rather than believe it is still running --"
run jod daemon --once
runsh "q \"SELECT id, name, status FROM runs ORDER BY created_at_ms DESC LIMIT 1\""
run jod ls
run jod report
