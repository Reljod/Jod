#!/usr/bin/env bash
#
# run.sh — e2e fitness check for `setup-project` scaffolding across a wide
# array of representative project shapes ("fixtures" under ./fixtures/).
# Runs the real setup-project.sh against each fixture and checks two things:
#
#   1. Structural correctness (hard assertions, via assert.sh): the scaffold
#      completes, AGENTS.md/CLAUDE.md land where expected, no leftover
#      {{PLACEHOLDER}} tokens, requested skills copy cleanly.
#   2. Fitness (soft, logged as "gaps"): whether the generated charter is a
#      plausible fit for that kind of repo. A gap is a finding, not a
#      failure — it's how we discover what to support next, per the
#      charter's "extend by writing it down" rule.
#
# Deliberately NOT part of the tests.yml push/PR gate: spinning up N
# scaffolds is comparatively expensive, and gaps are exploratory, not a
# merge-blocking contract. Run on demand via the "E2E Scaffold Fitness"
# workflow_dispatch, or automatically when release.yml cuts a release.
# Run locally: tests/e2e/run.sh
set -u

E2E_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$E2E_DIR/../.." && pwd)"
SETUP="$REPO_ROOT/.agents/skills/setup-project/scripts/setup-project.sh"
DETECT_RUNNER="$REPO_ROOT/.agents/skills/tdd-loop/scripts/detect-test-runner.sh"
# shellcheck source=/dev/null
source "$REPO_ROOT/.agents/skills/test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

REPORT="$E2E_DIR/gaps-report.md"
: > "$REPORT"

# gap <fixture> <finding> — log a fitness gap. Informational: doesn't affect
# assert_summary's pass/fail tally, only the report.
gap() {
  printf -- '- **%s**: %s\n' "$1" "$2" >> "$REPORT"
  printf '  \033[33mGAP\033[0m  [%s] %s\n' "$1" "$2"
}

# scaffold <fixture> <preset> <skills> [extra setup-project.sh args...]
# Copies the fixture fresh into $WORK/run/<fixture> and scaffolds it there.
scaffold() {
  local fixture="$1" preset="$2" skills="$3"; shift 3
  local src="$E2E_DIR/fixtures/$fixture" dst="$WORK/run/$fixture"
  rm -rf "$dst"; mkdir -p "$(dirname "$dst")"; cp -R "$src" "$dst"
  local skills_args=()
  [ -n "$skills" ] && skills_args=(--skills "$skills")
  "$SETUP" --preset "$preset" "${skills_args[@]}" \
    --name "$fixture" --desc "Fixture repo for the setup-project e2e fitness check." \
    --ticket JOD --target "$dst" "$@"
}

echo "== setup-project e2e fitness check =="
echo "fixtures: $E2E_DIR/fixtures"

# --- node-lib: greenfield JS library, no existing conventions ---------------
section "node-lib -> jod preset (greenfield, no test setup yet)"
d="$WORK/run/node-lib"
assert_ok scaffold node-lib jod "create-pr,setup-git-hooks,tdd-loop,test-scenarios"
assert_file    "$d/AGENTS.md"          "node-lib: AGENTS.md written"
assert_link_to "$d/CLAUDE.md" "AGENTS.md" "node-lib: CLAUDE.md -> AGENTS.md"
assert_no_grep "{{" "$d/AGENTS.md"     "node-lib: no leftover placeholders"
for s in create-pr setup-git-hooks tdd-loop test-scenarios; do
  assert_dir "$d/.agents/skills/$s" "node-lib: skill $s copied"
done
if runner="$(cd "$d" && bash "$DETECT_RUNNER" 2>/dev/null)"; then
  ok true "node-lib: tdd-loop detects a runner ($runner)"
else
  gap node-lib "jod preset points at tdd-loop, but there's no test script or framework in package.json for it to detect yet — a brand-new repo gets the same test-first charter as one with a runner already wired up, with no starter test/config seeded."
fi

# --- python-cli: existing tests/, pyproject.toml -----------------------------
section "python-cli -> tdd-strict preset (existing tests/ dir)"
d="$WORK/run/python-cli"
assert_ok scaffold python-cli tdd-strict "tdd-loop,test-scenarios,setup-git-hooks"
assert_file "$d/AGENTS.md" "python-cli: AGENTS.md written"
assert_grep "Coverage is a required gate" "$d/AGENTS.md" "python-cli: tdd-strict coverage language present"
if runner="$(cd "$d" && bash "$DETECT_RUNNER" 2>/dev/null)"; then
  assert_eq "$runner" "pytest" "python-cli: tdd-loop detects pytest"
else
  gap python-cli "tdd-loop could not detect a runner even though tests/ + pyproject.toml exist."
fi
if [ ! -d "$d/.github/workflows" ]; then
  gap python-cli "tdd-strict preset says coverage is 'enforced in CI' and 'a required gate', but the scaffold doesn't create any CI config — a repo with no workflows yet gets a charter promising an enforcement mechanism that doesn't exist."
fi

# --- monorepo: multiple independently-versioned packages ---------------------
section "monorepo -> jod preset (multiple packages)"
d="$WORK/run/monorepo"
assert_ok scaffold monorepo jod "create-pr,setup-git-hooks"
assert_file "$d/AGENTS.md" "monorepo: AGENTS.md written"
pkg_count="$(find "$d/packages" -mindepth 2 -maxdepth 2 -name package.json 2>/dev/null | wc -l | tr -d ' ')"
ok "[ '$pkg_count' -ge 2 ]" "monorepo: fixture has multiple sub-packages ($pkg_count)"
if ! grep -qiE 'packages/|monorepo|sub-?package' "$d/AGENTS.md"; then
  gap monorepo "repo has $pkg_count independently-versioned packages under packages/, but no preset has any concept of per-package scope (ticket prefix, branch prefix, test runner can all differ per package) — the charter is written as if the whole repo is one unit."
fi

# --- oss-project: existing CONTRIBUTING.md / CODE_OF_CONDUCT.md / PR template
section "oss-project -> team preset (existing OSS conventions)"
d="$WORK/run/oss-project"
assert_ok scaffold oss-project team "create-pr,setup-git-hooks"
assert_file "$d/AGENTS.md" "oss-project: AGENTS.md written"
assert_grep "Conventional Commits" "$d/AGENTS.md" "oss-project: team preset agrees with CONTRIBUTING.md's commit convention"
if ! grep -qi "code of conduct" "$d/AGENTS.md"; then
  gap oss-project "repo ships a CODE_OF_CONDUCT.md, but no preset mentions it — an agent reading only AGENTS.md/CLAUDE.md would never learn it exists."
fi

# --- existing-charter: repo already has a hand-written AGENTS.md/CLAUDE.md --
section "existing-charter -> overwrite guard, then --force"
d="$WORK/run/existing-charter"
assert_fails scaffold existing-charter jod "create-pr"
assert_grep "ACME-CUSTOM-MARKER" "$d/AGENTS.md" "existing-charter: refuses without --force, original preserved"
assert_ok scaffold existing-charter jod "create-pr" --force
assert_no_grep "ACME-CUSTOM-MARKER" "$d/AGENTS.md" "existing-charter: --force overwrote the custom charter"
gap existing-charter "--force fully replaces a hand-written charter with no way to keep/merge prior custom sections (e.g. this fixture's 'Legacy custom section') — adopting Jod on a repo with existing conventions is all-or-nothing."

# --- docs-only: documentation site, no source code or tests -----------------
section "docs-only -> minimal preset (no source or tests)"
d="$WORK/run/docs-only"
assert_ok scaffold docs-only minimal ""
assert_file "$d/AGENTS.md" "docs-only: AGENTS.md written"
assert_no_grep "{{" "$d/AGENTS.md" "docs-only: no leftover placeholders"
gap docs-only "no preset has any docs-repo-specific guidance (prose/style conventions, link-checking, doc build validation) — a docs-only repo gets the same generic charter as a code repo."

# --- empty: baseline control, nothing pre-existing ---------------------------
section "empty -> minimal preset (control case)"
d="$WORK/run/empty"
assert_ok scaffold empty minimal ""
assert_file "$d/AGENTS.md" "empty: AGENTS.md written"
assert_link_to "$d/CLAUDE.md" "AGENTS.md" "empty: CLAUDE.md -> AGENTS.md"

# --- summary ------------------------------------------------------------------
gap_count="$(grep -c '^- ' "$REPORT" 2>/dev/null || true)"
echo
echo "gap report ($gap_count found): $REPORT"

assert_summary
exit
