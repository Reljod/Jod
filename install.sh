#!/usr/bin/env bash
#
# install.sh — bootstrap the Jod portable toolkit onto a new Linux/macOS
# machine, no clone-and-remember-the-path required.
#
#   curl -fsSL https://raw.githubusercontent.com/Reljod/Jod/main/install.sh | bash
#
# Clones (or updates) this repo into $JOD_HOME, checks out a version, and
# links a small `jod` CLI onto $JOD_BIN_DIR, so any repo on the machine can
# run, e.g.:
#
#   cd ~/code/some-other-repo
#   jod setup-project --list
#   jod setup-project --preset jod --skills create-pr,setup-git-hooks,tdd-loop
#
# Versioning: releases are tagged vMAJOR.MINOR.PATCH (see
# .github/workflows/release.yml — run it manually to cut one). By default
# this installs the newest tag; pin an older one with JOD_VERSION.
#
#   curl -fsSL .../install.sh | bash                      # latest release
#   curl -fsSL .../install.sh | JOD_VERSION=v1.2.0 bash    # pinned release
#   curl -fsSL .../install.sh | JOD_VERSION=main bash      # bleeding edge
#
# `jod update` later only ever moves within that same MAJOR.MINOR — it takes
# new patches automatically but never jumps you to a new minor/major release
# out from under you. Re-run install.sh with a new JOD_VERSION for that.
#
# Safe to re-run: it fetches into the existing checkout and relinks the CLI
# rather than re-cloning or overwriting local changes.
#
# Env overrides:
#   JOD_REPO_URL   git remote to clone      (default: github.com/Reljod/Jod)
#   JOD_VERSION    version/ref to install   (default: latest)
#                  "latest" | vX.Y.Z | a branch or commit SHA
#   JOD_REF        fallback branch when no release tags exist yet, and the
#                  branch `git clone` starts on (default: main)
#   JOD_HOME       where the toolkit lives       (default: $HOME/.jod)
#   JOD_BIN_DIR    where the `jod` CLI is linked  (default: $HOME/.local/bin)
set -euo pipefail

REPO_URL="${JOD_REPO_URL:-https://github.com/Reljod/Jod.git}"
VERSION="${JOD_VERSION:-latest}"
REPO_REF="${JOD_REF:-main}"
JOD_HOME="${JOD_HOME:-$HOME/.jod}"
BIN_DIR="${JOD_BIN_DIR:-$HOME/.local/bin}"

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
if [ -d "$JOD_HOME/.git" ]; then
  info "Fetching updates into existing checkout at $JOD_HOME"
  git -C "$JOD_HOME" fetch --quiet --tags --force origin
elif [ -e "$JOD_HOME" ]; then
  err "$JOD_HOME exists and is not a git checkout — remove it or set \$JOD_HOME"
else
  info "Cloning $REPO_URL into $JOD_HOME"
  git clone --quiet --branch "$REPO_REF" "$REPO_URL" "$JOD_HOME"
fi

# shellcheck source=bin/lib/semver.sh
source "$JOD_HOME/bin/lib/semver.sh"

resolve_version() {
  case "$1" in
    latest)
      local tag
      tag="$(git -C "$JOD_HOME" tag --list 'v*.*.*' | highest_semver_tag)"
      if [ -z "$tag" ]; then
        info "no release tags found yet — using the '$REPO_REF' branch" >&2
        printf '%s' "$REPO_REF"
      else
        printf '%s' "$tag"
      fi
      ;;
    [0-9]*.[0-9]*.[0-9]*) printf 'v%s' "$1" ;;   # bare X.Y.Z -> vX.Y.Z
    *) printf '%s' "$1" ;;                        # vX.Y.Z, a branch, or a SHA
  esac
}
TARGET_REF="$(resolve_version "$VERSION")"

info "Checking out $TARGET_REF"
git -C "$JOD_HOME" checkout --quiet "$TARGET_REF" 2>/dev/null \
  || git -C "$JOD_HOME" checkout --quiet -B "$TARGET_REF" "origin/$TARGET_REF" 2>/dev/null \
  || err "unknown version/ref: $TARGET_REF"
# If it's a branch, fast-forward it to origin so re-running stays current.
if git -C "$JOD_HOME" show-ref --verify --quiet "refs/remotes/origin/$TARGET_REF"; then
  git -C "$JOD_HOME" merge --quiet --ff-only "origin/$TARGET_REF"
fi
# Record what's installed explicitly rather than inferring it later — tags
# cut back-to-back can land on the same commit, which makes `git describe`
# ambiguous about which one you actually asked for.
printf '%s\n' "$TARGET_REF" > "$JOD_HOME/.jod-version"
ok "toolkit at $JOD_HOME ($TARGET_REF)"

mkdir -p "$BIN_DIR"
chmod +x "$JOD_HOME/bin/jod"
ln -sf "$JOD_HOME/bin/jod" "$BIN_DIR/jod"
ok "linked $BIN_DIR/jod -> $JOD_HOME/bin/jod"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo
    info "$BIN_DIR is not on your PATH yet. Add this to your shell profile:"
    printf '    export PATH="%s:$PATH"\n' "$BIN_DIR"
    ;;
esac

cat <<EOF

✓ Jod toolkit installed ($TARGET_REF).

Next, in the repo you want to set up:
  cd /path/to/your/repo
  jod setup-project --list
  jod setup-project --preset jod --skills create-pr,setup-git-hooks,tdd-loop

Run 'jod help' for all commands, 'jod version' to see what's installed, or
'jod update' later to take newer patches of this same release.
EOF
