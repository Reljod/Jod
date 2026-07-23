#!/usr/bin/env bash
#
# semver.sh — tiny, portable semver-tag helpers shared by install.sh and
# bin/jod. Tags are expected in `vMAJOR.MINOR.PATCH` form (see
# .github/workflows/release.yml, the only place that mints them).
#
# Pure bash string/arithmetic comparisons on purpose: GNU `sort -V` doesn't
# exist in stock macOS's BSD `sort`, and this repo has to work there without
# a coreutils install.

# is_semver_tag <tag> — true iff <tag> is exactly vMAJOR.MINOR.PATCH
# (all-numeric parts, no pre-release/build suffix).
is_semver_tag() {
  case "$1" in
    v*.*.*)
      local ver="${1#v}" major minor patch rest
      major="${ver%%.*}"; rest="${ver#*.}"
      minor="${rest%%.*}"; patch="${rest#*.}"
      case "$major" in ''|*[!0-9]*) return 1 ;; esac
      case "$minor" in ''|*[!0-9]*) return 1 ;; esac
      case "$patch" in ''|*[!0-9]*) return 1 ;; esac
      return 0
      ;;
    *) return 1 ;;
  esac
}

# highest_semver_tag — reads candidate tags on stdin (one per line), prints
# the highest vMAJOR.MINOR.PATCH one (empty if none qualify). Non-semver
# lines (a branch name, a stray annotation) are ignored rather than erroring,
# so callers can pipe `git tag --list` straight in.
highest_semver_tag() {
  local best="" best_major=-1 best_minor=-1 best_patch=-1
  local tag ver major minor patch rest
  while IFS= read -r tag; do
    is_semver_tag "$tag" || continue
    ver="${tag#v}"; major="${ver%%.*}"; rest="${ver#*.}"
    minor="${rest%%.*}"; patch="${rest#*.}"
    if [ "$major" -gt "$best_major" ] \
      || { [ "$major" -eq "$best_major" ] && [ "$minor" -gt "$best_minor" ]; } \
      || { [ "$major" -eq "$best_major" ] && [ "$minor" -eq "$best_minor" ] && [ "$patch" -gt "$best_patch" ]; }; then
      best="$tag"; best_major="$major"; best_minor="$minor"; best_patch="$patch"
    fi
  done
  printf '%s\n' "$best"
}
