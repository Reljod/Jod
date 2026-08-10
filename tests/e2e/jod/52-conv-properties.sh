#!/usr/bin/env bash
# The three properties of `jod conv` that are worth attacking rather than
# demonstrating, each checked in the way that could actually fail.
#
#   1. `handoff --to agy` must warn on STDERR, before the transcript.
#      50-/51- captured `2>&1` and so could not have told the streams apart —
#      a warning printed to stdout would have looked identical and passed.
#      Here each stream is captured to its own file.
#   2. `revert` must be non-destructive: the abandoned tail still reachable.
#   3. `fork` must leave the original untouched.
set -uo pipefail
AREA=convprops
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }
O=/tmp/jod-e2e/convprops-stdout.txt
E=/tmp/jod-e2e/convprops-stderr.txt

# Run a command capturing the two streams SEPARATELY, and say which said what.
split() {
  echo "\$ $*"
  "$@" > "$O" 2> "$E"
  local rc=$?
  echo "--- stdout ---"
  cat "$O"
  echo "--- stderr ---"
  cat "$E"
  echo "--- exit $rc ---"
  echo
}

section "0. build under test"
runsh "cat '$BIN/COMMIT'"

section "1. one real turn, to have a transcript"
run jod run "say APPLE and nothing else" -n t1
CID="$(python3 "$REPO/tests/e2e/jod/db.py" "$DB" 'SELECT id FROM conversations LIMIT 1' | sed -n 3p)"
echo "conversation: $CID"
runsh "q \"SELECT id, conversation_id, parent_id, role, active, text FROM messages ORDER BY id\""

section "2. PROPERTY: handoff --to agy warns on stderr, not stdout"
split "$BIN/jod" conv handoff "$CID" --to agy
echo "== is the warning on stderr? =="
runsh "grep -q 'accepts no transcript' '$E' && echo 'YES — the warning is on stderr' || echo 'NO — nothing about it on stderr'"
runsh "grep -q 'accepts no transcript' '$O' && echo 'PROBLEM: the warning is on stdout, where it would be piped into the next harness' || echo 'good: stdout carries only the carrier'"
echo "== and does stdout stay clean enough to pipe? =="
runsh "cat '$O'"

section "3. the same for the two harnesses that DO take a transcript"
split "$BIN/jod" conv handoff "$CID" --to claude
runsh "test -s '$E' && echo 'stderr is non-empty' || echo 'stderr empty, as it should be for a harness that takes a transcript'"
split "$BIN/jod" conv handoff "$CID" --to opencode
runsh "test -s '$E' && echo 'stderr is non-empty' || echo 'stderr empty'"

section "4. is the claude carrier actually valid stream-json?"
runsh "'$BIN/jod' conv handoff '$CID' --to claude 2>/dev/null | python3 -c \"
import sys, json
n = 0
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    json.loads(line); n += 1
print(f'{n} lines, all valid JSON')\""

section "5. is the opencode carrier valid JSON?"
runsh "'$BIN/jod' conv handoff '$CID' --to opencode 2>/dev/null | python3 -c \"
import sys, json
d = json.load(sys.stdin)
print('valid JSON, keys:', sorted(d))\""

section "6. PROPERTY: revert is non-destructive and goto gets back"
echo "-- a second and third turn, so there is a tail to abandon --"
run jod run "say BANANA and nothing else" -C -n t2
runsh "q \"SELECT id, conversation_id, parent_id, role, active, text FROM messages ORDER BY id\""
echo "-- everything on this conversation's thread --"
runsh "q \"SELECT id, role, text FROM messages WHERE conversation_id='$CID' ORDER BY id\""
FIRST="$(python3 "$REPO/tests/e2e/jod/db.py" "$DB" "SELECT id FROM messages WHERE conversation_id='$CID' ORDER BY id LIMIT 1" | sed -n 3p)"
LAST="$(python3 "$REPO/tests/e2e/jod/db.py" "$DB" "SELECT id FROM messages WHERE conversation_id='$CID' ORDER BY id DESC LIMIT 1" | sed -n 3p)"
echo "first=$FIRST last=$LAST"

runsh "'$BIN/jod' conv show '$CID'"
runsh "'$BIN/jod' conv revert '$CID' '$FIRST'"
echo "-- the head moved --"
runsh "'$BIN/jod' conv show '$CID'"
runsh "q \"SELECT id, head_id FROM conversations WHERE id='$CID'\""
echo "-- but NOTHING was deleted --"
runsh "q \"SELECT count(*) AS rows_still_there FROM messages WHERE conversation_id='$CID'\""
echo "-- and the abandoned tail is reachable again --"
runsh "'$BIN/jod' conv goto '$CID' '$LAST'"
runsh "'$BIN/jod' conv show '$CID'"
runsh "q \"SELECT id, head_id FROM conversations WHERE id='$CID'\""

section "7. PROPERTY: fork leaves the original untouched"
runsh "q \"SELECT id, head_id FROM conversations WHERE id='$CID'\""
runsh "'$BIN/jod' conv fork '$CID' --title 'a fork'"
FORK="$(python3 "$REPO/tests/e2e/jod/db.py" "$DB" "SELECT id FROM conversations WHERE forked_from='$CID'" | sed -n 3p)"
echo "fork: $FORK"
echo "-- the original's head, message count and text must all be unchanged --"
runsh "q \"SELECT id, head_id FROM conversations WHERE id='$CID'\""
runsh "q \"SELECT id, role, text FROM messages WHERE conversation_id='$CID' ORDER BY id\""
runsh "'$BIN/jod' conv show '$CID'"
echo "-- and the fork reads the same history --"
runsh "'$BIN/jod' conv show '$FORK'"
runsh "q \"SELECT id, title, head_id, forked_from, forked_at_id FROM conversations\""

section "8. does writing into the fork disturb the original?"
echo "-- revert the FORK to its first message --"
runsh "'$BIN/jod' conv revert '$FORK' '$FIRST'"
runsh "'$BIN/jod' conv show '$FORK'"
echo "-- the original must still be at its own head --"
runsh "'$BIN/jod' conv show '$CID'"
runsh "q \"SELECT id, head_id FROM conversations ORDER BY id\""

section "9. goto across the fork boundary — the help says 'sharing this root'"
runsh "'$BIN/jod' conv goto '$FORK' '$LAST'"
runsh "'$BIN/jod' conv show '$FORK'"
