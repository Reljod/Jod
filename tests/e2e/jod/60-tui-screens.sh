#!/usr/bin/env bash
# The TUI's nine workspaces, rendered at 100x30 off a database this script
# populated with the CLI.
#
# The TUI needs a TTY, so it cannot be driven here directly. `cli/examples/
# screens.rs` renders the same widgets through ratatui's TestBackend, which is a
# buffer rather than a terminal. That example is *uncommitted* work in the tree,
# so this suite — like 51- — describes the working tree, not the PR's HEAD.
set -uo pipefail
AREA=tui
JOD_E2E_BIN=/tmp/jod-e2e/bin-wip
export JOD_E2E_BIN
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"

section "0. which build"
runsh "cat '$BIN/COMMIT'"
runsh "cat '$BIN/DIRTY'"

section "1. seed the store through the CLI"
run jod remember reljod uses jod
run jod remember reljod prefers "linear for tasks"
run jod remember jod runs-on jod-cloud
run jod remember jod-cloud hosted-by hetzner
run jod remember hetzner located-in falkenstein
run jod remember reljod based-in manila --scope personal
run jod remember budget is "50 eur per month" --scope finance
run jod remember spam claims "anything at all" --origin untrusted

run jod schedule add nightly "summarise the day" --cron "0 2 * * *"
run jod schedule add manila-brief "morning brief" --cron "0 7 * * *" -z Asia/Manila
run jod schedule add every15 "check the queue" --cron "*/15 * * * *"
run jod schedule pause every15

run jod goal add inbox-zero "keep the inbox empty" --budget 5.00 --stall-after 4
run jod goal add tidy-repo "keep the repo tidy" -c "0 */6 * * *" --max-iterations 20

run jod remember "graph-only" links "another-node"

section "2. a real run, so the fleet and activity screens have something"
run jod run "say OK and stop" -n seeded-run

section "3. what is in the store"
runsh "python3 '$REPO/tests/e2e/jod/db.py' '$DB' \"SELECT (SELECT count(*) FROM facts) AS facts, (SELECT count(*) FROM entities) AS entities, (SELECT count(*) FROM relations) AS relations, (SELECT count(*) FROM schedules) AS schedules, (SELECT count(*) FROM goals) AS goals, (SELECT count(*) FROM runs) AS runs, (SELECT count(*) FROM conversations) AS conversations\""

section "4. every workspace at 100x30"
runsh "'$BIN/screens' '$DB'"

section "5. the memory workspace with a filter typed into it"
runsh "'$BIN/screens' '$DB' prefers"

section "6. and against an empty database, since that is a first run"
runsh "rm -rf /tmp/jod-e2e/tui-empty; mkdir -p /tmp/jod-e2e/tui-empty; JOD_HOME=/tmp/jod-e2e/tui-empty '$BIN/jod' schedule ls >/dev/null 2>&1; ls /tmp/jod-e2e/tui-empty"
runsh "'$BIN/screens' /tmp/jod-e2e/tui-empty/jod.db"
