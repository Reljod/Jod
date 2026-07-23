#!/usr/bin/env bash
#
# test.sh — deterministic, dependency-free tests for setup-project.sh.
# Enumerated against test-scenarios/references/scenario-checklist.md and built
# on its assert.sh helper. Run: .agents/skills/setup-project/tests/test.sh
#
set -u

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$TEST_DIR/../scripts/setup-project.sh"
# shellcheck source=/dev/null
source "$TEST_DIR/../../test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
fresh() { local d="$WORK/$1"; rm -rf "$d"; mkdir -p "$d"; printf '%s' "$d"; }

echo "== setup-project.sh test suite =="

# --- 1. happy path: --list ---------------------------------------------------
section "--list"
LIST="$("$SCRIPT" --list 2>&1)"
for p in jod minimal team tdd-strict; do
  ok "grep -q '$p' <<<\"\$LIST\"" "--list shows preset '$p'"
done
for s in create-pr setup-git-hooks tdd-loop test-scenarios; do
  ok "grep -q '$s' <<<\"\$LIST\"" "--list shows skill '$s'"
done
SKILLS_SECTION="$(sed -n '/Skills available/,$p' <<<"$LIST")"
ok "! grep -q 'setup-project' <<<\"\$SKILLS_SECTION\"" "--list excludes setup-project from skills"
# A non-preset .md in the templates dir (README.md) must never be selectable.
PRESET_SECTION="$(sed -n '/Behavior presets/,/Skills available/p' <<<"$LIST")"
ok "! grep -qi 'README' <<<\"\$PRESET_SECTION\"" "--list excludes README from presets"

# --- 2. happy path: EVERY preset renders cleanly ----------------------------
section "preset rendering (all four variants)"
for preset in jod minimal team tdd-strict; do
  d="$(fresh "p-$preset")"
  "$SCRIPT" --preset "$preset" --name "Proj-$preset" \
    --desc "Desc for $preset." --ticket TKT --branch bot --target "$d" >/dev/null 2>&1
  assert_file    "$d/AGENTS.md"                       "$preset: AGENTS.md written"
  assert_no_grep "{{" "$d/AGENTS.md"                  "$preset: no leftover placeholders"
  assert_grep    "Proj-$preset" "$d/AGENTS.md"        "$preset: PROJECT_NAME substituted"
  assert_grep    "Desc for $preset." "$d/AGENTS.md"   "$preset: PROJECT_DESC substituted"
  assert_link_to "$d/CLAUDE.md" "AGENTS.md"           "$preset: CLAUDE.md -> AGENTS.md"
done
assert_grep "TKT-" "$WORK/p-jod/AGENTS.md"            "jod: TICKET_PREFIX substituted"
assert_grep "bot/" "$WORK/p-jod/AGENTS.md"            "jod: BRANCH_PREFIX substituted"

# --- 3. boundary: special characters in name/desc ---------------------------
section "special characters in --name / --desc"
d="$(fresh special)"
NAME='Acme & Co <widgets>'
DESC='Pipes | slashes / and $vars & "quotes".'
"$SCRIPT" --preset minimal --name "$NAME" --desc "$DESC" --target "$d" >/dev/null 2>&1
assert_grep "$NAME" "$d/AGENTS.md"                    "special: name substituted literally"
assert_grep "$DESC" "$d/AGENTS.md"                    "special: desc substituted literally"
assert_no_grep "{{" "$d/AGENTS.md"                    "special: no leftover placeholders"

# --- 4. --skills all / selective / whitespace -------------------------------
section "--skills all"
d="$(fresh skills-all)"
"$SCRIPT" --preset team --skills all --name X --target "$d" >/dev/null 2>&1
for s in create-pr setup-git-hooks tdd-loop test-scenarios; do
  assert_dir  "$d/.agents/skills/$s"      "all: skill '$s' copied"
  assert_file "$d/.claude/commands/$s.md" "all: command '/$s' copied"
done
assert_missing "$d/.agents/skills/setup-project" "all: setup-project NOT copied into target"

section "--skills selective, with whitespace in the list"
d="$(fresh skills-some)"
"$SCRIPT" --preset jod --skills "create-pr, tdd-loop" --name X --target "$d" >/dev/null 2>&1
assert_dir     "$d/.agents/skills/create-pr"       "some: create-pr copied"
assert_dir     "$d/.agents/skills/tdd-loop"        "some: tdd-loop copied (whitespace trimmed)"
assert_missing "$d/.agents/skills/setup-git-hooks" "some: setup-git-hooks NOT copied"

# --- 5. --no-symlink ---------------------------------------------------------
section "--no-symlink"
d="$(fresh nosym)"
"$SCRIPT" --preset minimal --no-symlink --name X --target "$d" >/dev/null 2>&1
ok "[ -f '$d/CLAUDE.md' ] && [ ! -L '$d/CLAUDE.md' ]" "no-symlink: CLAUDE.md is a regular file"
assert_ok diff -q "$d/AGENTS.md" "$d/CLAUDE.md"       # byte-identical to AGENTS.md

# --- 6. state: overwrite guard + --force ------------------------------------
section "overwrite guard"
d="$(fresh guard)"
"$SCRIPT" --preset jod --name First --target "$d" >/dev/null 2>&1
assert_fails "$SCRIPT" --preset jod --name Second --target "$d"   # refuses w/o --force
assert_grep  "First" "$d/AGENTS.md"                  "guard: original untouched after refusal"
assert_ok    "$SCRIPT" --preset jod --name Second --target "$d" --force
assert_grep  "Second" "$d/AGENTS.md"                 "guard: --force overwrote AGENTS.md"

# --- 7. invalid & hostile input ---------------------------------------------
section "invalid & hostile input"
d="$(fresh bad)"
assert_fails "$SCRIPT" --preset nope --target "$d"                # unknown preset
assert_missing "$d/AGENTS.md"                        "unknown preset: nothing written (no partial state)"

d="$(fresh traversal)"
assert_fails "$SCRIPT" --preset "../../etc/passwd" --target "$d"  # path traversal in preset
assert_missing "$d/AGENTS.md"                        "preset traversal: nothing written"

d="$(fresh readmepreset)"
assert_fails "$SCRIPT" --preset README --target "$d"             # README.md is not a preset
assert_missing "$d/AGENTS.md"                        "README preset: nothing written"

d="$(fresh badskill)"
"$SCRIPT" --preset jod --skills "create-pr,../../../evil" --name X --target "$d" >/dev/null 2>&1
assert_dir     "$d/.agents/skills/create-pr"         "hostile skill: valid skill still copied"
assert_missing "$d/.agents/skills/../../../evil"     "hostile skill: traversal not written outside target"

d="$(fresh missingdir)"; rmdir "$d"
assert_fails "$SCRIPT" --preset jod --target "$d"                 # nonexistent target dir

d="$(fresh unknownskill)"
assert_ok  "$SCRIPT" --preset jod --skills "create-pr,doesnotexist" --name X --target "$d"
assert_dir     "$d/.agents/skills/create-pr"         "unknown skill: valid one copied"
assert_missing "$d/.agents/skills/doesnotexist"      "unknown skill: bogus one not created"

# --- 8. environment: defaults ------------------------------------------------
section "defaults (cwd target, basename name, trailing slash)"
d="$(fresh widgetco)"
( cd "$d" && "$SCRIPT" --preset minimal >/dev/null 2>&1 )       # target defaults to cwd
assert_file "$d/AGENTS.md"                            "default target: scaffolds into cwd"
assert_grep "widgetco" "$d/AGENTS.md"                 "default name: falls back to dir basename"

d="$(fresh trailing)"
assert_ok "$SCRIPT" --preset minimal --name X --target "$d/"     # trailing slash on --target
assert_file "$d/AGENTS.md"                            "trailing slash: normalised, scaffolds"

# --- 9. output contract: --help ---------------------------------------------
section "output contract"
ok "\"$SCRIPT\" --help 2>&1 | grep -q 'Usage:'"       "--help renders usage"

assert_summary
