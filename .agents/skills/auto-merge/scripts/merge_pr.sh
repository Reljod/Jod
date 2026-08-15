#!/usr/bin/env bash
# Merges a pull request only if every unattended-merge precondition holds.
#
# Usage: merge_pr.sh <pr-number> [--repo owner/name] [--method squash|rebase]
#                    [--dry-run] [--ready] [--update-branch] [--base <ref>]
#
# Exit 0 = merged (or, with --dry-run, would have merged). Exit 1 = refused,
# with every reason printed. Refusing is the normal outcome, not an error.
#
# The exit code tracks one thing only: whether the pull request merged. It does
# not track whether the branch cleanup afterwards worked. `gh pr merge
# --delete-branch` fails that cleanup every time this runs inside a git
# worktree, and a merged PR reported as a failure is worse than no gate, since
# the charter tells agents to obey this exit code. See the merge step at the
# bottom for how that is separated.
#
# History stays linear: only `squash` and `rebase` are accepted, and a branch
# that is behind its base is refused rather than merged. `--merge` is not an
# option here — a merge commit is the one method that can't be replayed as a
# straight line, and "no merge commits" is only true if nothing can create one.
#
# Why a script and not a checklist in prose: the agent running this is often
# the same agent that wrote the PR, and "check the boxes, then merge" puts the
# author in charge of grading itself. Here the conditions are evaluated before
# `gh pr merge` is reachable at all, so the model cannot merge by being
# persuaded — only by every check actually passing.
#
# It re-derives the triage verdict from the diff instead of reading the
# `merge:auto` label. Labels are mutable by anyone with write access (an agent
# included); the diff is the thing the verdict is actually about.
set -euo pipefail

pr=""
repo=""
method="squash"
dry_run=""
ready=""
update_branch=""
base_override=""

die() { echo "error: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --repo)    repo="${2:-}"; shift 2 ;;
    --method)  method="${2:-}"; shift 2 ;;
    --base)    base_override="${2:-}"; shift 2 ;;
    --dry-run) dry_run="1"; shift ;;
    --ready) ready="1"; shift ;;
    --update-branch) update_branch="1"; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    -*)        die "Unknown flag: $1" ;;
    *)         [ -n "$pr" ] && die "Unexpected argument: $1"; pr="$1"; shift ;;
  esac
done

[ -n "$pr" ] || die "Usage: $0 <pr-number> [--dry-run]"
case "$pr" in *[!0-9]*) die "PR number must be numeric, got: $pr" ;; esac
case "$method" in
  squash|rebase) ;;
  merge) die "--method merge would create a merge commit; this repo keeps history linear (use squash or rebase)" ;;
  *) die "--method must be squash or rebase" ;;
esac
command -v gh >/dev/null || die "gh is required"
command -v jq >/dev/null || die "jq is required"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TRIAGE="$SCRIPT_DIR/pr_triage.sh"
[ -x "$TRIAGE" ] || die "pr_triage.sh not found next to this script"

gh_args=(--json 'number,title,state,isDraft,mergeable,mergeStateStatus,reviewDecision,baseRefName,headRefOid,headRefName,url,statusCheckRollup')
[ -n "$repo" ] && gh_args+=(--repo "$repo")

pr_json="$(gh pr view "$pr" "${gh_args[@]}")" || die "could not read PR #$pr"

field() { printf '%s' "$pr_json" | jq -r "$1"; }

title="$(field .title)"
url="$(field .url)"
base="${base_override:-$(field .baseRefName)}"
head_sha="$(field .headRefOid)"

refusals=""
refuse() { refusals="$refusals - $1
"; }

echo "PR #$pr — $title"
echo "$url"
echo

# --- 1. the PR is in a mergeable state ---------------------------------------

[ "$(field .state)" = "OPEN" ] || refuse "PR is $(field .state), not OPEN"

# Draft is the repo's default, so an agent finishing its own work has to
# publish before it can merge. `--ready` is that opt-in, and it stays an
# explicit flag rather than an implicit side effect: un-drafting is what
# tells everyone watching the PR that it is finished, and a script should
# not announce that on the author's behalf unless asked.
was_draft=""
if [ "$(field .isDraft)" != "false" ]; then
  if [ -n "$ready" ]; then
    was_draft="1"
  else
    refuse "PR is a draft — publishing it is the author's call (pass --ready)"
  fi
fi

case "$(field .mergeable)" in
  MERGEABLE) ;;
  CONFLICTING) refuse "PR has merge conflicts against $base" ;;
  *) refuse "GitHub has not finished computing mergeability (state: $(field .mergeable)); retry shortly" ;;
esac

case "$(field .reviewDecision)" in
  CHANGES_REQUESTED) refuse "a reviewer requested changes" ;;
  REVIEW_REQUIRED)   refuse "branch protection requires a review that has not been given" ;;
esac

# --- 2. every required check is green ----------------------------------------
#
# Pending is refused, not waited on: an unattended merge that polls is an
# unattended merge with an unbounded window in which someone pushes.

checks="$(printf '%s' "$pr_json" | jq -r '
  (.statusCheckRollup // [])[]
  | if .__typename == "CheckRun"
    then "\(.name)\t\(if .status != "COMPLETED" then "PENDING" else (.conclusion // "NEUTRAL") end)"
    else "\(.context)\t\(.state // "PENDING")" end')"

if [ -z "$checks" ]; then
  refuse "no CI checks reported on the head commit — nothing to trust"
else
  while IFS='	' read -r name state; do
    [ -z "$name" ] && continue
    case "$state" in
      SUCCESS|NEUTRAL|SKIPPED) echo "  ok       $name" ;;
      PENDING|EXPECTED|QUEUED|IN_PROGRESS)
        echo "  pending  $name"; refuse "check still running: $name" ;;
      *) echo "  FAILED   $name"; refuse "check not green: $name ($state)" ;;
    esac
  done <<< "$checks"
  echo
fi

# --- 3. the branch is a straight line on top of base -------------------------
#
# A branch that is behind base was tested against a tree that no longer
# exists: its green checks describe the old base, not the merge result. Squash
# and rebase both replay the commits onto current base without re-running
# anything, so "green" and "behind" together is exactly how a broken main
# happens. Refuse, rebase, let CI run again on the real thing.

# Fetch what the rest of this script needs to exist locally: the base branch,
# and the PR head. Asking for a bare SHA only works on servers configured to
# allow it, but `refs/pull/<n>/head` is always fetchable — which is what lets
# this run against a PR that was never checked out here, as the sweep does.
git fetch --quiet origin "$base" 2>/dev/null || true
git fetch --quiet origin "pull/$pr/head" 2>/dev/null || true
git fetch --quiet origin "$head_sha" 2>/dev/null || true

behind=""
case "$(field .mergeStateStatus)" in
  BEHIND) behind="1"; refuse "branch is behind $base — rebase it and let CI re-run" ;;
  DIRTY)  refuse "branch does not apply cleanly onto $base" ;;
  BLOCKED) refuse "branch protection is not satisfied (mergeStateStatus: BLOCKED)" ;;
esac

if git rev-parse --verify --quiet "origin/$base" >/dev/null 2>&1 \
   && git rev-parse --verify --quiet "$head_sha" >/dev/null 2>&1; then
  n_behind="$(git rev-list --count "$head_sha..origin/$base" 2>/dev/null || echo 0)"
  echo "  base     $base is $n_behind commit(s) ahead of this branch"
  if [ "$n_behind" -gt 0 ] && [ -z "$behind" ]; then
    behind="1"
    refuse "branch is $n_behind commit(s) behind $base — rebase it and let CI re-run"
  fi
fi

# Rebase and stop. Merging in the same breath would merge on the checks that
# ran before the rebase, which is the failure this whole section exists to
# prevent.
if [ -n "$behind" ] && [ -n "$update_branch" ]; then
  echo
  echo "Updating the branch by rebase; re-run this script once CI is green."
  up_args=(--rebase)
  [ -n "$repo" ] && up_args+=(--repo "$repo")
  gh pr update-branch "$pr" "${up_args[@]}"
  exit 1
fi

# --- 4. the diff itself says a human is not needed ---------------------------

if git rev-parse --verify --quiet "$head_sha" >/dev/null 2>&1 \
   && git rev-parse --verify --quiet "origin/$base" >/dev/null 2>&1; then
  triage="$("$TRIAGE" "origin/$base...$head_sha" --format env)"
  verdict="$(printf '%s' "$triage" | sed -n 's/^verdict=//p')"
  cats="$(printf '%s' "$triage" | sed -n 's/^categories=//p')"
  echo "  triage   $verdict (categories: ${cats:-none})"
  echo
  [ "$verdict" = "auto-merge" ] \
    || refuse "triage says human-review — run pr_triage.sh for the reasons"
else
  refuse "could not fetch $base and $head_sha locally to re-derive the verdict"
fi

# --- verdict -----------------------------------------------------------------

if [ -n "$refusals" ]; then
  echo "REFUSED to merge PR #$pr:"
  printf '%s' "$refusals"
  echo
  echo "None of these are bypassable from here. Fix the cause, or hand the PR"
  echo "to a human and say which reason sent it there."
  [ -n "$behind" ] && echo "For the behind-base reason: re-run with --update-branch."
  exit 1
fi

if [ -n "$dry_run" ]; then
  echo "DRY RUN — every precondition holds; would run:"
  [ -n "$was_draft" ] && echo "  gh pr ready $pr"
  echo "  gh pr merge $pr --$method --delete-branch"
  exit 0
fi

if [ -n "$was_draft" ]; then
  # `${a[@]}` on an *empty* array is an unbound-variable error under `set -u` in
  # bash 3.2, which is what macOS ships — and without `--repo` this array is
  # empty on every run. The guard is the same `${a[@]+...}` form pr_sweep.sh
  # uses. Reached only after every precondition already passed, so the crash
  # landed between the verdict and the merge: the gate said `auto-merge`, then
  # died, and the PR sat open looking refused.
  ready_args=()
  [ -n "$repo" ] && ready_args+=(--repo "$repo")
  gh pr ready "$pr" ${ready_args[@]+"${ready_args[@]}"}
  echo "Marked PR #$pr ready for review."
fi

merge_args=("--$method" --delete-branch)
[ -n "$repo" ] && merge_args+=(--repo "$repo")

# `gh pr merge --delete-branch` does three things, in this order: it merges the
# PR through the API, then deletes the local branch, then deletes the remote
# one. The local step runs `git checkout <base>`, and that fails every time
# this script is run from inside a git worktree, because the base branch is
# already checked out in the primary checkout:
#
#   failed to run git: fatal: 'main' is already used by worktree at '/repo'
#
# gh exits non-zero there and never reaches the remote delete — but the PR is
# merged by then. Under `set -e` that used to end the script, so the caller saw
# exit 1 and no "Merged" line. The charter tells every agent to run this script
# and obey its exit code, so a merged PR reported as exit 1 makes finished work
# look refused, and a genuine refusal exits 1 as well. One run of PR #125 hit
# both in a row: first a real "branch is 1 commit behind main", then, after the
# rebase, a successful merge that still exited 1. Nothing but the message told
# them apart.
#
# So the exit code has to answer one question — did the merge happen? Only the
# server can answer it. When gh fails we ask, and we forgive the failure only
# when the PR itself reports MERGED. We ask rather than matching on the error
# text, because the wording of a gh or git error is not a stable contract, and
# rather than trusting where in the script the error appeared, because "already
# merged" and "refused to merge" are not distinguishable that way.
#
# This is deliberately the narrowest possible forgiveness. It is reachable only
# after `gh pr merge` has already run, so none of the refusals above it are
# affected: a branch behind base is still refused before this line, and a merge
# that genuinely did not happen still exits with gh's own status.
merge_rc=0
gh pr merge "$pr" "${merge_args[@]}" || merge_rc=$?

if [ "$merge_rc" -ne 0 ]; then
  state_args=(--json state)
  [ -n "$repo" ] && state_args+=(--repo "$repo")
  # `|| true` only so that a failed read reaches the check below and gets
  # explained. Without it `set -e` kills the script here and the caller is left
  # with the same bare exit code this whole change exists to fix. An unread
  # state is not forgiven — it falls straight into the failure branch.
  post_state="$(gh pr view "$pr" "${state_args[@]}" 2>/dev/null | jq -r '.state // empty')" || true

  # Unknown is treated as not merged. If we cannot read the state, we do not
  # get to claim the merge landed.
  if [ "$post_state" != "MERGED" ]; then
    echo >&2
    echo "gh pr merge exited $merge_rc and PR #$pr is not merged (state: ${post_state:-unknown})." >&2
    echo "Nothing landed. The error above is the reason." >&2
    exit "$merge_rc"
  fi

  echo
  echo "gh exited $merge_rc, but PR #$pr is merged on the server."
  echo "The error above came from the branch cleanup gh does after merging,"
  echo "not from the merge. This happens when the script runs inside a git"
  echo "worktree, because gh cannot check out a base branch that the primary"
  echo "checkout already holds."

  # gh gave up before deleting either branch, so say which ones are still here
  # instead of leaving the caller to find out. Only meaningful when we are
  # talking about this checkout's own remote, which `--repo` may not be.
  leftovers=""
  if [ -z "$repo" ]; then
    head_ref="$(field .headRefName)"
    if [ -n "$head_ref" ]; then
      if git show-ref --verify --quiet "refs/heads/$head_ref"; then
        echo "Left behind — local branch:  git branch -D $head_ref"
        leftovers="1"
      fi
      if git ls-remote --exit-code --heads origin "$head_ref" >/dev/null 2>&1; then
        echo "Left behind — remote branch: git push origin --delete $head_ref"
        leftovers="1"
      fi
    fi
  fi
  echo

  # The last line on screen has to say what happened. A reader who only sees
  # the tail of a long run — which is the ordinary way to read one — would
  # otherwise finish on a bare git error and call a merged PR blocked. That is
  # how this trap caught four sessions in a row, one of them while merging the
  # write-up describing the trap.
  if [ -n "$leftovers" ]; then
    tail_note=" The branches above were left behind; delete them by hand."
  elif [ -n "$repo" ]; then
    tail_note=" gh's branch cleanup did not finish; check for a leftover branch."
  else
    tail_note=" gh's branch cleanup did not finish, but no branch was left behind."
  fi
fi

echo "Merged PR #$pr ($method).${tail_note:-}"
