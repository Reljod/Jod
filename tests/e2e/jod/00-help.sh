#!/usr/bin/env bash
# Capture the real command surface, so the report describes what shipped
# rather than what the PR description says shipped.
set -uo pipefail
AREA=help
. "$(dirname "$0")/env.sh"

section "top level"
run jod --help
run jod --version

for c in remember recall forget related path conv schedule goal daemon tui chat team; do
  section "jod $c"
  run jod "$c" --help
done

section "schedule subcommands"
for s in add ls pause resume run rm log; do
  run jod schedule "$s" --help
done

section "goal subcommands"
for s in add ls pause resume run rm log; do
  run jod goal "$s" --help
done

section "conv subcommands"
for s in ls show fork revert goto search compact handoff; do
  run jod conv "$s" --help
done
