#!/usr/bin/env bash
#
# assert.sh — sourceable assertion helpers for deterministic, dependency-free
# test suites. No framework, no network. Source it, call the assert_* / ok
# helpers, and end with `assert_summary` (its exit status is 0 iff every
# assertion passed, so the whole suite works as a CI/pre-push gate).
#
#   source "$(dirname "$0")/../../test-scenarios/scripts/assert.sh"
#   section "happy path"
#   assert_ok   mytool --run
#   assert_file out.txt
#   section "bad input"
#   assert_fails mytool --nope
#   assert_summary; exit
#
# Every helper prints one PASS/FAIL line and bumps a counter — the contract of
# the test-scenarios skill: one deterministic assertion per scenario, nothing
# left to "probably fine".

_T_PASS=0
_T_FAIL=0

pass() { printf '  \033[32mPASS\033[0m %s\n' "$*"; _T_PASS=$((_T_PASS + 1)); }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$*"; _T_FAIL=$((_T_FAIL + 1)); }
section() { printf '\n-- %s\n' "$*"; }

# ok <shell-expression> <name> — pass iff the expression evaluates true.
# The escape hatch for assertions the typed helpers below don't cover.
ok() { if eval "$1"; then pass "${2:-$1}"; else fail "${2:-$1}"; fi; }

# assert_ok <cmd...> / assert_fails <cmd...> — run a command, assert its exit.
# Output is muted so suites stay readable; the assertion is the exit code.
assert_ok()    { if "$@" >/dev/null 2>&1; then pass "succeeds: $*"; else fail "should succeed: $*"; fi; }
assert_fails() { if "$@" >/dev/null 2>&1; then fail "should fail: $*"; else pass "fails as expected: $*"; fi; }

# Filesystem predicates.
assert_file()    { if [ -f "$1" ]; then pass "${2:-file exists: $1}"; else fail "${2:-file missing: $1}"; fi; }
assert_dir()     { if [ -d "$1" ]; then pass "${2:-dir exists: $1}"; else fail "${2:-dir missing: $1}"; fi; }
assert_missing() { if [ ! -e "$1" ]; then pass "${2:-absent: $1}"; else fail "${2:-should be absent: $1}"; fi; }
assert_symlink() { if [ -L "$1" ]; then pass "${2:-symlink: $1}"; else fail "${2:-not a symlink: $1}"; fi; }

# assert_link_to <link> <target> — symlink exists and points where expected.
assert_link_to() {
  if [ -L "$1" ] && [ "$(readlink "$1")" = "$2" ]; then pass "${3:-$1 -> $2}"
  else fail "${3:-$1 should -> $2}"; fi
}

# Content predicates. Fixed-string (grep -F) so patterns with regex-special
# characters (. & | / etc.) match literally — important for edge-case inputs.
assert_grep()    { if grep -qF -- "$1" "$2"; then pass "${3:-'$1' in $2}"; else fail "${3:-'$1' not in $2}"; fi; }
assert_no_grep() { if grep -qF -- "$1" "$2"; then fail "${3:-'$1' unexpectedly in $2}"; else pass "${3:-no '$1' in $2}"; fi; }

# assert_eq <actual> <expected> <name> — exact string equality.
assert_eq() { if [ "$1" = "$2" ]; then pass "${3:-'$1' == '$2'}"; else fail "${3:-expected '$2', got '$1'}"; fi; }

# assert_summary — print the tally; return non-zero if anything failed.
assert_summary() {
  printf '\n== %d passed, %d failed ==\n' "$_T_PASS" "$_T_FAIL"
  [ "$_T_FAIL" -eq 0 ]
}
