#!/usr/bin/env bash
#
# install.test.sh — deterministic, network-free tests for install.sh and
# bin/jod. Builds a throwaway file:// "remote" from the real bin/ and a
# couple of .agents/skills/ so the installer is exercised end to end without
# touching github.com or the developer's real $HOME.
# Run: tests/install.test.sh
set -u

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$REPO_ROOT/.agents/skills/test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "== install.sh / bin/jod test suite =="

# --- fixture: a minimal, file:// "Jod" remote -------------------------------
# Seeded from the real bin/jod + two real skills, so we exercise actual
# behavior rather than a hand-written stand-in.
SEED="$WORK/seed"
REMOTE="$WORK/remote.git"
mkdir -p "$SEED/bin" "$SEED/.agents/skills" "$SEED/.claude/commands"
cp "$REPO_ROOT/bin/jod" "$SEED/bin/jod"
chmod +x "$SEED/bin/jod"
cp -R "$REPO_ROOT/.agents/skills/setup-project" "$SEED/.agents/skills/"
cp -R "$REPO_ROOT/.agents/skills/setup-git-hooks" "$SEED/.agents/skills/"

git init --quiet -b main "$SEED"
git -C "$SEED" -c user.name=test -c user.email=test@example.com \
  add -A
git -C "$SEED" -c user.name=test -c user.email=test@example.com \
  commit --quiet -m "seed"
git init --quiet --bare "$REMOTE"
git -C "$SEED" remote add origin "$REMOTE"
git -C "$SEED" push --quiet origin main

export JOD_REPO_URL="file://$REMOTE"
export JOD_REF="main"
export JOD_HOME="$WORK/home/.jod"
export JOD_BIN_DIR="$WORK/home/bin"

# --- 1. fresh install --------------------------------------------------------
section "fresh install"
assert_ok "$REPO_ROOT/install.sh"
assert_dir "$JOD_HOME/.git" "clones the toolkit into \$JOD_HOME"
assert_symlink "$JOD_BIN_DIR/jod" "links jod onto \$JOD_BIN_DIR"
ok '[ -x "$JOD_HOME/bin/jod" ]' "installed jod is executable"

# --- 2. jod CLI dispatch -----------------------------------------------------
section "jod CLI"
assert_eq "$("$JOD_BIN_DIR/jod" home)" "$JOD_HOME" "'jod home' prints \$JOD_HOME"
assert_ok "$JOD_BIN_DIR/jod" setup-project --list
LIST="$("$JOD_BIN_DIR/jod" setup-project --list 2>&1)"
ok "grep -q 'jod' <<<\"\$LIST\"" "'jod setup-project --list' reaches the real script"
assert_fails "$JOD_BIN_DIR/jod" not-a-real-command
assert_ok "$JOD_BIN_DIR/jod" help

# --- 3. idempotent re-run picks up upstream changes -------------------------
section "re-run fast-forwards an existing checkout"
echo "marker" > "$SEED/UPDATED.txt"
git -C "$SEED" -c user.name=test -c user.email=test@example.com add UPDATED.txt
git -C "$SEED" -c user.name=test -c user.email=test@example.com \
  commit --quiet -m "add marker"
git -C "$SEED" push --quiet origin main
assert_ok "$REPO_ROOT/install.sh"
assert_file "$JOD_HOME/UPDATED.txt" "re-run fast-forwards to the new commit"

# --- 4. refuses to clobber a non-git \$JOD_HOME -----------------------------
section "\$JOD_HOME exists and isn't a git checkout"
export JOD_HOME="$WORK/not-git"
mkdir -p "$JOD_HOME"
assert_fails "$REPO_ROOT/install.sh"

assert_summary
exit
