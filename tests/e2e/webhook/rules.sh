#!/usr/bin/env bash
# `jod webhook` end to end: create a rule, narrow it, arm it, and prove the
# error paths are errors rather than cheerful no-ops.
#
# Run from the repo root after `cargo build --release --bin jod`.
set -u
jod=${JOD:-./target/release/jod}
export JOD_HOME=${JOD_HOME:-/tmp/jod-hook-e2e}
rm -rf "$JOD_HOME"

say() { printf '\n=== %s ===\n' "$1"; }

say "empty"
"$jod" webhook ls

say "add"
"$jod" webhook add ci-failed \
  'A check failed on {{title}} by {{author}}. Look at it.' \
  --event pull_request --action closed --repo Reljod/Jod --label urgent

say "ls"
"$jod" webhook ls

say "disarm, then arm"
"$jod" webhook disable ci-failed
"$jod" webhook "enable" ci-failed

say "a mistyped name is an error, not a silent success"
"$jod" webhook "enable" ci-faild
echo "exit=$?"

say "a duplicate name is refused"
"$jod" webhook add ci-failed dup --event issues
echo "exit=$?"

say "a paused rule is added disarmed"
"$jod" webhook add nightly 'Look at {{title}}' --event push --paused
"$jod" webhook ls

say "deliveries, before anything has arrived"
"$jod" webhook deliveries

say "rm"
"$jod" webhook rm nightly
"$jod" webhook ls
