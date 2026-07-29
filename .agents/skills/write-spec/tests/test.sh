#!/usr/bin/env bash
#
# Scenario suite for write-spec/scripts/check-spec.sh.
#
# Enumerated against test-scenarios/references/scenario-checklist.md and built
# on its assert.sh helper. Run: .agents/skills/write-spec/tests/test.sh
set -uo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$TEST_DIR/.." && pwd)"
CHECK="$SKILL_DIR/scripts/check-spec.sh"
TEMPLATE="$SKILL_DIR/templates/SPEC.md"

# shellcheck source=../../test-scenarios/scripts/assert.sh
source "$TEST_DIR/../../test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK" || exit 1

# A spec that satisfies every rule. Each negative case below breaks exactly
# one thing in it, so a failure names the rule that caught it.
write_valid() {
  cat > "$1" <<'SPEC'
# SPEC — retry the sync worker

## Goal

Failed syncs retry three times with backoff instead of dropping the batch.

## Files & interfaces

| Path | What changes |
|---|---|
| `src/sync/worker.py` | wrap `run_batch` in a retry |

- `run_batch(batch) -> Result`

## Out of scope

- the scheduler
- the dead-letter queue

## Verification

```
pytest tests/test_sync.py::test_retries_three_times -q
```

Expected: exit 0, 3 attempts logged.

**Done means one of exactly two things:** the check passes with real output,
or a `BLOCKED.md` names the missing capability, what was tried, and what is
needed.

## Sanctioned fakes

None

## Escalate on

- irreversible actions, migrations, auth, money, deletion
- public API or schema contracts

## Decision log

| Decision | Why | Confidence |
|---|---|---|
SPEC
}

# Exit-code predicate: check-spec distinguishes "gaps" (1) from "usage" (2).
assert_exit() {
  local want="$1" file="$2" name="$3"
  "$CHECK" "$file" >/dev/null 2>&1
  local got=$?
  if [ "$got" -eq "$want" ]; then pass "$name (exit $got)"
  else fail "$name — expected exit $want, got $got"; fi
}

# --- 1. the script itself ----------------------------------------------------
section "the script and template ship together"
assert_file "$CHECK"     "check-spec.sh exists"
ok "[ -x '$CHECK' ]"     "check-spec.sh is executable"
assert_file "$TEMPLATE"  "SPEC.md template exists"
assert_ok bash -n "$CHECK" "check-spec.sh parses"

# --- 2. happy path -----------------------------------------------------------
section "happy path"
write_valid good.md
assert_exit 0 good.md "a fully filled spec passes"
"$CHECK" good.md > out.txt 2>&1
assert_grep "is complete" out.txt "success message names the outcome"

section "the shipped template is (correctly) not executable as-is"
cp "$TEMPLATE" blank.md
assert_exit 1 blank.md "the unfilled template fails — its guidance is not content"

# --- 3. every required section, one at a time -------------------------------
section "each required section is enforced"
while IFS= read -r h; do
  [ -n "$h" ] || continue
  slug="$(printf '%s' "$h" | tr -cd 'a-z')"
  write_valid "no-$slug.md"
  grep -vxF -- "$h" "no-$slug.md" > tmp && mv tmp "no-$slug.md"
  assert_exit 1 "no-$slug.md" "missing '$h' is a gap"
  "$CHECK" "no-$slug.md" > "err-$slug.txt" 2>&1
  assert_grep "$h" "err-$slug.txt" "the report names the missing '$h'"
done <<'HEADINGS'
## Goal
## Files & interfaces
## Out of scope
## Verification
## Sanctioned fakes
## Escalate on
## Decision log
HEADINGS

# --- 4. emptied sections -----------------------------------------------------
section "a section emptied of content fails even though the heading is there"
write_valid empty-goal.md
awk '/^## Goal$/ { print; print ""; skip = 1; next } skip && /^## / { skip = 0 } !skip' \
  empty-goal.md > tmp && mv tmp empty-goal.md
assert_exit 1 empty-goal.md "heading present but body blank is a gap"

section "a section holding only an HTML comment counts as empty"
write_valid comment-only.md
awk '/^## Out of scope$/ { print; print ""; print "<!-- name the tempting"; print "     adjacent work -->"; print ""; skip = 1; next } skip && /^## / { skip = 0 } !skip' \
  comment-only.md > tmp && mv tmp comment-only.md
assert_exit 1 comment-only.md "multi-line comment is stripped, section reads as empty"

section "scaffolding is not content"
write_valid scaffold.md
awk '/^## Out of scope$/ { print; print ""; print "-"; print ""; skip = 1; next } skip && /^## / { skip = 0 } !skip' \
  scaffold.md > tmp && mv tmp scaffold.md
assert_exit 1 scaffold.md "a lone '-' bullet does not fill a section"

# --- 5. the Verification section's two specific rules -----------------------
section "Verification needs a runnable command"
write_valid nocmd.md
grep -vF 'pytest tests/test_sync.py' nocmd.md > tmp && mv tmp nocmd.md
assert_exit 1 nocmd.md "an empty fenced block is not a command"

section "Verification must keep blocked as a legal exit"
write_valid noblocked.md
sed 's/`BLOCKED.md`/a note/' noblocked.md > tmp && mv tmp noblocked.md
assert_exit 1 noblocked.md "dropping the BLOCKED.md exit is a gap"
"$CHECK" noblocked.md > errb.txt 2>&1
assert_grep "two-exit rule" errb.txt "the report explains why blocked must stay"

# --- 6. leftover placeholders ------------------------------------------------
section "unfilled markers and deferred decisions"
write_valid tbd.md
printf '\n- rollout strategy: TBD\n' >> tbd.md
assert_exit 1 tbd.md "a TBD anywhere means the interview is not finished"

write_valid angle.md
sed '1s/.*/# SPEC — <feature name>/' angle.md > tmp && mv tmp angle.md
assert_exit 1 angle.md "the template's title placeholder is caught"

section "a placeholder inside a comment is invisible"
write_valid commented-tbd.md
printf '\n<!-- rollout strategy: TBD, decide later -->\n' >> commented-tbd.md
assert_exit 0 commented-tbd.md "commented-out guidance does not trip the marker scan"

# --- 7. Decision log is heading-only ----------------------------------------
section "Decision log is filled during execution, not at spec time"
write_valid emptylog.md
assert_grep "## Decision log" emptylog.md "heading present"
assert_exit 0 emptylog.md "an empty decision table still passes"

# --- 8. bad usage / environment ---------------------------------------------
section "bad usage"
assert_exit 2 nope.md "a missing spec file exits 2, distinct from a gap"
"$CHECK" nope.md > usage.txt 2>&1
assert_grep "templates/SPEC.md" usage.txt "points at the template to copy"

section "defaults to ./SPEC.md"
write_valid SPEC.md
assert_ok "$CHECK"
rm -f SPEC.md
assert_fails "$CHECK"

# --- 9. determinism ----------------------------------------------------------
section "deterministic output"
write_valid det.md
"$CHECK" det.md > d1.txt 2>&1
"$CHECK" det.md > d2.txt 2>&1
assert_eq "$(cat d1.txt)" "$(cat d2.txt)" "same input, same output"

section "spaces and unicode in the spec path"
mkdir -p "a dir"
write_valid "a dir/SPÉC file.md"
assert_exit 0 "a dir/SPÉC file.md" "path with a space and a non-ASCII char works"

assert_summary; exit
