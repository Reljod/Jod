#!/usr/bin/env bash
#
# install.test.sh — deterministic, network-free tests for install.sh: version
# resolution, the patch-only cascade `jod update` runs, and the binary
# replacement that has to work while the old binary is still running.
#
# Builds a throwaway file:// "remote" holding the *real* install.sh and
# bin/lib/semver.sh over a miniature cargo workspace with the real package and
# binary names (jod-cli → `jod`, jod-supervisor → `jod-run`, jod-api →
# `jod-api`). Every crate is dependency-free, so the builds are seconds long
# and need no registry — the installer is exercised end to end without
# touching github.com, crates.io, or the developer's real $HOME.
#
# Each fixture binary prints the VERSION file it was compiled from, which is
# how a test tells a rebuild from a no-op: the string can only change if cargo
# actually ran again over the new checkout.
#
# Run: tests/install.test.sh
set -u

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$REPO_ROOT/.agents/skills/test-scenarios/scripts/assert.sh"

command -v cargo >/dev/null 2>&1 || {
  echo "error: cargo is required — install.sh builds Jod from source" >&2
  exit 1
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "== install.sh test suite =="

# --- fixture: a minimal, file:// "Jod" remote -------------------------------
SEED="$WORK/seed"
REMOTE="$WORK/remote.git"
mkdir -p "$SEED/bin/lib" "$SEED/cli/src" "$SEED/supervisor/src" "$SEED/api/src"
cp "$REPO_ROOT/install.sh" "$SEED/install.sh"
cp "$REPO_ROOT/bin/lib/semver.sh" "$SEED/bin/lib/semver.sh"
chmod +x "$SEED/install.sh"

cat > "$SEED/Cargo.toml" <<'TOML'
[workspace]
resolver = "2"
members = ["cli", "supervisor", "api"]
TOML

# A binary that reports which source it came from, and — for `jod` — one that
# can be made to *stay running*, so the replace-a-live-binary case is testable.
seed_crate() {
  local dir="$1" pkg="$2" bin="$3"
  cat > "$SEED/$dir/Cargo.toml" <<TOML
[package]
name = "$pkg"
version = "0.0.0"
edition = "2021"

[[bin]]
name = "$bin"
path = "src/main.rs"
TOML
  cat > "$SEED/$dir/src/main.rs" <<RUST
fn main() {
    if std::env::args().nth(1).as_deref() == Some("sleep") {
        std::thread::sleep(std::time::Duration::from_secs(120));
    }
    println!("$bin {}", include_str!("../../VERSION").trim());
}
RUST
}
seed_crate cli jod-cli jod
seed_crate supervisor jod-supervisor jod-run
seed_crate api jod-api jod-api

seed_commit() { git -C "$SEED" -c user.name=test -c user.email=test@example.com "$@"; }

echo "0.0.0-main" > "$SEED/VERSION"
git init --quiet -b main "$SEED"
( cd "$SEED" && cargo generate-lockfile --offline >/dev/null 2>&1 )
seed_commit add -A
seed_commit commit --quiet -m "seed"
git init --quiet --bare "$REMOTE"
git -C "$SEED" remote add origin "$REMOTE"
git -C "$SEED" push --quiet origin main

# Cut a release: stamp VERSION so the built binary can be told apart, commit,
# tag, push. Mirrors what .github/workflows/release.yml does to the manifests.
release() {
  echo "$1" > "$SEED/VERSION"
  seed_commit add VERSION
  seed_commit commit --quiet -m "release $1"
  git -C "$SEED" tag "v$1"
  git -C "$SEED" push --quiet origin main "v$1"
}

export CARGO_NET_OFFLINE=true
export JOD_REPO_URL="file://$REMOTE"
export JOD_REF="main"
export JOD_HOME="$WORK/home/.jod"
export JOD_SRC="$JOD_HOME/src"
export JOD_BIN_DIR="$WORK/home/bin"
unset JOD_VERSION

JOD="$JOD_BIN_DIR/jod"
installed_version() { "$JOD" | awk '{print $2}'; }

# --- 1. fresh install, no release tags yet ----------------------------------
section "fresh install (no release tags yet)"
assert_ok "$REPO_ROOT/install.sh"
assert_dir "$JOD_SRC/.git" "clones the source into \$JOD_SRC"
assert_file "$JOD" "puts jod on \$JOD_BIN_DIR"
assert_file "$JOD_BIN_DIR/jod-run" "installs the supervisor beside it"
ok '[ ! -L "$JOD" ]' "installs a real binary, not a symlink into the checkout"
ok '[ -x "$JOD" ]' "the installed binary is executable"
assert_eq "$(cat "$JOD_SRC/.jod-version")" "main" "no tags: falls back to \$JOD_REF"
assert_eq "$(installed_version)" "0.0.0-main" "the binary was built from that checkout"

# --- 2. jod-api is opt-in ----------------------------------------------------
section "jod-api is not installed by default"
assert_missing "$JOD_BIN_DIR/jod-api" "an endpoint that spawns agents is never a side effect"
JOD_WITH_API=1 "$REPO_ROOT/install.sh" >/dev/null 2>&1
assert_file "$JOD_BIN_DIR/jod-api" "JOD_WITH_API=1 installs it"

# --- 3. default install pins to the newest release tag ----------------------
section "default (latest) install picks the newest semver tag"
release 1.0.0
release 1.0.1
release 1.1.0
assert_ok "$REPO_ROOT/install.sh"
assert_eq "$(cat "$JOD_SRC/.jod-version")" "v1.1.0" "latest resolves to the highest tag"
assert_eq "$(installed_version)" "1.1.0" "and that is the build on PATH"

# --- 4. pinning an explicit / bare version -----------------------------------
section "pinning a version"
JOD_VERSION=v1.0.0 "$REPO_ROOT/install.sh" >/dev/null 2>&1
assert_eq "$(installed_version)" "1.0.0" "JOD_VERSION=v1.0.0 pins exactly"
JOD_VERSION=1.0.1 "$REPO_ROOT/install.sh" >/dev/null 2>&1
assert_eq "$(installed_version)" "1.0.1" "bare X.Y.Z is normalised to vX.Y.Z"
assert_fails env JOD_VERSION=v9.9.9 "$REPO_ROOT/install.sh"
assert_eq "$(installed_version)" "1.0.1" "a refused version leaves the install alone"

# --- 5. --check reports and changes nothing ----------------------------------
section "--check"
JOD_VERSION=v1.0.0 "$REPO_ROOT/install.sh" >/dev/null 2>&1
CHECK_OUT="$(JOD_VERSION=patch "$REPO_ROOT/install.sh" --check 2>&1)"
assert_eq "$(installed_version)" "1.0.0" "--check does not install anything"
ok "grep -q 'v1.0.1' <<<\"\$CHECK_OUT\"" "--check names the patch it would take"
ok "grep -q 'v1.1.0' <<<\"\$CHECK_OUT\"" "--check names the newer release it would not"

# --- 6. the patch-only cascade `jod update` runs -----------------------------
section "JOD_VERSION=patch: patch-only cascade"
UPDATE_OUT="$(JOD_VERSION=patch "$REPO_ROOT/install.sh" 2>&1)"
assert_eq "$(installed_version)" "1.0.1" "update takes the newer v1.0.x patch"
ok "grep -q 'v1.1.0' <<<\"\$UPDATE_OUT\"" "and says the newer v1.1.0 release exists without taking it"

UPDATE_OUT2="$(JOD_VERSION=patch "$REPO_ROOT/install.sh" 2>&1)"
assert_eq "$(installed_version)" "1.0.1" "second update is a no-op"
ok "grep -qi 'nothing to build' <<<\"\$UPDATE_OUT2\"" "and skips the rebuild rather than repeating it"

release 1.0.2
JOD_VERSION=patch "$REPO_ROOT/install.sh" >/dev/null 2>&1
assert_eq "$(installed_version)" "1.0.2" "a newly published v1.0.2 patch is picked up"

# --- 7. --force rebuilds an install that is already current ------------------
section "--force"
BEFORE="$(ls -i "$JOD" | awk '{print $1}')"
JOD_VERSION=patch "$REPO_ROOT/install.sh" --force >/dev/null 2>&1
AFTER="$(ls -i "$JOD" | awk '{print $1}')"
ok '[ "$BEFORE" != "$AFTER" ]' "--force reinstalls (a new inode) where a plain run would skip"

# --- 8. updating while the old binary is still running -----------------------
# The case this has to survive on the VPS: the console is a long-lived `jod`
# process, and writing a running executable in place fails with ETXTBSY.
section "replacing a binary that is running"
"$JOD" sleep >/dev/null 2>&1 &
SLEEPER=$!
sleep 0.3
assert_ok env JOD_VERSION=patch "$REPO_ROOT/install.sh" --force
ok 'kill -0 "$SLEEPER" 2>/dev/null' "the running process is untouched by the swap"
kill "$SLEEPER" 2>/dev/null
wait "$SLEEPER" 2>/dev/null

# --- 9. branch installs fast-forward -----------------------------------------
section "branch install"
JOD_VERSION=main "$REPO_ROOT/install.sh" >/dev/null 2>&1
assert_eq "$(installed_version)" "1.0.2" "a branch install builds the branch tip"
echo "0.0.0-moved" > "$SEED/VERSION"
seed_commit add VERSION
seed_commit commit --quiet -m "move main"
git -C "$SEED" push --quiet origin main
assert_ok env JOD_VERSION=patch "$REPO_ROOT/install.sh"
assert_eq "$(installed_version)" "0.0.0-moved" "update fast-forwards a branch install and rebuilds"

# --- 10. refuses to clobber a non-git \$JOD_SRC ------------------------------
section "\$JOD_SRC exists and isn't a git checkout"
export JOD_SRC="$WORK/not-git"
mkdir -p "$JOD_SRC"
assert_fails "$REPO_ROOT/install.sh"
assert_missing "$JOD_SRC/.git" "leaves the directory alone"

# --- 11. --check with nothing installed --------------------------------------
section "--check before anything is installed"
export JOD_SRC="$WORK/never-installed"
assert_fails "$REPO_ROOT/install.sh" --check
assert_missing "$JOD_SRC" "checking an install that doesn't exist clones nothing"

assert_summary
exit
