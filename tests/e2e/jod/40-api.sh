#!/usr/bin/env bash
# The HTTP API: tokens, scopes, and every route, over a real socket with curl.
set -uo pipefail
AREA=api
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

PORT=18787
BASE="http://127.0.0.1:$PORT"
export JOD_API_BIND="127.0.0.1:$PORT"
export JOD_GITHUB_WEBHOOK_SECRET="the-e2e-secret"

# Status code, then body. `-sS` so a connection failure is loud rather than an
# empty string that reads like an empty response.
c() {
  local desc="$1"; shift
  echo "\$ curl $*"
  curl -sS -o /tmp/jod-e2e/api-body.txt -w '%{http_code}' "$@" 2>&1 \
    | sed 's/^/HTTP /'
  echo
  head -c 2000 /tmp/jod-e2e/api-body.txt
  echo
  echo
}

section "1. mint tokens"
run jod-api token issue laptop --scope read
run jod-api token issue phone --scope write
run jod-api token list

echo "-- capture them again, since a token is printed once --"
READ_TOKEN="$(jod-api token issue reader --scope read 2>&1 | grep -oE '[A-Za-z0-9_-]{24,}' | tail -1)"
WRITE_TOKEN="$(jod-api token issue writer --scope write 2>&1 | grep -oE '[A-Za-z0-9_-]{24,}' | tail -1)"
echo "read  token: ${READ_TOKEN:0:12}… (${#READ_TOKEN} chars)"
echo "write token: ${WRITE_TOKEN:0:12}… (${#WRITE_TOKEN} chars)"
run jod-api token list

section "2. start the daemon"
jod-api serve > "$JOD_HOME/api.log" 2>&1 &
API_PID=$!
trap 'kill $API_PID 2>/dev/null' EXIT
sleep 2
echo "pid $API_PID"
runsh "cat '$JOD_HOME/api.log'"

section "3. health needs no credential"
c "health" "$BASE/v1/health"

section "4. every other route must refuse an anonymous caller"
c "no token" "$BASE/v1/agents"
c "no token" "$BASE/v1/harnesses"
c "no token" "$BASE/v1/report"
c "garbage token" -H "Authorization: Bearer not-a-real-token" "$BASE/v1/agents"
c "empty bearer" -H "Authorization: Bearer " "$BASE/v1/agents"
c "wrong scheme" -H "Authorization: Basic $READ_TOKEN" "$BASE/v1/agents"

section "5. a read token reads"
c "agents" -H "Authorization: Bearer $READ_TOKEN" "$BASE/v1/agents"
c "harnesses" -H "Authorization: Bearer $READ_TOKEN" "$BASE/v1/harnesses"
c "report" -H "Authorization: Bearer $READ_TOKEN" "$BASE/v1/report"
c "teams" -H "Authorization: Bearer $READ_TOKEN" "$BASE/v1/teams"

section "6. a read token must NOT spawn"
c "spawn with read token" -X POST -H "Authorization: Bearer $READ_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"prompt":"say OK and stop","harness":"claude_code"}' "$BASE/v1/agents"

section "7. a write token spawning outside allowed_cwd must fail closed"
echo "-- allowed_cwd defaults to empty, which the config says denies every spawn --"
c "spawn with write token" -X POST -H "Authorization: Bearer $WRITE_TOKEN" \
  -H 'content-type: application/json' \
  -d "{\"prompt\":\"say OK and stop\",\"harness\":\"claude_code\",\"cwd\":\"$JOD_HOME\"}" \
  "$BASE/v1/agents"

section "8. unknown routes and unknown ids"
c "unknown route" -H "Authorization: Bearer $READ_TOKEN" "$BASE/v1/nope"
c "unknown agent" -H "Authorization: Bearer $READ_TOKEN" "$BASE/v1/agents/does-not-exist"
c "unknown agent events" -H "Authorization: Bearer $READ_TOKEN" "$BASE/v1/agents/does-not-exist/events"
c "delete unknown agent" -X DELETE -H "Authorization: Bearer $WRITE_TOKEN" "$BASE/v1/agents/does-not-exist"

section "9. revoking a token must take effect"
run jod-api token revoke reader
c "revoked token" -H "Authorization: Bearer $READ_TOKEN" "$BASE/v1/agents"
run jod-api token list

section "10. the audit log"
runsh "q \"SELECT name FROM sqlite_master WHERE type='table' ORDER BY name\""
runsh "ls -la '$JOD_HOME'"
runsh "tail -40 '$JOD_HOME/audit.log' 2>/dev/null || echo 'no audit.log at that path'"

section "11. the daemon's own log"
runsh "cat '$JOD_HOME/api.log'"

kill $API_PID 2>/dev/null
wait $API_PID 2>/dev/null
echo "daemon stopped"
