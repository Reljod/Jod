#!/usr/bin/env bash
#
# jod-upgrade.sh — replace this box's Jod binaries with the ones the Release
# workflow already built, downloaded from a GitHub release.
#
# What `jod upgrade` runs. The sibling of install.sh, and deliberately not the
# same thing:
#
#   install.sh   clones, checks out a tag, and runs `cargo build --release`.
#                Needs git and a Rust toolchain. Drives `jod update`, which
#                only ever moves within the installed MAJOR.MINOR.
#   this script  downloads jod-<target>.tar.gz from a release, checks it
#                against the .sha256 published beside it, and renames the
#                binaries into place. Needs curl and tar, and nothing else —
#                no checkout, no compiler. Moves to the newest release,
#                whatever its major and minor.
#
# The split is the point. A box installed from the prebuilt tarball — the
# first path the README offers, and the one that advertises "no Rust toolchain
# needed" — has no checkout, so `jod update` cannot run there at all. This is
# how that box takes a new release.
#
# It is embedded in the `jod` binary (cli/src/upgrade.rs) rather than only
# living here, for the same reason: the box that most needs it is the one with
# no copy of this repo on disk. Running it straight out of a checkout works
# identically, which is what tests/upgrade.test.sh does.
#
# Flags:
#   --check   say what an upgrade would do and change nothing
#   --force   download and reinstall even when already on the target release
#
# Env overrides:
#   JOD_UPGRADE_VERSION  release to install (default: latest)
#                        "latest" | vX.Y.Z | X.Y.Z
#   JOD_BIN_DIR          where the binaries are      (default: $HOME/.local/bin)
#   JOD_TARGET           platform triple to fetch    (default: from uname)
#   JOD_CURRENT_VERSION  version considered installed (default: ask the binary)
#   JOD_WITH_API         also install jod-api. Implied when $JOD_BIN_DIR
#                        already holds one — a box that opted into an endpoint
#                        that spawns agents keeps it across an upgrade, and one
#                        that did not is not handed one by an upgrade.
#   JOD_RELEASE_REPO     owner/name                  (default: Reljod/Jod)
#   JOD_API_BASE         GitHub API root             (default: https://api.github.com)
#   JOD_DOWNLOAD_BASE    release asset root          (default: from the repo)
#   GITHUB_TOKEN/GH_TOKEN  sent to the API when set, for the rate limit
set -euo pipefail

REPO="${JOD_RELEASE_REPO:-Reljod/Jod}"
API_BASE="${JOD_API_BASE:-https://api.github.com}"
DOWNLOAD_BASE="${JOD_DOWNLOAD_BASE:-https://github.com/$REPO/releases/download}"
VERSION="${JOD_UPGRADE_VERSION:-latest}"
BIN_DIR="${JOD_BIN_DIR:-$HOME/.local/bin}"

CHECK_ONLY=""
FORCE=""
for arg in "$@"; do
  case "$arg" in
    --check) CHECK_ONLY=1 ;;
    --force) FORCE=1 ;;
    # The header comment is the help text, printed by shape rather than by
    # line number so a growing header cannot start leaking `set -euo pipefail`
    # and the internals below it. Embedded in the binary, this script is
    # written to a temp file that $0 does point at, so unlike install.sh's
    # piped case there is always a file to read back.
    -h|--help)
      if [ -r "$0" ]; then
        awk 'NR > 1 { if (!/^#/) exit; sub(/^# ?/, ""); print }' "$0"
      else
        printf 'jod upgrade — see bin/jod-upgrade.sh in the Jod repository\n'
      fi
      exit 0 ;;
    *) printf 'error: unknown argument: %s\n' "$arg" >&2; exit 2 ;;
  esac
done

info() { printf '→ %s\n' "$*"; }
ok()   { printf '✓ %s\n' "$*"; }
err()  { printf 'error: %s\n' "$*" >&2; exit 1; }

# Version comparison is shared with install.sh rather than re-derived: two
# implementations of "which tag is higher" that disagree would mean `jod
# update` and `jod upgrade` reading the same tag list differently. The CLI
# writes this file out beside the script for exactly this line.
LIB="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/lib/semver.sh"
[ -r "$LIB" ] || err "missing $LIB — this script needs bin/lib/semver.sh beside it"
# shellcheck source=bin/lib/semver.sh
source "$LIB"

for tool in curl tar; do
  command -v "$tool" >/dev/null 2>&1 \
    || err "$tool is required but not found on PATH"
done

# --- what platform this box wants -------------------------------------------
# The `jod` binary knows the triple it was compiled for and passes it in;
# uname is the fallback for a standalone run. Compiled-in beats detected —
# uname reports the kernel's architecture, which is not always the one the
# running binary was built for.
detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os/$arch" in
    Darwin/arm64)          printf 'aarch64-apple-darwin' ;;
    Darwin/x86_64)         printf 'x86_64-apple-darwin' ;;
    Linux/x86_64)          printf 'x86_64-unknown-linux-gnu' ;;
    Linux/aarch64|Linux/arm64) printf 'aarch64-unknown-linux-gnu' ;;
    *) err "no prebuilt Jod for $os/$arch — build from source instead:
  curl -fsSL https://raw.githubusercontent.com/Reljod/Jod/main/install.sh | bash" ;;
  esac
}
TARGET="${JOD_TARGET:-$(detect_target)}"

# --- what is installed now ---------------------------------------------------
# `jod --version` prints `0.1.0 (f4e4c72 2026-08-13)`, so the release number is
# the first field. Absent or unreadable is not an error: a box part-way through
# a failed upgrade still has to be able to run one.
installed_version() {
  if [ -n "${JOD_CURRENT_VERSION:-}" ]; then
    printf '%s' "$JOD_CURRENT_VERSION"
    return
  fi
  local said
  said="$("$BIN_DIR/jod" --version 2>/dev/null)" || return 0
  # `jod 0.4.1 (abc1234 …)` — drop the program name, keep the number.
  said="${said#jod }"
  printf '%s' "${said%% *}"
}
CURRENT="$(installed_version)"

# --- which release ----------------------------------------------------------
# The API's `releases/latest` rather than the /releases/latest redirect: it
# skips drafts and pre-releases by definition, and it is one code path that a
# file:// fixture can serve verbatim, so the tests exercise what runs.
resolve_latest() {
  local auth=() body tag
  local token="${GITHUB_TOKEN:-${GH_TOKEN:-}}"
  [ -n "$token" ] && auth=(-H "Authorization: Bearer $token")
  # Guarded expansion: `auth` is empty on every run with no token, and
  # `"${auth[@]}"` on an empty array is an unbound-variable error under `set -u`
  # on macOS's bash 3.2. → tests/shell-arrays.test.sh
  body="$(curl -fsSL ${auth[@]+"${auth[@]}"} "$API_BASE/repos/$REPO/releases/latest" 2>/dev/null)" \
    || err "cannot reach $API_BASE to find the latest release — check the network, or name a version with --version"
  # One field, so sed rather than a jq this box may not have.
  tag="$(printf '%s' "$body" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$tag" ] || err "no tag_name in the release $API_BASE returned for $REPO"
  printf '%s' "$tag"
}

case "$VERSION" in
  latest) TAG="$(resolve_latest)" ;;
  [0-9]*.[0-9]*.[0-9]*) TAG="v$VERSION" ;;   # bare X.Y.Z -> vX.Y.Z
  *) TAG="$VERSION" ;;
esac
is_semver_tag "$TAG" \
  || err "not a release tag: $TAG — jod upgrade installs published releases (vX.Y.Z). To install a branch or a commit, build from source with install.sh"

# --- already there? ----------------------------------------------------------
# The comparison is on the release number the running binary reports, which the
# release workflow stamps into Cargo.toml on the tag. Nothing else is recorded:
# unlike a source install there is no checkout to keep a .jod-version beside,
# and the binary answering for itself cannot go stale.
up_to_date() {
  [ -n "$CURRENT" ] || return 1
  [ "v$CURRENT" = "$TAG" ] || return 1
  [ -x "$BIN_DIR/jod" ] && [ -x "$BIN_DIR/jod-run" ]
}

printf 'installed: %s\n' "${CURRENT:-unknown}"
printf 'target:    %s (%s)\n' "$TAG" "$TARGET"

if [ -n "$CHECK_ONLY" ]; then
  if up_to_date; then
    ok "already on $TAG — 'jod upgrade' would do nothing"
  else
    info "'jod upgrade' would download and install $TAG"
  fi
  exit 0
fi

if [ -z "$FORCE" ] && up_to_date; then
  ok "already on $TAG — nothing to download"
  exit 0
fi

# --- download and verify -----------------------------------------------------
WORK="$(mktemp -d "${TMPDIR:-/tmp}/jod-upgrade.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

ASSET="jod-$TARGET.tar.gz"
URL="$DOWNLOAD_BASE/$TAG/$ASSET"

info "Downloading $ASSET from $TAG"
curl -fsSL -o "$WORK/$ASSET" "$URL" \
  || err "could not download $URL
That release may not carry a build for $TARGET. The platforms built are listed
on the release page: https://github.com/$REPO/releases/tag/$TAG"

# The checksum is published beside the tarball by the same job that built it.
# A download that cannot be checked is refused rather than installed with a
# shrug: these binaries are not signed, so this is the only integrity check
# there is, and skipping it when it is inconvenient would make it decorative.
curl -fsSL -o "$WORK/$ASSET.sha256" "$URL.sha256" \
  || err "downloaded $ASSET but its .sha256 is missing from the release — refusing to install an unverified binary"

verify_checksum() {
  local expected actual
  # The file is `<sha256>  <name>`, written by sha256sum or shasum -a 256.
  expected="$(cut -d' ' -f1 < "$WORK/$ASSET.sha256")"
  [ -n "$expected" ] || err "empty checksum file for $ASSET"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$WORK/$ASSET" | cut -d' ' -f1)"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$WORK/$ASSET" | cut -d' ' -f1)"
  else
    err "neither sha256sum nor shasum is available — cannot verify $ASSET, refusing to install it"
  fi
  [ "$expected" = "$actual" ] \
    || err "checksum mismatch for $ASSET
  expected $expected
  got      $actual
Refusing to install. Re-run to retry the download; if it keeps failing, the
release asset itself is wrong and should not be installed."
  ok "sha256 verified"
}
verify_checksum

# --- unpack ------------------------------------------------------------------
# The tarball holds three bare binaries at its root (release.yml packages them
# with `tar -C`), so it unpacks into a scratch directory and the ones this box
# wants are picked out of it by name.
mkdir -p "$WORK/unpacked"
tar -xzf "$WORK/$ASSET" -C "$WORK/unpacked" \
  || err "could not unpack $ASSET"

BINARIES="jod jod-run"
# An upgrade must not quietly drop a binary this box has, nor add one it
# deliberately does not.
if [ -n "${JOD_WITH_API:-}" ] || [ -e "$BIN_DIR/jod-api" ]; then
  BINARIES="$BINARIES jod-api"
fi

for b in $BINARIES; do
  [ -f "$WORK/unpacked/$b" ] \
    || err "$ASSET does not contain $b — this release cannot be installed onto this box"
done

# --- install -----------------------------------------------------------------
# /usr/local/bin is the normal home for this on a server, and is root-owned.
SUDO=""
if [ ! -d "$BIN_DIR" ]; then
  mkdir -p "$BIN_DIR" 2>/dev/null || SUDO="sudo"
elif [ ! -w "$BIN_DIR" ]; then
  SUDO="sudo"
fi
if [ -n "$SUDO" ]; then
  command -v sudo >/dev/null 2>&1 \
    || err "$BIN_DIR is not writable and sudo is not available — set \$JOD_BIN_DIR to somewhere you own"
  info "$BIN_DIR needs root — using sudo to install"
  $SUDO mkdir -p "$BIN_DIR"
fi

# Installed as a fresh file and *renamed* over the old one, never written in
# place — the same rule install.sh follows, and for the same reason: replacing
# a running binary in place fails with ETXTBSY on Linux, and the binary running
# this is routinely the TUI on the VPS upgrading itself. rename() swaps the
# directory entry; the running process keeps the inode it started with.
install_binary() {
  local name="$1" from="$WORK/unpacked/$1" tmp="$BIN_DIR/.$1.new.$$"
  $SUDO install -m 0755 "$from" "$tmp"
  # Downloaded rather than compiled here, so on macOS it may carry a
  # quarantine flag that would make an unsigned binary refuse to launch.
  # Cleared on the staged copy, before it becomes the live one.
  if [ "$(uname -s)" = "Darwin" ]; then
    $SUDO xattr -d com.apple.quarantine "$tmp" 2>/dev/null || true
  fi
  $SUDO mv -f "$tmp" "$BIN_DIR/$name"
  ok "installed $BIN_DIR/$name"
}
for b in $BINARIES; do install_binary "$b"; done

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo
    info "$BIN_DIR is not on your PATH yet. Add this to your shell profile:"
    printf '    export PATH="%s:$PATH"\n' "$BIN_DIR"
    ;;
esac

echo
ok "Jod $TAG installed — $("$BIN_DIR/jod" --version 2>/dev/null || echo 'jod')"

# A source checkout that is now behind what is installed is a live trap: `jod
# update` resolves its target from the checkout's own .jod-version, so it would
# happily rebuild an older release straight over the top of this one. Say so
# rather than let it be discovered as a mystery downgrade.
SRC="${JOD_SRC:-${JOD_HOME:-$HOME/.jod}/src}"
if [ -f "$SRC/.jod-version" ]; then
  checkout_ref="$(cat "$SRC/.jod-version")"
  if is_semver_tag "$checkout_ref" && [ "$checkout_ref" != "$TAG" ] \
    && [ "$(printf '%s\n%s\n' "$checkout_ref" "$TAG" | highest_semver_tag)" = "$TAG" ]; then
    echo
    info "the source checkout at $SRC is still on $checkout_ref."
    info "'jod update' builds from there, so it would install $checkout_ref over this."
    info "Bring it along with:  JOD_VERSION=$TAG bash $SRC/install.sh"
  fi
fi

# A replaced binary is not a restarted process. Everything long-running keeps
# the old inode until it is told otherwise, so say which ones those are rather
# than leaving someone to wonder why the upgrade they just took isn't there.
running_note=""
for unit in jod-daemon jod-api; do
  if command -v systemctl >/dev/null 2>&1 \
    && systemctl is-active --quiet "$unit" 2>/dev/null; then
    running_note="$running_note  sudo systemctl restart $unit
"
  fi
done
if [ -n "$running_note" ]; then
  echo
  info "Still running the previous build — restart to pick this up:"
  printf '%s' "$running_note"
fi
if pgrep -f 'jod tui' >/dev/null 2>&1; then
  info "A 'jod tui' console is running: quit and reopen it to pick this up."
fi
