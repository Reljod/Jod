#!/usr/bin/env bash
#
# Scenario suite for create-pr's diff-derived scripts.
#
# The evidence bundle is what a reviewer trusts instead of re-reading the
# diff, so its flagging has to be checked, not assumed. Each case builds a
# throwaway git repo, makes one kind of change, and asserts what the report
# says about it.
#
# Enumerated against test-scenarios/references/scenario-checklist.md.
# Run: .agents/skills/create-pr/tests/test.sh
set -uo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$TEST_DIR/.." && pwd)"
BUNDLE="$SKILL_DIR/scripts/evidence_bundle.sh"
CATEGORIZE="$SKILL_DIR/scripts/categorize_diff.sh"
SKELETON="$SKILL_DIR/scripts/pr_body_skeleton.sh"

# shellcheck source=../../test-scenarios/scripts/assert.sh
source "$TEST_DIR/../../test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# scenario <name> — a fresh git repo with a committed baseline, cwd inside it.
scenario() {
  local d="$WORK/$1"
  mkdir -p "$d/src" "$d/tests" && cd "$d" || return 1
  git init -q .
  git config user.email "t@example.com"
  git config user.name "T"
  git config commit.gpgsign false
  printf 'def run():\n    return 1\n' > src/app.py
  printf 'def test_run():\n    assert run() == 1\n' > tests/test_app.py
  git add -A && git commit -qm base
}

# report — commit the working changes and render the bundle for that commit.
report() {
  git add -A && git commit -qm change >/dev/null
  "$BUNDLE" "HEAD~1...HEAD" "$@" > report.md 2> report.err
  echo "$?"
}

# --- 1. the scripts ship runnable -------------------------------------------
section "the scripts ship runnable"
for s in "$BUNDLE" "$CATEGORIZE" "$SKELETON"; do
  assert_file "$s"
  ok "[ -x '$s' ]" "executable: $(basename "$s")"
  assert_ok bash -n "$s" "parses: $(basename "$s")"
done

# --- 2. usage and empty input ------------------------------------------------
section "bad usage"
assert_fails "$BUNDLE"
scenario usage >/dev/null
assert_fails "$BUNDLE" HEAD...HEAD --spec
assert_fails "$BUNDLE" HEAD...HEAD --bogus-flag

section "an empty range is not an error"
assert_ok "$BUNDLE" "HEAD...HEAD"

# --- 3. blast radius ---------------------------------------------------------
section "blast radius tiers by attention, not alphabet"
scenario blast >/dev/null
mkdir -p migrations api tests/api docs
printf 'ALTER TABLE users ADD COLUMN plan text;\n' > migrations/002_plan.sql
printf 'def handler():\n    return {}\n'           > api/routes.py
printf 'def helper():\n    return 2\n'             > src/util.py
printf 'def test_api():\n    assert 1\n'           > tests/api/test_routes.py
printf 'notes\n'                                   > docs/notes.md
assert_eq "$(report)" "0" "bundle exits 0 on a mixed diff"
assert_grep 'migrations/002_plan.sql' report.md   "a migration is reported"
assert_grep 'api/routes.py'           report.md   "an api file is reported"
ok "awk '/^\*\*High/,/^\*\*Medium/' report.md | grep -q 'migrations/002_plan.sql'" "migration lands in High"
ok "awk '/^\*\*High/,/^\*\*Medium/' report.md | grep -q 'api/routes.py'"           "api file lands in High"
ok "awk '/^\*\*Medium/,/^\*\*Low/'  report.md | grep -q 'src/util.py'"             "plain source lands in Medium"
ok "awk '/^\*\*Low/,0'              report.md | grep -q 'tests/api/test_routes.py'" "a test under api/ is Low, not High"
ok "awk '/^\*\*Low/,0'              report.md | grep -q 'docs/notes.md'"           "docs land in Low"
assert_grep '+1/-0' report.md "line counts are included"

# --- 4. contract diff --------------------------------------------------------
section "contract diff surfaces what callers depend on"
scenario contract >/dev/null
printf 'export function charge(cents) {}\n' > src/api.js
assert_eq "$(report)" "0" "runs on an added export"
assert_grep 'export function charge' report.md "an added export is contract"

scenario contract-removed >/dev/null
printf 'def gone():\n    return 1\n' >> src/app.py
git add -A && git commit -qm add >/dev/null
printf 'def run():\n    return 1\n' > src/app.py
assert_eq "$(report)" "0" "runs on a removed def"
assert_grep 'Removed (breaking' report.md "a removed def is called out as breaking"

scenario contract-none >/dev/null
printf '    # tidy up\n' >> src/app.py
report >/dev/null
assert_grep 'No public surface' report.md "an internal-only diff says so plainly"

# --- 5. substitutions — the workaround scan ---------------------------------
section "skipped and disabled checks are flagged"
scenario subs-skip >/dev/null
printf '@pytest.mark.skip(reason="flaky")\ndef test_run():\n    assert 1\n' > tests/test_app.py
report >/dev/null
assert_grep 'Skipped / disabled checks' report.md "a newly skipped test is flagged"
assert_grep 'pytest.mark.skip'          report.md "the offending line is quoted"

section "silenced failures are flagged"
scenario subs-silence >/dev/null
printf 'def run():\n    try:\n        go()\n    except:\n        pass\n' > src/app.py
report >/dev/null
assert_grep 'Silenced failures' report.md "a bare except is flagged"

section "credential-shaped literals are flagged"
scenario subs-creds >/dev/null
printf 'api_key = "sk_live_abc123def"\n' > src/config.py
report >/dev/null
assert_grep 'Hardcoded credential-shaped values' report.md "an invented key is flagged"

section "a mock is judged by where it lives"
scenario subs-mock-src >/dev/null
printf 'from unittest.mock import MagicMock\nclient = MagicMock()\n' > src/client.py
report >/dev/null
assert_grep 'Mocks/stubs in non-test code' report.md "a mock in shipped code is flagged"

scenario subs-mock-test >/dev/null
printf 'from unittest.mock import MagicMock\ndef test_run():\n    assert MagicMock()\n' > tests/test_app.py
report >/dev/null
assert_no_grep 'Mocks/stubs in non-test code' report.md "a mock in a test is normal, not flagged"

section "net assertion loss is flagged"
scenario subs-assert >/dev/null
printf 'def test_run():\n    pass\n' > tests/test_app.py
report >/dev/null
assert_grep 'Net assertions removed' report.md "deleting an assertion is visible"

section "a clean diff flags nothing but still reports what it touched"
scenario subs-clean >/dev/null
printf 'def helper():\n    return 2\n' > src/util.py
report >/dev/null
assert_grep 'None flagged'   report.md "nothing invented means nothing flagged"
assert_grep 'Touched anyway' report.md "test/CI file counts are stated either way"
assert_grep '0 test file(s), 0 CI file(s)' report.md "and the counts are right"

section "touching CI during a change is stated, not hidden"
scenario subs-ci >/dev/null
mkdir -p .github/workflows
printf 'name: t\non: [push]\n' > .github/workflows/t.yml
report >/dev/null
assert_grep '1 CI file(s)' report.md "a CI edit is counted"

# --- 6. spec deviation -------------------------------------------------------
section "spec deviation: diff vs stated intent"
scenario spec-match >/dev/null
printf '# SPEC\n\n## Files & interfaces\n\n- `src/app.py`\n' > SPEC.md
git add -A && git commit -qm spec >/dev/null
printf 'def run():\n    return 2\n' > src/app.py
report >/dev/null
assert_grep 'Diff matches' report.md "a diff inside the spec reports as matching"

scenario spec-creep >/dev/null
printf '# SPEC\n\n- `src/app.py`\n' > SPEC.md
git add -A && git commit -qm spec >/dev/null
printf 'def run():\n    return 2\n' > src/app.py
printf 'def extra():\n    return 3\n' > src/elsewhere.py
report >/dev/null
assert_grep 'not named in'      report.md "an unplanned file is called out"
assert_grep 'src/elsewhere.py'  report.md "and named"

scenario spec-dropped >/dev/null
printf '# SPEC\n\n- `src/app.py`\n- `src/never_touched.py`\n' > SPEC.md
git add -A && git commit -qm spec >/dev/null
printf 'def run():\n    return 2\n' > src/app.py
report >/dev/null
assert_grep 'but unchanged'          report.md "a planned-but-skipped file is called out"
assert_grep 'src/never_touched.py'   report.md "and named"

section "an explicit --spec path is honored"
scenario spec-flag >/dev/null
mkdir -p docs/specs
printf '# SPEC\n\n- `src/app.py`\n' > docs/specs/retry.md
printf 'def run():\n    return 2\n' > src/app.py
report --spec docs/specs/retry.md >/dev/null
assert_grep 'docs/specs/retry.md' report.md "the named spec is the one compared against"

section "no spec at all points at write-spec instead of staying quiet"
scenario spec-none >/dev/null
printf 'def run():\n    return 2\n' > src/app.py
report >/dev/null
assert_grep 'write-spec' report.md "the report says how to get a spec"

# --- 7. determinism ----------------------------------------------------------
section "deterministic output"
scenario det >/dev/null
printf 'def run():\n    return 2\n' > src/app.py
report >/dev/null
cp report.md r1.md
"$BUNDLE" "HEAD~1...HEAD" > r2.md 2>/dev/null
assert_eq "$(cat r1.md)" "$(cat r2.md)" "same range, same report"

# --- 8. the skeleton still composes with the categorizer --------------------
section "pr_body_skeleton seeds the sections the diff touches"
scenario skeleton >/dev/null
mkdir -p .github/workflows
printf 'name: t\non: [push]\n' > .github/workflows/t.yml
git add -A && git commit -qm change >/dev/null
"$SKELETON" "HEAD~1...HEAD" > body.md 2>/dev/null
assert_grep '## Summary'  body.md "skeleton has a summary"
assert_grep '## Visuals'  body.md "skeleton front-loads visuals"
assert_grep '## Evidence' body.md "skeleton asks for evidence"
assert_grep 'Infra'       body.md "a workflow change seeds the infra hint"

assert_summary; exit
