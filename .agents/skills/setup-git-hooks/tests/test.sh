#!/usr/bin/env bash
#
# test.sh — deterministic tests for the commit-msg gate and the installer.
# Enumerated against test-scenarios/references/scenario-checklist.md and built
# on its assert.sh helper. Run: .agents/skills/setup-git-hooks/tests/test.sh
#
# The gate's whole value is "same message in -> same pass/fail out", so it is
# tested the way it runs: pipe a message file at the hook, check the exit.
#
set -u

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd -- "$TEST_DIR/.." && pwd)"
HOOK="$SKILL_DIR/hooks/commit-msg"
INSTALL="$SKILL_DIR/scripts/install-hooks.sh"
CONF_TPL="$SKILL_DIR/templates/commit-convention.conf"
# shellcheck source=/dev/null
source "$SKILL_DIR/../test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Run the hook against a message, with an optional conf placed beside a copy
# of it (the hook reads ./commit-convention.conf relative to itself).
# usage: try <msg> [conf-contents]
try() {
  local msg="$1" conf="${2-}" dir
  dir="$(mktemp -d "$WORK/hookXXXXXX")"
  cp "$HOOK" "$dir/commit-msg"
  if [ -n "$conf" ]; then printf '%s\n' "$conf" > "$dir/commit-convention.conf"; fi
  printf '%s\n' "$msg" > "$dir/MSG"
  "$dir/commit-msg" "$dir/MSG" >/dev/null 2>&1
}
accepts() { if try "$1" "${3-}"; then pass "accepts: ${2:-$1}"; else fail "should accept: ${2:-$1}"; fi; }
rejects() { if try "$1" "${3-}"; then fail "should reject: ${2:-$1}"; else pass "rejects: ${2:-$1}"; fi; }

TICKET_ON='TICKET_REGEX="[A-Z][A-Z0-9]+-[0-9]+"'

echo "== setup-git-hooks test suite =="

# --- 1. the default convention: <type>: <subject>, no ticket -----------------
section "default convention (no ticket required)"
accepts "feat: add retry to the sync worker"
accepts "fix(api)!: drop the v1 orders route"     "scope + breaking marker"
accepts "chore: bump ruff to 0.6"
accepts "revert: undo the cache change"
for t in feat fix bug chore docs refactor test perf ci build style revert; do
  accepts "$t: do the thing" "type '$t' allowed"
done

section "a ticket key is accepted, never demanded, by default"
accepts "feat: ENG-123 add retry to the sync worker" "ticket key still allowed"
accepts "feat: add retry to the sync worker"         "same subject without one"

# --- 2. structural rejections ------------------------------------------------
section "structural rejections"
rejects "update stuff"                "no type"
rejects "feat add retry"              "missing colon"
rejects "feat:add retry"              "missing space after colon"
rejects "feat: "                      "type but empty subject"
rejects "nope: add retry"             "unknown type"
rejects "Feat: add retry"             "type is case-sensitive"
rejects ""                            "empty message"

# --- 3. the length ceiling ---------------------------------------------------
section "subject length ceiling"
LONG="$(printf 'feat: %s' "$(head -c 80 < /dev/zero | tr '\0' 'x')")"
rejects "$LONG"                       "86-char subject exceeds the 72 cap"
accepts "feat: $(head -c 60 < /dev/zero | tr '\0' 'x')" "66-char subject fits"
accepts "$LONG" "no cap when MAX_SUBJECT_LENGTH=0" 'MAX_SUBJECT_LENGTH=0'

# --- 4. git's own auto-generated messages pass through -----------------------
section "git plumbing messages are never gated"
accepts "Merge branch 'main' into feat/x"
accepts "Revert \"feat: add retry\""
accepts "fixup! feat: add retry"
accepts "squash! feat: add retry"
accepts "amend! feat: add retry"

# --- 5. comments and blank lines ---------------------------------------------
section "subject extraction"
printf '%s\n' "" "# a comment git added" "feat: add retry" > "$WORK/msg-lead"
cp "$HOOK" "$WORK/hook-lead"
assert_ok "$WORK/hook-lead" "$WORK/msg-lead"   # first non-blank, non-comment line wins

# --- 6. opt-in ticket enforcement --------------------------------------------
section "TICKET_REGEX set: the key becomes mandatory"
accepts "feat: ENG-123 add retry"     "ticketed subject"                 "$TICKET_ON"
accepts "fix(api)!: PLATFORM-7 drop the v1 route" "scoped + ticketed"    "$TICKET_ON"
rejects "feat: add retry"             "missing ticket when required"     "$TICKET_ON"
rejects "feat: eng-123 add retry"     "lower-case key does not match"    "$TICKET_ON"
rejects "feat: ENG-123"               "ticket with no subject after it"  "$TICKET_ON"
accepts "chore: bump ruff to 0.6"     "exempt type needs no ticket"      "$TICKET_ON"
accepts "docs: fix a typo"            "docs is exempt too"               "$TICKET_ON"
rejects "test: add a case"            "test is NOT exempt"               "$TICKET_ON"
ok "SKIP_TICKET=1 try 'feat: add retry' '$TICKET_ON'" "SKIP_TICKET=1 bypasses the requirement"

section "a custom TICKET_REGEX is honoured"
JIRA='TICKET_REGEX="[A-Z]+-[0-9]+"
TICKET_EXEMPT_TYPES=""'
accepts "feat: AB-1 add retry"        "custom pattern matches"           "$JIRA"
rejects "chore: bump ruff"            "no exempt types left"             "$JIRA"

section "a custom ALLOWED_TYPES is honoured"
TYPES='ALLOWED_TYPES="feat|fix"'
accepts "fix: patch it"               "listed type"                      "$TYPES"
rejects "chore: bump it"              "type dropped from the list"       "$TYPES"

# --- 7. the shipped template is the no-ticket default ------------------------
section "shipped commit-convention.conf"
assert_grep 'TICKET_REGEX=""' "$CONF_TPL"     "template ships TICKET_REGEX empty"
ok "! grep -qE '^TICKET_REGEX=\"[^\"]' '$CONF_TPL'" "no non-empty TICKET_REGEX assignment"
ok "grep -qE '^TICKET_REGEX=\"\"' <(grep -v '^#' '$CONF_TPL')" "the live line, not just a comment"

# --- 8. the installer ---------------------------------------------------------
section "install-hooks.sh"
REPO="$WORK/repo"; mkdir -p "$REPO"; git -C "$REPO" init --quiet
assert_ok "$INSTALL" "$REPO"
for h in commit-msg pre-commit pre-push; do
  assert_file "$REPO/.githooks/$h" "installed $h"
  ok "[ -x '$REPO/.githooks/$h' ]" "$h is executable"
done
assert_file "$REPO/.githooks/commit-convention.conf" "installed the conf"
assert_eq "$(git -C "$REPO" config core.hooksPath)" ".githooks" "core.hooksPath wired up"

section "installed hook enforces the default convention"
printf '%s\n' "feat: add retry" > "$REPO/GOOD"
printf '%s\n' "update stuff"    > "$REPO/BAD"
assert_ok    "$REPO/.githooks/commit-msg" "$REPO/GOOD"
assert_fails "$REPO/.githooks/commit-msg" "$REPO/BAD"

section "re-running keeps a tuned conf (idempotency)"
printf '%s\n' 'ALLOWED_TYPES="feat"' >> "$REPO/.githooks/commit-convention.conf"
assert_ok "$INSTALL" "$REPO"
assert_grep 'ALLOWED_TYPES="feat"' "$REPO/.githooks/commit-convention.conf" "local tuning survived"
assert_ok "$INSTALL" "$REPO" --force
assert_no_grep 'ALLOWED_TYPES="feat"' "$REPO/.githooks/commit-convention.conf" "--force restored the template"

section "installer refuses a non-repo"
NOTREPO="$WORK/notrepo"; mkdir -p "$NOTREPO"
assert_fails "$INSTALL" "$NOTREPO"

assert_summary
