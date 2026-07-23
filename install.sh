#!/usr/bin/env bash
#
# install.sh — bootstrap the Jod portable toolkit onto a new Linux/macOS
# machine, no clone-and-remember-the-path required.
#
#   curl -fsSL https://raw.githubusercontent.com/Reljod/Jod/main/install.sh | bash
#
# Clones (or fast-forwards) this repo into $JOD_HOME and links a small `jod`
# CLI onto $JOD_BIN_DIR, so any repo on the machine can run, e.g.:
#
#   cd ~/code/some-other-repo
#   jod setup-project --list
#   jod setup-project --preset jod --skills create-pr,setup-git-hooks,tdd-loop
#
# Safe to re-run: it fast-forwards the existing checkout and relinks the CLI
# rather than re-cloning or overwriting local changes.
#
# Env overrides:
#   JOD_REPO_URL   git remote to clone (default: github.com/Reljod/Jod)
#   JOD_REF        branch to track      (default: main)
#   JOD_HOME       where the toolkit lives (default: $HOME/.jod)
#   JOD_BIN_DIR    where the `jod` CLI is linked (default: $HOME/.local/bin)
set -euo pipefail

REPO_URL="${JOD_REPO_URL:-https://github.com/Reljod/Jod.git}"
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

if [ -d "$JOD_HOME/.git" ]; then
  info "Updating existing toolkit checkout at $JOD_HOME"
  git -C "$JOD_HOME" fetch --quiet origin "$REPO_REF"
  git -C "$JOD_HOME" checkout --quiet "$REPO_REF"
  git -C "$JOD_HOME" merge --quiet --ff-only "origin/$REPO_REF"
elif [ -e "$JOD_HOME" ]; then
  err "$JOD_HOME exists and is not a git checkout — remove it or set \$JOD_HOME"
else
  info "Cloning $REPO_URL@$REPO_REF into $JOD_HOME"
  git clone --quiet --branch "$REPO_REF" "$REPO_URL" "$JOD_HOME"
fi
ok "toolkit at $JOD_HOME"

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

✓ Jod toolkit installed.

Next, in the repo you want to set up:
  cd /path/to/your/repo
  jod setup-project --list
  jod setup-project --preset jod --skills create-pr,setup-git-hooks,tdd-loop

Run 'jod help' for all commands, or 'jod update' later to pull the latest
toolkit changes.
EOF
