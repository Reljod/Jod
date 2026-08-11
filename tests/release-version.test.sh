#!/usr/bin/env bash
#
# release-version.test.sh — the runnable check behind the release-branch flow.
#
# The workflow that publishes a tag cannot be tested by running it: dispatching
# it *is* the release. So every decision it makes lives in
# .github/scripts/release_version.sh and is asserted here instead, offline —
# no git, no gh, no network.
#
# What it must never get wrong, in order of how expensive the mistake is:
#   - cutting a version that already exists (the push fails after the tests ran)
#   - cutting a version *below* the newest tag (succeeds, and installs nothing)
#   - reading the version out of the branch name at all
#
# Run: tests/release-version.test.sh
set -u

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$REPO_ROOT/.agents/skills/test-scenarios/scripts/assert.sh"
# shellcheck source=/dev/null
source "$REPO_ROOT/.github/scripts/release_version.sh"
cd "$REPO_ROOT" || exit 1

# The tag list every case resolves against, unless it passes its own.
TAGS='v0.0.1
v0.1.0'

# resolve <requested> [bump] [tags] — the version only, dropping the reason.
resolve() { printf '%s\n' "${3-$TAGS}" | resolve_release_version "$1" "${2:-patch}" | cut -d' ' -f1; }
# reason <requested> [bump] [tags] — how it was decided.
reason() { printf '%s\n' "${3-$TAGS}" | resolve_release_version "$1" "${2:-patch}" | cut -d' ' -f2; }

section "the branch name decides the version"

assert_eq "$(branch_version refs/heads/release/v1.2.0)" "v1.2.0" "full ref"
assert_eq "$(branch_version release/v1.2.0)" "v1.2.0" "short name"
assert_eq "$(branch_version release/1.2.0)" "v1.2.0" "a missing v is added, not rejected"
assert_eq "$(branch_version release/v10.20.30)" "v10.20.30" "multi-digit parts"

section "a branch that is not a release branch has no version"

assert_fails branch_version refs/heads/main
assert_fails branch_version feat/release/v1.0.0
assert_fails branch_version release/v1.2        # not three parts
assert_fails branch_version release/v1.2.0-rc1  # no pre-release suffix
assert_fails branch_version release/next
assert_fails branch_version release/

section "normally the requested version is cut as asked"

assert_eq "$(resolve v0.2.0)" "v0.2.0" "ahead of the latest tag by a minor"
assert_eq "$(resolve v1.0.0)" "v1.0.0" "ahead by a major"
assert_eq "$(resolve v0.1.1)" "v0.1.1" "ahead by a patch"
assert_eq "$(reason v0.2.0)" "branch-name" "and says the branch name decided it"
assert_eq "$(resolve 0.2.0)" "v0.2.0" "the v is optional in the request too"

section "asking for the version that already shipped bumps the patch"

# The rule the user asked for: same as the latest tag → resolve it rather than
# failing at `git tag`, which is the only other thing that could happen.
assert_eq "$(resolve v0.1.0)" "v0.1.1" "v0.1.0 is the latest tag"
assert_eq "$(reason v0.1.0)" "auto-bumped" "and says it was resolved, not requested"
assert_eq "$(resolve v0.1.7 patch 'v0.1.0
v0.1.7
v0.1.3')" "v0.1.8" "the highest tag decides, whatever order they arrive in"

section "asking for a version behind the latest tag is refused"

# Not a bump: cutting v0.0.9 while v0.1.0 exists publishes a tag that
# install.sh (highest tag wins) would never install.
assert_fails resolve_release_version v0.0.9 patch <<<"$TAGS"

# Including one that *has* shipped but is no longer the newest. Only the latest
# tag is auto-bumped; an older one means the branch is stale or mistyped, and
# guessing v0.1.1 from a branch that says v0.0.1 would publish a version nobody
# asked for.
assert_fails resolve_release_version v0.0.1 patch <<<"$TAGS"

section "a malformed request is refused, never guessed at"

assert_fails resolve_release_version v1.2 patch <<<"$TAGS"
assert_fails resolve_release_version v1.2.0-rc1 patch <<<"$TAGS"
assert_fails resolve_release_version latest patch <<<"$TAGS"
assert_fails resolve_release_version v1.2.x patch <<<"$TAGS"

section "with no request, the latest tag is bumped by the named part"

assert_eq "$(resolve '' patch)" "v0.1.1"
assert_eq "$(resolve '' minor)" "v0.2.0"
assert_eq "$(resolve '' major)" "v1.0.0"
assert_eq "$(reason '' patch)" "bumped-latest"
assert_fails resolve_release_version '' sideways <<<"$TAGS"

section "the first release, with no tags at all"

assert_eq "$(resolve v0.1.0 patch '')" "v0.1.0" "an empty tag list is not a collision"
assert_eq "$(resolve '' minor '')" "v0.1.0" "bumping from the v0.0.0 floor"

section "non-semver tags in the list are ignored, not tripped over"

assert_eq "$(resolve v0.2.0 patch 'v0.1.0
nightly
v1.0.0-rc1
release/v9.9.9')" "v0.2.0" "only vX.Y.Z counts as a shipped version"

section "stamping writes the version into every file that carries one"

# Against real copies of the repo's own files, so a renamed key or a new
# manifest layout fails here rather than on the tagged commit.
STAMP="$PWD/.github/scripts/stamp_version.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/.claude-plugin"
cp .claude-plugin/plugin.json "$TMP/.claude-plugin/plugin.json"
cp Cargo.toml Cargo.lock "$TMP/"

assert_ok "$STAMP" v9.8.7 "$TMP"

assert_eq "$(grep -c '"version": "9.8.7"' "$TMP/.claude-plugin/plugin.json")" "1" \
  "plugin.json carries the new version, without the v"
assert_eq "$(awk '/^\[workspace.package\]/{s=1;next} /^\[/{s=0} s&&/^version/{print;exit}' "$TMP/Cargo.toml")" \
  'version = "9.8.7"' "Cargo.toml's [workspace.package] version"
assert_eq "$(grep -c '^version = "9.8.7"' "$TMP/Cargo.lock")" \
  "$(grep -c '^name = "jod-' "$TMP/Cargo.lock")" \
  "every jod-* lock entry moved, and exactly those"

section "stamping leaves every other version string alone"

# The failure this guards: one greedy `sed s/version/` bumping rusqlite to
# 9.8.7 and taking the build down at release time.
assert_grep 'rusqlite = { version = "0.37"' "$TMP/Cargo.toml" \
  "a dependency's version is untouched"
assert_eq "$(grep -c '"\$schema"' "$TMP/.claude-plugin/plugin.json")" "1" \
  "plugin.json is still whole"
assert_ok python3 -c "import json,sys; json.load(open('$TMP/.claude-plugin/plugin.json'))"
assert_eq "$(grep -c '^version = ' "$TMP/Cargo.lock")" \
  "$(grep -c '^version = ' Cargo.lock)" \
  "the lock gained and lost no version lines"
assert_eq "$(awk 'NR==FNR{a[FNR]=$0;next} a[FNR]!=$0{n++} END{print n+0}' \
  Cargo.lock "$TMP/Cargo.lock")" \
  "$(grep -c '^name = "jod-' Cargo.lock)" \
  "exactly the jod-* version lines differ from the original lock"

section "the workflow calls exactly what is tested here"

WF=".github/workflows/release.yml"
assert_file "$WF"
assert_grep ".github/scripts/release_version.sh" "$WF" "the workflow sources the resolver"
assert_grep "resolve_release_version" "$WF" "…and calls it rather than re-deriving"
assert_grep ".github/scripts/stamp_version.sh" "$WF" "…and stamps through the tested script"
assert_ok bash -n .github/scripts/release_version.sh
assert_ok bash -n .github/scripts/stamp_version.sh

# The publish job is the irreversible one. It must stay manual: no push, no
# schedule can reach it — only a human dispatch, through an environment that
# can carry a required approval.
assert_grep "workflow_dispatch:" "$WF" "publishing is dispatch-triggered"
assert_grep "environment: release" "$WF" "…inside an environment that can gate it"
assert_grep "if: github.event_name == 'workflow_dispatch'" "$WF" \
  "…and the publish job runs on nothing else"
assert_no_grep "on: push" "$WF" "no push trigger anywhere near a tag"

assert_summary
exit
