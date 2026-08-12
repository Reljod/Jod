#!/usr/bin/env bash
#
# arm_schedule.sh — put the sweep on jod's clock, deterministically.
#
# Two commands, and the second is the one that matters:
#
#   jod schedule add   — a name, a cron expression, a timezone.
#   jod monitor set --no-agent
#                      — "the script is the whole job: its stdout is the result
#                        and no model is ever woken. Empty stdout means stay
#                        quiet."
#
# Without `--no-agent` a schedule fires a *prompt*, and cleaning up disk would
# mean waking a model hourly to re-derive a decision that is already settled in
# shell. With it, the hourly cost is one `df` and a `find`, and the ledger gains
# an entry only on an hour that actually freed something.
#
# Idempotent: run it twice and the second run replaces the schedule rather than
# adding a duplicate.
#
# Run:  arm_schedule.sh                 # arm it
#       arm_schedule.sh --dry-run       # print the exact commands, run nothing
set -uo pipefail

NAME="reclaim-disk"
CRON="17 * * * *"          # hourly, off the hour: nothing else here fires at :17
TZ_NAME="Asia/Manila"      # a zone name, never an offset — see core/src/schedule.rs
MIN_FREE_GB=8
IDLE_MINUTES=90
DRY_RUN=0

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --cron) CRON="${2:-}"; shift ;;
    --timezone) TZ_NAME="${2:-}"; shift ;;
    --min-free-gb) MIN_FREE_GB="${2:-}"; shift ;;
    --idle-minutes) IDLE_MINUTES="${2:-}"; shift ;;
    --name) NAME="${2:-}"; shift ;;
    -h|--help)
      sed -n '2,/^set -uo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'; exit 0 ;;
    *) printf 'arm_schedule.sh: unknown argument %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

command -v jod >/dev/null 2>&1 || {
  printf 'arm_schedule.sh: jod is not on PATH — install it first\n' >&2; exit 1; }

# The repo the sweep should walk, resolved here so the recorded command is
# absolute and does not depend on where the daemon happens to be running.
repo_root="$(git -C "$script_dir" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
[ -n "$repo_root" ] && repo_root="$(dirname -- "$repo_root")"
[ -n "$repo_root" ] || { printf 'arm_schedule.sh: cannot locate the repository\n' >&2; exit 1; }

# A schedule outlives the session that armed it, so the probe must not point at a
# path that session owns. Run from a worktree under .claude/worktrees/, this
# script's own directory is deleted when the worktree is — and a monitor whose
# command has vanished fails silently every hour, which is worse than never
# having armed it. So the durable copy in the shared checkout wins whenever it
# exists, and only a repo without one falls back to this script's own directory.
#
# That twin sits at the same path relative to the checkout root, so it is derived
# from this script's own location rather than spelled out: a skill that writes its
# own repo path stops working the moment it is installed anywhere else, which is
# what tests/plugin.test.sh enforces.
worktree_root="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null || true)"
rel_dir="${script_dir#"$worktree_root"/}"
SWEEP=""
if [ -n "$worktree_root" ] && [ "$rel_dir" != "$script_dir" ]; then
  SWEEP="$repo_root/$rel_dir/sweep_targets.sh"
fi
# An underivable twin falls through to the same guard as a missing one, rather
# than quietly arming the copy that dies with this worktree.
if [ -z "$SWEEP" ] || [ ! -x "$SWEEP" ]; then
  SWEEP="$script_dir/sweep_targets.sh"
  case "$SWEEP" in
    */.claude/worktrees/*)
      printf 'arm_schedule.sh: refusing to arm a probe inside a worktree.\n' >&2
      printf '  %s\n' "$SWEEP" >&2
      printf '  That path dies with the worktree and the monitor would then fail\n' >&2
      printf '  silently every hour. Land the change first, then arm from %s.\n' "$repo_root" >&2
      exit 1 ;;
  esac
fi
[ -x "$SWEEP" ] || { printf 'arm_schedule.sh: %s is not executable\n' "$SWEEP" >&2; exit 1; }

PROBE="$SWEEP --apply --root $repo_root --min-free-gb $MIN_FREE_GB --idle-minutes $IDLE_MINUTES"

run() {
  if [ "$DRY_RUN" -eq 1 ]; then
    printf '  %s\n' "$*"
  else
    "$@" || return 1
  fi
}

[ "$DRY_RUN" -eq 1 ] && printf 'would run:\n'

# `schedule add` refuses a duplicate name, so an existing one is removed first.
# `|| true`: a missing schedule is the normal case on a first run.
if [ "$DRY_RUN" -eq 1 ]; then
  printf '  jod schedule rm %s   # if it exists\n' "$NAME"
else
  jod schedule rm "$NAME" >/dev/null 2>&1 || true
fi

# The prompt is never sent — `--no-agent` means no model runs — but a schedule
# requires one, and it is what `jod schedule ls` shows a human. So it says what
# the schedule is for.
run jod schedule add "$NAME" \
  "Reclaim disk by deleting cargo build output no worktree is using. Handled entirely by sweep_targets.sh; no agent runs." \
  --cron "$CRON" \
  --timezone "$TZ_NAME" \
  --cwd "$repo_root" \
  --misfire fire_once \
  --overlap skip || { printf 'arm_schedule.sh: could not add the schedule\n' >&2; exit 1; }

# shellcheck disable=SC2086  # PROBE is one command line and must word-split
run jod monitor set "$NAME" --command "$PROBE" --cwd "$repo_root" --no-agent \
  || { printf 'arm_schedule.sh: could not attach the monitor\n' >&2; exit 1; }

if [ "$DRY_RUN" -eq 0 ]; then
  printf 'armed %s — %s (%s), sweeping below %s GB free\n' \
    "$NAME" "$CRON" "$TZ_NAME" "$MIN_FREE_GB"
  printf 'verify with:  jod monitor check %s\n' "$NAME"
fi
