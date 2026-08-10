#!/usr/bin/env bash
#
# shell-arrays.test.sh — catches the one bash portability bug that reliably
# breaks this repo's scripts on the maintainer's own machine.
#
# macOS ships bash 3.2, where `"${a[@]}"` on an *empty* array is an
# unbound-variable error under `set -u`. bash 4+ and zsh expand it to nothing,
# so the bug is invisible on CI and on the VPS and fires only on the Mac.
#
# It is not a theoretical class. `merge_pr.sh` built `ready_args=()` and left it
# empty on every run that did not pass `--repo`, so `gh pr ready` crashed the
# gate *after* it had already printed its verdict — checks green, base fresh,
# `triage auto-merge` — and the PR sat open looking as though it had been
# refused. A merge gate that dies between deciding and acting is worse than one
# that refuses, because the exit code says "refused" and the transcript says
# "approved".
#
# The guard is `${a[@]+"${a[@]}"}`: the `+` form expands to nothing when the
# array is unset-or-empty and to the properly-quoted elements otherwise.
# pr_sweep.sh already documented and used it; merge_pr.sh had missed one site.
#
# So: for every `set -u` script in the repo, every array that is *initialized
# empty* must use the guard at each of its `[@]` expansions. Arrays seeded with
# at least one element are left alone — they can never trip it.
#
# Deliberately static, offline, read-only: no gh, no network, no git.
# Run: tests/shell-arrays.test.sh
set -u

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$REPO_ROOT/.agents/skills/test-scenarios/scripts/assert.sh"
cd "$REPO_ROOT" || exit 1

section "the bug is real on this bash"

# Proves the premise rather than asserting it. On bash 3.2 the unguarded form
# fails and the guarded one succeeds; on bash 4+ both succeed, and the guard
# is still correct, so only the guarded case is asserted.
if bash -c 'set -euo pipefail; a=(); : "${a[@]}"' 2>/dev/null; then
  pass "this bash tolerates the unguarded form (4+); the guard is still required for 3.2"
else
  pass "this bash rejects the unguarded form (3.2) — exactly the Mac failure"
fi
assert_ok bash -c 'set -euo pipefail; a=(); : ${a[@]+"${a[@]}"}'

section "every empty-initialized array is expanded through the guard"

scripts="$(find .agents bin hooks tests -type f -name '*.sh' 2>/dev/null | sort)"
[ -n "$scripts" ] || fail "found no shell scripts to scan"

checked=0
SELF="tests/shell-arrays.test.sh"

for f in $scripts; do
  # This file quotes both the broken and the fixed form on purpose, so scanning
  # it would report its own examples as bugs.
  [ "$f" = "./$SELF" ] || [ "$f" = "$SELF" ] && continue
  grep -q 'set -[a-z]*u' "$f" || continue   # only `set -u` scripts can trip it

  # Arrays initialized empty, in any of the spellings this repo uses:
  # `name=()`, `local name=()`, `local -a name=()`, and inline in a longer
  # `local a b opts=() c` declaration. Matching the assignment token itself
  # rather than the whole line keeps this portable — BSD sed (macOS) has no
  # `\?`, which is the sort of thing this file exists to notice.
  arrays="$(grep -oE '[A-Za-z_][A-Za-z0-9_]*=\(\)' "$f" | sed 's/=()$//' | sort -u)"
  [ -n "$arrays" ] || continue

  for a in $arrays; do
    checked=$((checked + 1))
    # Every expansion of this array, minus the ones already inside a `+` guard.
    # The guard itself contains `${a[@]}`, so strip guarded forms before looking
    # for bare ones — otherwise the fix would read as the bug. Both spellings
    # count: `${a[@]+…}` and the scalar `${a+…}`, which tests `a[0]` and is
    # equally correct for an array (rebase-main.sh uses that one).
    #
    # `${#a[@]}` is deliberately not stripped and never matches: taking the
    # length of an empty array is safe under `set -u`, and the pattern below
    # requires `${` immediately before the name.
    # Whole-line comments are dropped first: they cannot execute, and the fixed
    # sites explain themselves by quoting the broken form, which would otherwise
    # read as the bug it warns about. Only lines that are entirely a comment go,
    # so a `#` inside a string is never mistaken for one.
    bare="$(grep -v '^[[:space:]]*#' "$f" \
      | sed \
        -e "s/\\\${$a\[@\]+\"\\\${$a\[@\]}\"}//g" \
        -e "s/\\\${$a+\"\\\${$a\[@\]}\"}//g" \
        -e "s/\\\${$a\[@\]:\{0,1\}+[^}]*}//g" \
      | grep -c "\${$a\[@\]}" || true)"
    # Two ways to be correct, and the second is not a loophole. A script that
    # tests `${#a[@]}` has *proved* the array is populated before expanding it —
    # taking a length is safe on an empty array, so the check itself can never
    # trip. tdd-loop.sh does this and must NOT be "fixed": with `CMD` empty,
    # `${CMD[@]+"${CMD[@]}"}` would expand `"${CMD[@]}"; rc=$?` down to a bare
    # `; rc=$?` — a syntax error in place of a clear unbound-variable message,
    # in a state its guard already rules out.
    #
    # What separated the real bug from these: `ready_args` was empty on *every*
    # run that omitted `--repo`, and nothing checked it.
    proves_nonempty="$(grep -c "\${#$a\[@\]}" "$f" || true)"
    if [ "$bare" -eq 0 ]; then
      pass "$f: \$$a is empty-initialized and every [@] expansion is guarded"
    elif [ "$proves_nonempty" -gt 0 ]; then
      pass "$f: \$$a is empty-initialized but checks \${#$a[@]} before expanding"
    else
      fail "$f: \$$a is empty-initialized, unguarded at $bare \${$a[@]} expansion(s), and never checks \${#$a[@]} — crashes on macOS bash 3.2"
    fi
  done
done

ok "[ $checked -gt 0 ]" "scanned at least one empty-initialized array ($checked found)"

section "the site that actually broke stays fixed"

GATE=".agents/skills/auto-merge/scripts/merge_pr.sh"
assert_file "$GATE"
assert_grep 'gh pr ready "$pr" ${ready_args[@]+"${ready_args[@]}"}' "$GATE" \
  "merge_pr.sh marks a draft ready through the guarded expansion"
assert_ok bash -n "$GATE"

assert_summary
exit
