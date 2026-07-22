#!/usr/bin/env bash
# Buckets the files changed between two refs into visualization categories.
# Usage: categorize_diff.sh <base>...<head>   (or any valid `git diff` range)
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

declare -A buckets
buckets[ui]='\.(tsx|jsx|vue|svelte)$|(^|/)components/|(^|/)pages/|(^|/)views/|\.(css|scss|less)$'
buckets[api]='(^|/)(api|routes|controllers|handlers|resolvers)/|\.proto$|openapi\.(ya?ml|json)$|graphql'
buckets[infra]='(^|/)terraform/|\.tf$|\.tfvars$|(^|/)(k8s|helm)/|Dockerfile|docker-compose\.ya?ml$|(^|/)\.github/workflows/'
buckets[tooling]='(^|/)scripts/|Makefile$|package\.json$|(^|/)(webpack|vite|eslint|tsconfig|babel)\.config'
buckets[docs]='\.md$|(^|/)docs/'

declare -A matched
other_files=()

while IFS= read -r f; do
  [ -z "$f" ] && continue
  hit=""
  for cat in ui api infra tooling docs; do
    if [[ "$f" =~ ${buckets[$cat]} ]]; then
      matched[$cat]+="$f"$'\n'
      hit="1"
    fi
  done
  if [ -z "$hit" ]; then
    other_files+=("$f")
  fi
done <<< "$files"

for cat in ui api infra tooling docs; do
  if [ -n "${matched[$cat]:-}" ]; then
    echo "## $cat"
    printf '%s' "${matched[$cat]}"
    echo
  fi
done

if [ "${#other_files[@]}" -gt 0 ]; then
  echo "## other"
  printf '%s\n' "${other_files[@]}"
fi
