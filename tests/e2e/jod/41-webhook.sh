#!/usr/bin/env bash
# The GitHub webhook endpoint, signed for real.
#
# HMAC-SHA256 over the exact bytes sent, `sha256=`-prefixed, in
# `x-hub-signature-256` — the scheme api/tests/webhook.rs uses. Every payload
# below is signed by an independent implementation (python hmac), not by Jod's
# own `sign`, so a bug shared between signer and verifier cannot hide.
#
# Also settles the revocation question 40- raised: does restarting the daemon
# make a revoked token stop working?
set -uo pipefail
AREA=webhook
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

PORT=18788
BASE="http://127.0.0.1:$PORT"
HOOK="$BASE/webhooks/github"
SECRET="the-e2e-secret"
export JOD_API_BIND="127.0.0.1:$PORT"
export JOD_GITHUB_WEBHOOK_SECRET="$SECRET"

# Sign a body with an independent HMAC implementation.
sig() {
  python3 -c "
import hmac, hashlib, sys
body = open(sys.argv[1],'rb').read()
print('sha256=' + hmac.new(sys.argv[2].encode(), body, hashlib.sha256).hexdigest())
" "$1" "$2"
}

post() {
  local label="$1" body_file="$2"; shift 2
  echo "\$ POST /webhooks/github  [$label]"
  curl -sS -o /tmp/jod-e2e/hook-body.txt -w 'HTTP %{http_code}\n' \
    -X POST -H 'content-type: application/json' \
    --data-binary "@$body_file" "$@" "$HOOK"
  head -c 600 /tmp/jod-e2e/hook-body.txt
  echo
  echo
}

BODY=/tmp/jod-e2e/hook-payload.json
cat > "$BODY" <<'JSON'
{"action":"opened","number":7,
 "repository":{"full_name":"Reljod/Jod"},
 "pull_request":{"title":"a real pull request","body":"please review"}}
JSON

section "0. start the daemon with a secret configured"
jod-api serve > "$JOD_HOME/api.log" 2>&1 &
API_PID=$!
trap 'kill $API_PID 2>/dev/null' EXIT
sleep 2
runsh "cat '$JOD_HOME/api.log'"
runsh "curl -sS -w ' HTTP %{http_code}\n' '$BASE/v1/health'"

section "1. the payload and its true signature"
runsh "cat '$BODY'"
TAG="$(sig "$BODY" "$SECRET")"
echo "signature: $TAG"

section "2. a correctly signed delivery"
post "valid" "$BODY" \
  -H "x-github-delivery: d-001" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: $TAG"
runsh "q \"SELECT delivery_id, event, status, detail FROM webhook_deliveries ORDER BY received_at_ms\""

section "3. a WRONG signature must be refused"
BADTAG="$(sig "$BODY" "not-the-secret")"
echo "tag signed with the wrong secret: $BADTAG"
post "wrong signature" "$BODY" \
  -H "x-github-delivery: d-002" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: $BADTAG"

section "4. a TAMPERED body against a valid signature must be refused"
TAMPERED=/tmp/jod-e2e/hook-tampered.json
runsh "sed 's/a real pull request/a tampered pull request/' '$BODY' > '$TAMPERED'; cat '$TAMPERED'"
echo "-- sending the tampered body with the signature for the ORIGINAL body --"
post "tampered body, original signature" "$TAMPERED" \
  -H "x-github-delivery: d-003" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: $TAG"

section "5. malformed and missing signatures"
post "no signature header" "$BODY" \
  -H "x-github-delivery: d-004" -H "x-github-event: pull_request"
post "empty signature" "$BODY" \
  -H "x-github-delivery: d-005" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: "
post "prefix only" "$BODY" \
  -H "x-github-delivery: d-006" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: sha256="
post "not hex" "$BODY" \
  -H "x-github-delivery: d-007" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: sha256=zzzz"
post "sha1 scheme" "$BODY" \
  -H "x-github-delivery: d-008" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: sha1=${TAG#sha256=}"
post "correct hex, no prefix" "$BODY" \
  -H "x-github-delivery: d-009" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: ${TAG#sha256=}"
post "uppercased hex" "$BODY" \
  -H "x-github-delivery: d-010" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: sha256=$(python3 -c "print('${TAG#sha256=}'.upper())")"

section "6. GitHub's own headers are required"
post "no delivery id" "$BODY" -H "x-github-event: pull_request" -H "x-hub-signature-256: $TAG"
post "no event" "$BODY" -H "x-github-delivery: d-011" -H "x-hub-signature-256: $TAG"
post "neither" "$BODY" -H "x-hub-signature-256: $TAG"

section "7. REDELIVERY: the same delivery id twice"
echo "-- d-001 was already accepted in section 2 --"
post "replay of d-001" "$BODY" \
  -H "x-github-delivery: d-001" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: $TAG"
runsh "q \"SELECT delivery_id, event, status, detail FROM webhook_deliveries ORDER BY received_at_ms\""
runsh "q \"SELECT count(*) AS rows_for_d001 FROM webhook_deliveries WHERE delivery_id='d-001'\""

section "8. a body that is signed but not JSON"
NOTJSON=/tmp/jod-e2e/hook-notjson.txt
runsh "printf 'this is not json at all' > '$NOTJSON'"
NJTAG="$(sig "$NOTJSON" "$SECRET")"
post "signed non-JSON" "$NOTJSON" \
  -H "x-github-delivery: d-012" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: $NJTAG"

section "9. every delivery this endpoint recorded"
runsh "q \"SELECT delivery_id, event, repo, action, status, detail FROM webhook_deliveries ORDER BY received_at_ms\""
runsh "cat '$JOD_HOME/audit.jsonl' 2>/dev/null | tail -20"

section "10. with NO secret configured, a valid-looking delivery must be refused"
kill $API_PID 2>/dev/null; wait $API_PID 2>/dev/null
env -u JOD_GITHUB_WEBHOOK_SECRET JOD_HOME="$JOD_HOME" JOD_API_BIND="127.0.0.1:$PORT" \
  "$BIN/jod-api" serve > "$JOD_HOME/api2.log" 2>&1 &
API_PID=$!
sleep 2
runsh "cat '$JOD_HOME/api2.log'"
post "no secret configured" "$BODY" \
  -H "x-github-delivery: d-020" -H "x-github-event: pull_request" \
  -H "x-hub-signature-256: $TAG"
echo "-- the refusal must be indistinguishable from a bad signature, but recorded --"
runsh "q \"SELECT delivery_id, status, detail FROM webhook_deliveries WHERE delivery_id='d-020'\""

section "11. does restarting the daemon make a revoked token stop working?"
run jod-api token issue tempdevice --scope read
TOK="$(jod-api token issue tempdevice2 --scope read 2>&1 | grep -oE 'jod_[a-f0-9]+' | tail -1)"
runsh "curl -sS -o /dev/null -w 'before revoke: HTTP %{http_code}\n' -H 'Authorization: Bearer $TOK' '$BASE/v1/agents'"
run jod-api token revoke tempdevice2
runsh "curl -sS -o /dev/null -w 'after revoke, same daemon: HTTP %{http_code}\n' -H 'Authorization: Bearer $TOK' '$BASE/v1/agents'"
kill $API_PID 2>/dev/null; wait $API_PID 2>/dev/null
env -u JOD_GITHUB_WEBHOOK_SECRET JOD_HOME="$JOD_HOME" JOD_API_BIND="127.0.0.1:$PORT" \
  "$BIN/jod-api" serve > "$JOD_HOME/api3.log" 2>&1 &
API_PID=$!
sleep 2
runsh "curl -sS -o /dev/null -w 'after revoke, restarted daemon: HTTP %{http_code}\n' -H 'Authorization: Bearer $TOK' '$BASE/v1/agents'"

kill $API_PID 2>/dev/null; wait $API_PID 2>/dev/null
echo "daemon stopped"
