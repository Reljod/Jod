#!/usr/bin/env bash
#
# Scenario suite for .claude/hooks/task-completed-tests.sh.
#
# The hook is the deterministic half of "blocked is a legal ending", so its
# own behavior can't rest on an agent's word either. Every case runs the hook
# against a throwaway CLAUDE_PROJECT_DIR holding fixture suites — never this
# repo — so the hook can't recurse into this file.
#
# Run: .claude/hooks/tests/task-completed.test.sh
set -uo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
HOOK="$TEST_DIR/../task-completed-tests.sh"

# shellcheck source=../../../.agents/skills/test-scenarios/scripts/assert.sh
source "$TEST_DIR/../../../.agents/skills/test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# fresh <name> — an isolated fake project root.
fresh() { local d="$WORK/$1"; mkdir -p "$d/suites"; printf '%s' "$d"; }

pass_suite() { printf '#!/usr/bin/env bash\nexit 0\n' > "$1"; }
fail_suite() { printf '#!/usr/bin/env bash\necho "boom: 1 assertion failed"\nexit 1\n' > "$1"; }

OUT="$WORK/out"; ERR="$WORK/err"
run_hook() {
  printf '%s' "${2:-\{\}}" | CLAUDE_PROJECT_DIR="$1" bash "$HOOK" >"$OUT" 2>"$ERR"
}

# assert_hook <expected-exit> <root> <name> [payload]
assert_hook() {
  local want="$1" root="$2" name="$3" payload="${4:-}"
  run_hook "$root" "$payload"
  local got=$?
  if [ "$got" -eq "$want" ]; then pass "$name (exit $got)"
  else fail "$name — expected exit $want, got $got"$'\n'"$(sed 's/^/      /' "$ERR")"; fi
}

# --- 1. the hook itself ------------------------------------------------------
section "the hook ships runnable"
assert_file "$HOOK"        "hook exists"
ok "[ -x '$HOOK' ]"        "hook is executable"
assert_ok bash -n "$HOOK"  "hook parses"

# --- 2. green and empty ------------------------------------------------------
section "green suites close the task"
d="$(fresh green)"; pass_suite "$d/suites/a.test.sh"; pass_suite "$d/suites/b.test.sh"
assert_hook 0 "$d" "all suites passing closes the task"

section "a project with no suites is not a failure"
d="$(fresh nosuites)"
assert_hook 0 "$d" "no discoverable suites closes the task"

section "both discovery forms are honored (mirrors tests.yml)"
d="$(fresh discovery)"; mkdir -p "$d/pkg/tests"; fail_suite "$d/pkg/tests/test.sh"
assert_hook 2 "$d" "*/tests/test.sh is discovered, not just *.test.sh"

section "worktrees are pruned"
d="$(fresh wt)"; mkdir -p "$d/.claude/worktrees/peer"
fail_suite "$d/.claude/worktrees/peer/x.test.sh"; pass_suite "$d/suites/a.test.sh"
assert_hook 0 "$d" "a red suite inside .claude/worktrees does not block"

# --- 3. red, undocumented ----------------------------------------------------
section "red suites block, with the failure fed back"
d="$(fresh red)"; pass_suite "$d/suites/a.test.sh"; fail_suite "$d/suites/b.test.sh"
assert_hook 2 "$d" "a failing suite refuses the completion"
assert_grep "boom: 1 assertion failed" "$ERR" "raw suite output is fed back"
assert_grep "suites/b.test.sh"         "$ERR" "the failing suite is named"
assert_grep "BLOCKED.md"               "$ERR" "the blocked exit is offered"
assert_grep "weakening an assertion"   "$ERR" "the forbidden workarounds are named"

section "the task subject is echoed when jq can read it"
run_hook "$d" '{"task_subject":"wire up the sync retry"}'
if command -v jq >/dev/null 2>&1; then
  assert_grep "wire up the sync retry" "$ERR" "subject makes the message concrete"
else
  ok "true" "jq absent — subject echo skipped, gate still ran (exit was $?)"
fi

section "a malformed payload does not fail the gate open"
assert_hook 2 "$d" "garbage on stdin still blocks a red suite" 'not json at all'

# --- 4. red, documented as blocked ------------------------------------------
note() { printf 'Missing: %s\nTried: %s\nNeeds: %s\n' "$1" "$2" "$3"; }

section "a complete BLOCKED.md closes the task as blocked"
d="$(fresh blocked)"; fail_suite "$d/suites/b.test.sh"
note "STRIPE_API_KEY" "ran the suite against the sandbox" "a test key in the env" \
  > "$d/BLOCKED.md"
echo "failing: suites/b.test.sh" >> "$d/BLOCKED.md"
assert_hook 0 "$d" "documented blockage is a valid ending"
assert_grep "valid ending" "$ERR" "the hook says so rather than staying silent"
assert_grep "committed"    "$ERR" "and asks for the note to be committed"

section "every failing suite must be covered, not just one"
d="$(fresh partial)"; fail_suite "$d/suites/b.test.sh"; fail_suite "$d/suites/c.test.sh"
note "STRIPE_API_KEY" "ran it" "a key" > "$d/BLOCKED.md"
echo "failing: suites/b.test.sh" >> "$d/BLOCKED.md"
assert_hook 2 "$d" "a note covering one of two failures still blocks"
assert_grep "never mentions" "$ERR" "the report says which failure is uncovered"
assert_grep "suites/c.test.sh" "$ERR" "and names it"

section "covering both closes it"
echo "failing: suites/c.test.sh" >> "$d/BLOCKED.md"
assert_hook 0 "$d" "a note covering every failure closes the task"

section "a stale note cannot act as a permanent bypass"
d="$(fresh stale)"; fail_suite "$d/suites/new.test.sh"
note "OLD_TOKEN" "last week's attempt" "the token" > "$d/BLOCKED.md"
echo "failing: suites/gone.test.sh" >> "$d/BLOCKED.md"
assert_hook 2 "$d" "a note naming a different suite does not wave this one through"

# --- 5. incomplete notes -----------------------------------------------------
section "an incomplete note is not a bypass"
for label in Missing Tried Needs; do
  d="$(fresh "no$label")"; fail_suite "$d/suites/b.test.sh"
  note "K" "t" "n" | grep -v "^${label}:" > "$d/BLOCKED.md"
  echo "failing: suites/b.test.sh" >> "$d/BLOCKED.md"
  assert_hook 2 "$d" "a note without '${label}:' still blocks"
  assert_grep "${label}:" "$ERR" "the report names the missing '${label}:' label"
done

section "an empty note is not a note"
d="$(fresh emptynote)"; fail_suite "$d/suites/b.test.sh"; : > "$d/BLOCKED.md"
assert_hook 2 "$d" "an empty BLOCKED.md blocks"
assert_grep "does not yet account" "$ERR" "and says the note is insufficient"

# --- 6. environment ----------------------------------------------------------
section "environment"
d="$(fresh nonexistent)"; rm -rf "$d"
assert_hook 0 "$d" "an unreachable project dir fails open rather than wedging the task"

section "deterministic"
d="$(fresh det)"; fail_suite "$d/suites/b.test.sh"
run_hook "$d"; e1="$(cat "$ERR")"
run_hook "$d"; e2="$(cat "$ERR")"
assert_eq "$e1" "$e2" "same state, same feedback"

assert_summary; exit
