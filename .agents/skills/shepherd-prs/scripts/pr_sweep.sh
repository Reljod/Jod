#!/usr/bin/env bash
# Finds the open PRs an agent is allowed to act on, and runs the merge gate
# on each one.
#
# Usage: pr_sweep.sh [--repo owner/name] [--pr N] [--author LOGIN]
#                    [--format md|tsv] [--limit N] [--ready] [--base REF]
#
# This script never merges. It enumerates, applies the filters that are not a
# matter of judgement, and reports what merge_pr.sh already says about each
# PR. Merging stays a separate act, one PR at a time, so a bug in the sweep
# can widen what gets *considered* and never what gets *merged*.
#
# Two filters here are hard, meaning no flag relaxes them:
#
#   forks    — a fork PR's head is written by someone outside the repo. The
#              scheduled job that runs this holds a write token, so treating
#              fork PRs as sweep candidates would be handing that token's
#              reach to anyone who can open a PR.
#   authors  — AGENTS.md: a teammate's branch closes when they say it does.
#              The default allowlist is the repo owner, and --author adds to
#              it deliberately rather than a flag switching the rule off.
set -euo pipefail

repo=""
one_pr=""
extra_authors=""
format="md"
limit="30"
ready=""
base_override=""

die() { echo "error: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --repo)    repo="${2:-}"; shift 2 ;;
    --pr)      one_pr="${2:-}"; shift 2 ;;
    --author)  extra_authors="$extra_authors ${2:-}"; shift 2 ;;
    --format)  format="${2:-}"; shift 2 ;;
    --limit)   limit="${2:-}"; shift 2 ;;
    --base)    base_override="${2:-}"; shift 2 ;;
    --ready)   ready="1"; shift ;;
    -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
    *)         die "Unknown argument: $1" ;;
  esac
done

case "$format" in md|tsv) ;; *) die "--format must be md or tsv" ;; esac
case "$limit" in *[!0-9]*) die "--limit must be an integer" ;; esac
[ -n "$one_pr" ] && case "$one_pr" in *[!0-9]*) die "--pr must be numeric, got: $one_pr" ;; esac

command -v gh >/dev/null || die "gh is required"
command -v jq >/dev/null || die "jq is required"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MERGE="$SCRIPT_DIR/../../auto-merge/scripts/merge_pr.sh"
[ -x "$MERGE" ] || die "auto-merge/scripts/merge_pr.sh not found — this skill runs the gate, it does not reimplement it"

# --- who owns this repo, and whose PRs may be swept ---------------------------

# `${a[@]}` on an empty array is an unbound-variable error under `set -u` in
# bash 3.2, which is what macOS ships — so every expansion of an array that may
# be empty is guarded with the `${a[@]+...}` form.
repo_args=()
[ -n "$repo" ] && repo_args+=("$repo")
owner="$(gh repo view ${repo_args[@]+"${repo_args[@]}"} --json owner | jq -r '.owner.login // empty')" \
  || die "could not read the repository owner"
[ -n "$owner" ] || die "repository owner came back empty"

allowed=" $owner$extra_authors "

# --- the open PRs -------------------------------------------------------------

fields='number,title,author,isDraft,headRefName,headRepositoryOwner,url'

if [ -n "$one_pr" ]; then
  view_args=(--json "$fields")
  [ -n "$repo" ] && view_args+=(--repo "$repo")
  prs="$(gh pr view "$one_pr" "${view_args[@]}" | jq -c '[.]')" \
    || die "could not read PR #$one_pr"
else
  list_args=(--state open --limit "$limit" --json "$fields")
  [ -n "$repo" ] && list_args+=(--repo "$repo")
  prs="$(gh pr list "${list_args[@]}")" || die "could not list pull requests"
fi

rows="$(printf '%s' "$prs" | jq -r '
  .[] | [ .number,
          (.author.login // "?"),
          (.headRepositoryOwner.login // "?"),
          (.title // "") ] | @tsv')"

# --- run the gate on each candidate -------------------------------------------

n_ready=0
n_blocked=0
n_skipped=0
report=""

# emit <num> <status> <title> <detail> — one tab-separated row. The title is
# carried for the reader, not for the decision: a job summary listing bare
# numbers and verdicts is unreadable when you are scanning it at a glance.
emit() { report="$report$1	$2	$3	$4
"; }

while IFS=$'\t' read -r num author head_owner title; do
  [ -z "$num" ] && continue

  # Hard filters first: these are not gate refusals, they are PRs this routine
  # has no business touching at all, so they never reach merge_pr.sh.
  if [ "$head_owner" != "$owner" ]; then
    n_skipped=$((n_skipped + 1))
    emit "$num" skipped "$title" "from a fork ($head_owner) — never swept"
    continue
  fi
  case "$allowed" in
    *" $author "*) ;;
    *)
      n_skipped=$((n_skipped + 1))
      emit "$num" skipped "$title" "authored by $author, not in the allowlist"
      continue ;;
  esac

  gate_args=("$num" --dry-run)
  [ -n "$repo" ] && gate_args+=(--repo "$repo")
  [ -n "$base_override" ] && gate_args+=(--base "$base_override")
  [ -n "$ready" ] && gate_args+=(--ready)

  if out="$("$MERGE" "${gate_args[@]}" 2>&1)"; then
    n_ready=$((n_ready + 1))
    cats="$(printf '%s\n' "$out" | sed -n 's/.*(categories: \(.*\))$/\1/p' | tail -1)"
    emit "$num" ready "$title" "gate clear${cats:+ (categories: $cats)}"
  else
    n_blocked=$((n_blocked + 1))
    reasons="$(printf '%s\n' "$out" \
      | sed -n 's/^ - //p' \
      | awk '{ printf "%s%s", (NR > 1 ? "; " : ""), $0 } END { if (NR) print "" }')"
    emit "$num" blocked "$title" "${reasons:-refused without a stated reason}"
  fi
done <<EOF
$rows
EOF

# --- report -------------------------------------------------------------------

if [ "$format" = tsv ]; then
  printf '%s' "$report"
  exit 0
fi

echo "## PR sweep"
echo
if [ -z "$report" ]; then
  echo "No open pull requests."
  exit 0
fi

echo "| PR | Status | Title | Detail |"
echo "|---|---|---|---|"
printf '%s' "$report" | while IFS=$'\t' read -r num status title detail; do
  [ -z "$num" ] && continue
  # A PR title is free text and a bare `|` in one would silently shear the
  # table into the wrong columns, which reads as a different verdict.
  echo "| #$num | \`$status\` | ${title//|/\\|} | $detail |"
done
echo
echo "$n_ready ready, $n_blocked blocked, $n_skipped skipped."
echo
if [ "$n_ready" -gt 0 ]; then
  echo "\`ready\` means the gate found nothing — **not** that the PR should merge."
  echo "Each one still needs its review agents to come back CLEAR first."
fi
