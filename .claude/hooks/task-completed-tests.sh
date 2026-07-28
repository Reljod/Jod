#!/usr/bin/env bash
#
# task-completed-tests.sh — TaskCompleted gate for agent teams.
#
# Agent teams do NOT isolate teammates in worktrees: every teammate edits the
# same checkout. So a teammate can finish its own task green and still have
# broken a file another teammate owns, and nobody notices until CI. This runs
# the same suites `.github/workflows/tests.yml` runs — discovery logic mirrored
# deliberately, including excluding tests/e2e/run.sh by naming rather than path
# — and refuses the completion if any fail, feeding the failure back to the
# teammate so it fixes or escalates instead of closing the task.
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
status=0
while IFS= read -r -d '' suite; do
  if ! output="$(run_suite "$suite")"; then
    status=1
    failures+="--- ${suite} ---"$'\n'"${output}"$'\n\n'
  fi
done < <(
  find . \
    \( -name .git -o -path ./.claude/worktrees \) -prune -o \
    -type f \( -name '*.test.sh' -o -path '*/tests/test.sh' \) -print0
)

if [ "$status" -ne 0 ]; then
  {
    echo "Test suites are failing, so this task cannot be marked complete${subject:+: ${subject}}."
    echo
    echo "Teammates share one checkout, so the break may be yours or a teammate's."
    echo "Fix it if the failing files are yours; otherwise message the teammate who"
    echo "owns them and keep the task open until it is green."
    echo
    printf '%s' "$failures"
  } >&2
  exit 2
fi

exit 0
