#!/usr/bin/env bash
# Harness parity, end to end, with real harnesses and no fakes.
#
# The verification command in SPECS.md: for each harness present on this box it
# drives one run that sets two roots, mentions a file in the second, records a
# decision, asks a blocking question answered from the CLI, requests a secret,
# and prints it — then asserts the cards exist, the answer is stored, and the
# secret's value appears nowhere in the database.
#
# What is faked: nothing but the credential. `PARITY_SECRET` is a token this
# script generates at run time; it authenticates nothing anywhere, and it exists
# so that finding those bytes in a file is unambiguous evidence of a leak.
# SPECS.md sanctions exactly that and nothing else — no fake harness, no fake
# MCP client, no seeded rows.
#
# Slow on purpose. Every run here is a real model turn with real tool calls, and
# the blocking question genuinely waits for a person (this script) to answer it
# from the command line. This is an on-demand suite, not a push gate.
set -uo pipefail
AREA=parity
. "$(dirname "$0")/jod/env.sh"

# ---------------------------------------------------------------------------
# Two guards, and they are checked rather than commented.
#
# `jod daemon` calls `mcp_install::ensure_registered()` at startup, which
# rewrites ~/.claude.json, ~/.config/opencode/opencode.jsonc and
# ~/.gemini/config/mcp_config.json to point at whatever JOD_HOME is current. A
# suite that runs without opting out therefore repoints the developer's real
# harnesses at a scratch database under /tmp and leaves them there. The a2a
# suite discovered that the hard way, which is why this is asserted below and
# not merely set: an `export` that a later edit removes is a silent failure,
# and the thing it fails at is somebody's working machine.
export JOD_NO_MCP_INSTALL=1
# ---------------------------------------------------------------------------

PASSED=0
FAILED=0
# `check <description> <test-expression...>` — what makes this a gate rather
# than a transcript somebody has to read.
check() {
  local what="$1"; shift
  if "$@"; then
    echo "PASS  $what"
    PASSED=$((PASSED + 1))
  else
    echo "FAIL  $what"
    FAILED=$((FAILED + 1))
  fi
}

# A refusal to start. Anything here means the suite cannot run safely, as
# opposed to running and failing, and the difference matters: one is a bug in
# Jod and the other is about to damage the machine it is running on.
refuse() {
  echo
  echo "REFUSING TO RUN: $*"
  exit 2
}

section "0. the guards, before anything is launched"

[ "${JOD_NO_MCP_INSTALL:-}" = "1" ] ||
  refuse "JOD_NO_MCP_INSTALL is not set. Without it, anything here that starts
          a daemon rewrites the real harness configs to point at a scratch
          JOD_HOME and leaves them that way."
echo "JOD_NO_MCP_INSTALL=1 — the real harness configs are off limits"

# The second guard. `env.sh` points JOD_HOME at a scratch directory, but this
# suite stores a credential, so "probably scratch" is not good enough: an
# earlier version of one of these scripts wrote a secret file into the real
# ~/.jod/secrets, and nobody noticed until it was grepped for.
case "$JOD_HOME" in
  "$HOME"/.jod | "$HOME"/.jod/*)
    refuse "JOD_HOME is $JOD_HOME, which is the real one. This suite stores a
            secret and deletes its home afterwards." ;;
  /tmp/*) echo "JOD_HOME=$JOD_HOME — scratch, and not the real ~/.jod" ;;
  *) refuse "JOD_HOME is $JOD_HOME, which is neither /tmp nor recognisably
             scratch. Set JOD_E2E_HOME." ;;
esac

# Fingerprint the three files a stray `ensure_registered()` would rewrite, so
# the claim "this suite did not touch your machine" is checked at the end
# rather than asserted here. Missing is a legitimate state and is recorded as
# such — a file that did not exist before must not exist after either.
CONFIGS=("$HOME/.claude.json" "$HOME/.config/opencode/opencode.jsonc"
         "$HOME/.config/opencode/opencode.json" "$HOME/.gemini/config/mcp_config.json")
fingerprint() {
  for f in "${CONFIGS[@]}"; do
    if [ -f "$f" ]; then echo "$f $(cksum < "$f")"; else echo "$f absent"; fi
  done
}
BEFORE="$(fingerprint)"
echo "-- harness configs as found --"
echo "$BEFORE"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }
# One value out of the store, unadorned, for a shell test to compare.
#
# Not `db.py | sed -n 3p`, which is how the other suites spell this: with no
# rows, the third line of that output is the literal `(0 rows)`, and a poll
# waiting for a card to appear therefore sees a card id immediately and answers
# a card called `(0 rows)`. Empty has to mean empty.
val() {
  JOD_SQL="$1" python3 - <<'PY'
import sqlite3, os
db = os.environ['JOD_HOME'] + '/jod.db'
con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
row = con.execute(os.environ['JOD_SQL']).fetchone()
print('' if row is None or row[0] is None else str(row[0]).strip())
PY
}

# Every byte of the store, including the write-ahead log: a value sitting in
# `jod.db-wal` is as leaked as one in a table, and a check that opened the
# database through SQLite would never see it.
leaked_in_db() {
  JOD_NEEDLE="$1" python3 - <<'PY'
import os, glob, sys
needle = os.environ['JOD_NEEDLE'].encode()
hits = [p for p in glob.glob(os.environ['JOD_HOME'] + '/jod.db*')
        if needle in open(p, 'rb').read()]
print('\n'.join(hits) if hits else '')
sys.exit(0)
PY
}

# What the CLI calls a harness, as the database and `open_work` spell it.
harness_id() {
  case "$1" in
    claude) echo claude_code ;;
    opencode) echo open_code ;;
    agy) echo agy ;;
    *) echo "$1" ;;
  esac
}

# Every event payload one conversation's runs produced, concatenated.
#
# Keyed by conversation rather than by run name: a work session's run is named
# after the work's *model-generated* title, which is not something this script
# can predict. Two routes to the same rows, unioned, because which one is
# populated depends on who was still holding the run when it ended — a run's
# events are always in `events`, but the fold into `messages` belongs to
# whichever process was following.
#
# Its own reader rather than `val`, which reads one line: a payload carrying a
# newline would make that half an event and quietly weaken every `grep` below.
transcript_of_conversation() {
  JOD_CID="$1" python3 - <<'PY'
import sqlite3, os
db = os.environ['JOD_HOME'] + '/jod.db'
cid = os.environ['JOD_CID']
con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
runs = {r[0] for r in con.execute(
    "SELECT DISTINCT run_id FROM messages WHERE conversation_id = ? AND run_id IS NOT NULL",
    (cid,))}
runs |= {r[0] for r in con.execute(
    "SELECT r.id FROM runs r JOIN conversations c ON c.session_id = r.session_id "
    "WHERE c.id = ? AND r.session_id IS NOT NULL", (cid,))}
if not runs:
    print(''); raise SystemExit
marks = ','.join('?' * len(runs))
rows = con.execute(
    f"SELECT payload FROM events WHERE run_id IN ({marks}) ORDER BY run_id, seq",
    tuple(runs)).fetchall()
print(' '.join(r[0] or '' for r in rows))
PY
}

# Wait until every run has reached a terminal status, or give up loudly.
settle() {
  echo "\$ (waiting for every run to finish)"
  JOD_SETTLE_SECONDS="${1:-600}" python3 - <<'PY'
import sqlite3, time, os, sys
db = os.environ['JOD_HOME'] + '/jod.db'
limit = int(os.environ['JOD_SETTLE_SECONDS'])
for i in range(0, limit, 5):
    con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
    runs = con.execute('SELECT id, status FROM runs').fetchall()
    con.close()
    if runs and all(r[1] not in ('running', 'starting', 'queued') for r in runs):
        print(f'settled after {i}s'); sys.exit(0)
    time.sleep(5)
print(f'DID NOT SETTLE in {limit}s')
PY
  echo
}

section "1. what this box can actually run"
run jod --version
runsh "cat '$BIN/COMMIT' 2>/dev/null || echo '(binaries staged from the working tree, not a commit)'"
run jod harnesses --json

available() {
  jod harnesses --json | python3 -c "
import json, sys
want = sys.argv[1]
print('yes' if any(h['id'] == want and h['available'] for h in json.load(sys.stdin)) else 'no')
" "$1"
}

# Being installed is not the question. This run has to *call Jod's tools* —
# record a decision, ask a question, request a secret — and the three harnesses
# are handed those tools by three different routes, only two of which exist
# without editing the real machine:
#
#   Claude Code — a per-run `--mcp-config`, written by `mcp_config::config_for`
#                 and passed by the adapter. Always available.
#   OpenCode    — its own config, which honours XDG_CONFIG_HOME. Pointing that
#                 at a scratch directory gives it the tools without touching
#                 the developer's real config.
#   AGY         — `~/.gemini/config/mcp_config.json`, derived from `$HOME`.
#                 Redirecting `$HOME` is the only lever and it would take the
#                 harness's own credentials with it.
#
# Anything that cannot take part is skipped **by name, with the reason**, and
# never silently passed.
section "2. who takes part, and who is skipped by name"
HARNESSES=()
if [ "$(available claude_code)" = yes ]; then
  echo "  claude_code: eligible — takes a per-run MCP config"
  HARNESSES+=(claude)
else
  echo "  SKIPPED: claude_code is not installed on this box"
fi

export XDG_CONFIG_HOME="$JOD_HOME/xdg"
mkdir -p "$XDG_CONFIG_HOME"
if [ "$(available open_code)" = yes ]; then
  echo "  open_code: eligible — registering it in a scratch XDG_CONFIG_HOME"
  run jod mcp install --harness opencode --access delegate
  if [ -n "$(find "$XDG_CONFIG_HOME" -name 'opencode.json*' -print -quit)" ]; then
    HARNESSES+=(opencode)
  else
    echo "  SKIPPED: open_code — the scratch registration did not land, so it"
    echo "           would run with none of Jod's tools"
  fi
else
  echo "  SKIPPED: open_code is not installed on this box"
fi

if [ "$(available agy)" = yes ]; then
  echo "  SKIPPED: agy is installed, but its MCP config lives under \$HOME and"
  echo "           redirecting \$HOME would take its credentials with it. It"
  echo "           cannot be handed Jod's tools without editing the real"
  echo "           machine, which this suite refuses to do."
else
  echo "  SKIPPED: agy is not installed on this box"
fi

if [ "${#HARNESSES[@]}" -eq 0 ]; then
  refuse "no harness on this box can be handed Jod's tools, so there is no
          parity to check"
fi
echo
echo "harnesses under test: ${HARNESSES[*]}"

# ---------------------------------------------------------------------------
# One harness, one run, six things.
# ---------------------------------------------------------------------------
parity_run() {
  local h="$1"
  local work="$JOD_HOME/work/$h"
  local root_a="$work/checkout"
  local root_b="$work/notes"
  # The marker the run has to quote back. Unique per harness, so a transcript
  # cannot pass by echoing another harness's answer.
  local marker="parity-marker-$h-$$"
  # The credential. Long enough that `Scrubber` will redact it — anything under
  # MIN_REDACTABLE_LEN is injected and deliberately left alone.
  local token="jod-parity-token-$h-$$-$(date +%s)"
  local secret_name="PARITY_SECRET"
  local requested_name="PARITY_REQUESTED_TOKEN"

  section "3.$h  $h — preparing two roots and a stored secret"
  rm -rf "$work"; mkdir -p "$root_a" "$root_b"
  printf 'the second root holds this line: %s\n' "$marker" > "$root_b/parity-marker.txt"
  echo "root A (checkout): $root_a"
  echo "root B (notes):    $root_b/parity-marker.txt -> $marker"

  # `printf`, never `echo`: a trailing newline would become part of the
  # credential, and the value must never be an argument — anything on a command
  # line is world-readable through /proc for as long as the process lives.
  runsh "printf %s '$token' | jod secret set $secret_name --global --hint 'the parity suite token'"
  run jod secret ls
  check "[$h] the stored secret's value is not in the database" \
    test -z "$(leaked_in_db "$token")"

  # The run under test is a **work session**, opened from the main chat, and
  # that is not an implementation detail — it is the only path in Jod that hands
  # a run Jod's own tools.
  #
  # `jod run` and `jod team start` both build their request with `tools: None`,
  # and `harness/claude.rs` writes no `--mcp-config` without it: a run started
  # that way has no `record_decision`, no `ask_question` and no
  # `request_secret`, so five of the six things below are not merely untested
  # there, they are impossible. `orchestrator::prepare_work` is the one
  # construction site that sets `tools`, and — since the wiring audit — the only
  # one that sets `roots` and `secrets` too. Driving anything else would be
  # writing a suite that measures the path the feature is not on.
  section "3.$h  $h — opening the work whose first session is the run under test"
  run jod main "Open a work now with your open_work tool. Pass harness='$(harness_id "$h")',
checkout='$root_a', and exactly this instruction:

'You are checking Jod's own plumbing end to end. Use your jod MCP tools. Do all six of
these, in order, then stop. 1. Call list_roots and state how many directories you were
given. 2. Read the file $root_b/parity-marker.txt and quote the single line it contains,
exactly. 3. Call record_decision: decision \"quote the marker verbatim\", because \"the
parity suite compares it byte for byte\", options [\"verbatim\", \"paraphrased\"]. 4. Call
ask_question with blocking set to true, asking exactly: \"Parity check for $h: what word
should I finish with?\" — WAIT for the answer, it is coming from the command line, and
then quote the answer back exactly as you received it. 5. Call request_secret for the
name $requested_name, hint \"the parity suite asks for one so the card flow is
exercised\". 6. Print the value of the environment variable $secret_name on a line of its
own, in the form TOKEN=<value>; if it is unset or empty print TOKEN=<unset> instead, and
do not call any tool for this — read the environment variable directly.'

Do not do any of that work yourself, and do not ask me anything: open the work and stop." \
    --wait -H claude

  # The session is spawned by `open_work` inside the orchestrator's own run, so
  # it exists within seconds of that turn ending — but "within seconds" is not
  # "before the next line", and a poll that gave up immediately would report a
  # missing session that was about to appear.
  local cid=""
  local waited=0
  while [ "$waited" -lt 120 ]; do
    cid="$(val "SELECT id FROM conversations
                 WHERE work_id IS NOT NULL AND harness='$(harness_id "$h")'
                 ORDER BY created_at_ms DESC LIMIT 1")"
    [ -n "$cid" ] && break
    sleep 5
    waited=$((waited + 5))
  done
  echo "work session conversation: ${cid:-<none>} (after ${waited}s)"
  if [ -z "$cid" ]; then
    echo "FAIL  [$h] the orchestrator opened no work for $h, so there is no run to check"
    FAILED=$((FAILED + 1))
    return 0
  fi

  # `prepare_work` adds the checkout as the session's first root. The second is
  # added here, which is also the CLI half of E1: a root a person attached to a
  # session that is already running.
  runsh "'$BIN/jod' root add '$root_b' -c '$cid'"
  run jod root ls -c "$cid"
  check "[$h] both roots are on the session" \
    test "$(val "SELECT count(*) FROM conversation_roots WHERE conversation_id='$cid'")" = 2

  # The blocking question, answered from the command line, which is the half of
  # D2 no unit test can reach: `ask_question` polls the card row from inside the
  # harness's tool call, so this is a real person's answer arriving mid-turn.
  section "3.$h  $h — answering the blocking question from the CLI"
  local card=""
  local waited=0
  while [ "$waited" -lt 300 ]; do
    card="$(val "SELECT id FROM cards WHERE conversation_id='$cid' AND kind='question' AND status='open' ORDER BY id DESC LIMIT 1")"
    [ -n "$card" ] && break
    sleep 5
    waited=$((waited + 5))
  done
  if [ -z "$card" ]; then
    echo "FAIL  [$h] no question card appeared in ${waited}s — the run never asked"
    FAILED=$((FAILED + 1))
  else
    echo "question card $card appeared after ${waited}s"
    runsh "'$BIN/jod' card show '$card'"
    runsh "'$BIN/jod' card answer '$card' PINEAPPLE"
  fi

  settle 900

  section "3.$h  $h — what the run left behind"
  runsh "q \"SELECT id, kind, status, blocking, substr(title,1,60) AS title, secret_name, answer
               FROM cards WHERE conversation_id='$cid' ORDER BY id\""
  runsh "q \"SELECT r.name, r.status FROM runs r ORDER BY r.created_at_ms\""
  echo "-- what the agent actually said --"
  runsh "q \"SELECT substr(text,1,600) FROM messages
              WHERE conversation_id='$cid' AND role='assistant' ORDER BY id DESC LIMIT 3\""

  local transcript
  transcript="$(transcript_of_conversation "$cid")"

  # --- the six things, each asserted on its own ---------------------------
  check "[$h] the run recorded a decision" \
    test "$(val "SELECT count(*) FROM cards WHERE conversation_id='$cid' AND kind='decision'")" -ge 1
  check "[$h] the run asked a blocking question" \
    test "$(val "SELECT count(*) FROM cards WHERE conversation_id='$cid' AND kind='question' AND blocking=1")" -ge 1
  check "[$h] the CLI's answer is stored on the card" \
    test "$(val "SELECT answer FROM cards WHERE conversation_id='$cid' AND kind='question' ORDER BY id DESC LIMIT 1")" = PINEAPPLE
  check "[$h] the agent received the answer mid-turn and quoted it" \
    grep -q PINEAPPLE <<<"$transcript"
  check "[$h] the run requested a secret, by name and with no value" \
    test "$(val "SELECT count(*) FROM cards WHERE conversation_id='$cid' AND kind='secret' AND secret_name='$requested_name'")" -ge 1
  check "[$h] the file in the second root reached the agent" \
    grep -q "$marker" <<<"$transcript"

  # --- the check SPECS.md names -------------------------------------------
  #
  # Two halves, and both have to hold. A transcript with no marker in it
  # because nothing was ever injected would satisfy "no value in the database"
  # while proving nothing at all, so the injection half is asserted first and
  # the leak count is only meaningful behind it.
  section "3.$h  $h — injection and redaction"
  echo "-- what the run printed for TOKEN= --"
  runsh "q \"SELECT substr(text,1,200) FROM messages
              WHERE conversation_id='$cid' AND text LIKE '%TOKEN=%' ORDER BY id\""
  check "[$h] the secret was injected into the run" \
    test "$(grep -c 'TOKEN=<unset>' <<<"$transcript")" -eq 0
  check "[$h] printing the secret produced the redaction marker" \
    grep -q '\[redacted\]' <<<"$transcript"
  check "[$h] the secret's value appears nowhere in the database" \
    test -z "$(leaked_in_db "$token")"

  LEAK_TOKENS+=("$token")
}

LEAK_TOKENS=()
for h in "${HARNESSES[@]}"; do
  parity_run "$h"
done

section "4. zero leaked secrets, counted across the whole store"
LEAKS=0
for token in ${LEAK_TOKENS[@]+"${LEAK_TOKENS[@]}"}; do
  where="$(leaked_in_db "$token")"
  if [ -n "$where" ]; then
    echo "LEAKED: a token reached $where"
    LEAKS=$((LEAKS + 1))
  fi
done
echo "leaked secrets: $LEAKS"
check "zero leaked secrets" test "$LEAKS" -eq 0

section "5. the machine is as it was found"
AFTER="$(fingerprint)"
if [ "$BEFORE" = "$AFTER" ]; then
  echo "PASS  no harness config was touched"
  PASSED=$((PASSED + 1))
else
  echo "FAIL  a harness config changed under this suite:"
  diff <(echo "$BEFORE") <(echo "$AFTER") || true
  FAILED=$((FAILED + 1))
fi
check "the real ~/.jod/secrets holds nothing this suite stored" \
  test -z "$(ls -A "$HOME/.jod/secrets" 2>/dev/null | grep PARITY || true)"

section "summary"
echo "harnesses under test: ${HARNESSES[*]}"
echo "passed: $PASSED"
echo "failed: $FAILED"
[ "$FAILED" -eq 0 ] || exit 1
