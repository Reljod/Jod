#!/usr/bin/env bash
#
# test.sh — deterministic tests for `jod orc`.
# Run: .agents/skills/orchestrate/tests/test.sh
#
# Everything here is hermetic: a fake CLAUDE_BIN records the argv it was
# handed instead of starting a real session, and a fake HOME supplies a
# ~/.claude.json + ~/.claude/jobs tree. No session is ever spawned, so the
# suite is safe to run in CI and costs nothing.
#
# What that buys: the parts that can silently corrupt an orchestration —
# @project resolution, the untrusted-directory refusal, and the exact argv
# handed to `claude` — are pinned. The live behaviour of `claude --bg`
# itself is the CLI's contract, not ours, and is verified by hand.
#
set -u

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd -- "$TEST_DIR/.." && pwd)"
ORC="$SKILL_DIR/scripts/orc.mjs"
# shellcheck source=/dev/null
source "$SKILL_DIR/../test-scenarios/scripts/assert.sh"

command -v node >/dev/null 2>&1 || { echo "SKIP: node not on PATH"; exit 0; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FAKE_HOME="$WORK/home"
ARGV_LOG="$WORK/argv.log"
TRUSTED="$WORK/trusted-proj"
UNTRUSTED="$WORK/untrusted-proj"
mkdir -p "$FAKE_HOME/.claude/jobs" "$TRUSTED" "$UNTRUSTED"

# A stand-in for the real binary: log argv, then emit the one line
# `claude --bg` prints so orc's id parser has something to chew on.
cat > "$WORK/fake-claude" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$ARGV_LOG"
case "$1" in
  agents) printf '[]\n' ;;
  *)      printf 'backgrounded · deadbeef\n' ;;
esac
SH
chmod +x "$WORK/fake-claude"

cat > "$FAKE_HOME/.claude.json" <<JSON
{ "projects": {
  "$TRUSTED":   { "hasTrustDialogAccepted": true },
  "$UNTRUSTED": { "hasTrustDialogAccepted": false }
} }
JSON

# A finished session, shaped like a real state.json.
mkdir -p "$FAKE_HOME/.claude/jobs/abc123"
cat > "$FAKE_HOME/.claude/jobs/abc123/state.json" <<JSON
{ "state": "done", "name": "demo session", "sessionId": "sid-abc-123",
  "cwd": "$TRUSTED", "output": { "result": "THE RESULT" } }
JSON

orc() { HOME="$FAKE_HOME" ARGV_LOG="$ARGV_LOG" CLAUDE_BIN="$WORK/fake-claude" node "$ORC" "$@" 2>&1; }
last_argv() { tail -1 "$ARGV_LOG"; }

echo "== orchestrate test suite =="

# --- @project resolution -----------------------------------------------------
out="$(orc projects)"
case "$out" in
  *"@$(basename "$TRUSTED")"*) pass "projects: lists a trusted project" ;;
  *) fail "projects: missing trusted project — got: $out" ;;
esac
case "$out" in
  *"! @$(basename "$UNTRUSTED")"*) pass "projects: flags untrusted with '!'" ;;
  *) fail "projects: untrusted not flagged — got: $out" ;;
esac

out="$(orc spawn "@nope-does-not-exist" "task")"
case "$out" in
  *"no project matching"*) pass "spawn: unknown @project is refused" ;;
  *) fail "spawn: unknown @project not refused — got: $out" ;;
esac

# --- the untrusted-directory guard ------------------------------------------
# The bug this prevents: a session that starts, appears in the agent view, and
# hangs on an unattended trust prompt without ever reaching the model.
: > "$ARGV_LOG"
out="$(orc spawn "$UNTRUSTED" "task")"
case "$out" in
  *"not a trusted project directory"*) pass "spawn: refuses an untrusted directory" ;;
  *) fail "spawn: untrusted directory not refused — got: $out" ;;
esac
if [ ! -s "$ARGV_LOG" ]; then pass "spawn: refusal starts no session"; else fail "spawn: called claude anyway"; fi

# --- spawn -------------------------------------------------------------------
: > "$ARGV_LOG"
out="$(orc spawn "$TRUSTED" "do the thing")"
[ "$out" = "deadbeef" ] && pass "spawn: prints the new session id" || fail "spawn: bad id — got: $out"
[ "$(last_argv)" = "--bg do the thing" ] && pass "spawn: argv is '--bg <task>'" || fail "spawn: argv was '$(last_argv)'"

# --- send: must resume by sessionId, not job id ------------------------------
: > "$ARGV_LOG"
orc send abc123 "follow up" >/dev/null
[ "$(last_argv)" = "--bg -r sid-abc-123 follow up" ] \
  && pass "send: resumes the sessionId with -r" || fail "send: argv was '$(last_argv)'"

out="$(orc send "demo" "x")"
[ "$out" = "deadbeef" ] && pass "send: resolves a session by name" || fail "send: name lookup failed — got: $out"

out="$(orc send "no-such-session" "x")"
case "$out" in
  *"no session matching"*) pass "send: unknown session is refused" ;;
  *) fail "send: unknown session not refused — got: $out" ;;
esac

# --- fanout ------------------------------------------------------------------
: > "$ARGV_LOG"
orc fanout "$TRUSTED" "$TRUSTED" -- "shared task" >/dev/null
[ "$(wc -l < "$ARGV_LOG")" -eq 2 ] && pass "fanout: starts one session per target" \
  || fail "fanout: started $(wc -l < "$ARGV_LOG") sessions, expected 2"

# Validation happens before any spawn, so one bad target cannot leave half a
# team running.
: > "$ARGV_LOG"
out="$(orc fanout "$TRUSTED" "$UNTRUSTED" -- "shared task")"
case "$out" in
  *"not a trusted project directory"*) pass "fanout: refuses if any target is untrusted" ;;
  *) fail "fanout: bad target not refused — got: $out" ;;
esac
if [ ! -s "$ARGV_LOG" ]; then pass "fanout: refusal starts no partial team"; else fail "fanout: started $(wc -l < "$ARGV_LOG") sessions before failing"; fi

out="$(orc fanout "$TRUSTED" "no separator")"
case "$out" in
  *"usage"*) pass "fanout: requires the '--' separator" ;;
  *) fail "fanout: missing '--' not caught — got: $out" ;;
esac

# --- result ------------------------------------------------------------------
out="$(orc result abc123)"
[ "$out" = "THE RESULT" ] && pass "result: prints the session's result" || fail "result: got '$out'"

mkdir -p "$FAKE_HOME/.claude/jobs/working1"
printf '{ "state": "working", "sessionId": "sid-w", "detail": "still going" }\n' \
  > "$FAKE_HOME/.claude/jobs/working1/state.json"
orc result working1 >/dev/null 2>&1
[ $? -eq 2 ] && pass "result: exits 2 when there is no result yet" || fail "result: wrong exit for pending session"

assert_summary
