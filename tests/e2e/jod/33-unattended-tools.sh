#!/usr/bin/env bash
# Is an unattended run actually read-only?
#
# `ToolAccess::unattended()` returns `ReadOnly`, and the doc comment is emphatic
# about why: "A scheduled run that could schedule is a schedule that can
# multiply while you sleep". `capped_for` then leans on the same value as the
# security boundary for webhook payloads: "Untrusted means read-only, whatever
# anything else says."
#
# In practice that becomes `claude -p --allowedTools Read,Grep,Glob,WebSearch,
# WebFetch`. This asks a scheduled run to write a file and to spend money, and
# looks at whether either was actually prevented.
set -uo pipefail
AREA=tools
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }
MARK=/tmp/jod-e2e/tools/WRITTEN_BY_AN_UNATTENDED_RUN

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

due_now() {
  python3 - "$DB" <<'PY'
import sqlite3, sys, time
con = sqlite3.connect(sys.argv[1])
con.execute("UPDATE schedules SET next_fire_at_ms = ?", (int(time.time()*1000) - 1000,))
con.commit()
PY
}

section "1. a scheduled run asked to write a file"
run rm -f "$MARK"
run jod schedule add writer "Write the word hello into the file $MARK using a shell command, then stop." --cron "@daily"
due_now
run jod daemon --once
settle

section "2. the tool allowlist the run was actually given"
runsh "find /tmp/jod-e2e/tools/runs -name spawn.json | head -1 | xargs python3 -c \"
import json,sys
d = json.load(open(sys.argv[1]))
print('program:', d['program'])
print('args   :', json.dumps(d['args'][2:], indent=2))
print('cwd    :', d['cwd'])\""

section "3. did the file get written?"
runsh "ls -la '$MARK' 2>&1 || echo 'ABSENT — the read-only restriction held'"
runsh "cat '$MARK' 2>/dev/null || true"

section "4. what the transcript says the agent did"
runsh "q \"SELECT seq, kind, substr(payload,1,300) FROM events ORDER BY run_id, seq\""

section "5. the run's own verdict"
run jod ls
runsh "RID=\$(python3 \"$REPO/tests/e2e/jod/db.py\" \"$DB\" 'SELECT id FROM runs LIMIT 1' | sed -n 3p); '$BIN/jod' watch \$RID"

section "6. and can an unattended run reach Jod itself?"
echo "-- ToolAccess::unattended exists so a scheduled run cannot create schedules --"
runsh "q \"SELECT name, cron FROM schedules ORDER BY name\""
