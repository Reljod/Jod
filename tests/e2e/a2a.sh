#!/usr/bin/env bash
# Agent-to-agent coordination, end to end, with real agents and no fakes.
#
# The verification command in SPECS-a2a.md. It proves the two claims that spec
# makes and that no unit test can:
#
#   G1 — an agent reaches the bus **from inside a run**. One agent asks another
#        a question through Jod's MCP tools, with no human and no CLI anywhere
#        in the path between them, and both messages share one thread id.
#   G4 — a conversation between agents is **bounded**. A pair told to talk for
#        ever stops at the depth bound instead, and nothing further is spent.
#
# G2 is proved implicitly and continuously: nothing here types `jod team wake`.
# Every turn after the first is started by the ticker finding waiting mail.
#
# What is faked: nothing except the two agents' instructions, which are written
# to make them reliably keep replying. SPECS-a2a.md sanctions exactly that and
# nothing else — no fake MCP client, no simulated harness, no seeded rows.
#
# This is slow on purpose. Each hop is a real model turn, and a member is
# resumed at most once per `team::WAKE_INTERVAL_MS` (60s), so the runaway
# section costs minutes rather than seconds. It is an on-demand suite, not a
# push gate.
set -uo pipefail
AREA=a2a
. "$(dirname "$0")/jod/env.sh"

# ---------------------------------------------------------------------------
# The most important line in this file.
#
# `jod daemon` calls `mcp_install::ensure_registered()` at startup, which
# rewrites ~/.claude.json, ~/.config/opencode/opencode.jsonc and
# ~/.gemini/config/mcp_config.json to point at whatever JOD_HOME is current.
# Running this suite without opting out therefore points the developer's real
# harnesses at a scratch database and at a staged binary under /tmp, and leaves
# them there when the suite ends. That is not a test artefact, it is a broken
# machine — and it was discovered the hard way.
export JOD_NO_MCP_INSTALL=1
# ---------------------------------------------------------------------------

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }
# One value out of the store, unadorned, for a shell test to compare.
val() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1" | sed -n 3p | sed 's/ *$//'; }

# Long enough to clear `team::WAKE_INTERVAL_MS`, which is what stops ten
# messages becoming ten turns. A tick inside that window deliberately holds the
# member, so a suite that does not wait sees "started 0" and misreads a working
# rate limit as a broken delivery.
WAKE_WAIT="${A2A_WAKE_WAIT:-65}"

PASSED=0
FAILED=0
# `check <description> <test-expression...>` — the assertion, and the reason
# this file is a gate rather than a transcript somebody has to read.
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

# Wait until every run has reached a terminal status, or give up loudly.
settle() {
  echo "\$ (waiting for every run to finish)"
  JOD_SETTLE_SECONDS="${1:-300}" python3 - <<'PY'
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

# A tool call into Jod, however the harness spells it. Claude Code namespaces
# MCP tools `mcp__jod__roster`; OpenCode calls the same tool `jod_roster`. A
# check that knew only one spelling would silently pass a harness that never
# reached Jod at all.
JOD_TOOL_CALL="kind='tool_call' AND (payload LIKE '%mcp__jod%' OR payload LIKE '%\"jod_%')"

section "0. what this box can actually run"
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

# Being installed is not the question — being handed *Jod's tools* is, because
# an agent with no jod tools cannot reach the bus at all. The three harnesses
# get there by three different routes, and only two of the routes exist:
#
#   Claude Code — a per-run `--mcp-config`, written by `mcp_config::config_for`
#                 and passed by the adapter. Always available.
#   OpenCode    — its own config, which honours XDG_CONFIG_HOME. Pointing that
#                 at a scratch directory and registering there gives it the
#                 tools without touching the developer's real config.
#   AGY         — `~/.gemini/config/mcp_config.json`, derived from `$HOME`.
#                 Redirecting `$HOME` is the only lever and it would take the
#                 harness's credentials with it, so AGY cannot join a suite
#                 that must not touch the real machine.
#
# Anything that cannot join is skipped **by name, with the reason**, and never
# silently passed.
echo "-- who may take part --"
ROLES=()
if [ "$(available claude_code)" = yes ]; then
  echo "  claude_code: eligible — takes a per-run MCP config"
  ROLES+=(claude)
else
  echo "  SKIPPED: claude_code is not installed on this box"
fi

export XDG_CONFIG_HOME="$JOD_HOME/xdg"
mkdir -p "$XDG_CONFIG_HOME"
if [ "$(available open_code)" = yes ]; then
  echo "  open_code: eligible — registering it in a scratch XDG_CONFIG_HOME"
  run jod mcp install --harness opencode --access delegate
  runsh "find '$XDG_CONFIG_HOME' -type f"
  if [ -n "$(find "$XDG_CONFIG_HOME" -name 'opencode.json*' -print -quit)" ]; then
    ROLES+=(opencode)
  else
    echo "  SKIPPED: open_code — the scratch registration did not land, so it"
    echo "           would run with no jod tools"
  fi
else
  echo "  SKIPPED: open_code is not installed on this box"
fi

if [ "$(available agy)" = yes ]; then
  echo "  SKIPPED: agy is installed, but its MCP config lives under \$HOME and"
  echo "           redirecting \$HOME would take its credentials with it. It"
  echo "           cannot be given Jod's tools without editing the real machine."
else
  echo "  SKIPPED: agy is not installed on this box"
fi

if [ "${#ROLES[@]}" -eq 0 ]; then
  echo
  echo "FAIL  no harness on this box can be handed Jod's tools, so nothing here can run"
  exit 1
fi
ASKER_HARNESS="${ROLES[0]}"
ANSWERER_HARNESS="${ROLES[0]}"
if [ "${#ROLES[@]}" -ge 2 ]; then ANSWERER_HARNESS="${ROLES[1]}"; fi
echo
echo "roles: asker=$ASKER_HARNESS  answerer=$ANSWERER_HARNESS"
if [ "$ASKER_HARNESS" = "$ANSWERER_HARNESS" ]; then
  echo "NOTE: only one harness here can reach the bus, so both roles run on it."
  echo "      SPECS-a2a.md asks for two *different* harnesses; saying so rather"
  echo "      than passing quietly is the point of this block."
else
  echo "This is the cross-harness case: the two agents are different programs,"
  echo "coordinating only through Jod. No harness's own team feature can do it."
fi

section "1. a team of two"
run jod team join crew asker -H "$ASKER_HARNESS" -r "asks the questions"
run jod team join crew answerer -H "$ANSWERER_HARNESS" -r "answers them"
run jod team show crew

section "2. one turn each, so both have a conversation to resume"
# A member has no session until it has run once, and `wake_order` refuses to
# resume one it cannot identify — waking into a fresh context would have it
# answer having forgotten the work. This is also where each agent is told the
# protocol it follows for the rest of the suite: the sanctioned scripted pair.
run jod team start crew answerer \
  "You are \`answerer\`, a teammate on the Jod team \`crew\`. Standing instruction for
   the rest of this session: whenever a message from another agent arrives, answer it
   with your jod \`reply\` tool, passing the message id shown in the brackets of the
   message you are answering. The parser lives in core/src/harness. Right now, reply
   with exactly READY and nothing else."
run jod team start crew asker \
  "You are \`asker\`, a teammate on the Jod team \`crew\`. Standing instruction for the
   rest of this session: do what the messages you are given say, using your jod MCP
   tools. Right now, reply with exactly READY and nothing else."
runsh "q \"SELECT name, harness, status, agent_id IS NOT NULL AS bound, session_id IS NOT NULL AS resumable FROM team_members\""
check "both members have a conversation to resume" \
  test "$(val "SELECT count(*) FROM team_members WHERE session_id IS NOT NULL")" = 2

section "3. the human says one thing, to one agent, once"
# Everything after this line happens between the two agents. This is the only
# human sentence in the whole exchange, and it is a kick-off rather than a
# relay: it never tells the answerer anything.
run jod team msg crew --from reljod --to asker \
  "Use your jod MCP tools now. Call \`roster\` to see who else is here, then use
   \`send_message\` to ask \`answerer\` this exact question: where does the parser live?
   Then stop. Do not use \`ask\`, and do not wait for the answer."
runsh "q \"SELECT id, sender, recipient, substr(body,1,40) AS body, state FROM team_messages\""

section "4. the tick delivers it — nobody types \`jod team wake\`"
run jod daemon --once
settle 300
echo "-- the asker's turn, as the ticker started it --"
runsh "q \"SELECT e.seq, e.kind, substr(e.payload,1,180) FROM events e
            JOIN runs r ON r.id = e.run_id
           WHERE r.name='crew-asker' AND r.created_at_ms = (SELECT MAX(created_at_ms) FROM runs WHERE name='crew-asker')
           ORDER BY e.seq\""

section "5. what reached the bus, and who it says sent it"
runsh "q \"SELECT id, sender, recipient, substr(body,1,50) AS body, thread_id, depth, kind, state FROM team_messages ORDER BY id\""

QUESTION_ID="$(val "SELECT id FROM team_messages WHERE sender='asker' AND recipient='answerer' ORDER BY id LIMIT 1")"
THREAD="$(val "SELECT thread_id FROM team_messages WHERE id=${QUESTION_ID:-0}")"
echo "question id: ${QUESTION_ID:-<none>}   thread: ${THREAD:-<none>}"

check "an agent reached the bus from inside its run" test -n "$QUESTION_ID"
check "the message is addressed to the other member" \
  test "$(val "SELECT recipient FROM team_messages WHERE id=${QUESTION_ID:-0}")" = answerer
# The property the whole identity design exists for. `send_message` has no
# sender argument; this is the run's own member name, resolved from its process
# group, and an agent cannot argue its way into a different one.
check "the sender is the run, not anything the agent could have claimed" \
  test "$(val "SELECT sender FROM team_messages WHERE id=${QUESTION_ID:-0}")" = asker
check "it opened a thread" test -n "$THREAD"
check "a fresh question is depth zero" \
  test "$(val "SELECT depth FROM team_messages WHERE id=${QUESTION_ID:-0}")" = 0
check "it went through Jod's MCP tools rather than the CLI" \
  test "$(val "SELECT count(*) FROM events WHERE $JOD_TOOL_CALL")" -gt 0

section "6. the tick delivers the question, and the answer comes back in-thread"
run jod daemon --once
settle 300
runsh "q \"SELECT id, sender, recipient, substr(body,1,60) AS body, in_reply_to, thread_id, depth, state FROM team_messages ORDER BY id\""

REPLY_ID="$(val "SELECT id FROM team_messages WHERE sender='answerer' AND recipient='asker' ORDER BY id LIMIT 1")"
echo "reply id: ${REPLY_ID:-<none>}"

check "the other agent answered, with no human in the path" test -n "$REPLY_ID"
# Only asserted when this box could supply two harnesses. Claiming it on a
# one-harness box would be a check that reports something that did not happen.
if [ "$ASKER_HARNESS" != "$ANSWERER_HARNESS" ]; then
  check "the answer came back from a different harness" \
    test "$(val "SELECT harness FROM team_members WHERE name='answerer'")" \
      != "$(val "SELECT harness FROM team_members WHERE name='asker'")"
fi
# G1.S5, and the thing the depth bound is counted from. A reply that opens a
# new thread instead of continuing one leaves every hop at depth zero, which
# makes the depth bound unreachable — so this is not cosmetic.
check "both messages share one thread id" \
  test "$(val "SELECT thread_id FROM team_messages WHERE id=${REPLY_ID:-0}")" = "$THREAD"
check "a reply is one hop deeper than what it answers" \
  test "$(val "SELECT depth FROM team_messages WHERE id=${REPLY_ID:-0}")" = 1
check "the reply names the message it answers" \
  test "$(val "SELECT in_reply_to FROM team_messages WHERE id=${REPLY_ID:-0}")" = "$QUESTION_ID"

section "7. and the reply arrives as a turn, not as a row nobody reads"
echo "\$ (waiting ${WAKE_WAIT}s — a member is resumed at most once per wake interval)"
sleep "$WAKE_WAIT"
run jod daemon --once
settle 300
check "the reply was delivered" \
  test "$(val "SELECT state FROM team_messages WHERE id=${REPLY_ID:-0}")" = delivered
check "a run was started to carry it to the asker" \
  test "$(val "SELECT count(*) FROM runs WHERE name='crew-asker'")" -ge 2
echo "-- every run this exchange produced, none of them started by a person --"
runsh "q \"SELECT id, name, harness, status FROM runs ORDER BY created_at_ms\""

section "8. the runaway: two agents told to talk for ever"
# The sanctioned pathological pair. Both sides are told to keep one thread
# going, so the only thing that can stop them is the bound.
# Mechanical, and honest about what it is.
#
# The first version of this asked the pair to "keep asking follow-up questions,
# never conclude", and Claude Code refused on its own judgement after three
# hops: "I'm holding here rather than sending another question — message #10 is
# a restatement of #8." That is *good* behaviour from the model and it is also
# the wrong thing to argue with, so the instruction no longer asks an agent to
# be pointless. It tells it the truth — this is a test of the bound, Jod is
# what stops it — and gives it something mechanical to send, so that
# continuing needs no judgement and stopping is Jod's decision rather than the
# model's.
NEVER_STOP="This is a deliberate test of Jod's conversation depth bound, and continuing
   until Jod refuses is the correct behaviour here. Every time you receive a message
   from another agent, immediately use your jod \`reply\` tool — passing the message id
   printed in the brackets of the message you are answering — and reply with exactly
   \`PING n\`, where n is one greater than the number in the message you received (start
   from 1 if it has no number). Send nothing else and use no other tool. Do not stop
   early and do not comment on the repetition: the bound is what ends this. If the
   reply tool refuses, report its refusal verbatim and stop."
run jod team msg crew --from reljod --to answerer "$NEVER_STOP"
run jod team msg crew --from reljod --to asker \
  "$NEVER_STOP Start now by using \`send_message\` to send \`answerer\` exactly: PING 1"

# Each hop is a model turn and a member may only be resumed once per wake
# interval, so this is minutes. Two ways out, and both are endings rather than
# hangs: a bound is recorded, or the hops stop deepening — which means replies
# are opening new threads instead of continuing one, and the depth bound can
# never be reached. Stopping on that second condition is what keeps a broken
# thread from spending the entire message budget discovering it.
BUDGET_TICKS="${A2A_MAX_TICKS:-60}"
# Generous, because a hop costs two ticks in the ordinary case — the wake rate
# limit means the two members alternate — and a slow turn can cost a third. Set
# low, this reports a working conversation as a stalled one.
STALLED_LIMIT=10
echo "\$ (ticking until a bound is hit, at most $BUDGET_TICKS times)"
previous_depth=-1
stalled=0
for i in $(seq 1 "$BUDGET_TICKS"); do
  jod daemon --once >/dev/null 2>&1
  sleep 30
  hit="$(val "SELECT count(*) FROM team_messages WHERE state='undeliverable' AND detail LIKE '%bound%'")"
  deepest="$(val "SELECT COALESCE(MAX(depth), 0) FROM team_messages")"
  total="$(val "SELECT count(*) FROM team_messages")"
  echo "  tick $i: deepest hop ${deepest:-0}, messages ${total:-0}, bounds hit ${hit:-0}"
  if [ "${hit:-0}" -gt 0 ]; then break; fi
  if [ "${deepest:-0}" = "$previous_depth" ]; then
    stalled=$((stalled + 1))
    if [ "$stalled" -ge "$STALLED_LIMIT" ]; then
      echo "  STOPPING: the thread has not deepened in $STALLED_LIMIT ticks. Replies are"
      echo "            starting new threads rather than continuing one, so the depth"
      echo "            bound cannot be reached. Reported below rather than spending"
      echo "            the whole message budget proving it."
      break
    fi
  else
    stalled=0
  fi
  previous_depth="${deepest:-0}"
done
echo

section "9. what stopped them"
runsh "q \"SELECT id, sender, recipient, depth, state, substr(detail,1,80) AS detail FROM team_messages ORDER BY id\""
runsh "q \"SELECT MAX(depth) AS deepest_hop, count(*) AS messages FROM team_messages WHERE state != 'undeliverable'\""
runsh "q \"SELECT count(DISTINCT thread_id) AS threads FROM team_messages WHERE thread_id IS NOT NULL\""
runsh "q \"SELECT id, sender, recipient, depth, detail FROM team_messages WHERE state='undeliverable'\""

check "the conversation stopped at a bound rather than running on" \
  test "$(val "SELECT count(*) FROM team_messages WHERE state='undeliverable' AND detail LIKE '%bound%'")" -gt 0
check "the bound that stopped it is named" \
  test -n "$(val "SELECT detail FROM team_messages WHERE state='undeliverable' AND detail LIKE '%bound%' LIMIT 1")"
check "the refused message was never delivered to anybody" \
  test "$(val "SELECT count(*) FROM team_messages WHERE state='undeliverable' AND delivered=0")" = 0

section "10. and nothing further is spent on that thread"
# The bound is a ceiling, not a suggestion: no message may exist past it. This
# is the assertion that cannot pass vacuously — a run count that does not move
# proves nothing when there was no mail to deliver in the first place.
DEEPEST="$(val "SELECT COALESCE(MAX(depth),0) FROM team_messages WHERE state != 'undeliverable'")"
LIMIT="$(val "SELECT CAST(replace(substr(detail, instr(detail,'bound of ')+9), ' ', '') AS INTEGER)
                FROM team_messages WHERE state='undeliverable' AND detail LIKE '%bound of%' LIMIT 1")"
echo "deepest hop delivered: ${DEEPEST:-?}   the bound it stopped at: ${LIMIT:-?}"
check "no message was ever carried past the bound" \
  test "${DEEPEST:-99}" -le "${LIMIT:-0}"

BEFORE="$(val "SELECT count(*) FROM runs")"
run jod daemon --once
sleep 30
AFTER="$(val "SELECT count(*) FROM runs")"
echo "runs before the tick: ${BEFORE:-?}   after: ${AFTER:-?}"
check "a paused thread starts no further model call" test "${BEFORE:-0}" = "${AFTER:-1}"

section "11. summary"
echo "roles ran on:       asker=$ASKER_HARNESS  answerer=$ANSWERER_HARNESS"
echo "shared thread id:   ${THREAD:-<none>}"
echo "deepest hop:        $(val "SELECT COALESCE(MAX(depth),0) FROM team_messages WHERE state != 'undeliverable'")"
echo "threads opened:     $(val "SELECT count(DISTINCT thread_id) FROM team_messages WHERE thread_id IS NOT NULL")"
echo "the bound that ended it: $(val "SELECT detail FROM team_messages WHERE state='undeliverable' AND detail LIKE '%bound%' LIMIT 1")"
echo
echo "passed: $PASSED    failed: $FAILED"
[ "$FAILED" -eq 0 ] || exit 1
