#!/usr/bin/env bash
# Conversations again, against the WORKING TREE rather than HEAD.
#
# 50- found `jod conv` unreachable at HEAD: `new_conversation` is called only
# from its own unit tests, so three real runs produced zero conversations. The
# uncommitted tree wires it (core/src/service.rs calls `new_conversation`), so
# this re-runs the same commands against that build to see how much of the
# claimed feature set actually works once it is reachable.
#
# Results here describe uncommitted work in progress, not the PR's HEAD.
set -uo pipefail
AREA=convwip
JOD_E2E_BIN=/tmp/jod-e2e/bin-wip
export JOD_E2E_BIN
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

section "0. which build this is"
runsh "cat '$BIN/COMMIT'"
echo "-- uncommitted files in the tree at test time --"
runsh "cat '$BIN/DIRTY'"
run which jod

section "1. three real turns"
run jod run "say APPLE and nothing else" -n t1
run jod run "say BANANA and nothing else" -C -n t2
run jod run "say CHERRY and nothing else" -C -n t3

section "2. what got recorded this time"
runsh "q \"SELECT id, title, head_id, harness, session_id FROM conversations\""
runsh "q \"SELECT id, conversation_id, parent_id, role, active, substr(text,1,60) FROM messages ORDER BY id\""

CID="$(python3 "$REPO/tests/e2e/jod/db.py" "$DB" 'SELECT id FROM conversations LIMIT 1' | sed -n 3p)"
echo "conversation under test: $CID"
if [ "$CID" = "(0 rows)" ] || [ -z "$CID" ]; then
  echo "STILL NO CONVERSATION — the rest of this suite cannot run."
  exit 0
fi

section "3. conv ls and show"
run jod conv ls
run jod conv ls --json
runsh "'$BIN/jod' conv show '$CID'"
runsh "'$BIN/jod' conv show '$CID' --live"

section "4. conv search"
run jod conv search BANANA
run jod conv search CHERRY
run jod conv search "no such words anywhere"

section "5. conv fork leaves the original untouched"
runsh "'$BIN/jod' conv fork '$CID' --title 'a fork'"
run jod conv ls
runsh "q \"SELECT id, title, head_id, forked_from, forked_at_id FROM conversations\""
echo "-- the original's head must not have moved --"
runsh "q \"SELECT id, head_id FROM conversations WHERE id='$CID'\""

section "6. conv revert is non-destructive"
runsh "q \"SELECT id, role, substr(text,1,40) FROM messages WHERE conversation_id='$CID' ORDER BY id\""
MID="$(python3 "$REPO/tests/e2e/jod/db.py" "$DB" "SELECT id FROM messages WHERE conversation_id='$CID' ORDER BY id LIMIT 1" | sed -n 3p)"
LAST="$(python3 "$REPO/tests/e2e/jod/db.py" "$DB" "SELECT id FROM messages WHERE conversation_id='$CID' ORDER BY id DESC LIMIT 1" | sed -n 3p)"
echo "first=$MID last=$LAST"
runsh "'$BIN/jod' conv revert '$CID' '$MID'"
runsh "'$BIN/jod' conv show '$CID'"
runsh "q \"SELECT count(*) AS still_on_disk FROM messages WHERE conversation_id='$CID'\""

section "7. conv goto reaches the abandoned tail again"
runsh "'$BIN/jod' conv goto '$CID' '$LAST'"
runsh "'$BIN/jod' conv show '$CID'"

section "8. bad arguments"
runsh "'$BIN/jod' conv revert '$CID' 999999"
runsh "'$BIN/jod' conv goto '$CID' 999999"
runsh "'$BIN/jod' conv revert '$CID' notanumber"
run jod conv fork does-not-exist
run jod conv show does-not-exist

section "9. conv compact"
runsh "'$BIN/jod' conv compact '$CID' 'The user asked for three fruits and got them.'"
echo "-- full transcript --"
runsh "'$BIN/jod' conv show '$CID'"
echo "-- what a harness would still be sent --"
runsh "'$BIN/jod' conv show '$CID' --live"
runsh "q \"SELECT count(*) AS messages, sum(active) AS active FROM messages WHERE conversation_id='$CID'\""
runsh "q \"SELECT id, from_id, to_id, before_chars, after_chars, reason, substr(summary,1,50) FROM compactions\""

section "10. compacted-out text must stay searchable"
run jod conv search APPLE
run jod conv search fruits

section "11. handoff to each harness"
runsh "'$BIN/jod' conv handoff '$CID' --to claude"
runsh "'$BIN/jod' conv handoff '$CID' --to opencode"
runsh "'$BIN/jod' conv handoff '$CID' --to agy"
