#!/usr/bin/env bash
#
# install.sh — install the Jod binaries onto a Linux/macOS box, no
# clone-and-remember-the-path required.
#
#   curl -fsSL https://raw.githubusercontent.com/Reljod/Jod/main/install.sh | bash
#
# Clones (or updates) this repo into $JOD_SRC, checks out a version, builds the
# workspace with cargo, and puts the binaries on $JOD_BIN_DIR:
#
#   jod       the CLI and the TUI
#   jod-run   the supervisor every run is launched through — jod without it
#             can list and read, but cannot start anything
#   jod-api   only with JOD_WITH_API=1. Standing up an endpoint that spawns
#             agents is a deliberate act, never a side effect of an install.
#             → deploy/README.md
#
# Built from source rather than downloaded: releases here carry no binary
# assets, and a box that can build is one that can also be debugged on.
# → docs/decisions.md
#
# Versioning: releases are tagged vMAJOR.MINOR.PATCH (see
# .github/workflows/release.yml — run it manually to cut one). By default this
# installs the newest tag; pin an older one with JOD_VERSION.
#
#   curl -fsSL .../install.sh | bash                      # latest release
#   curl -fsSL .../install.sh | JOD_VERSION=v1.2.0 bash    # pinned release
#   curl -fsSL .../install.sh | JOD_VERSION=main bash      # bleeding edge
#
# `jod update` later runs this same script with JOD_VERSION=patch, which only
# ever moves within the installed MAJOR.MINOR — it takes new patches
# automatically but never jumps you to a new minor/major out from under the
# TUI and the daemon. Re-run install.sh with a new JOD_VERSION for that.
#
# Safe to re-run: it fetches into the existing checkout, and skips the build
# entirely when the installed binaries already match the target commit.
#
# Flags:
#   --check   say what an update would do and change nothing
#   --force   rebuild and reinstall even when already at the target commit
#
# Env overrides:
#   JOD_REPO_URL   git remote to clone      (default: github.com/Reljod/Jod)
#   JOD_VERSION    version/ref to install   (default: latest)
#                  "latest" | "patch" | vX.Y.Z | a branch or commit SHA
#   JOD_REF        fallback branch when no release tags exist yet, and the
#                  branch `git clone` starts on (default: main)
#   JOD_HOME       Jod's state directory        (default: $HOME/.jod)
#   JOD_SRC        where the source lives       (default: $JOD_HOME/src)
#   JOD_BIN_DIR    where the binaries are put   (default: $HOME/.local/bin)
#   JOD_WITH_API   also build and install jod-api (default: off)
set -euo pipefail

# --- run from a copy of ourselves ------------------------------------------
# `git checkout` below rewrites this very file, and bash reads a script
# incrementally — so an update run from $JOD_SRC/install.sh would carry on
# reading whatever landed at the same byte offset in the new version. Copy
# first, then run that. A piped run (curl | bash) has no file to copy and no
# checkout that could overwrite it, so it skips this — and reads from stdin,
# which leaves BASH_SOURCE *empty*, so the default below is what keeps `set -u`
# from killing the one invocation the README tells people to use.
if [ -z "${JOD_INSTALLER_COPY:-}" ] && [ -f "${BASH_SOURCE[0]:-}" ]; then
  _self_copy="$(mktemp "${TMPDIR:-/tmp}/jod-install.XXXXXX")"
  cat "${BASH_SOURCE[0]}" > "$_self_copy"
  export JOD_INSTALLER_COPY="$_self_copy"
  exec bash "$_self_copy" "$@"
fi
[ -n "${JOD_INSTALLER_COPY:-}" ] && trap 'rm -f "$JOD_INSTALLER_COPY"' EXIT

REPO_URL="${JOD_REPO_URL:-https://github.com/Reljod/Jod.git}"
VERSION="${JOD_VERSION:-latest}"
REPO_REF="${JOD_REF:-main}"
JOD_HOME="${JOD_HOME:-$HOME/.jod}"
# The source is kept *inside* the state directory, not beside it, so `jod
# update` can find its own checkout from JOD_HOME alone — including when Jod
# runs as a system user whose $HOME is not the installer's.
SRC="${JOD_SRC:-$JOD_HOME/src}"
BIN_DIR="${JOD_BIN_DIR:-$HOME/.local/bin}"

CHECK_ONLY=""
FORCE=""
for arg in "$@"; do
  case "$arg" in
    --check) CHECK_ONLY=1 ;;
    --force) FORCE=1 ;;
    # The header comment *is* the help text, so print it by shape rather than
    # by line number — a hard-coded range silently starts leaking `set -euo
    # pipefail` and the internals below it the first time the header grows.
    # A piped run has no file to read it back out of ($0 is "bash"), so it
    # says where the text lives instead of dying on a missing file.
    -h|--help)
      if [ -r "$0" ]; then
        awk 'NR > 1 { if (!/^#/) exit; sub(/^# ?/, ""); print }' "$0"
      else
        printf 'jod installer — flags and env overrides are documented at the top of\n  %s\n' \
          "https://raw.githubusercontent.com/Reljod/Jod/main/install.sh"
      fi
      exit 0 ;;
    *) printf 'error: unknown argument: %s\n' "$arg" >&2; exit 2 ;;
  esac
done

info() { printf '→ %s\n' "$*"; }
ok()   { printf '✓ %s\n' "$*"; }
err()  { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v git >/dev/null 2>&1 || err "git is required but not found on PATH"

case "$(uname -s)" in
  Linux|Darwin) ;;
  *) err "unsupported platform: $(uname -s) (Linux and macOS only)" ;;
esac

# --- get (or refresh) a full clone, incl. tags — resolving a version needs
# --- the tag list, so this can't be a shallow/single-branch clone. -------
if [ -d "$SRC/.git" ]; then
  info "Fetching updates into existing checkout at $SRC"
  git -C "$SRC" fetch --quiet --tags --force origin
elif [ -e "$SRC" ]; then
  err "$SRC exists and is not a git checkout — remove it or set \$JOD_SRC"
else
  [ -n "$CHECK_ONLY" ] && err "nothing installed yet at $SRC — there is no update to check"
  info "Cloning $REPO_URL into $SRC"
  mkdir -p "$(dirname "$SRC")"
  git clone --quiet --branch "$REPO_REF" "$REPO_URL" "$SRC"
fi

# shellcheck source=bin/lib/semver.sh
source "$SRC/bin/lib/semver.sh"

# What is checked out right now. install.sh records the ref it used rather
# than leaving it to be inferred later: tags cut back-to-back can land on the
# same commit, which makes `git describe` ambiguous about which one was asked
# for.
installed_ref() {
  if [ -f "$SRC/.jod-version" ]; then
    cat "$SRC/.jod-version"
  else
    git -C "$SRC" describe --tags --exact-match 2>/dev/null \
      || git -C "$SRC" rev-parse --abbrev-ref HEAD
  fi
}

filter_cargo_tags() {
  local tag
  while IFS= read -r tag; do
    [ -n "$tag" ] || continue
    if git -C "$SRC" cat-file -e "$tag:Cargo.toml" 2>/dev/null; then
      printf '%s\n' "$tag"
    fi
  done
}

# The highest patch tag within one MAJOR.MINOR — the only move `jod update`
# is allowed to make on its own.
highest_patch_of() {
  local ver="${1#v}" major rest minor
  major="${ver%%.*}"; rest="${ver#*.}"; minor="${rest%%.*}"
  git -C "$SRC" tag --list "v$major.$minor.*" | filter_cargo_tags | highest_semver_tag
}

newest_tag() { git -C "$SRC" tag --list 'v*.*.*' | filter_cargo_tags | highest_semver_tag; }


resolve_version() {
  case "$1" in
    latest)
      local tag
      tag="$(newest_tag)"
      if [ -z "$tag" ]; then
        info "no release tags found yet — using the '$REPO_REF' branch" >&2
        printf '%s' "$REPO_REF"
      else
        printf '%s' "$tag"
      fi
      ;;
    patch)
      # What `jod update` asks for: newer patches of what is installed, and
      # nothing else. A branch install stays on its branch and fast-forwards.
      local cur patch
      cur="$(installed_ref)"
      if is_semver_tag "$cur"; then
        patch="$(highest_patch_of "$cur")"
        printf '%s' "${patch:-$cur}"
      else
        printf '%s' "$cur"
      fi
      ;;
    [0-9]*.[0-9]*.[0-9]*) printf 'v%s' "$1" ;;   # bare X.Y.Z -> vX.Y.Z
    *) printf '%s' "$1" ;;                        # vX.Y.Z, a branch, or a SHA
  esac
}
CURRENT_REF="$(installed_ref 2>/dev/null || true)"
TARGET_REF="$(resolve_version "$VERSION")"

# A newer minor/major is worth saying out loud on every run — `jod update`
# will never take it, so nothing else would ever mention it exists.
announce_newer() {
  local newest
  newest="$(newest_tag)"
  if [ -n "$newest" ] && [ "$newest" != "$TARGET_REF" ] && is_semver_tag "$TARGET_REF"; then
    if [ "$(printf '%s\n%s\n' "$newest" "$TARGET_REF" | highest_semver_tag)" = "$newest" ]; then
      info "a newer release is available: $newest — take it with JOD_VERSION=$newest"
    fi
  fi
}

BINARIES="jod jod-run"
[ -n "${JOD_WITH_API:-}" ] && BINARIES="$BINARIES jod-api"

# Everything installed and at the target commit? Then there is nothing to do,
# and saying so beats a five-minute rebuild that changes nothing.
target_commit() { git -C "$SRC" rev-parse --verify --quiet "$1^{commit}" 2>/dev/null; }

up_to_date() {
  local want="$1" b
  [ -f "$SRC/.jod-commit" ] || return 1
  [ "$(cat "$SRC/.jod-commit")" = "$want" ] || return 1
  [ "$CURRENT_REF" = "$TARGET_REF" ] || return 1
  for b in $BINARIES; do
    [ -x "$BIN_DIR/$b" ] || return 1
  done
  return 0
}

# --- check mode: report, change nothing -------------------------------------
if [ -n "$CHECK_ONLY" ]; then
  # A branch's remote tip, a tag's own commit — the thing an install would
  # actually check out.
  WANT="$(git -C "$SRC" rev-parse --verify --quiet "origin/$TARGET_REF^{commit}" 2>/dev/null \
    || target_commit "$TARGET_REF" || true)"
  [ -n "$WANT" ] || err "unknown version/ref: $TARGET_REF"
  printf 'installed: %s (%s)\n' "${CURRENT_REF:-unknown}" "$(git -C "$SRC" rev-parse --short HEAD)"
  printf 'target:    %s (%s)\n' "$TARGET_REF" "$(git -C "$SRC" rev-parse --short "$WANT")"
  if up_to_date "$WANT"; then
    ok "already up to date — 'jod update' would do nothing"
  else
    info "'jod update' would build and install $TARGET_REF"
  fi
  announce_newer
  exit 0
fi

info "Checking out $TARGET_REF"
git -C "$SRC" checkout --quiet "$TARGET_REF" 2>/dev/null \
  || git -C "$SRC" checkout --quiet -B "$TARGET_REF" "origin/$TARGET_REF" 2>/dev/null \
  || err "unknown version/ref: $TARGET_REF"
# If it's a branch, fast-forward it to origin so re-running stays current.
if git -C "$SRC" show-ref --verify --quiet "refs/remotes/origin/$TARGET_REF"; then
  git -C "$SRC" merge --quiet --ff-only "origin/$TARGET_REF"
fi
HEAD_COMMIT="$(git -C "$SRC" rev-parse HEAD)"

if [ -z "$FORCE" ] && up_to_date "$HEAD_COMMIT"; then
  ok "already at $TARGET_REF ($(git -C "$SRC" rev-parse --short HEAD)) — nothing to build"
  announce_newer
  exit 0
fi

# --- build -------------------------------------------------------------------
[ -f "$SRC/Cargo.toml" ] \
  || err "no Cargo.toml found in $SRC at $TARGET_REF — $TARGET_REF cannot be built as a Rust package"

command -v cargo >/dev/null 2>&1 \
  || err "cargo is required to build Jod — install Rust from https://rustup.rs, then re-run this"

PACKAGES="-p jod-cli -p jod-supervisor"
[ -n "${JOD_WITH_API:-}" ] && PACKAGES="$PACKAGES -p jod-api"

info "Building $TARGET_REF — this takes a few minutes the first time"
# --locked: the lockfile is committed and stamped by the release, so a build
# that silently resolved different dependency versions would not be the
# release it claims to be.
( cd "$SRC" && cargo build --release --locked $PACKAGES )

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
# place: replacing a running binary in place fails with ETXTBSY on Linux, and
# the running binary here is routinely the TUI on the VPS updating itself.
# rename() swaps the directory entry; the process already running keeps the
# inode it started with, and picks the new one up when it restarts.
install_binary() {
  local name="$1" from="$SRC/target/release/$1" tmp="$BIN_DIR/.$1.new.$$"
  [ -f "$from" ] || err "built binary missing: $from"
  $SUDO install -m 0755 "$from" "$tmp"
  $SUDO mv -f "$tmp" "$BIN_DIR/$name"
  ok "installed $BIN_DIR/$name"
}
for b in $BINARIES; do install_binary "$b"; done

printf '%s\n' "$TARGET_REF" > "$SRC/.jod-version"
printf '%s\n' "$HEAD_COMMIT" > "$SRC/.jod-commit"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo
    info "$BIN_DIR is not on your PATH yet. Add this to your shell profile:"
    printf '    export PATH="%s:$PATH"\n' "$BIN_DIR"
    ;;
esac

echo
ok "Jod $TARGET_REF installed — $("$BIN_DIR/jod" --version 2>/dev/null || echo 'jod')"
announce_newer

# A replaced binary is not a restarted process. Everything long-running keeps
# the old inode until it is told otherwise, so say which ones those are rather
# than leaving someone to wonder why the fix they just installed isn't there.
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

cat <<EOF

  jod tui          the full-screen console
  jod run "…"      delegate one prompt to a harness
  jod update       take newer patches of $TARGET_REF later
  jod --help       everything else
EOF
