#!/bin/bash
# Wait for the small-corpus sweep to finish, then run the over-window
# experiment. E2 is the centrepiece - it is the only cell set where the corpus
# is larger than one context window, which is where delegation is supposed to
# start paying - so it runs on its own rather than competing for API capacity.
set -u
EXP_DIR="$(dirname "$0")"
RES=/home/reljod/.claude/jobs/95056dbd/tmp/results.jsonl
LOG=/home/reljod/.claude/jobs/95056dbd/tmp/drive_small.log
E2LOG=/home/reljod/.claude/jobs/95056dbd/tmp/drive_e2.log

while pgrep -f "drive.py --out $RES" >/dev/null 2>&1; do
  sleep 30
done
echo "small sweep finished; starting E2" >> "$E2LOG"
cd "$EXP_DIR" || exit 1
python3 drive.py --out "$RES" --trials 2 --concurrency 2 --timeout 3000 \
  --only E2 >> "$E2LOG" 2>&1
echo "E2 CHAIN COMPLETE" >> "$E2LOG"
