#!/usr/bin/env bash
#
# test.sh — deterministic, dependency-free tests for setup-project.sh.
# Scaffolds into throwaway temp dirs and asserts the outcome. No network,
# no test framework. Run: .agents/skills/setup-project/tests/test.sh
#
set -u

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$TEST_DIR/../scripts/setup-project.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASSED=0; FAILED=0
pass() { printf '  \033[32mPASS\033[0m %s\n' "$1"; PASSED=$((PASSED+1)); }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; FAILED=$((FAILED+1)); }
ok()   { if eval "$1"; then pass "$2"; else fail "$2"; fi; }

fresh() { local d="$WORK/$1"; rm -rf "$d"; mkdir -p "$d"; printf '%s' "$d"; }

echo "== setup-project.sh test suite =="

# --- 1. --list ---------------------------------------------------------------
echo "-- --list"
LIST="$("$SCRIPT" --list 2>&1)"
for p in jod minimal team tdd-strict; do
  ok "grep -q '$p' <<<\"\$LIST\"" "--list shows preset '$p'"
done
for s in create-pr setup-git-hooks tdd-loop; do
  ok "grep -q '$s' <<<\"\$LIST\"" "--list shows skill '$s'"
done
# self-exclusion: setup-project must not appear in the *skills* section
SKILLS_SECTION="$(sed -n '/Skills available/,$p' <<<"$LIST")"
ok "! grep -q 'setup-project' <<<\"\$SKILLS_SECTION\"" "--list excludes setup-project from skills"

# --- 2. every preset renders cleanly ----------------------------------------
echo "-- preset rendering (all four)"
for preset in jod minimal team tdd-strict; do
  d="$(fresh "p-$preset")"
  "$SCRIPT" --preset "$preset" --name "Proj-$preset" \
    --desc "Desc for $preset." --ticket TKT --branch bot \
    --target "$d" >/dev/null 2>&1
  ok "[ -f '$d/AGENTS.md' ]"                        "$preset: AGENTS.md written"
  ok "! grep -q '{{' '$d/AGENTS.md'"               "$preset: no leftover placeholders"
  ok "grep -q 'Proj-$preset' '$d/AGENTS.md'"       "$preset: PROJECT_NAME substituted"
  ok "grep -q 'Desc for $preset.' '$d/AGENTS.md'"  "$preset: PROJECT_DESC substituted"
  ok "[ -L '$d/CLAUDE.md' ]"                        "$preset: CLAUDE.md is a symlink"
  ok "[ \"\$(readlink '$d/CLAUDE.md')\" = 'AGENTS.md' ]" "$preset: symlink -> AGENTS.md"
done
# presets that carry ticket/branch tokens actually substitute them
ok "grep -q 'TKT-' '$WORK/p-jod/AGENTS.md'"        "jod: TICKET_PREFIX substituted"
ok "grep -q 'bot/' '$WORK/p-jod/AGENTS.md'"        "jod: BRANCH_PREFIX substituted"

# --- 3. --skills all copies everything, excludes self ------------------------
echo "-- --skills all"
d="$(fresh skills-all)"
"$SCRIPT" --preset team --skills all --name X --target "$d" >/dev/null 2>&1
for s in create-pr setup-git-hooks tdd-loop; do
  ok "[ -d '$d/.agents/skills/$s' ]"       "all: skill '$s' copied"
  ok "[ -f '$d/.claude/commands/$s.md' ]"  "all: command '/$s' copied"
done
ok "[ ! -d '$d/.agents/skills/setup-project' ]" "all: setup-project NOT copied into target"

# --- 4. selective skills -----------------------------------------------------
echo "-- --skills create-pr,tdd-loop"
d="$(fresh skills-some)"
"$SCRIPT" --preset jod --skills create-pr,tdd-loop --name X --target "$d" >/dev/null 2>&1
ok "[ -d '$d/.agents/skills/create-pr' ]"        "some: create-pr copied"
ok "[ -d '$d/.agents/skills/tdd-loop' ]"         "some: tdd-loop copied"
ok "[ ! -d '$d/.agents/skills/setup-git-hooks' ]" "some: setup-git-hooks NOT copied"

# --- 5. --no-symlink ---------------------------------------------------------
echo "-- --no-symlink"
d="$(fresh nosym)"
"$SCRIPT" --preset minimal --no-symlink --name X --target "$d" >/dev/null 2>&1
ok "[ -f '$d/CLAUDE.md' ] && [ ! -L '$d/CLAUDE.md' ]" "no-symlink: CLAUDE.md is a regular file"
ok "diff -q '$d/AGENTS.md' '$d/CLAUDE.md' >/dev/null" "no-symlink: CLAUDE.md == AGENTS.md"

# --- 6. overwrite guard + --force -------------------------------------------
echo "-- overwrite guard"
d="$(fresh guard)"
"$SCRIPT" --preset jod --name First --target "$d" >/dev/null 2>&1
if "$SCRIPT" --preset jod --name Second --target "$d" >/dev/null 2>&1; then
  fail "guard: second run without --force should fail"
else
  pass "guard: second run without --force refused"
fi
ok "grep -q 'First' '$d/AGENTS.md'" "guard: original AGENTS.md untouched"
if "$SCRIPT" --preset jod --name Second --target "$d" --force >/dev/null 2>&1; then
  pass "guard: --force succeeds"
else
  fail "guard: --force should succeed"
fi
ok "grep -q 'Second' '$d/AGENTS.md'" "guard: --force overwrote AGENTS.md"

# --- 7. unknown preset errors -----------------------------------------------
echo "-- error handling"
d="$(fresh badpreset)"
if "$SCRIPT" --preset nope --target "$d" >/dev/null 2>&1; then
  fail "unknown preset should exit non-zero"
else
  pass "unknown preset exits non-zero"
fi
ok "[ ! -f '$d/AGENTS.md' ]" "unknown preset writes nothing"

# --- 8. unknown skill is skipped, scaffold still succeeds --------------------
d="$(fresh badskill)"
if "$SCRIPT" --preset jod --skills create-pr,doesnotexist --name X --target "$d" >/dev/null 2>&1; then
  pass "unknown skill: scaffold still succeeds"
else
  fail "unknown skill: scaffold should still succeed"
fi
ok "[ -d '$d/.agents/skills/create-pr' ]"          "unknown skill: valid skill still copied"
ok "[ ! -d '$d/.agents/skills/doesnotexist' ]"     "unknown skill: bogus skill not created"

# --- 9. defaults: target=cwd, name=basename ---------------------------------
echo "-- defaults"
d="$(fresh widgetco)"
( cd "$d" && "$SCRIPT" --preset minimal >/dev/null 2>&1 )
ok "[ -f '$d/AGENTS.md' ]"              "default target: scaffolds into cwd"
ok "grep -q 'widgetco' '$d/AGENTS.md'" "default name: falls back to dir basename"

# --- 10. --help --------------------------------------------------------------
if "$SCRIPT" --help 2>&1 | grep -q 'Usage:'; then
  pass "--help renders usage"
else
  fail "--help should render usage"
fi

# --- summary -----------------------------------------------------------------
echo
echo "== $PASSED passed, $FAILED failed =="
[ "$FAILED" -eq 0 ]
