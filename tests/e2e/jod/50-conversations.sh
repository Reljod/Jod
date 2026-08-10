#!/usr/bin/env bash
# Conversations Jod owns: ls, show, fork, revert, goto, search, compact,
# handoff. Real runs create the transcript — scheduled runs did not produce a
# conversation in any earlier suite, so the interactive `jod run` path is used.
set -uo pipefail
AREA=conv
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

section "1. three real turns, so there is a transcript to work on"
run jod run "say APPLE and nothing else" -n t1
run jod run "say BANANA and nothing else" -C -n t2
run jod run "say CHERRY and nothing else" -C -n t3

section "2. what got recorded"
runsh "q \"SELECT id, title, head_id, harness, session_id FROM conversations\""
runsh "q \"SELECT id, conversation_id, parent_id, role, substr(text,1,80) FROM messages ORDER BY id\""

section "3. conv ls"
run jod conv ls
run jod conv ls --json

CID="$(python3 "$REPO/tests/e2e/jod/db.py" "$DB" 'SELECT id FROM conversations LIMIT 1' | sed -n 3p)"
echo "conversation under test: $CID"

section "4. conv show"
runsh "'$BIN/jod' conv show '$CID'"
runsh "'$BIN/jod' conv show '$CID' --live"
run jod conv show does-not-exist

section "5. conv search"
run jod conv search BANANA
run jod conv search CHERRY
run jod conv search "nothing at all like this"
run jod conv search

section "6. conv fork — the original must be untouched"
runsh "'$BIN/jod' conv fork '$CID' --title 'a fork of the whole thing'"
run jod conv ls
runsh "q \"SELECT id, title, head_id, forked_from, forked_at_id FROM conversations\""

section "7. conv revert — non-destructive"
echo "-- messages in the original, so a revert target can be named --"
runsh "q \"SELECT id, conversation_id, role, substr(text,1,50) FROM messages WHERE conversation_id='$CID' ORDER BY id\""
MID="$(python3 "$REPO/tests/e2e/jod/db.py" "$DB" "SELECT id FROM messages WHERE conversation_id='$CID' ORDER BY id LIMIT 1" | sed -n 3p)"
echo "reverting to message $MID"
runsh "'$BIN/jod' conv revert '$CID' '$MID'"
runsh "'$BIN/jod' conv show '$CID'"
echo "-- the abandoned tail must still be on disk --"
runsh "q \"SELECT count(*) AS messages_still_stored FROM messages WHERE conversation_id='$CID'\""

section "8. conv goto — reach the abandoned branch again"
LAST="$(python3 "$REPO/tests/e2e/jod/db.py" "$DB" "SELECT id FROM messages WHERE conversation_id='$CID' ORDER BY id DESC LIMIT 1" | sed -n 3p)"
echo "going to message $LAST"
runsh "'$BIN/jod' conv goto '$CID' '$LAST'"
runsh "'$BIN/jod' conv show '$CID'"

section "9. bad arguments"
runsh "'$BIN/jod' conv revert '$CID' 999999"
runsh "'$BIN/jod' conv goto '$CID' 999999"
runsh "'$BIN/jod' conv revert '$CID' notanumber"
run jod conv fork does-not-exist

section "10. conv compact"
runsh "'$BIN/jod' conv compact '$CID' 'The user asked for three fruits and got them.'"
runsh "'$BIN/jod' conv show '$CID'"
echo "-- --live is what a harness would still be sent --"
runsh "'$BIN/jod' conv show '$CID' --live"
echo "-- but nothing was deleted --"
runsh "q \"SELECT count(*) AS messages FROM messages WHERE conversation_id='$CID'\""
runsh "q \"SELECT id, conversation_id, from_id, to_id, substr(summary,1,60) FROM compactions\""

section "11. conv handoff to each harness"
runsh "'$BIN/jod' conv handoff '$CID' --to claude"
runsh "'$BIN/jod' conv handoff '$CID' --to opencode"
runsh "'$BIN/jod' conv handoff '$CID' --to agy"

section "12. search still finds compacted-out text"
run jod conv search APPLE
run jod conv search fruits
