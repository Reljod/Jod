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

# The interactive cases drive the skill picker by cursor position, so they need
# to know which skill actually sits at a given row. Derive that the same way
# the script does rather than hard-coding names — otherwise adding a skill
# silently re-points every keystroke-driven assertion below.
SKILLS_SRC="$(cd -- "$TEST_DIR/../.." && pwd)"
skill_at() {  # skill_at <0-based row in the picker>
  find "$SKILLS_SRC" -maxdepth 1 -mindepth 1 -type d -exec basename {} \; \
    | grep -vx setup-project | sort | sed -n "$(($1 + 1))p"
}

echo "== setup-project.sh test suite =="

# --- 1. happy path: --list ---------------------------------------------------
section "--list"
LIST="$("$SCRIPT" --list 2>&1)"
for p in jod minimal team tdd-strict; do
  ok "grep -q '$p' <<<\"\$LIST\"" "--list shows preset '$p'"
done
for s in create-pr orchestrate setup-git-hooks tdd-loop test-scenarios write-spec; do
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
for preset in jod minimal team tdd-strict; do
  assert_no_grep "<!-- blurb" "$WORK/p-$preset/AGENTS.md" "$preset: picker blurb stripped from the charter"
done

# --- 2b. issue keys are OPT-IN, never scaffolded by default ------------------
section "ticket rules are opt-in (no --ticket)"
for preset in jod tdd-strict; do
  d="$(fresh "noticket-$preset")"
  "$SCRIPT" --preset "$preset" --name X --target "$d" >/dev/null 2>&1
  assert_no_grep "{{"        "$d/AGENTS.md" "$preset/no-ticket: no leftover placeholders"
  assert_no_grep "<TICKET>"  "$d/AGENTS.md" "$preset/no-ticket: commit format carries no <TICKET>"
  assert_no_grep "issue key" "$d/AGENTS.md" "$preset/no-ticket: no issue-key rule at all"
  assert_grep    "<type>: <subject>" "$d/AGENTS.md" "$preset/no-ticket: plain <type>: <subject>"
  # Dropping the token must not leave a hole in the prose around it.
  ok "! grep -qE '^[[:space:]]*-[[:space:]]*$' '$d/AGENTS.md'" "$preset/no-ticket: no orphaned list marker"
  ok "[ -z \"\$(sed -n '/^$/{N;/^\n$/p}' '$d/AGENTS.md')\" ]" "$preset/no-ticket: no double blank lines left behind"
done

section "ticket rules when --ticket IS given"
for preset in jod tdd-strict; do
  d="$(fresh "ticket-$preset")"
  "$SCRIPT" --preset "$preset" --name X --ticket ENG --target "$d" >/dev/null 2>&1
  assert_grep    "ENG-12" "$d/AGENTS.md" "$preset/ticket: the prefix is used in the example"
  assert_grep    "issue key" "$d/AGENTS.md" "$preset/ticket: the rule is stated"
  assert_no_grep "{{" "$d/AGENTS.md"      "$preset/ticket: no leftover placeholders"
done
# The lean preset has no commit convention to attach a ticket rule to.
d="$(fresh ticket-minimal)"
"$SCRIPT" --preset minimal --name X --ticket ENG --target "$d" >/dev/null 2>&1
assert_no_grep "ENG-12" "$d/AGENTS.md" "minimal/ticket: stays lean, no rule injected"

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
for s in create-pr orchestrate setup-git-hooks tdd-loop test-scenarios write-spec; do
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

# --- 9. interactive mode -----------------------------------------------------
# Driven exactly as a terminal would drive it: keystrokes in, UI out. See
# prompt.test.sh for the pickers themselves; these cases are about the wizard
# actually deciding what gets scaffolded.
section "interactive: a full run scaffolds what was picked"
UI="$WORK/wizard-ui.log"
wizard() {  # wizard <target> <keystrokes...>
  local d="$1"; shift
  # "$@", not "$*": joining the groups with a space would feed the picker a
  # stray space, which is the toggle key.
  printf '%s' "$@" > "$WORK/wizard-keys"
  JOD_PROMPT_IN="$WORK/wizard-keys" JOD_PROMPT_OUT="$UI" "$SCRIPT" --target "$d"
}
DOWN=$'\033[B'; ENTER=$'\n'
d="$(fresh wizard-full)"
wizard "$d" \
  "$DOWN$DOWN$ENTER" \
  "n$DOWN $ENTER" \
  "Widget Co$ENTER" \
  "Widgets, at last.$ENTER" \
  "bot$ENTER" \
  "$ENTER" \
  "y" >/dev/null 2>&1
assert_file    "$d/AGENTS.md"                       "wizard: charter written"
assert_grep    "Coverage is a required gate" "$d/AGENTS.md" "wizard: picked the 3rd preset (tdd-strict)"
assert_grep    "Widget Co" "$d/AGENTS.md"           "wizard: typed name used"
assert_grep    "Widgets, at last." "$d/AGENTS.md"   "wizard: typed description used"
assert_grep    "bot/" "$d/AGENTS.md"                "wizard: typed branch prefix used"
assert_no_grep "issue key" "$d/AGENTS.md"           "wizard: blank ticket answer -> no ticket rule"
# "n" deselects all, one DOWN moves to row 1, space toggles just that one.
assert_dir     "$d/.agents/skills/$(skill_at 1)"    "wizard: the toggled-on skill was copied"
assert_missing "$d/.agents/skills/$(skill_at 0)"    "wizard: untoggled skills were not"
assert_link_to "$d/CLAUDE.md" "AGENTS.md"           "wizard: CLAUDE.md -> AGENTS.md"

section "interactive: the ticket answer is honoured when given"
d="$(fresh wizard-ticket)"
wizard "$d" "$ENTER" "n$ENTER" "X$ENTER" "$ENTER" "$ENTER" "ENG$ENTER" "y" >/dev/null 2>&1
assert_grep "ENG-12" "$d/AGENTS.md"                 "wizard: typed issue-key prefix reaches the charter"

section "interactive: declining writes nothing"
d="$(fresh wizard-no)"
wizard "$d" "$ENTER" "n$ENTER" "X$ENTER" "$ENTER" "$ENTER" "$ENTER" "n" >/dev/null 2>&1
assert_missing "$d/AGENTS.md"                       "wizard: answering 'no' at the summary writes nothing"

d="$(fresh wizard-cancel)"
wizard "$d" "q" >/dev/null 2>&1
assert_missing "$d/AGENTS.md"                       "wizard: cancelling at the first picker writes nothing"

section "interactive: an existing charter is never clobbered silently"
d="$(fresh wizard-guard)"
"$SCRIPT" --preset jod --name First --target "$d" >/dev/null 2>&1
wizard "$d" "$ENTER" "n$ENTER" "Second$ENTER" "$ENTER" "$ENTER" "$ENTER" "n" >/dev/null 2>&1
assert_grep "First" "$d/AGENTS.md"                  "wizard: declining the overwrite keeps the original"
wizard "$d" "$ENTER" "n$ENTER" "Second$ENTER" "$ENTER" "$ENTER" "$ENTER" "yy" >/dev/null 2>&1
assert_grep "Second" "$d/AGENTS.md"                 "wizard: confirming the overwrite replaces it"

section "interactive: only choice-bearing flags turn the wizard off"
d="$(fresh wizard-off)"
# --preset answers a wizard question, so this must scaffold without prompting
# even though a perfectly good key source is available.
JOD_PROMPT_IN="$WORK/wizard-keys" JOD_PROMPT_OUT="$UI" \
  "$SCRIPT" --preset minimal --name Flagged --target "$d" >/dev/null 2>&1
assert_grep "Flagged" "$d/AGENTS.md"                "flags-only run scaffolds without prompting"

d="$(fresh wizard-explicit-off)"
JOD_PROMPT_IN="$WORK/wizard-keys" JOD_PROMPT_OUT="$UI" \
  "$SCRIPT" --no-interactive --target "$d" >/dev/null 2>&1
assert_file "$d/AGENTS.md"                          "--no-interactive scaffolds from defaults"

section "interactive: no terminal falls back to --list"
d="$(fresh wizard-notty)"
OUT="$(JOD_PROMPT_IN="$WORK/no/such/tty" JOD_PROMPT_OUT="$UI" "$SCRIPT" --target "$d" 2>&1)"
ok "grep -q 'Behavior presets' <<<\"\$OUT\""        "no tty: prints the list instead of hanging"
assert_missing "$d/AGENTS.md"                       "no tty: writes nothing"

# --- 10. output contract: --help ---------------------------------------------
section "output contract"
ok "\"$SCRIPT\" --help 2>&1 | grep -q 'Usage:'"       "--help renders usage"
ok "\"$SCRIPT\" --help 2>&1 | grep -q -- '--interactive'" "--help documents interactive mode"

assert_summary
