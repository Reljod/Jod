#!/usr/bin/env bash
# Does `jod-api token revoke` actually revoke?
#
# 40- saw a revoked token keep working, and 41- saw a freshly issued token
# refused. Both point at the same thing: the daemon reads api-tokens.json once
# at boot. This is the clean sequence that settles it.
set -uo pipefail
AREA=revoke
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
PORT=18789
BASE="http://127.0.0.1:$PORT"
export JOD_API_BIND="127.0.0.1:$PORT"

try() {
  curl -sS -o /dev/null -w "$1: HTTP %{http_code}\n" \
    -H "Authorization: Bearer $2" "$BASE/v1/agents"
}

section "1. issue a token BEFORE the daemon starts"
TOK="$(jod-api token issue victim --scope read 2>&1 | grep -oE 'jod_[a-f0-9]+' | head -1)"
echo "token: ${TOK:0:16}…"
run jod-api token list

section "2. start the daemon"
jod-api serve > "$JOD_HOME/api.log" 2>&1 &
API_PID=$!
trap 'kill $API_PID 2>/dev/null' EXIT
sleep 2
runsh "try 'while valid' '$TOK'"

section "3. revoke it. The device is compromised; this is the emergency lever."
run jod-api token revoke victim
run jod-api token list
echo "-- the token store no longer contains it. Does the daemon care? --"
runsh "try 'after revoke, daemon still running' '$TOK'"

section "4. a token issued while the daemon runs"
NEW="$(jod-api token issue latecomer --scope read 2>&1 | grep -oE 'jod_[a-f0-9]+' | head -1)"
echo "token: ${NEW:0:16}…"
runsh "try 'newly issued, daemon still running' '$NEW'"

section "5. restart the daemon"
kill $API_PID 2>/dev/null; wait $API_PID 2>/dev/null
jod-api serve > "$JOD_HOME/api2.log" 2>&1 &
API_PID=$!
sleep 2
runsh "try 'revoked token, after restart' '$TOK'"
runsh "try 'latecomer token, after restart' '$NEW'"

kill $API_PID 2>/dev/null; wait $API_PID 2>/dev/null
echo "daemon stopped"
