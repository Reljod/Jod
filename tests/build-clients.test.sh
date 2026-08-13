#!/usr/bin/env bash
#
# build-clients.test.sh — the runnable check behind the client build workflow.
#
# `build-clients.yml` cannot be tested by running it: a dispatch that publishes
# *is* the publish. So every decision it makes before it builds anything lives
# in .github/scripts/build_target.sh and is asserted here instead, offline — no
# git, no gh, no network.
#
# What it must never get wrong, in order of how expensive the mistake is:
#   - creating or moving a tag (it must never do this at all — release.yml owns
#     versions, and a second opinion about what v0.2.0 means is unrecoverable)
#   - publishing assets for a tag that does not exist
#   - publishing from a branch that is not the default one
#   - publishing at all when nobody asked for a version
#
# Run: tests/build-clients.test.sh
set -u

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$REPO_ROOT/.agents/skills/test-scenarios/scripts/assert.sh"
# shellcheck source=/dev/null
source "$REPO_ROOT/.github/scripts/build_target.sh"
cd "$REPO_ROOT" || exit 1

# The tag list every case resolves against, unless it passes its own.
TAGS='v0.0.1
v0.1.0
v0.2.0'

# resolve <requested> [ref] [base] [tags] — the whole "<publish> <tag>" line.
resolve() {
  printf '%s\n' "${4-$TAGS}" | resolve_build_target "$1" "${2-main}" "${3-main}"
}
# publish/tag — the two fields on their own.
publish() { resolve "$@" | cut -d' ' -f1; }
tag_of()  { resolve "$@" | cut -d' ' -f2; }

section "no version means build only"

assert_eq "$(resolve '')" "false -" "blank publishes nothing"
assert_eq "$(publish '' feat/anything main)" "false" \
  "a build-only run is not restricted to the default branch"
assert_eq "$(resolve '' main main '')" "false -" \
  "blank does not even need a tag list"

section "an existing tag is built and published"

assert_eq "$(resolve v0.2.0)" "true v0.2.0" "the newest tag"
assert_eq "$(resolve v0.0.1)" "true v0.0.1" \
  "an older tag — rebuilding assets for a past release is legitimate"
assert_eq "$(tag_of 0.2.0)" "v0.2.0" "a missing v is added, not rejected"

section "a tag that does not exist is refused, never created"

assert_fails resolve_build_target v9.9.9 main main <<<"$TAGS"
assert_fails resolve_build_target v0.3.0 main main <<<"$TAGS"
assert_fails resolve_build_target v0.2.0 main main <<<""
ok '! resolve v0.1.1 2>/dev/null' "a plausible next patch is still not a tag"

# The refusal has to say what to do instead, or the next person invents a tag by
# hand and the two version stories diverge.
assert_eq "$(resolve_build_target v9.9.9 main main 2>&1 >/dev/null <<<"$TAGS" \
  | grep -c 'release.yml')" "1" "the refusal names release.yml"

section "publishing only happens from the default branch"

assert_fails resolve_build_target v0.2.0 feat/x main <<<"$TAGS"
assert_fails resolve_build_target v0.2.0 release/v0.2.0 main <<<"$TAGS"
assert_fails resolve_build_target v0.2.0 '' main <<<"$TAGS"
assert_eq "$(publish v0.2.0 trunk trunk)" "true" \
  "the default branch is whatever the repo says it is, not the literal 'main'"

section "malformed versions are refused before anything is built"

assert_fails resolve_build_target v1.2 main main <<<"$TAGS"
assert_fails resolve_build_target v1.2.0-rc1 main main <<<"$TAGS"
assert_fails resolve_build_target latest main main <<<"$TAGS"
assert_fails resolve_build_target v1.2.0.1 main main <<<"$TAGS"

section "a tag is matched exactly, never by proximity"

# v0.2 is not a prefix match for v0.2.0, and v0.20.0 must not answer for v0.2.0.
assert_fails resolve_build_target v0.2.0 main main <<<'v0.20.0'
assert_eq "$(tag_of v0.2.0 main main 'v0.20.0
v0.2.0')" "v0.2.0" "the exact tag wins over a longer one that shares its prefix"

assert_summary
