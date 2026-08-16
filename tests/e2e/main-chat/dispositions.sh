#!/usr/bin/env bash
# Does the main chat pick the right branch for the size of the task?
#
# The orchestrator's preamble now opens with a choice rather than with a single
# instruction to hand everything over. A quick question it can answer in one
# turn, it answers. Something that outlasts the turn goes to an agent. Something
# that is really a project gets a work. This suite is the check on that: a table
# of instructions, each with the disposition it should produce, driven through a
# real `jod main --wait` and graded on the tool the orchestrator actually called.
#
# It needs a live model, so it is not part of the plain suite. Like
# `harness_parity.sh` it is named so the CI discovery in `.github/workflows/
# tests.yml` — which finds `*.test.sh` and `*/tests/test.sh` — does not pick it
# up. Run it by hand:
#
#   tests/e2e/jod/build.sh
#   tests/e2e/main-chat/dispositions.sh
#
# Slow on purpose. Every row is a real model turn, and the two-turn row is two.
# Each turn is capped at 420 seconds; a row that hits the cap is still graded,
# because the grade comes from the store rather than from the process exiting.
#
# It asserts in both directions, and that is the reason it is worth running at
# all. Three rows must be answered directly and check that nothing was handed
# over. Five rows must be handed over and check the exact tool, which an empty
# value does not satisfy. So a preamble that had collapsed to "always answer"
# fails five rows, and one that had collapsed to "always delegate" fails three.
# Neither direction can pass on its own.
#
# It is not deterministic, and pretending otherwise would be worse than saying
# so. Every row is a live model turn, so a row can pass or fail on how the model
# reads a sentence that minute. The bug it guards is itself a coin flip — the
# same question answered directly one time and delegated the next, depending on
# whether the words "in this project" appeared — and a check cannot be more
# solid than the thing it measures. What makes it useful anyway is the paragraph
# above: it cannot report green on an orchestrator that has simply stopped
# choosing.
#
# Nothing here is faked. There is no stub harness, no seeded row and no
# pre-written answer: each instruction goes to the same code path `jod main`
# uses, and the grade is read back out of the store afterwards.
set -uo pipefail
AREA=dispositions
. "$(dirname "$0")/../jod/env.sh"

# `jod` writes the MCP registration for the real harnesses at daemon start-up,
# pointed at whatever JOD_HOME is current. Without this a run of this suite
# leaves the developer's own Claude Code talking to a scratch database under
# /tmp. Asserted rather than merely exported, because an edit that drops the
# export would fail silently and the thing it fails at is somebody's machine.
export JOD_NO_MCP_INSTALL=1

refuse() {
  echo
  echo "REFUSING TO RUN: $*"
  exit 2
}

[ "${JOD_NO_MCP_INSTALL:-}" = "1" ] ||
  refuse "JOD_NO_MCP_INSTALL is not set."
case "$JOD_HOME" in
  "$HOME"/.jod | "$HOME"/.jod/*)
    refuse "JOD_HOME is $JOD_HOME, which is the real one. Every row here wipes
            its home before it runs." ;;
  /tmp/*) : ;;
  *) refuse "JOD_HOME is $JOD_HOME, which is neither /tmp nor recognisably
             scratch. Set JOD_E2E_HOME." ;;
esac
command -v jod >/dev/null ||
  refuse "no staged \`jod\` on PATH. Run tests/e2e/jod/build.sh first."

PASSED=0
FAILED=0
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

# The six routing tools. A turn that called one of these handed the instruction
# over; a turn that called none of them dealt with it itself.
#
# Read tools are deliberately not in this list. The preamble tells the
# orchestrator to call `list_agents` first almost always, and `recall` before
# asking Reljod something he has already said, so a rule of "answered means
# called nothing at all" would fail every row including the ones that are right.
# What distinguishes answering is that nothing was handed over.
ROUTING_TOOLS="continue_agent open_work delegate schedule_create goal_create"

# Every routing tool called by messages newer than $2, newest last.
routed_since() {
  JOD_DB="$1" JOD_AFTER="$2" python3 - <<'PY'
import os, sqlite3
db = os.environ['JOD_DB']
con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
rows = con.execute(
    "SELECT tool_name FROM messages "
    "WHERE role = 'tool_call' AND tool_name IS NOT NULL AND id > ? "
    "ORDER BY id",
    (int(os.environ['JOD_AFTER']),),
).fetchall()
routing = set(os.environ['JOD_ROUTING'].split())
for (name,) in rows:
    # Tool names arrive fully qualified (`mcp__jod__delegate`) from some
    # harnesses and bare from others. Grade on the last segment either way.
    short = name.rsplit('__', 1)[-1]
    if short in routing:
        print(short)
PY
}

# Everything the turn called, routing or not, for the transcript.
called_since() {
  JOD_DB="$1" JOD_AFTER="$2" python3 - <<'PY'
import os, sqlite3
db = os.environ['JOD_DB']
con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
rows = con.execute(
    "SELECT tool_name FROM messages "
    "WHERE role = 'tool_call' AND tool_name IS NOT NULL AND id > ? ORDER BY id",
    (int(os.environ['JOD_AFTER']),),
).fetchall()
print(', '.join(r[0].rsplit('__', 1)[-1] for r in rows) or '(none)')
PY
}

# What the chat said back, which is the whole product of an `answer` row.
said_since() {
  JOD_DB="$1" JOD_AFTER="$2" python3 - <<'PY'
import os, sqlite3
db = os.environ['JOD_DB']
con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
rows = con.execute(
    "SELECT text FROM messages "
    "WHERE role = 'assistant' AND id > ? ORDER BY id",
    (int(os.environ['JOD_AFTER']),),
).fetchall()
print(' '.join(r[0].strip() for r in rows if r[0] and r[0].strip()))
PY
}

high_water() {
  JOD_DB="$1" python3 - <<'PY'
import os, sqlite3
db = os.environ['JOD_DB']
con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
row = con.execute("SELECT COALESCE(MAX(id), 0) FROM messages").fetchone()
print(row[0])
PY
}
export JOD_ROUTING="$ROUTING_TOOLS"

# A scratch repository for the chat to sit in.
#
# Not this checkout. Two reasons, and the second is the one that matters. It
# matches the conditions the A2A failure was observed under — a console in a
# scratch repository on a fresh store — and it means a row that *is* supposed
# to delegate spawns its child somewhere harmless instead of into a working
# tree several agents share.
# Which permission mode the turns run under, stated rather than inherited.
#
# `jod main` defaults to `edits`, and below `bypass` Jod writes its own
# `PreToolUse` approval hook into the run's settings — `jod approve-hook --run
# <id> --wait 60` on matcher `*`. Every tool call then waits a full minute for an
# approval from a person who is not here before the tool runs, so a turn making
# seven calls cannot finish inside the cap however well it routed. Measured, not
# guessed: hooked runs on this box show a median call-to-result gap of 60.394s,
# unhooked ones 0.033s.
#
# The default here stays `edits` so the suite exercises the mode a scripted
# caller gets. Reljod's console runs at `bypass`, where the hook is absent and
# the same table finishes in a fraction of the time. Set DISPOSITION_PERMISSION
# to compare them.
MODE="${DISPOSITION_PERMISSION:-edits}"
echo "permission mode: $MODE"

SCRATCH="$JOD_HOME/scratch-repo"
rm -rf "$SCRATCH"; mkdir -p "$SCRATCH"
git -C "$SCRATCH" init -q
printf '# scratch\n\nA throwaway repository for the routing fixtures.\n' > "$SCRATCH/README.md"
git -C "$SCRATCH" add -A >/dev/null 2>&1
git -C "$SCRATCH" -c user.email=e2e@jod -c user.name=e2e commit -qm "init" >/dev/null 2>&1
echo "scratch repository: $SCRATCH"

# Stop everything a row started, so the next row begins with nothing running and
# no child of this suite outlives it.
#
# Empty input is a legitimate state and is handled by name: a home whose store
# holds no runs prints nothing. Anything else that is not JSON is a real fault
# and is left to raise, rather than being caught into an empty list — a reaper
# that silently decides there is nothing to stop would leave this suite's
# children running and say it had cleaned up.
reap() {
  JOD_HOME="$1" jod ls --json 2>/dev/null | python3 -c '
import json, sys
raw = sys.stdin.read().strip()
runs = json.loads(raw) if raw else []
for r in runs:
    if r.get("status") in ("running", "starting", "queued"):
        print(r["id"])
' | while read -r rid; do
    JOD_HOME="$1" jod kill "$rid" >/dev/null 2>&1
  done
}

# `fixture <expected> <instruction...>`
#
# One row: a fresh home, one turn, and the grade. The home is fresh per row on
# purpose — the routing decision is supposed to be made from the instruction,
# and a shared chat would let the previous row's transcript explain a result.
fixture() {
  local expected="$1"; shift
  local instruction="$*"
  local home="$JOD_HOME/row-$(echo "$expected-$instruction" | tr -c 'a-zA-Z0-9' '-' | cut -c1-60)"

  rm -rf "$home"; mkdir -p "$home"
  local db="$home/jod.db"

  echo
  echo "--------------------------------------------------------------"
  echo "expected: $expected"
  echo "asked   : $instruction"
  JOD_HOME="$home" timeout "${DISPOSITION_TURN_TIMEOUT:-420}" \
    jod main --wait --permission "$MODE" --cwd "$SCRATCH" "$instruction" \
    >"$home/turn.log" 2>&1
  local rc=$?
  [ "$rc" -eq 0 ] || echo "  (jod main exited $rc — see $home/turn.log)"

  local called routed said
  called="$(called_since "$db" 0)"
  routed="$(routed_since "$db" 0 | tail -1)"
  said="$(said_since "$db" 0)"
  echo "tools   : $called"
  echo "routed  : ${routed:-(nothing handed over)}"
  echo "said    : $(echo "$said" | cut -c1-160)"

  if [ "$expected" = "answer" ]; then
    check "\"$instruction\" is answered rather than handed over" \
      [ -z "$routed" ]
    check "\"$instruction\" comes back with an actual answer" \
      [ -n "$said" ]
  else
    check "\"$instruction\" is routed to $expected" \
      [ "$routed" = "$expected" ]
  fi
  reap "$home"
}

section "the fixture table"

# The row this suite exists for, and it is not invented. A console on a fresh
# store was asked exactly this. It spawned a child called `a2a-acronym-lookup`,
# polled `list_agents` waiting for it, and after 42 seconds and 39 cents replied
# "Still working — the lookup agent is mid-search." Reljod never got the answer.
# The words "in this project" are what tipped it: they read as an errand into a
# checkout when they are context on a definition the chat already knew.
fixture answer "what does the acronym A2A stand for in this project? answer in one line"

# The same branch without the project in the phrasing.
fixture answer "What does A2A stand for?"
fixture answer "What time is it in Manila right now?"

# Needs a tool the chat does not have and no repository — a one-shot, which is
# what `delegate` is for.
fixture delegate "Search the web for the current version of the Rust compiler and tell me the number."

# Touches a repository and will outlast one session.
fixture open_work "In this repository, write a CONTRIBUTING.md explaining how to build and test it, wire it into the README, and add a test that keeps the two in step."

# Says when.
fixture schedule_create "Every weekday at 8am Asia/Manila, sweep the open PRs and tell me what needs me."

# Says keep.
fixture goal_create "Keep working until the README explains what jod main does, then stop."

section "the two-turn row: a follow-up belongs to the agent already holding it"

# `continue_agent` cannot be reached from a cold chat, because there is nothing
# to continue. This row is two turns in one home by design: the first hands
# something over, the second carries it on, and only the second is graded.
CONT="$JOD_HOME/continue"
rm -rf "$CONT"; mkdir -p "$CONT"
CONT_DB="$CONT/jod.db"

echo "asked (1): Count how many files are tracked in this repository and tell me."
JOD_HOME="$CONT" timeout "${DISPOSITION_TURN_TIMEOUT:-420}" \
  jod main --wait --permission "$MODE" --cwd "$SCRATCH" \
  "Count how many files are tracked in this repository and tell me." \
  >"$CONT/turn1.log" 2>&1
MARK="$(high_water "$CONT_DB")"
echo "first turn routed to: $(routed_since "$CONT_DB" 0 | tail -1)"

echo "asked (2): Follow up on that file count — also break it down by extension."
JOD_HOME="$CONT" timeout "${DISPOSITION_TURN_TIMEOUT:-420}" \
  jod main --wait --permission "$MODE" --cwd "$SCRATCH" \
  "Follow up on that file count — also break it down by extension." \
  >"$CONT/turn2.log" 2>&1
SECOND="$(routed_since "$CONT_DB" "$MARK" | tail -1)"
echo "second turn tools : $(called_since "$CONT_DB" "$MARK")"
echo "second turn routed: ${SECOND:-(nothing handed over)}"
check "a follow-up goes to the agent already holding the context" \
  [ "$SECOND" = "continue_agent" ]

section "result"
echo "$PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
