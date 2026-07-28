#!/usr/bin/env bash
#
# setup-project.sh — scaffold a repo with an AGENTS.md charter, a CLAUDE.md
# symlink, and a chosen set of Jod skills + their slash commands.
#
# Run with no arguments on a terminal, it walks the choices interactively
# (↑/↓ to move, space to toggle skills, enter to confirm). Every choice is
# also a flag, so the same scaffold is scriptable and re-runnable — the
# wizard only decides the values, never what the scaffold does with them.
#
# Usage:
#   setup-project.sh                       # interactive (or --list if no tty)
#   setup-project.sh --list
#   setup-project.sh --preset <name> [options]
#
# Options:
#   -i, --interactive     Force the interactive wizard
#   --no-interactive      Never prompt; use flags/defaults only
#   --preset <name>       Behavior preset (see --list). Default: jod
#   --skills a,b,c        Comma-separated skills to copy in. Default: none.
#                         Use "all" for every available skill.
#   --name  <str>         Project name          (fills {{PROJECT_NAME}})
#   --desc  <str>         One-line description   (fills {{PROJECT_DESC}})
#   --ticket <str>        Issue-key prefix, e.g. PROJ. Opt-in: without it the
#                         scaffolded charter asks for no ticket in commits.
#   --branch <str>        Branch prefix          (fills {{BRANCH_PREFIX}}, default: claude)
#   --target <dir>        Target repo (default: current directory)
#   --no-symlink          Write CLAUDE.md as a copy instead of a symlink
#   --force               Overwrite an existing AGENTS.md / CLAUDE.md
#   -h, --help            This help
#
set -euo pipefail

# --- locate sources ---------------------------------------------------------
# Templates ship *inside* this skill, so the whole toolkit under .agents/ stays
# copyable into any repo without reaching into domains/ (the charter's rule).
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TPL_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)/templates/agents"
JOD_ROOT="$(cd -- "$SCRIPT_DIR/../../../.." && pwd)"
SKILLS_SRC="$JOD_ROOT/.agents/skills"
CMDS_SRC="$JOD_ROOT/.claude/commands"
SELF_SKILL="setup-project"   # never copy the scaffolder into a target repo

# --- defaults ---------------------------------------------------------------
PRESET="jod"
SKILLS=""
PROJECT_NAME=""
PROJECT_DESC=""
TICKET_PREFIX=""            # empty = no issue-key rule in the charter
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

# One-line summaries, read from where they already live so nothing goes
# stale: a preset's own `<!-- blurb: ... -->` header (stripped at render
# time), and a skill's slash-command frontmatter.
preset_blurb() {
  sed -n '/^<!-- blurb:/{s/^<!-- blurb:[[:space:]]*//; s/[[:space:]]*-->[[:space:]]*$//; p;}' \
    "$TPL_DIR/$1.md" 2>/dev/null | head -n1
}
skill_blurb() {
  [ -f "$CMDS_SRC/$1.md" ] || return 0
  sed -n 's/^description:[[:space:]]*//p' "$CMDS_SRC/$1.md" | head -n1
}
clip() {
  local s="$1" n="${2:-56}"
  if [ "${#s}" -gt "$n" ]; then printf '%s…' "${s:0:$((n - 1))}"; else printf '%s' "$s"; fi
}

do_list() {
  info "Behavior presets (setup-project/templates/agents/):"
  while read -r p; do
    if [ "$p" = "$PRESET" ]; then
      printf '  %-12s (default)  %s\n' "$p" "$(preset_blurb "$p")"
    else
      printf '  %-12s            %s\n' "$p" "$(preset_blurb "$p")"
    fi
  done < <(list_presets)
  info ""
  info "Skills available to copy in (.agents/skills/):"
  while read -r s; do
    printf '  %-16s %s\n' "$s" "$(clip "$(skill_blurb "$s")" 60)"
  done < <(list_skills)
}

# --- interactive wizard ------------------------------------------------------
# Collects the same values the flags carry. Nothing below this function
# branches on "was it interactive" — the wizard just fills the variables.
run_interactive() {
  # shellcheck source=lib/prompt.sh
  source "$SCRIPT_DIR/lib/prompt.sh"
  prompt_have_tty || err "no terminal available — pass --preset/--skills, or --list"
  prompt_begin || err "could not open the terminal for input"
  trap prompt_end EXIT INT TERM

  info ""
  info "  Jod · setup-project — scaffolding $(basename "$TARGET")"
  info ""

  local p s opts=() chosen preselect
  while read -r p; do opts+=("$p|$(clip "$(preset_blurb "$p")" 60)"); done < <(list_presets)
  if ! PRESET="$(prompt_select_one "Behavior preset" "$PRESET" "${opts[@]}")"; then
    err "cancelled — nothing written"
  fi

  # Preselect what this preset's charter actually leans on, per the skill's
  # recommendation: everything for jod/team, the test-first trio for
  # tdd-strict, nothing for minimal (it exists to stay lean).
  case "$PRESET" in
    minimal)    preselect="" ;;
    tdd-strict) preselect="tdd-loop,test-scenarios,setup-git-hooks" ;;
    *)          preselect="$(list_skills | paste -sd',' -)" ;;
  esac
  opts=()
  while read -r s; do opts+=("$s|$(clip "$(skill_blurb "$s")" 56)"); done < <(list_skills)
  if ! chosen="$(prompt_select_many "Skills to copy in" "$preselect" "${opts[@]}")"; then
    err "cancelled — nothing written"
  fi
  SKILLS="$(printf '%s' "$chosen" | tr '\n' ',' | sed 's/,$//')"

  PROJECT_NAME="$(prompt_text "Project name" "$(basename "$TARGET")")"
  PROJECT_DESC="$(prompt_text "One-line description" "")"
  BRANCH_PREFIX="$(prompt_text "Branch prefix" "$BRANCH_PREFIX")"
  # Opt-in, and phrased so blank is the obvious answer: a required issue key
  # is a house rule, not a default (see the charter's Design choices).
  TICKET_PREFIX="$(prompt_text "Issue-key prefix for commits, blank for none" "")"

  info ""
  info "  target   $TARGET"
  info "  preset   $PRESET"
  info "  skills   ${SKILLS:-(none)}"
  info "  name     $PROJECT_NAME"
  info "  branch   $BRANCH_PREFIX/<short-description>"
  info "  commits  <type>: ${TICKET_PREFIX:+$TICKET_PREFIX-12 }<subject>"
  info ""

  if [ -e "$TARGET/AGENTS.md" ] && [ "$FORCE" -ne 1 ]; then
    if prompt_confirm "AGENTS.md already exists — overwrite it?" n; then
      FORCE=1
    else
      err "kept the existing AGENTS.md — nothing written"
    fi
  fi
  prompt_confirm "Scaffold this?" y || err "cancelled — nothing written"
  prompt_end
  info ""
}

# --- parse args -------------------------------------------------------------
# Interactive when nobody has already answered the questions: any flag that
# carries one of the wizard's *choices* (preset, skills, name, ...) means the
# caller is scripting this, so don't prompt. Flags that only say where and
# how to write (--target, --force, --no-symlink) leave the wizard on.
# With no terminal at all, `auto` falls back to --list rather than hanging.
INTERACTIVE=auto
CHOSE_BY_FLAG=0
while [ $# -gt 0 ]; do
  case "$1" in
    -i|--interactive)  INTERACTIVE=1; shift ;;
    --no-interactive)  INTERACTIVE=0; shift ;;
    --list)       do_list; exit 0 ;;
    --preset)     PRESET="${2:?}";        CHOSE_BY_FLAG=1; shift 2 ;;
    --skills)     SKILLS="${2:?}";        CHOSE_BY_FLAG=1; shift 2 ;;
    --name)       PROJECT_NAME="${2:?}";  CHOSE_BY_FLAG=1; shift 2 ;;
    --desc)       PROJECT_DESC="${2:?}";  CHOSE_BY_FLAG=1; shift 2 ;;
    --ticket)     TICKET_PREFIX="${2:?}"; CHOSE_BY_FLAG=1; shift 2 ;;
    --branch)     BRANCH_PREFIX="${2:?}"; CHOSE_BY_FLAG=1; shift 2 ;;
    --target)     TARGET="${2:?}"; shift 2 ;;
    --no-symlink) DO_SYMLINK=0; shift ;;
    --force)      FORCE=1; shift ;;
    -h|--help)    awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"; exit 0 ;;
    *)            err "unknown argument: $1 (try --help)" ;;
  esac
done

# --- validate ---------------------------------------------------------------
# The target has to be resolved before the wizard runs — it defaults the
# project name from the directory and reports where it is about to write.
[ -d "$TARGET" ] || err "target directory does not exist: $TARGET"
TARGET="$(cd -- "$TARGET" && pwd)"

if [ "$INTERACTIVE" = auto ] && [ "$CHOSE_BY_FLAG" -eq 1 ]; then INTERACTIVE=0; fi
case "$INTERACTIVE" in
  1)    run_interactive ;;
  auto)
    # shellcheck source=lib/prompt.sh
    source "$SCRIPT_DIR/lib/prompt.sh"
    if prompt_have_tty; then run_interactive; else do_list; exit 0; fi
    ;;
esac

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

# The leading `<!-- blurb: ... -->` line is picker metadata, not charter
# text — drop it so it never lands in a scaffolded AGENTS.md.
content="$(sed '/^<!-- blurb:.*-->[[:space:]]*$/d' "$TPL")"
content="${content//\{\{PROJECT_NAME\}\}/$(esc_repl "$PROJECT_NAME")}"
content="${content//\{\{PROJECT_DESC\}\}/$(esc_repl "$PROJECT_DESC")}"
content="${content//\{\{BRANCH_PREFIX\}\}/$(esc_repl "$BRANCH_PREFIX")}"

# Ticket keys are opt-in. Without --ticket, every line carrying the
# {{TICKET_RULE}} token is dropped outright, so the scaffolded charter never
# asks for an issue key a repo may have no tracker for.
if [ -n "$TICKET_PREFIX" ]; then
  TICKET_RULE="Reference the issue key (e.g. \`$TICKET_PREFIX-12\`) right after the colon —
  \`feat: $TICKET_PREFIX-12 add retry\`. Set \`TICKET_REGEX\` in \`setup-git-hooks\` to enforce it."
  content="${content//\{\{TICKET_RULE\}\}/$(esc_repl "$TICKET_RULE")}"
else
  # cat -s: dropping the line can leave the blank lines that surrounded it
  # stacked, so squeeze runs of blanks back down to one.
  content="$(printf '%s\n' "$content" | grep -vF '{{TICKET_RULE}}' | cat -s || true)"
fi
content="${content//\{\{TICKET_PREFIX\}\}/$(esc_repl "$TICKET_PREFIX")}"
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
    # Not mapfile: stock macOS ships bash 3.2, which lacks it.
    WANT=()
    while IFS= read -r s; do WANT+=("$s"); done < <(list_skills)
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
TICKET_NOTE=""
if [ -n "$TICKET_PREFIX" ]; then TICKET_NOTE=", ticket prefix '$TICKET_PREFIX'"; fi
cat <<EOF

Done. Next steps in $TARGET:
  1. Read AGENTS.md — fill in the project description and adjust anything the
     preset assumed (branch prefix '$BRANCH_PREFIX'$TICKET_NOTE).
  2. If you copied 'setup-git-hooks', run /setup-git-hooks to install the
     local commit-message + lint gate.
  3. Commit the scaffold:  git add AGENTS.md CLAUDE.md .agents .claude
EOF
