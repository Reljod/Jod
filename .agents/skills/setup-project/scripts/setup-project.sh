#!/usr/bin/env bash
#
# setup-project.sh — scaffold a repo with an AGENTS.md charter, a CLAUDE.md
# symlink, and a chosen set of Jod skills + their slash commands.
#
# The interactive "which preset / which skills" choices are made by the agent
# (or the human) BEFORE calling this; this script does the mechanical scaffold
# from flags so it is deterministic and re-runnable.
#
# Usage:
#   setup-project.sh --list
#   setup-project.sh --preset <name> [options]
#
# Options:
#   --preset <name>       Behavior preset (see --list). Default: jod
#   --skills a,b,c        Comma-separated skills to copy in. Default: none.
#                         Use "all" for every available skill.
#   --name  <str>         Project name          (fills {{PROJECT_NAME}})
#   --desc  <str>         One-line description   (fills {{PROJECT_DESC}})
#   --ticket <str>        Issue-key prefix       (fills {{TICKET_PREFIX}}, e.g. PROJ)
#   --branch <str>        Branch prefix          (fills {{BRANCH_PREFIX}}, default: claude)
#   --target <dir>        Target repo (default: current directory)
#   --no-symlink          Write CLAUDE.md as a copy instead of a symlink
#   --force               Overwrite an existing AGENTS.md / CLAUDE.md
#   -h, --help            This help
#
set -euo pipefail

# --- locate the Jod source repo (where templates + skills live) -------------
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
JOD_ROOT="$(cd -- "$SCRIPT_DIR/../../../.." && pwd)"
TPL_DIR="$JOD_ROOT/domains/coding/templates/agents"
SKILLS_SRC="$JOD_ROOT/.agents/skills"
CMDS_SRC="$JOD_ROOT/.claude/commands"
SELF_SKILL="setup-project"   # never copy the scaffolder into a target repo

# --- defaults ---------------------------------------------------------------
PRESET="jod"
SKILLS=""
PROJECT_NAME=""
PROJECT_DESC=""
TICKET_PREFIX="PROJ"
BRANCH_PREFIX="claude"
TARGET="$PWD"
DO_SYMLINK=1
FORCE=0

err()  { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '%s\n' "$*"; }

list_presets() {
  find "$TPL_DIR" -maxdepth 1 -name '*.md' ! -name 'README.md' \
    -exec basename {} .md \; | sort
}
list_skills()  {
  find "$SKILLS_SRC" -maxdepth 1 -mindepth 1 -type d -exec basename {} \; \
    | grep -vx "$SELF_SKILL" | sort
}

do_list() {
  info "Behavior presets (domains/coding/templates/agents/):"
  while read -r p; do
    [ "$p" = "$PRESET" ] && printf '  %-12s (default)\n' "$p" || printf '  %s\n' "$p"
  done < <(list_presets)
  info ""
  info "Skills available to copy in (.agents/skills/):"
  list_skills | sed 's/^/  /'
}

# --- parse args -------------------------------------------------------------
[ $# -eq 0 ] && { do_list; exit 0; }
while [ $# -gt 0 ]; do
  case "$1" in
    --list)       do_list; exit 0 ;;
    --preset)     PRESET="${2:?}"; shift 2 ;;
    --skills)     SKILLS="${2:?}"; shift 2 ;;
    --name)       PROJECT_NAME="${2:?}"; shift 2 ;;
    --desc)       PROJECT_DESC="${2:?}"; shift 2 ;;
    --ticket)     TICKET_PREFIX="${2:?}"; shift 2 ;;
    --branch)     BRANCH_PREFIX="${2:?}"; shift 2 ;;
    --target)     TARGET="${2:?}"; shift 2 ;;
    --no-symlink) DO_SYMLINK=0; shift ;;
    --force)      FORCE=1; shift ;;
    -h|--help)    sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)            err "unknown argument: $1 (try --help)" ;;
  esac
done

# --- validate ---------------------------------------------------------------
# Preset is a single filename segment — reject path traversal so --preset can
# only ever name a template under templates/agents/.
case "$PRESET" in
  ''|*/*) err "invalid preset name: '$PRESET' (must not contain '/')" ;;
esac
# Validate against the actual preset set (not just file existence) so a stray
# .md in the templates dir — e.g. README.md — can never be selected as one.
if ! list_presets | grep -qxF "$PRESET"; then
  err "unknown preset '$PRESET'. Available: $(list_presets | paste -sd', ' -)"
fi
TPL="$TPL_DIR/$PRESET.md"
[ -d "$TARGET" ] || err "target directory does not exist: $TARGET"
TARGET="$(cd -- "$TARGET" && pwd)"
[ -z "$PROJECT_NAME" ] && PROJECT_NAME="$(basename "$TARGET")"
[ -z "$PROJECT_DESC" ] && PROJECT_DESC="_A one-line description of this project. Replace me._"

# --- render AGENTS.md -------------------------------------------------------
AGENTS_OUT="$TARGET/AGENTS.md"
CLAUDE_OUT="$TARGET/CLAUDE.md"
if [ -e "$AGENTS_OUT" ] && [ "$FORCE" -ne 1 ]; then
  err "$AGENTS_OUT already exists (use --force to overwrite)"
fi

# Bash 5.x treats '&' (and '\') specially in the *replacement* half of
# ${var//pat/repl} — like sed's '&'. Escape them so a value such as
# "Acme & Co" is substituted verbatim instead of re-inserting the match.
esc_repl() { local s=$1; s=${s//\\/\\\\}; s=${s//&/\\&}; printf '%s' "$s"; }

content="$(cat "$TPL")"
content="${content//\{\{PROJECT_NAME\}\}/$(esc_repl "$PROJECT_NAME")}"
content="${content//\{\{PROJECT_DESC\}\}/$(esc_repl "$PROJECT_DESC")}"
content="${content//\{\{TICKET_PREFIX\}\}/$(esc_repl "$TICKET_PREFIX")}"
content="${content//\{\{BRANCH_PREFIX\}\}/$(esc_repl "$BRANCH_PREFIX")}"
printf '%s\n' "$content" > "$AGENTS_OUT"
info "✓ wrote $AGENTS_OUT  (preset: $PRESET)"

# --- CLAUDE.md symlink (or copy) --------------------------------------------
if [ -e "$CLAUDE_OUT" ] || [ -L "$CLAUDE_OUT" ]; then
  if [ "$FORCE" -eq 1 ]; then rm -f "$CLAUDE_OUT"; else
    err "$CLAUDE_OUT already exists (use --force to overwrite)"
  fi
fi
if [ "$DO_SYMLINK" -eq 1 ]; then
  ln -s "AGENTS.md" "$CLAUDE_OUT"
  info "✓ linked $CLAUDE_OUT -> AGENTS.md"
else
  cp "$AGENTS_OUT" "$CLAUDE_OUT"
  info "✓ copied $CLAUDE_OUT (from AGENTS.md)"
fi

# --- copy chosen skills + their slash commands ------------------------------
if [ -n "$SKILLS" ]; then
  if [ "$SKILLS" = "all" ]; then
    mapfile -t WANT < <(list_skills)
  else
    IFS=',' read -r -a WANT <<< "$SKILLS"
  fi
  mkdir -p "$TARGET/.agents/skills" "$TARGET/.claude/commands"
  for raw in "${WANT[@]}"; do
    s="$(printf '%s' "$raw" | tr -d '[:space:]')"
    [ -z "$s" ] && continue
    # A skill is a single directory name — reject path traversal so a crafted
    # --skills entry can't copy from or write to outside the intended trees.
    case "$s" in
      */*|..) info "· skipping unsafe skill name: $s"; continue ;;
    esac
    [ "$s" = "$SELF_SKILL" ] && { info "· skipping $s (the scaffolder itself)"; continue; }
    src="$SKILLS_SRC/$s"
    [ -d "$src" ] || { info "· skipping unknown skill: $s"; continue; }
    dst="$TARGET/.agents/skills/$s"
    if [ -e "$dst" ] && [ "$FORCE" -ne 1 ]; then
      info "· skill $s already present in target (use --force to overwrite)"
    else
      rm -rf "$dst"; cp -R "$src" "$dst"
      info "✓ skill  .agents/skills/$s"
    fi
    # matching slash command, if one exists in the source repo
    cmd_src="$CMDS_SRC/$s.md"
    if [ -f "$cmd_src" ]; then
      cmd_dst="$TARGET/.claude/commands/$s.md"
      if [ -e "$cmd_dst" ] && [ "$FORCE" -ne 1 ]; then
        info "· command /$s already present in target"
      else
        cp "$cmd_src" "$cmd_dst"
        info "✓ command /$s"
      fi
    fi
  done
fi

# --- next steps -------------------------------------------------------------
cat <<EOF

Done. Next steps in $TARGET:
  1. Read AGENTS.md — fill in the project description and adjust anything the
     preset assumed (ticket prefix '$TICKET_PREFIX', branch prefix '$BRANCH_PREFIX').
  2. If you copied 'setup-git-hooks', run /setup-git-hooks to install the
     local commit-message + lint gate.
  3. Commit the scaffold:  git add AGENTS.md CLAUDE.md .agents .claude
EOF
