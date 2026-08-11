#!/usr/bin/env bash
# Does a scheduled run actually REACH Jod's MCP tools?
#
# 33- only established that the flags are on the command line
# (`--mcp-config … --strict-mcp-config`, `mcp__jod` in the allowlist). That is
# not the same as the server starting, the harness connecting to it, and a tool
# call landing on the store — any of which can fail silently and leave a run
# that looks perfectly healthy.
#
# So: seed a fact the agent could only know by calling `recall` through the
# server, then read the transcript for an `mcp__jod` tool call and check whether
# the answer came back.
set -uo pipefail
AREA=mcp
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

due_now() {
  python3 - "$DB" <<'PY'
import sqlite3, sys, time
con = sqlite3.connect(sys.argv[1])
con.execute("UPDATE schedules SET next_fire_at_ms = ?", (int(time.time()*1000) - 1000,))
con.commit()
PY
}

section "0. does `jod mcp` speak the protocol at all?"
echo "-- an initialize + tools/list handshake straight down stdin --"
runsh "printf '%s\n%s\n' \
  '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"e2e\",\"version\":\"0\"}}}' \
  '{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}' \
  | JOD_HOME='$JOD_HOME' '$BIN/jod' mcp --access read_only 2>&1 | head -4 | cut -c1-400"

section "1. the tool set really differs by access level"
for lvl in read_only delegate orchestrate; do
  echo "--- $lvl ---"
  runsh "printf '%s\n%s\n' \
    '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"e2e\",\"version\":\"0\"}}}' \
    '{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}' \
    | JOD_HOME='$JOD_HOME' '$BIN/jod' mcp --access $lvl 2>/dev/null \
    | python3 -c \"
import sys, json
for line in sys.stdin:
    try: d = json.loads(line)
    except Exception: continue
    if d.get('id') == 2:
        print(', '.join(t['name'] for t in d['result']['tools']))\""
done

section "2. seed a fact the agent can only learn by calling recall"
run jod remember cerulean-widget serial-number "QX-7741-ZULU"
runsh "q \"SELECT id, subject, predicate, object FROM facts\""

section "3. a scheduled run told to use the tool, not to guess"
run jod schedule add ask "Use your jod MCP tools to recall what you know about cerulean-widget, then reply with just its serial number. Do not guess." --cron "@daily"
due_now
run jod daemon --once
settle

section "4. did an mcp__jod tool call actually happen?"
runsh "q \"SELECT seq, kind, substr(payload,1,300) FROM events ORDER BY seq\""
echo
echo "== the test =="
runsh "q \"SELECT count(*) AS mcp_tool_calls FROM events WHERE kind='tool_call' AND payload LIKE '%mcp__jod%'\""
runsh "q \"SELECT count(*) AS answered_with_the_serial FROM events WHERE kind='finished' AND payload LIKE '%QX-7741-ZULU%'\""

section "5. what the run finally said"
runsh "RID=\$(python3 \"$REPO/tests/e2e/jod/db.py\" \"$DB\" 'SELECT id FROM runs LIMIT 1' | sed -n 3p); '$BIN/jod' watch \$RID"

section "6. and can that read_only run WRITE through the tools?"
echo "-- remember/schedule_create must not be in a read_only tool set --"
run jod schedule rm ask
run jod schedule add trywrite "Use your jod MCP tools to remember that sky is green, then say what tools you were given." --cron "@daily"
due_now
run jod daemon --once
settle
runsh "q \"SELECT seq, kind, substr(payload,1,300) FROM events ORDER BY run_id, seq\""
echo "-- the fact must NOT have been written --"
run jod recall sky
runsh "q \"SELECT id, subject, predicate, object, origin FROM facts ORDER BY id\""
