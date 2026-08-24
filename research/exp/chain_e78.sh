#!/bin/bash
# Run the two experiments that were added after the first results came in:
#   E7  the silent-failure case (every config got the scalar total wrong the
#       same way; does a second look catch what more agents did not?)
#   E8  the ambiguous-return case (a worker whose answer type matches its input
#       type returns something the merger cannot recognise as an answer)
# Both wait for the over-window sweep so they do not compete for API capacity.
set -u
EXP_DIR="$(dirname "$0")"
RES=/home/reljod/.claude/jobs/95056dbd/tmp/results.jsonl
E2LOG=/home/reljod/.claude/jobs/95056dbd/tmp/drive_e2.log
LOG=/home/reljod/.claude/jobs/95056dbd/tmp/drive_e78.log

while ! grep -q "E2 CHAIN COMPLETE" "$E2LOG" 2>/dev/null; do sleep 30; done
cd "$EXP_DIR" || exit 1
echo "starting E7/E8" >> "$LOG"
python3 drive.py --out "$RES" --trials 3 --concurrency 2 --only E7,E8 \
  >> "$LOG" 2>&1
echo "E78 CHAIN COMPLETE" >> "$LOG"
