#!/usr/bin/env bash
# Shared environment for the Jod end-to-end scripts.
#
# Every script sources this. It puts the *release* binaries on PATH — `jod-run`
# has to be found there or the scheduler cannot spawn a harness — and points
# JOD_HOME at a scratch directory so the real ~/.jod is never touched.

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
export REPO

# The binaries are *staged* rather than used from `target/release` directly.
# Four other agents build in this checkout; one `cargo clean` mid-suite left
# half a run reporting "jod: command not found", and an agent's uncommitted
# work-in-progress broke the build outright. So: build a clean export of one
# commit (build.sh), copy the three binaries here, and test that. The commit
# under test is recorded in COMMIT.
BIN="${JOD_E2E_BIN:-/tmp/jod-e2e/bin}"
export PATH="$BIN:$PATH"

# Each area gets its own JOD_HOME so a failure in one cannot explain a result in
# another. Callers set AREA before sourcing.
: "${AREA:=scratch}"
export JOD_HOME="${JOD_E2E_HOME:-/tmp/jod-e2e}/$AREA"

OUT="$REPO/tests/e2e/jod/out"
export OUT
mkdir -p "$OUT" "$JOD_HOME"

# `run` echoes the command, then its combined output and exit status, into both
# the transcript file and the terminal. The report quotes these verbatim, so the
# format has to be stable.
run() {
  echo "\$ $*"
  set +e
  "$@" 2>&1
  local rc=$?
  set -e
  if [ "$rc" -ne 0 ]; then echo "[exit $rc]"; fi
  echo
  return 0
}

# Same, but for a shell pipeline given as one string.
runsh() {
  echo "\$ $1"
  set +e
  eval "$1" 2>&1
  local rc=$?
  set -e
  if [ "$rc" -ne 0 ]; then echo "[exit $rc]"; fi
  echo
  return 0
}

section() {
  echo "=============================================================="
  echo "== $*"
  echo "=============================================================="
}
