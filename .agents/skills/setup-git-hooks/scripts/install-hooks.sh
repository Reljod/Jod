#!/usr/bin/env bash
# Install the deterministic git hooks into a target repo via core.hooksPath.
#
# Usage: install-hooks.sh [target-repo-dir] [--force]
#   target-repo-dir   repo to install into (default: current directory)
#   --force           overwrite an existing commit-convention.conf
#
# Copies the hook templates into <repo>/.githooks/, wires up
# core.hooksPath, makes the hooks executable, and pre-fills
# commit-convention.conf with lint/format commands detected for the repo's
# ecosystem. Idempotent: re-running preserves your tuned conf unless
# --force is given.
set -euo pipefail

SKILL_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_HOOKS="$SKILL_DIR/hooks"
SRC_CONF="$SKILL_DIR/templates/commit-convention.conf"

TARGET="."
FORCE=0
for arg in "$@"; do
  case "$arg" in
    --force) FORCE=1 ;;
    *) TARGET="$arg" ;;
  esac
done

TARGET="$(cd -- "$TARGET" && pwd)"
if ! git -C "$TARGET" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "✗ $TARGET is not a git repository." >&2
  exit 1
fi
REPO_ROOT="$(git -C "$TARGET" rev-parse --show-toplevel)"
DEST="$REPO_ROOT/.githooks"

echo "→ Installing hooks into $DEST"
mkdir -p "$DEST"

for hook in commit-msg pre-commit pre-push; do
  cp "$SRC_HOOKS/$hook" "$DEST/$hook"
  chmod +x "$DEST/$hook"
  echo "  • $hook"
done

# --- ecosystem detection: pre-fill PRECOMMIT_CMDS -------------------------
detect_precommit_cmds() {
  local cmds=""
  if [ -f "$REPO_ROOT/package.json" ]; then
    if grep -q '"lint-staged"' "$REPO_ROOT/package.json" 2>/dev/null; then
      cmds="npx lint-staged"
    elif grep -q '"lint"' "$REPO_ROOT/package.json" 2>/dev/null; then
      cmds="npm run lint"
    fi
  fi
  if [ -f "$REPO_ROOT/pyproject.toml" ] || [ -f "$REPO_ROOT/ruff.toml" ] || [ -f "$REPO_ROOT/.ruff.toml" ]; then
    cmds="${cmds:+$cmds$'\n'}ruff check --force-exclude \$(git diff --cached --name-only --diff-filter=ACM -- '*.py')
ruff format --check --force-exclude \$(git diff --cached --name-only --diff-filter=ACM -- '*.py')"
  fi
  if [ -f "$REPO_ROOT/go.mod" ]; then
    cmds="${cmds:+$cmds$'\n'}gofmt -l \$(git diff --cached --name-only --diff-filter=ACM -- '*.go')"
  fi
  printf '%s' "$cmds"
}

CONF_DEST="$DEST/commit-convention.conf"
if [ -f "$CONF_DEST" ] && [ "$FORCE" -ne 1 ]; then
  echo "  • commit-convention.conf already exists — kept (pass --force to overwrite)"
else
  cp "$SRC_CONF" "$CONF_DEST"
  DETECTED="$(detect_precommit_cmds || true)"
  if [ -n "$DETECTED" ]; then
    # Replace the empty PRECOMMIT_CMDS="" with the detected block.
    python3 - "$CONF_DEST" "$DETECTED" <<'PY'
import sys
path, detected = sys.argv[1], sys.argv[2]
with open(path) as f:
    text = f.read()
text = text.replace('PRECOMMIT_CMDS=""', 'PRECOMMIT_CMDS="%s"' % detected)
with open(path, "w") as f:
    f.write(text)
PY
    echo "  • commit-convention.conf written (detected pre-commit commands)"
  else
    echo "  • commit-convention.conf written (no linters auto-detected; edit PRECOMMIT_CMDS)"
  fi
fi

git -C "$REPO_ROOT" config core.hooksPath .githooks
echo "→ Set core.hooksPath = .githooks"

cat <<EOF

✓ Hooks installed. Verify both paths are deterministic:

    echo "update stuff"              | "$DEST/commit-msg" /dev/stdin   # expect FAIL
    echo "feat: ENG-123 add retries" | "$DEST/commit-msg" /dev/stdin   # expect PASS

Next:
  1. Review .githooks/commit-convention.conf (types, ticket regex, commands).
  2. Commit the .githooks/ directory so teammates inherit the hooks.
  3. Note: core.hooksPath is LOCAL config. Each fresh clone must run this
     installer once, or:  git config core.hooksPath .githooks
EOF
