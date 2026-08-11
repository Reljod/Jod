#!/usr/bin/env bash
# The claim this suite exists for: the daemon claims a due schedule and really
# spawns a harness. This one spends money — it starts Claude Code for real. The
# prompt is deliberately trivial.
#
# Two halves, because the PR shows both: without `jod-run` on PATH the fire must
# fail *and say why*, and with it the run must actually happen.
set -uo pipefail
AREA=daemon
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

section "0. what is on PATH"
run which jod
run which jod-run
run which claude
run jod harnesses

section "1. a schedule due every minute, with a one-word job"
run jod schedule add ping "say OK and stop" --cron "* * * * *"
run jod schedule ls

section "2. tick with the supervisor NOT on PATH"
echo "-- PR claims: 'spawn_failed  the jod-run supervisor was not found' --"
runsh "PATH=/usr/bin:/bin JOD_HOME='$JOD_HOME' '$BIN/jod' daemon --once"
run jod schedule log ping
runsh "q \"SELECT id, outcome, detail, run_id FROM schedule_fires ORDER BY id\""
echo "-- a failed fire must not be recorded as a success, and must count --"
runsh "q \"SELECT name, state, consecutive_failures, last_fire_at_ms FROM schedules\""

section "3. wait for the next minute boundary, then tick WITH the supervisor"
runsh "python3 -c \"
import time
t = time.time()
w = 61 - (t % 60)
print(f'sleeping {w:.1f}s for the next due instant')
time.sleep(w)\""
run jod daemon --once
echo "-- the fire record --"
run jod schedule log ping
runsh "q \"SELECT id, outcome, detail, run_id FROM schedule_fires ORDER BY id\""

section "4. is a real process actually running?"
run jod ls
runsh "q \"SELECT id, name, harness, status, pid, pgid FROM runs ORDER BY created_at_ms\""
runsh "pgrep -a -f 'jod-run' | head -5"
runsh "pgrep -a claude | grep -v 'bg-spare\|bg-pty\|daemon run' | head -5"

section "5. wait for it to finish, and read what the harness actually said"
runsh "python3 - <<'PY'
import sqlite3, time, os
db = os.environ['JOD_HOME'] + '/jod.db'
for i in range(60):
    con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
    rows = con.execute('SELECT id, status FROM runs').fetchall()
    con.close()
    print(f'[{i*5:3d}s] ' + ', '.join(f'{r[0][:8]}={r[1]}' for r in rows))
    if rows and all(r[1] not in ('running', 'starting', 'queued') for r in rows):
        print('settled')
        break
    time.sleep(5)
PY"
run jod ls
run jod schedule log ping
runsh "q \"SELECT id, name, harness, status, session_id, summary FROM runs\""

section "6. the transcript the supervisor wrote back"
runsh "q \"SELECT run_id, seq, kind, substr(text,1,200) FROM events ORDER BY run_id, seq LIMIT 40\""
echo "-- the last assistant message, which the PR quotes as proof --"
runsh "q \"SELECT status, summary FROM runs\""

section "7. jod watch replays a finished run"
runsh "RID=\$(python3 \"$REPO/tests/e2e/jod/db.py\" \"$DB\" 'SELECT id FROM runs LIMIT 1' | sed -n 3p); echo run=\$RID; '$BIN/jod' watch \$RID"

section "8. history and report"
run jod history
run jod report

section "9. a second tick must not double-fire the same instant"
run jod daemon --once
runsh "q \"SELECT id, due_at_ms, outcome FROM schedule_fires ORDER BY id\""

section "10. the run was recorded against the schedule"
runsh "q \"SELECT sf.outcome, sf.run_id, r.status FROM schedule_fires sf LEFT JOIN runs r ON r.id = sf.run_id ORDER BY sf.id\""
