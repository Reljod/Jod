#!/bin/bash
# The over-window semantic cell is the headline result of the whole study, and
# it ran at n=2 with high variance. Add trials once the other experiments are
# done, so the central claim does not rest on two runs.
set -u
EXP_DIR="$(dirname "$0")"
RES=/home/reljod/.claude/jobs/95056dbd/tmp/results.jsonl
E78LOG=/home/reljod/.claude/jobs/95056dbd/tmp/drive_e78.log
LOG=/home/reljod/.claude/jobs/95056dbd/tmp/drive_e2more.log

while ! grep -q "E78 CHAIN COMPLETE" "$E78LOG" 2>/dev/null; do sleep 30; done
cd "$EXP_DIR" || exit 1
echo "adding trials to the over-window experiment" >> "$LOG"
python3 drive.py --out "$RES" --trials 5 --concurrency 2 --timeout 3000 \
  --only E2 >> "$LOG" 2>&1
echo "E2MORE CHAIN COMPLETE" >> "$LOG"
