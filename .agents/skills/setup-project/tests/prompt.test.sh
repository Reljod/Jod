#!/usr/bin/env bash
#
# prompt.test.sh — deterministic tests for the interactive pickers.
# Enumerated against test-scenarios/references/scenario-checklist.md and built
# on its assert.sh helper.
# Run: .agents/skills/setup-project/tests/prompt.test.sh
#
# An arrow-key UI looks untestable, which is exactly why prompt.sh reads keys
# from $JOD_PROMPT_IN instead of stdin: a file of keystrokes drives it the
# same way a terminal does, with the UI diverted to $JOD_PROMPT_OUT, so every
# case below is a real end-to-end run of the picker — no mocks.
#
set -u

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
LIB="$TEST_DIR/../scripts/lib/prompt.sh"
# shellcheck source=/dev/null
source "$TEST_DIR/../../test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
UI="$WORK/ui.log"

# Key names, so the cases below read as keystrokes rather than escape codes.
UP=$'\033[A'; DOWN=$'\033[B'; SPACE=' '; ENTER=$'\n'

N=0
# drive <keystrokes> <prompt-fn> [args...] — run one picker in its own shell
# (fd 3 is opened per process) and echo whatever it wrote to stdout.
drive() {
  local keys="$1"; shift
  N=$((N + 1))
  local kf="$WORK/keys.$N"
  printf '%s' "$keys" > "$kf"
  JOD_PROMPT_IN="$kf" JOD_PROMPT_OUT="$UI" bash -c '
    source "$1"; shift; prompt_begin; "$@"
  ' _ "$LIB" "$@"
}

OPTS=(jod minimal team tdd-strict)
DOPTS=("jod|the default" "minimal|lean" "team|OSS" "tdd-strict|test-first")

echo "== prompt.sh test suite =="

# --- 1. single select: happy paths -------------------------------------------
section "prompt_select_one"
assert_eq "$(drive "$ENTER" prompt_select_one T jod "${OPTS[@]}")" \
  "jod" "enter takes the highlighted default"
assert_eq "$(drive "$ENTER" prompt_select_one T team "${OPTS[@]}")" \
  "team" "the cursor starts on the named default, wherever it is"
assert_eq "$(drive "$DOWN$ENTER" prompt_select_one T jod "${OPTS[@]}")" \
  "minimal" "↓ moves one down"
assert_eq "$(drive "$DOWN$DOWN$DOWN$ENTER" prompt_select_one T jod "${OPTS[@]}")" \
  "tdd-strict" "↓↓↓ reaches the last option"
assert_eq "$(drive "$UP$ENTER" prompt_select_one T jod "${OPTS[@]}")" \
  "tdd-strict" "↑ from the first option wraps to the last"
assert_eq "$(drive "$DOWN$UP$ENTER" prompt_select_one T jod "${OPTS[@]}")" \
  "jod" "↓ then ↑ returns to where it started"
assert_eq "$(drive "$DOWN$DOWN$DOWN$DOWN$ENTER" prompt_select_one T jod "${OPTS[@]}")" \
  "jod" "↓ past the last option wraps to the first"
assert_eq "$(drive "j$ENTER" prompt_select_one T jod "${OPTS[@]}")" \
  "minimal" "j is ↓"
assert_eq "$(drive "jk$ENTER" prompt_select_one T jod "${OPTS[@]}")" \
  "jod" "k is ↑"

section "prompt_select_one: boundaries"
assert_eq "$(drive "$ENTER" prompt_select_one T nosuch "${OPTS[@]}")" \
  "jod" "an unknown default falls back to the first option"
assert_eq "$(drive "$ENTER" prompt_select_one T only only)" \
  "only" "a single-option list still resolves"
assert_eq "$(drive "$ENTER" prompt_select_one T jod "${DOPTS[@]}")" \
  "jod" "the value|description form returns only the value"
assert_fails drive "$ENTER" prompt_select_one T jod   # no options at all

section "prompt_select_one: cancel"
out="$(drive "q" prompt_select_one T jod "${OPTS[@]}")"; rc=$?
assert_eq "$rc" "130" "q returns 130"
assert_eq "$out" ""   "q prints nothing"
out="$(drive "" prompt_select_one T jod "${OPTS[@]}")"; rc=$?
assert_eq "$rc" "130" "EOF (no keys at all) returns 130 instead of looping"

# --- 2. multi select ----------------------------------------------------------
section "prompt_select_many"
many() { drive "$1" prompt_select_many T "$2" create-pr setup-git-hooks tdd-loop; }
assert_eq "$(many "$ENTER" "")" "" \
  "nothing preselected, enter -> nothing chosen"
assert_eq "$(many "$SPACE$ENTER" "")" "create-pr" \
  "space toggles the highlighted option on"
assert_eq "$(many "$DOWN$SPACE$ENTER" "")" "setup-git-hooks" \
  "space applies to the cursor, not the first row"
assert_eq "$(many "$SPACE$DOWN$SPACE$ENTER" "")" "$(printf 'create-pr\nsetup-git-hooks')" \
  "two toggles -> two lines out, in list order"
assert_eq "$(many "$ENTER" "tdd-loop")" "tdd-loop" \
  "preselection is honoured without touching a key"
assert_eq "$(many "$ENTER" "create-pr,tdd-loop")" "$(printf 'create-pr\ntdd-loop')" \
  "a comma-separated preselection marks each one"
assert_eq "$(many "$SPACE$ENTER" "create-pr")" "" \
  "space on a preselected option toggles it back off"
assert_eq "$(many "a$ENTER" "")" "$(printf 'create-pr\nsetup-git-hooks\ntdd-loop')" \
  "a selects all"
assert_eq "$(many "n$ENTER" "create-pr,setup-git-hooks,tdd-loop")" "" \
  "n clears everything"
assert_eq "$(many "an$SPACE$ENTER" "")" "create-pr" \
  "all, none, then one toggle"
assert_eq "$(many "$ENTER" "nosuch")" "" \
  "an unknown name in the preselection marks nothing"
assert_eq "$(many "$ENTER" "create-pr,")" "create-pr" \
  "a trailing comma in the preselection is harmless"

out="$(many "q" "")"; rc=$?
assert_eq "$rc" "130" "q cancels the multi-select"
assert_eq "$out" ""   "a cancelled multi-select prints nothing"

# --- 3. free text -------------------------------------------------------------
section "prompt_text"
assert_eq "$(drive "My Widget$ENTER" prompt_text "Name" "fallback")" \
  "My Widget" "typed text wins"
assert_eq "$(drive "$ENTER" prompt_text "Name" "fallback")" \
  "fallback" "empty input takes the default"
assert_eq "$(drive "$ENTER" prompt_text "Name")" \
  "" "no default, no input -> empty"
assert_eq "$(drive "Acme & Co <x>|y$ENTER" prompt_text "Name")" \
  'Acme & Co <x>|y' "special characters survive verbatim"
assert_eq "$(drive "  padded  $ENTER" prompt_text "Name")" \
  "  padded  " "surrounding whitespace is preserved, not trimmed"

# --- 4. confirm ---------------------------------------------------------------
section "prompt_confirm"
assert_ok    drive "y" prompt_confirm "Go?" y
assert_fails drive "n" prompt_confirm "Go?" y
assert_ok    drive "Y" prompt_confirm "Go?" y      # case-insensitive
assert_fails drive "N" prompt_confirm "Go?" y
assert_ok    drive "$ENTER" prompt_confirm "Go?" y   # enter takes default yes
assert_fails drive "$ENTER" prompt_confirm "Go?" n   # ...and default no
assert_ok    drive "y" prompt_confirm "Go?" n       # explicit y beats default n

# --- 5. environment: is there a terminal at all? ------------------------------
section "prompt_have_tty"
ok "JOD_PROMPT_IN='$WORK/keys.1' JOD_PROMPT_OUT='$UI' bash -c 'source \"$LIB\"; prompt_have_tty'" \
  "true when both endpoints are usable"
ok "! JOD_PROMPT_IN='$WORK/nope/missing' JOD_PROMPT_OUT='$UI' bash -c 'source \"$LIB\"; prompt_have_tty'" \
  "false when the key source cannot be opened"
ok "! JOD_PROMPT_IN='$WORK/keys.1' JOD_PROMPT_OUT='$WORK/nope/missing' bash -c 'source \"$LIB\"; prompt_have_tty'" \
  "false when the UI sink cannot be written"

# --- 6. output contract: the UI never lands on stdout -------------------------
section "output contract"
assert_grep "the default" "$UI"  "descriptions are rendered in the UI"
assert_grep "[x]" "$UI"          "checked boxes are rendered"
assert_eq "$(drive "$ENTER" prompt_select_one Title jod "${DOPTS[@]}")" "jod" \
  "stdout carries the value alone — no title, no rows"

# A single picker used on its own (no prompt_begin) still works: it opens the
# stream itself. Only multi-prompt flows need the caller to begin/end.
section "standalone use without prompt_begin"
printf '%s' "$DOWN$ENTER" > "$WORK/solo"
assert_eq "$(JOD_PROMPT_IN="$WORK/solo" JOD_PROMPT_OUT="$UI" bash -c \
  'source "$1"; prompt_select_one T jod jod minimal team' _ "$LIB")" \
  "minimal" "a lone picker opens its own stream"

assert_summary
