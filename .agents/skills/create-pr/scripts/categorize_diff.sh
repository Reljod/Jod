#!/usr/bin/env bash
# Buckets the files changed between two refs into visualization categories.
# Usage: categorize_diff.sh <base>...<head>   (or any valid `git diff` range)
#
# Deliberately free of associative arrays (`declare -A`): those need bash 4,
# and macOS still ships bash 3.2 as /bin/bash. When this used them the script
# aborted on line 1 of its own logic, every category came back empty, and the
# PR skeleton quietly dropped its per-category hints — a silent wrong answer,
# not a visible failure. Categories are carried as "<cat>\t<file>" lines
# instead, which every bash can do.
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "Usage: $0 <base>...<head>" >&2
  exit 1
fi

range="$1"
files=$(git diff --name-only "$range")

if [ -z "$files" ]; then
  echo "No changed files in range: $range" >&2
  exit 0
fi

CATEGORIES="ui api infra tooling docs"

pattern_for() {
  case "$1" in
    ui)      printf '%s' '\.(tsx|jsx|vue|svelte)$|(^|/)components/|(^|/)pages/|(^|/)views/|\.(css|scss|less)$' ;;
    api)     printf '%s' '(^|/)(api|routes|controllers|handlers|resolvers)/|\.proto$|openapi\.(ya?ml|json)$|graphql' ;;
    infra)   printf '%s' '(^|/)terraform/|\.tf$|\.tfvars$|(^|/)(k8s|helm)/|Dockerfile|docker-compose\.ya?ml$|(^|/)\.github/workflows/' ;;
    tooling) printf '%s' '(^|/)scripts/|Makefile$|package\.json$|(^|/)(webpack|vite|eslint|tsconfig|babel)\.config' ;;
    docs)    printf '%s' '\.md$|(^|/)docs/' ;;
  esac
}

matched=""      # "<cat>\t<file>" per line
other_files=""  # one file per line

while IFS= read -r f; do
  [ -z "$f" ] && continue
  hit=""
  for cat in $CATEGORIES; do
    re="$(pattern_for "$cat")"
    # Unquoted on purpose — a quoted right-hand side is matched literally.
    if [[ "$f" =~ $re ]]; then
      matched="$matched$cat	$f
"
      hit="1"
    fi
  done
  if [ -z "$hit" ]; then
    other_files="$other_files$f
"
  fi
done <<< "$files"

for cat in $CATEGORIES; do
  list="$(printf '%s' "$matched" | awk -F'\t' -v c="$cat" '$1 == c { print $2 }')"
  if [ -n "$list" ]; then
    echo "## $cat"
    printf '%s\n' "$list"
    echo
  fi
done

if [ -n "$other_files" ]; then
  echo "## other"
  printf '%s' "$other_files"
fi
