#!/usr/bin/env bash
#
# prompt.sh — dependency-free interactive pickers: ↑/↓ to move, space to
# toggle, enter to confirm. Sourceable; no curses, no external binaries, no
# bashisms newer than 3.2 (stock macOS).
#
# The UI is drawn to $JOD_PROMPT_OUT and keys are read from $JOD_PROMPT_IN,
# both defaulting to /dev/tty — never stdin/stdout. That buys two things:
# a caller can capture a picker's *result* on stdout while its UI still goes
# to the terminal, and a test can drive a whole wizard from a file of
# keystrokes with the UI captured to a log.
#
#   source lib/prompt.sh
#   prompt_have_tty || exit 1
#   prompt_begin; trap prompt_end EXIT INT TERM
#   preset="$(prompt_select_one  "Preset" "jod" "jod|the default" "minimal|lean")"
#   skills="$(prompt_select_many "Skills" "create-pr" "create-pr|PR bodies" ...)"
#   prompt_end
#
# prompt_begin is not optional for a multi-prompt flow: each picker is read
# through a command substitution, i.e. a subshell, so the input has to be
# opened *once in the calling shell* for the reads to share one position in
# the stream. Opened per picker, a file-backed $JOD_PROMPT_IN would rewind
# and replay its first keystrokes into every prompt.
#
# Options are "value|description" (the description is optional). Results go
# to stdout: one line for select_one, one line per choice for select_many.
# A cancel (q) returns 130 and prints nothing.

PROMPT_IN="${JOD_PROMPT_IN:-/dev/tty}"
PROMPT_OUT="${JOD_PROMPT_OUT:-/dev/tty}"

# prompt_have_tty — can we actually run a picker? False under `curl | bash`,
# in CI, or with stdin closed, so callers can fall back to flags.
prompt_have_tty() {
  { : < "$PROMPT_IN"; } 2>/dev/null || return 1
  { : >> "$PROMPT_OUT"; } 2>/dev/null || return 1
  return 0
}

# --- frame plumbing ---------------------------------------------------------
# fd 3 = keys in, fd 4 = UI out. prompt_begin opens them in the *calling*
# shell so every picker — each of which runs in a command-substitution
# subshell — inherits the same descriptors and shares one read position.
prompt_begin() {
  if [ "${_P_FDS:-0}" -eq 1 ]; then return 0; fi
  exec 3< "$PROMPT_IN" 4>> "$PROMPT_OUT" || return 1
  _P_FDS=1
  if [ -t 4 ] && [ -z "${NO_COLOR:-}" ]; then
    _P_TTY=1
    _P_DIM=$'\033[2m'; _P_CYAN=$'\033[36m'; _P_GREEN=$'\033[32m'
    _P_BOLD=$'\033[1m'; _P_RST=$'\033[0m'
  else
    _P_TTY=0
    _P_DIM=''; _P_CYAN=''; _P_GREEN=''; _P_BOLD=''; _P_RST=''
  fi
  _P_DRAWN=0
}

# Every picker starts here: reuse the caller's descriptors when they exist,
# otherwise open our own so a single standalone prompt still works.
_p_open() {
  if [ "${_P_FDS:-0}" -eq 1 ]; then _P_DRAWN=0; return 0; fi
  prompt_begin
}

# prompt_end — release the terminal. Idempotent, and safe to trap on EXIT so
# a Ctrl-C mid-picker can't leave the cursor hidden.
prompt_end() {
  if [ "${_P_FDS:-0}" -eq 1 ]; then
    if [ "${_P_TTY:-0}" -eq 1 ]; then printf '\033[?25h' >&4; fi
    exec 3<&- 4>&-
    _P_FDS=0
  fi
  return 0
}

_p_hide_cursor() { if [ "${_P_TTY:-0}" -eq 1 ]; then printf '\033[?25l' >&4; fi; }
_p_show_cursor() { if [ "${_P_TTY:-0}" -eq 1 ]; then printf '\033[?25h' >&4; fi; }

# _p_frame_start — rewind over the previous frame so a redraw replaces it
# instead of scrolling. On a non-tty sink (tests) frames just stack up.
_p_frame_start() {
  if [ "${_P_TTY:-0}" -eq 1 ] && [ "$_P_DRAWN" -gt 0 ]; then
    printf '\033[%dA\033[J' "$_P_DRAWN" >&4
  fi
  _P_DRAWN=0
}
_p_line() { printf '%s\n' "$*" >&4; _P_DRAWN=$((_P_DRAWN + 1)); }

# _p_key — read one keypress, echo a symbolic name for it.
# Arrow keys arrive as ESC [ A/B; bash 3.2 can't do a sub-second read
# timeout, so on that vintage a bare ESC swallows the next two keys rather
# than being reported. ESC isn't a binding here (q cancels), so that costs
# nothing but a redraw.
_p_key() {
  local k rest
  IFS= read -rsn1 k <&3 || { printf 'eof'; return 0; }
  if [ -z "$k" ]; then printf 'enter'; return 0; fi
  case "$k" in
    $'\033')
      if [ "${BASH_VERSINFO[0]}" -ge 4 ]; then
        IFS= read -rsn2 -t 1 rest <&3 || rest=''
      else
        IFS= read -rsn2 rest <&3 || rest=''
      fi
      case "$rest" in
        '[A') printf 'up' ;;
        '[B') printf 'down' ;;
        *)    printf 'other' ;;
      esac
      ;;
    ' ')  printf 'space' ;;
    j|J)  printf 'down' ;;
    k|K)  printf 'up' ;;
    a|A)  printf 'all' ;;
    n|N)  printf 'none' ;;
    q|Q)  printf 'quit' ;;
    *)    printf 'other' ;;
  esac
}

# Split a "value|description" option.
_p_val()  { printf '%s' "${1%%|*}"; }
_p_desc() { case "$1" in *'|'*) printf '%s' "${1#*|}" ;; *) printf '' ;; esac; }

# Pad a value so descriptions line up in a column.
_p_pad() {
  local s="$1" w="$2" i
  printf '%s' "$s"
  i=${#s}
  while [ "$i" -lt "$w" ]; do printf ' '; i=$((i + 1)); done
}
_p_widest() {
  local w=0 o v
  for o in "$@"; do
    v="$(_p_val "$o")"
    if [ "${#v}" -gt "$w" ]; then w=${#v}; fi
  done
  printf '%s' "$w"
}

# --- prompt_select_one <title> <default-value> <opt>... ---------------------
# Single choice. Prints the chosen value on stdout.
prompt_select_one() {
  local title="$1" default="$2"; shift 2
  local opts=("$@") n=$# cur=0 i w v d row
  if [ "$n" -eq 0 ]; then return 1; fi
  for (( i = 0; i < n; i++ )); do
    if [ "$(_p_val "${opts[$i]}")" = "$default" ]; then cur=$i; fi
  done
  w="$(_p_widest "$@")"

  _p_open || return 1
  _p_hide_cursor
  while :; do
    _p_frame_start
    _p_line "${_P_BOLD}${title}${_P_RST}  ${_P_DIM}↑/↓ move · enter select · q cancel${_P_RST}"
    for (( i = 0; i < n; i++ )); do
      v="$(_p_val "${opts[$i]}")"; d="$(_p_desc "${opts[$i]}")"
      row="$(_p_pad "$v" "$w")"
      if [ "$i" -eq "$cur" ]; then
        _p_line "  ${_P_CYAN}❯ ${row}${_P_RST}  ${_P_DIM}${d}${_P_RST}"
      else
        _p_line "    ${row}  ${_P_DIM}${d}${_P_RST}"
      fi
    done
    case "$(_p_key)" in
      up)       cur=$(( (cur - 1 + n) % n )) ;;
      down)     cur=$(( (cur + 1) % n )) ;;
      enter)    break ;;
      quit|eof) _p_show_cursor; return 130 ;;
    esac
  done
  _p_show_cursor
  _p_val "${opts[$cur]}"; printf '\n'
}

# --- prompt_select_many <title> <preselected-csv> <opt>... ------------------
# Multi choice. Prints one chosen value per line (nothing if none picked).
prompt_select_many() {
  local title="$1" preselected="$2"; shift 2
  local opts=("$@") n=$# cur=0 i w v d row box
  if [ "$n" -eq 0 ]; then return 1; fi
  local marks=()
  for (( i = 0; i < n; i++ )); do
    v="$(_p_val "${opts[$i]}")"
    case ",$preselected," in
      *",$v,"*) marks[$i]=1 ;;
      *)        marks[$i]=0 ;;
    esac
  done
  w="$(_p_widest "$@")"

  _p_open || return 1
  _p_hide_cursor
  while :; do
    _p_frame_start
    _p_line "${_P_BOLD}${title}${_P_RST}  ${_P_DIM}↑/↓ move · space toggle · a all · n none · enter confirm${_P_RST}"
    for (( i = 0; i < n; i++ )); do
      v="$(_p_val "${opts[$i]}")"; d="$(_p_desc "${opts[$i]}")"
      row="$(_p_pad "$v" "$w")"
      if [ "${marks[$i]}" -eq 1 ]; then box="${_P_GREEN}[x]${_P_RST}"; else box="[ ]"; fi
      if [ "$i" -eq "$cur" ]; then
        _p_line "  ${_P_CYAN}❯${_P_RST} ${box} ${_P_CYAN}${row}${_P_RST}  ${_P_DIM}${d}${_P_RST}"
      else
        _p_line "    ${box} ${row}  ${_P_DIM}${d}${_P_RST}"
      fi
    done
    case "$(_p_key)" in
      up)       cur=$(( (cur - 1 + n) % n )) ;;
      down)     cur=$(( (cur + 1) % n )) ;;
      space)    if [ "${marks[$cur]}" -eq 1 ]; then marks[$cur]=0; else marks[$cur]=1; fi ;;
      all)      for (( i = 0; i < n; i++ )); do marks[$i]=1; done ;;
      none)     for (( i = 0; i < n; i++ )); do marks[$i]=0; done ;;
      enter)    break ;;
      quit|eof) _p_show_cursor; return 130 ;;
    esac
  done
  _p_show_cursor
  for (( i = 0; i < n; i++ )); do
    if [ "${marks[$i]}" -eq 1 ]; then _p_val "${opts[$i]}"; printf '\n'; fi
  done
  return 0
}

# --- prompt_text <label> [default] ------------------------------------------
# A free-text line. Empty input takes the default. Prints the value.
prompt_text() {
  local label="$1" default="${2:-}" line=""
  _p_open || return 1
  _p_show_cursor   # the terminal's own echo is what makes typing visible
  if [ -n "$default" ]; then
    printf '%s%s%s %s(%s)%s: ' "$_P_BOLD" "$label" "$_P_RST" "$_P_DIM" "$default" "$_P_RST" >&4
  else
    printf '%s%s%s %s(optional)%s: ' "$_P_BOLD" "$label" "$_P_RST" "$_P_DIM" "$_P_RST" >&4
  fi
  IFS= read -r line <&3 || line=""
  if [ "${_P_TTY:-0}" -eq 0 ]; then printf '%s\n' "$line" >&4; fi
  printf '%s\n' "${line:-$default}"
}

# --- prompt_confirm <question> [y|n] ----------------------------------------
# Single-key yes/no. Returns 0 for yes, 1 for no. Enter takes the default.
prompt_confirm() {
  local q="$1" default="${2:-y}" hint key
  if [ "$default" = "y" ]; then hint="[Y/n]"; else hint="[y/N]"; fi
  _p_open || return 1
  _p_show_cursor
  printf '%s%s%s %s%s%s ' "$_P_BOLD" "$q" "$_P_RST" "$_P_DIM" "$hint" "$_P_RST" >&4
  IFS= read -rsn1 key <&3 || key=''
  printf '%s\n' "$key" >&4
  case "$key" in
    y|Y) return 0 ;;
    n|N) return 1 ;;
    *)   [ "$default" = "y" ] ;;
  esac
}
