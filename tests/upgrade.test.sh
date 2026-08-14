#!/usr/bin/env bash
#
# upgrade.test.sh — deterministic, network-free tests for bin/jod-upgrade.sh:
# which release it resolves, what it refuses to install, and the binary
# replacement that has to work while the old binary is still running.
#
# Builds a throwaway file:// "GitHub" — a `releases/latest` API response and a
# download tree of real tarballs, laid out at exactly the paths the script asks
# for, so `curl` fetches them over file:// through the same code path that
# talks to github.com. Nothing here reaches the network, and no cargo build is
# needed: the "binaries" in the tarballs are shell scripts that report the
# version they were packaged as, which is how a test tells a real install from
# a no-op.
#
# The one thing this cannot cover is a genuine cross-platform asset, so the
# fixture releases are built for whatever triple the test asks for.
#
# Run: tests/upgrade.test.sh
set -u

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$REPO_ROOT/.agents/skills/test-scenarios/scripts/assert.sh"

UPGRADE="$REPO_ROOT/bin/jod-upgrade.sh"
[ -r "$UPGRADE" ] || { echo "error: no $UPGRADE" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "== jod upgrade test suite =="

REMOTE="$WORK/remote"
REPO="fixture/Jod"
TARGET="x86_64-unknown-linux-gnu"
API_BASE="file://$REMOTE/api"
DOWNLOAD_BASE="file://$REMOTE/download"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# --- fixture: a release, built the way release.yml builds one ---------------
# Three bare binaries at the tarball root (release.yml uses `tar -C`), a
# .sha256 beside the tarball, and nothing else.
publish_release() {
  # Declared separately: `local` expands every argument before it assigns any,
  # so a later default referring to an earlier one would expand to nothing.
  local tag="$1"
  local target="${2:-$TARGET}"
  local stage="$WORK/stage-$tag-$target"
  rm -rf "$stage"; mkdir -p "$stage"

  local b
  for b in jod jod-run jod-api; do
    cat > "$stage/$b" <<EOF
#!/usr/bin/env bash
case "\${1:-}" in
  --version) echo "$b ${tag#v} (fixture $tag)" ;;
  sleep)     sleep 120 ;;
  *)         echo "$b ${tag#v}" ;;
esac
EOF
    chmod +x "$stage/$b"
  done

  local dir="$REMOTE/download/$tag"
  mkdir -p "$dir"
  tar -czf "$dir/jod-$target.tar.gz" -C "$stage" jod jod-run jod-api
  ( cd "$dir" && printf '%s  %s\n' \
      "$(sha256_of "jod-$target.tar.gz")" "jod-$target.tar.gz" \
      > "jod-$target.tar.gz.sha256" )
}

# What the API's releases/latest returns. Only tag_name is read, but the shape
# is the real one so the parser is exercised against a realistic body.
set_latest() {
  local tag="$1" dir="$REMOTE/api/repos/$REPO/releases"
  mkdir -p "$dir"
  cat > "$dir/latest" <<EOF
{
  "url": "https://api.github.com/repos/$REPO/releases/1",
  "tag_name": "$tag",
  "name": "$tag",
  "draft": false,
  "prerelease": false
}
EOF
}

# A box with Jod already on it, at $1.
seed_bin_dir() {
  local version="$1" dir="$WORK/bin"
  rm -rf "$dir"; mkdir -p "$dir"
  local b
  for b in jod jod-run; do
    cat > "$dir/$b" <<EOF
#!/usr/bin/env bash
case "\${1:-}" in
  --version) echo "$b $version (installed)" ;;
  sleep)     sleep 120 ;;
  *)         echo "$b $version" ;;
esac
EOF
    chmod +x "$dir/$b"
  done
  printf '%s' "$dir"
}

# Run the upgrader against the fixture. Arguments containing `=` are extra
# environment for the run; the rest are flags for the script — otherwise `env`
# would take `--check` for one of its own options. Output lands in $WORK/out
# for grepping and the exit status in $STATUS, so an assertion never has to
# reason about how far `$?` has travelled.
STATUS=0
run_upgrade() {
  local envs=() flags=() a
  for a in "$@"; do
    case "$a" in
      *=*) envs+=("$a") ;;
      *)   flags+=("$a") ;;
    esac
  done
  env -u JOD_WITH_API \
    JOD_RELEASE_REPO="$REPO" \
    JOD_API_BASE="$API_BASE" \
    JOD_DOWNLOAD_BASE="$DOWNLOAD_BASE" \
    JOD_BIN_DIR="$WORK/bin" \
    JOD_TARGET="$TARGET" \
    JOD_HOME="$WORK/jod-home" \
    ${envs[@]+"${envs[@]}"} \
    bash "$UPGRADE" ${flags[@]+"${flags[@]}"} > "$WORK/out" 2>&1
  STATUS=$?
}

# assert_status <expected> <name>
assert_status() { assert_eq "$STATUS" "$1" "$2"; }

publish_release v0.4.3
publish_release v0.5.0
set_latest v0.5.0

# --- latest is what the API says, and --version overrides it ----------------
section "resolving which release"

seed_bin_dir 0.4.1 >/dev/null
run_upgrade --check
assert_grep "v0.5.0" "$WORK/out" "--check names the release the API calls latest"
assert_grep "installed: 0.4.1" "$WORK/out" "…and the version already on the box"
assert_grep "would download and install" "$WORK/out" "…and says it would install it"
assert_eq "$("$WORK/bin/jod" --version)" "jod 0.4.1 (installed)" \
  "--check installed nothing"

seed_bin_dir 0.4.1 >/dev/null
run_upgrade JOD_UPGRADE_VERSION=v0.4.3 --check
assert_grep "v0.4.3" "$WORK/out" "--version names the release asked for, not the newest"

# A branch or a commit is a source-install concept; there is no tarball for it
# and pretending otherwise would 404 halfway through an upgrade.
seed_bin_dir 0.4.1 >/dev/null
run_upgrade JOD_UPGRADE_VERSION=main
assert_status 1 "a branch name is refused rather than fetched"
assert_grep "not a release tag" "$WORK/out" "…and the refusal says why"
assert_grep "install.sh" "$WORK/out" "…and points at the path that can do it"

# --- the happy path ---------------------------------------------------------
section "installing a release"

seed_bin_dir 0.4.1 >/dev/null
run_upgrade
assert_status 0 "an upgrade to the newest release succeeds"
assert_grep "sha256 verified" "$WORK/out" "the download is checked before it is installed"
assert_eq "$("$WORK/bin/jod" --version)" "jod 0.5.0 (fixture v0.5.0)" \
  "jod is now the release's binary"
assert_eq "$("$WORK/bin/jod-run" --version)" "jod-run 0.5.0 (fixture v0.5.0)" \
  "so is jod-run"

# jod-api is opt-in at install time because it is an endpoint that spawns
# agents. An upgrade must not hand one to a box that never asked for it…
assert_missing "$WORK/bin/jod-api" "an upgrade does not add jod-api to a box without one"

# …nor drop one from a box that did.
seed_bin_dir 0.4.1 >/dev/null
cp "$WORK/bin/jod" "$WORK/bin/jod-api"
run_upgrade
assert_file "$WORK/bin/jod-api" "a box that has jod-api keeps it across an upgrade"
assert_eq "$("$WORK/bin/jod-api" --version)" "jod-api 0.5.0 (fixture v0.5.0)" \
  "…and it is upgraded too, not left behind at the old version"

# --- already there ----------------------------------------------------------
section "an upgrade that has nothing to do"

seed_bin_dir 0.5.0 >/dev/null
run_upgrade
assert_status 0 "being already on the newest release is a success, not an error"
assert_grep "already on v0.5.0" "$WORK/out" "…and says so"
assert_grep "nothing to download" "$WORK/out" "…and did not fetch anything"

# --force is the escape hatch, and it has to actually re-fetch.
seed_bin_dir 0.5.0 >/dev/null
run_upgrade --force
assert_grep "sha256 verified" "$WORK/out" "--force downloads and reinstalls anyway"
assert_eq "$("$WORK/bin/jod" --version)" "jod 0.5.0 (fixture v0.5.0)" \
  "…leaving the release's binary in place"

# --- integrity --------------------------------------------------------------
# These binaries are not code-signed, so the published checksum is the only
# integrity check there is. A run that installs anyway when it cannot verify
# would make it decorative.
section "refusing what it cannot verify"

publish_release v0.6.0
set_latest v0.6.0
# Corrupt the tarball, leaving the (now wrong) checksum in place.
printf 'not a tarball' > "$REMOTE/download/v0.6.0/jod-$TARGET.tar.gz"

seed_bin_dir 0.4.1 >/dev/null
run_upgrade
assert_status 1 "a checksum mismatch fails the upgrade"
assert_grep "checksum mismatch" "$WORK/out" "…and names the problem"
assert_eq "$("$WORK/bin/jod" --version)" "jod 0.4.1 (installed)" \
  "…and the box is left on the version it was already running"

publish_release v0.6.0
rm -f "$REMOTE/download/v0.6.0/jod-$TARGET.tar.gz.sha256"

seed_bin_dir 0.4.1 >/dev/null
run_upgrade
assert_status 1 "a release with no published checksum is refused"
assert_grep "refusing to install an unverified binary" "$WORK/out" "…saying exactly that"
assert_eq "$("$WORK/bin/jod" --version)" "jod 0.4.1 (installed)" \
  "…and again changes nothing"

# A release that carries no build for this box's platform is a clear sentence,
# not a confusing 404 from curl.
publish_release v0.6.0
set_latest v0.6.0
seed_bin_dir 0.4.1 >/dev/null
run_upgrade JOD_TARGET=powerpc-unknown-linux-gnu
assert_status 1 "a platform the release has no build for fails"
assert_grep "could not download" "$WORK/out" "…and says which asset was missing"
assert_eq "$("$WORK/bin/jod" --version)" "jod 0.4.1 (installed)" \
  "…and installs nothing"

# --- replacing a binary that is running -------------------------------------
# The console on the VPS upgrades itself, so the file being replaced is
# routinely the one executing the upgrade. Writing it in place fails with
# ETXTBSY on Linux; the script installs a fresh file and renames it over, which
# leaves the running process on the inode it started with.
section "upgrading a running binary"

set_latest v0.5.0
seed_bin_dir 0.4.1 >/dev/null
"$WORK/bin/jod" sleep &
RUNNING=$!
sleep 0.3

run_upgrade
assert_status 0 "an upgrade succeeds while the old binary is still running"
ok "kill -0 $RUNNING 2>/dev/null" "…the running process survived it"
assert_eq "$("$WORK/bin/jod" --version)" "jod 0.5.0 (fixture v0.5.0)" \
  "…and the path now holds the new build"
kill "$RUNNING" 2>/dev/null
wait "$RUNNING" 2>/dev/null

# --- the trap this closes ---------------------------------------------------
# `jod update` resolves its target from the checkout's own .jod-version, so a
# checkout left behind an upgrade would rebuild an older release straight over
# the top of it. Being told beats discovering it as a mystery downgrade.
section "warning about a checkout left behind"

seed_bin_dir 0.4.1 >/dev/null
mkdir -p "$WORK/jod-home/src"
printf 'v0.4.3\n' > "$WORK/jod-home/src/.jod-version"
run_upgrade
assert_grep "still on v0.4.3" "$WORK/out" "an out-of-date checkout is called out"
assert_grep "jod update" "$WORK/out" "…naming the command that would undo this"

# A checkout that is already at or ahead of the release is not a trap, and
# saying so anyway would train people to ignore the warning.
seed_bin_dir 0.4.1 >/dev/null
printf 'v0.5.0\n' > "$WORK/jod-home/src/.jod-version"
run_upgrade
assert_no_grep "still on" "$WORK/out" "a checkout already at the release says nothing"

assert_summary; exit
