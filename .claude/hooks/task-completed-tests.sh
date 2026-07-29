#!/usr/bin/env bash
#
# task-completed-tests.sh — TaskCompleted gate.
#
# Two jobs, and the second one is why this is not just "run the tests":
#
# 1. Agent teams do NOT isolate teammates in worktrees: every teammate edits
#    the same checkout, so a teammate can finish its own task green and still
#    have broken a file another teammate owns. This runs the same suites
#    `.github/workflows/tests.yml` runs — discovery logic mirrored
#    deliberately, including excluding tests/e2e/run.sh by naming rather than
#    path — and refuses the completion if any fail.
#
# 2. It accepts a documented BLOCKED.md instead. A gate whose only successful
#    exit is "tests pass" mathematically requires a fake when the tests cannot
#    pass — no key, no service, no fixture — so the cheapest remaining path
#    becomes mocking the client or skipping the test. Making honesty a legal
#    ending removes the incentive rather than trying to police the outcome.
#
# BLOCKED.md must carry `Missing:`, `Tried:`, `Needs:` and name every failing
# suite. A note that doesn't cover a failure can't wave that failure through,
# which is also what keeps a stale note from working as a permanent bypass.
#
# Cheap by design (the full suite is well under a second) because it fires on
# every task completion, in solo sessions too, not only team ones.
#
# Exit 0 = task may close. Exit 2 = blocked; stderr goes back to the model.
set -uo pipefail

ROOT="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
cd "$ROOT" || exit 0

# The payload is JSON on stdin; task_subject is only used to make the feedback
# message concrete. jq is optional so the gate never fails open on a bare box.
payload="$(cat)"
subject=""
if command -v jq >/dev/null 2>&1; then
  subject="$(printf '%s' "$payload" | jq -r '.task_subject // empty' 2>/dev/null)"
fi

# Cap a hung suite rather than blocking the teammate forever. macOS without
# coreutils has no `timeout`, so fall back to running the suite bare.
run_suite() {
  if command -v timeout >/dev/null 2>&1; then
    timeout 120 bash "$1" 2>&1
  else
    bash "$1" 2>&1
  fi
}

failures=""
failed_suites=""
status=0
while IFS= read -r -d '' suite; do
  if ! output="$(run_suite "$suite")"; then
    status=1
    failed_suites+="${suite#./}"$'\n'
    failures+="--- ${suite} ---"$'\n'"${output}"$'\n\n'
  fi
done < <(
  find . \
    \( -name .git -o -path ./.claude/worktrees \) -prune -o \
    -type f \( -name '*.test.sh' -o -path '*/tests/test.sh' \) -print0
)

[ "$status" -eq 0 ] && exit 0

NOTE="BLOCKED.md"

# Which of the three required labels the note is missing, and which failing
# suites it never mentions. Both empty = the note accounts for this failure.
note_gaps() {
  local missing_labels="" uncovered=""
  for label in "Missing:" "Tried:" "Needs:"; do
    grep -qF -- "$label" "$NOTE" || missing_labels+=" ${label}"
  done
  while IFS= read -r s; do
    [ -n "$s" ] || continue
    grep -qF -- "$s" "$NOTE" || uncovered+="    ${s}"$'\n'
  done <<< "$failed_suites"

  [ -n "$missing_labels" ] && echo "  missing label(s):${missing_labels}"
  [ -n "$uncovered" ] && { echo "  failing suites the note never mentions:"; printf '%s' "$uncovered"; }
}

if [ -f "$NOTE" ]; then
  gaps="$(note_gaps)"
  if [ -z "$gaps" ]; then
    {
      echo "Suites are red, but ${NOTE} documents why and covers every failing suite."
      echo "Closing this task as blocked — that is a valid ending, not a workaround."
      echo "Make sure ${NOTE} is committed so review sees it."
    } >&2
    exit 0
  fi
fi

{
  echo "Test suites are failing, so this task cannot be marked complete${subject:+: ${subject}}."
  echo
  echo "There are exactly two ways to close it, and neither is a workaround:"
  echo
  echo "  1. Make the suites pass with real output. Not by skipping or deleting a"
  echo "     test, weakening an assertion, widening an except/catch, or swapping a"
  echo "     real integration for a mock — those close the task and lose the signal."
  echo "     Teammates share one checkout, so the break may be yours or a peer's:"
  echo "     fix it if the files are yours, otherwise message whoever owns them."
  echo
  echo "  2. If it cannot pass because a capability is genuinely absent (no key, no"
  echo "     service, no fixture), write ${NOTE} and close as blocked. It needs:"
  echo "       Missing: <the capability that is absent>"
  echo "       Tried:   <what you attempted>"
  echo "       Needs:   <what would unblock it>"
  echo "     plus the path of every failing suite listed below."
  if [ -f "$NOTE" ]; then
    echo
    echo "${NOTE} exists but does not yet account for this failure:"
    note_gaps
  fi
  echo
  printf '%s' "$failures"
} >&2
exit 2
