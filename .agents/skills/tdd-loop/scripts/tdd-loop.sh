#!/usr/bin/env bash
# Run the focused test once, or watch and re-run on every change.
#
# Usage:
#   tdd-loop.sh [--watch] [-- <test command ...>]
#
#   (no command)   auto-detect the runner via detect-test-runner.sh
#   --watch        re-run on file changes (entr/fswatch/inotifywait/poll)
#
# Examples:
#   tdd-loop.sh -- pytest tests/test_sync.py::test_retry
#   tdd-loop.sh --watch -- npx vitest run sync.test.ts
#   tdd-loop.sh --watch                 # detected runner, whole suite
#
# One-shot mode exits with the test command's status, so it is safe to call
# from CI or a pre-push hook. Watch mode never exits on its own.
set -uo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

WATCH=0
CMD=()
while [ $# -gt 0 ]; do
  case "$1" in
    --watch) WATCH=1; shift ;;
    --) shift; CMD=("$@"); break ;;
    *) CMD+=("$1"); shift ;;
  esac
done

if [ "${#CMD[@]}" -eq 0 ]; then
  detected="$("$HERE/detect-test-runner.sh")" || exit 1
  # shellcheck disable=SC2206
  CMD=($detected)
  echo "• detected test command: ${CMD[*]}" >&2
fi

bold() { [ -t 1 ] && printf '\033[1m%s\033[0m' "$1" || printf '%s' "$1"; }
banner() { # $1 = GREEN|RED
  if [ "$1" = GREEN ]; then
    [ -t 1 ] && printf '\033[42;30m %s \033[0m\n' " GREEN — tests pass " || echo "[GREEN] tests pass"
  else
    [ -t 1 ] && printf '\033[41;37m %s \033[0m\n' " RED — tests fail " || echo "[RED] tests fail"
  fi
}

run_once() {
  echo; bold "→ ${CMD[*]}"; echo
  local start rc
  start=$(date +%s 2>/dev/null || echo 0)
  "${CMD[@]}"; rc=$?
  local end; end=$(date +%s 2>/dev/null || echo 0)
  echo
  [ "$rc" -eq 0 ] && banner GREEN || banner RED
  echo "  (exit $rc, $((end - start))s)"
  return "$rc"
}

if [ "$WATCH" -eq 0 ]; then
  run_once
  exit $?
fi

# --- watch mode -----------------------------------------------------------
echo "• watching for changes (Ctrl-C to stop)…" >&2
run_once || true

# Prefer a real file watcher; fall back to a git-tracked-file checksum poll.
if command -v entr >/dev/null 2>&1; then
  # entr restarts the command whenever a watched file changes.
  while true; do
    git ls-files --cached --others --exclude-standard 2>/dev/null | entr -d -c "$0" -- "${CMD[@]}" || true
  done
elif command -v fswatch >/dev/null 2>&1; then
  fswatch -o . | while read -r _; do run_once || true; done
elif command -v inotifywait >/dev/null 2>&1; then
  while inotifywait -q -r -e modify,create,delete --exclude '\.git/' . >/dev/null; do
    run_once || true
  done
else
  echo "• no file watcher found (entr/fswatch/inotifywait); polling every 2s" >&2
  fingerprint() {
    # GNU find prints mtimes; falls back to a name-only list elsewhere.
    find . -path ./.git -prune -o -type f -printf '%T@ %p\n' 2>/dev/null | sha1sum \
      || find . -path ./.git -prune -o -type f -print 2>/dev/null | sha1sum
  }
  prev="$(fingerprint)"
  while true; do
    sleep 2
    cur="$(fingerprint)"
    if [ "$cur" != "$prev" ]; then
      run_once || true
      prev="$cur"
    fi
  done
fi
