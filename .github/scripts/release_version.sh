#!/usr/bin/env bash
#
# release_version.sh — decides which version a release is cut as.
#
# The branch name is the request; the tag list is the fact. Everything here is
# one rule: **the branch name decides, unless it asks for a version that has
# already shipped** — in which case the patch is bumped rather than the release
# failing at `git tag` five minutes later, after the tests have run.
#
# Sourced by .github/workflows/release.yml and exercised offline by
# tests/release-version.test.sh. Pure bash for the same reason semver.sh is:
# this repo has to work on stock macOS bash 3.2, without coreutils.
#
# Lives here rather than in bin/lib/ because nothing a user installs ever calls
# it — only CI cuts releases. The tag *comparison* it borrows is the shipped
# one: install.sh and `jod update` resolve versions through bin/lib/semver.sh,
# and a release picked by a second, subtly different implementation of "which
# tag is newest" is a release that installs on nobody's machine.

# shellcheck source=bin/lib/semver.sh
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)/bin/lib/semver.sh"

# branch_version <ref> — prints the vMAJOR.MINOR.PATCH a `release/…` branch is
# asking for, or nothing (exit 1) if <ref> is not a release branch.
#
# Accepts the full ref or the short name, with or without the `v`
# (`refs/heads/release/v1.2.0`, `release/v1.2.0`, `release/1.2.0`), and always
# prints the `v` form — a tag in this repo is always `vX.Y.Z`.
branch_version() {
  local ref="${1#refs/heads/}" ver
  case "$ref" in
    release/*) ver="${ref#release/}" ;;
    *) return 1 ;;
  esac
  [ "${ver#v}" = "$ver" ] && ver="v$ver"
  is_semver_tag "$ver" || return 1
  printf '%s\n' "$ver"
}

# bump_version <vX.Y.Z> <major|minor|patch> — prints the next version.
bump_version() {
  local ver="${1#v}" part="$2" major minor patch rest
  major="${ver%%.*}"; rest="${ver#*.}"
  minor="${rest%%.*}"; patch="${rest#*.}"
  case "$part" in
    major) major=$((major + 1)); minor=0; patch=0 ;;
    minor) minor=$((minor + 1)); patch=0 ;;
    patch) patch=$((patch + 1)) ;;
    *) printf 'bump_version: unknown part %s\n' "$part" >&2; return 1 ;;
  esac
  printf 'v%s.%s.%s\n' "$major" "$minor" "$patch"
}

# _cmp_semver <a> <b> — 0 if a==b, 1 if a>b, 2 if a<b (both vX.Y.Z).
_cmp_semver() {
  local a="${1#v}" b="${2#v}" ax ay az bx by bz rest
  ax="${a%%.*}"; rest="${a#*.}"; ay="${rest%%.*}"; az="${rest#*.}"
  bx="${b%%.*}"; rest="${b#*.}"; by="${rest%%.*}"; bz="${rest#*.}"
  [ "$ax" -gt "$bx" ] && return 1; [ "$ax" -lt "$bx" ] && return 2
  [ "$ay" -gt "$by" ] && return 1; [ "$ay" -lt "$by" ] && return 2
  [ "$az" -gt "$bz" ] && return 1; [ "$az" -lt "$bz" ] && return 2
  return 0
}

# resolve_release_version <requested> <fallback-bump> — reads the existing tags
# on stdin, prints the version to cut and how it was decided:
#
#   <version> <reason>
#
# <requested> is a vX.Y.Z the branch (or a dispatch input) asked for, or empty
# to mean "just bump the latest tag by <fallback-bump>".
#
# Four outcomes, and the third is the whole point of this file:
#
#   requested > latest   → requested          (branch-name)  the normal path
#   requested empty      → bump(latest)       (bumped-latest)
#   requested == latest  → bump(latest,patch) (auto-bumped)   already shipped
#   requested < latest   → refused, exit 1
#
# The last one is deliberate. A branch asking for a version below the newest tag
# is a stale branch or a typo, never an intent — cutting it would publish a tag
# that `install.sh` (which resolves the *highest* tag) would never install and
# `jod update` would never see. Failing here says so; succeeding would ship a
# release nobody receives.
resolve_release_version() {
  local requested="${1:-}" fallback="${2:-patch}" latest next
  latest="$(highest_semver_tag)"
  latest="${latest:-v0.0.0}"

  if [ -z "$requested" ]; then
    next="$(bump_version "$latest" "$fallback")" || return 1
    printf '%s bumped-latest\n' "$next"
    return 0
  fi

  [ "${requested#v}" = "$requested" ] && requested="v$requested"
  if ! is_semver_tag "$requested"; then
    printf 'resolve_release_version: %s is not vMAJOR.MINOR.PATCH\n' \
      "$requested" >&2
    return 1
  fi

  # `|| rel=$?` rather than a bare call: _cmp_semver signals through its exit
  # status, and callers run under `set -e`, where a bare non-zero statement
  # would end the job instead of answering the question.
  local rel=0
  _cmp_semver "$requested" "$latest" || rel=$?
  case "$rel" in
    1) printf '%s branch-name\n' "$requested" ;;
    0)
      next="$(bump_version "$latest" patch)" || return 1
      printf '%s auto-bumped\n' "$next"
      ;;
    *)
      printf 'resolve_release_version: %s is behind the latest tag %s — refusing to cut a release nothing would install\n' \
        "$requested" "$latest" >&2
      return 1
      ;;
  esac
}
