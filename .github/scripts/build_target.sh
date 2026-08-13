#!/usr/bin/env bash
#
# build_target.sh — decides what `build-clients.yml` builds, and whether it is
# allowed to publish the result.
#
# This is the whole of that workflow's judgement, pulled out of the YAML for the
# same reason release_version.sh was: dispatching the workflow *is* the release,
# so the only way to test its refusals is to test them offline.
#
# One rule, stated once: **this workflow never brings a version into being.**
# It attaches binaries to a tag that already exists. `release_version.sh` and
# `release.yml` decide what a version is and create the tag; two places deciding
# that is exactly how `jod update` and `install.sh` end up installing different
# things from the same tag list.
#
# So there are only three answers, and two of them are refusals:
#
#   no version asked for   → build, publish nothing   (the safe default)
#   a tag that exists      → build it, publish to it
#   anything else          → refused, exit 1
#
# Sourced by .github/workflows/build-clients.yml, exercised by
# tests/build-clients.test.sh. Pure bash, no git and no network, so the tests
# run anywhere — including stock macOS bash 3.2.

# shellcheck source=bin/lib/semver.sh
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)/bin/lib/semver.sh"

# resolve_build_target <requested> <dispatch-ref> <default-branch>
#
# Reads the existing tags on stdin, one per line — the same shape
# `git tag --list 'v*'` produces. Prints:
#
#   <publish> <tag>
#
# as `false -` for a build-only run, or `true vX.Y.Z` when the assets are to be
# attached to that tag's release. Refuses (exit 1, reason on stderr) rather than
# guessing.
resolve_build_target() {
  local requested="${1:-}" ref="${2:-}" base="${3:-main}" tags found line

  # Nothing asked for: build whatever was dispatched and publish none of it.
  # The default is the harmless one deliberately — the same instinct that keeps
  # release.yml's publish half manual. Nothing outside the repo changes unless
  # somebody typed a version.
  if [ -z "$requested" ]; then
    printf 'false -\n'
    return 0
  fi

  # Read stdin here rather than per-branch: the caller has already piped it, and
  # a function that consumes its input only on some paths is a trap for the next
  # person to add a branch.
  tags="$(cat)"

  if [ "${requested#v}" = "$requested" ]; then
    requested="v$requested"
  fi

  if ! is_semver_tag "$requested"; then
    printf 'resolve_build_target: %s is not vMAJOR.MINOR.PATCH\n' \
      "$requested" >&2
    return 1
  fi

  # A published asset is visible outside the repo, so it gets the guard
  # release.yml puts on tagging: it comes off the default branch, not off
  # whichever branch happened to be selected in the dispatch UI. Building
  # without publishing is unrestricted — that is the point of it.
  if [ "$ref" != "$base" ]; then
    printf 'resolve_build_target: refusing to publish %s from %s — dispatch this from %s\n' \
      "$requested" "${ref:-an unnamed ref}" "$base" >&2
    return 1
  fi

  # Exact string match against the tag list, not `highest_semver_tag` or any
  # comparison: the question here is only "does this tag exist", and answering
  # it by ordering would let a typo resolve to a neighbouring release.
  found=""
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    if [ "$line" = "$requested" ]; then
      found=yes
      break
    fi
  done <<EOF
$tags
EOF

  if [ -z "$found" ]; then
    printf 'resolve_build_target: no tag %s — cut it first with: gh workflow run release.yml --ref %s -f version=%s\n' \
      "$requested" "$base" "$requested" >&2
    return 1
  fi

  printf 'true %s\n' "$requested"
}
