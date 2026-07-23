#!/usr/bin/env bash
#
# install.test.sh — deterministic, network-free tests for install.sh and
# bin/jod, including version resolution and the patch-only update cascade.
# Builds a throwaway file:// "remote" from the real bin/ and a couple of
# .agents/skills/ so the installer is exercised end to end without touching
# github.com or the developer's real $HOME.
# Run: tests/install.test.sh
set -u

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$REPO_ROOT/.agents/skills/test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "== install.sh / bin/jod test suite =="

# --- fixture: a minimal, file:// "Jod" remote -------------------------------
# Seeded from the real bin/ (jod + lib/semver.sh) + two real skills, so we
# exercise actual behavior rather than a hand-written stand-in.
SEED="$WORK/seed"
REMOTE="$WORK/remote.git"
mkdir -p "$SEED/bin/lib" "$SEED/.agents/skills" "$SEED/.claude/commands"
cp "$REPO_ROOT/bin/jod" "$SEED/bin/jod"
cp "$REPO_ROOT/bin/lib/semver.sh" "$SEED/bin/lib/semver.sh"
chmod +x "$SEED/bin/jod"
cp -R "$REPO_ROOT/.agents/skills/setup-project" "$SEED/.agents/skills/"
cp -R "$REPO_ROOT/.agents/skills/setup-git-hooks" "$SEED/.agents/skills/"

seed_commit() { git -C "$SEED" -c user.name=test -c user.email=test@example.com "$@"; }

git init --quiet -b main "$SEED"
seed_commit add -A
seed_commit commit --quiet -m "seed"
git init --quiet --bare "$REMOTE"
git -C "$SEED" remote add origin "$REMOTE"
git -C "$SEED" push --quiet origin main

tag_and_push() {
  git -C "$SEED" tag "$1"
  git -C "$SEED" push --quiet origin "$1"
}

export JOD_REPO_URL="file://$REMOTE"
export JOD_REF="main"
export JOD_HOME="$WORK/home/.jod"
export JOD_BIN_DIR="$WORK/home/bin"
unset JOD_VERSION

# --- 1. fresh install, no release tags yet ----------------------------------
section "fresh install (no release tags yet)"
assert_ok "$REPO_ROOT/install.sh"
assert_dir "$JOD_HOME/.git" "clones the toolkit into \$JOD_HOME"
assert_symlink "$JOD_BIN_DIR/jod" "links jod onto \$JOD_BIN_DIR"
ok '[ -x "$JOD_HOME/bin/jod" ]' "installed jod is executable"
assert_eq "$("$JOD_BIN_DIR/jod" version)" "main" "no tags: falls back to \$JOD_REF"

# --- 2. jod CLI dispatch -----------------------------------------------------
section "jod CLI"
assert_eq "$("$JOD_BIN_DIR/jod" home)" "$JOD_HOME" "'jod home' prints \$JOD_HOME"
assert_ok "$JOD_BIN_DIR/jod" setup-project --list
LIST="$("$JOD_BIN_DIR/jod" setup-project --list 2>&1)"
ok "grep -q 'jod' <<<\"\$LIST\"" "'jod setup-project --list' reaches the real script"
assert_fails "$JOD_BIN_DIR/jod" not-a-real-command
assert_ok "$JOD_BIN_DIR/jod" help

# --- 3. default install pins to the newest release tag ----------------------
section "default (latest) install picks the newest semver tag"
tag_and_push v1.0.0
tag_and_push v1.0.1
tag_and_push v1.1.0
assert_ok "$REPO_ROOT/install.sh"
assert_eq "$("$JOD_BIN_DIR/jod" version)" "v1.1.0" "latest resolves to the highest tag"

# --- 4. pinning an explicit / bare version -----------------------------------
section "pinning a version"
JOD_VERSION=v1.0.0 "$REPO_ROOT/install.sh" >/dev/null 2>&1
assert_eq "$("$JOD_BIN_DIR/jod" version)" "v1.0.0" "JOD_VERSION=v1.0.0 pins exactly"
JOD_VERSION=1.0.1 "$REPO_ROOT/install.sh" >/dev/null 2>&1
assert_eq "$("$JOD_BIN_DIR/jod" version)" "v1.0.1" "bare X.Y.Z is normalised to vX.Y.Z"
assert_fails env JOD_VERSION=v9.9.9 "$REPO_ROOT/install.sh"

# --- 5. `jod update` only ever takes a same-minor patch ----------------------
section "jod update: patch-only cascade"
JOD_VERSION=v1.0.0 "$REPO_ROOT/install.sh" >/dev/null 2>&1
UPDATE_OUT="$("$JOD_BIN_DIR/jod" update 2>&1)"
assert_eq "$("$JOD_BIN_DIR/jod" version)" "v1.0.1" "update takes the newer v1.0.x patch"
ok "grep -q 'v1.1.0' <<<\"\$UPDATE_OUT\"" "update mentions the newer v1.1.0 release without taking it"
UPDATE_OUT2="$("$JOD_BIN_DIR/jod" update 2>&1)"
assert_eq "$("$JOD_BIN_DIR/jod" version)" "v1.0.1" "second update is a no-op (already latest patch)"
ok "grep -qi 'already' <<<\"\$UPDATE_OUT2\"" "second update reports already-up-to-date"
tag_and_push v1.0.2
"$JOD_BIN_DIR/jod" update >/dev/null 2>&1
assert_eq "$("$JOD_BIN_DIR/jod" version)" "v1.0.2" "a newly published v1.0.2 patch is picked up"

# --- 6. branch installs still fast-forward via `jod update` -----------------
section "jod update on a branch install"
JOD_VERSION=main "$REPO_ROOT/install.sh" >/dev/null 2>&1
echo "marker" > "$SEED/UPDATED.txt"
seed_commit add UPDATED.txt
seed_commit commit --quiet -m "add marker"
git -C "$SEED" push --quiet origin main
assert_ok "$JOD_BIN_DIR/jod" update
assert_file "$JOD_HOME/UPDATED.txt" "branch install: update fast-forwards to the new commit"

# --- 7. refuses to clobber a non-git \$JOD_HOME -----------------------------
section "\$JOD_HOME exists and isn't a git checkout"
export JOD_HOME="$WORK/not-git"
mkdir -p "$JOD_HOME"
assert_fails "$REPO_ROOT/install.sh"

assert_summary
exit
