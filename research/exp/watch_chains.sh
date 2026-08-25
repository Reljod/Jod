#!/bin/bash
# Watch the two chained jobs (over-window sweep, long-context position test)
# and emit one line when each finishes, plus a line if either dies early.
E2LOG=/home/reljod/.claude/jobs/95056dbd/tmp/drive_e2.log
POSLOG=/home/reljod/.claude/jobs/95056dbd/tmp/pos_long.log
e2_done=0; pos_done=0
while true; do
  if [ $e2_done -eq 0 ] && grep -q "E2 CHAIN COMPLETE" "$E2LOG" 2>/dev/null; then
    echo "E2 over-window sweep COMPLETE"; e2_done=1
  fi
  if [ $pos_done -eq 0 ] && grep -q "POS LONG COMPLETE" "$POSLOG" 2>/dev/null; then
    echo "long-context position test COMPLETE"; pos_done=1
  fi
  if [ $e2_done -eq 1 ] && [ $pos_done -eq 1 ]; then
    echo "ALL CHAINED JOBS COMPLETE"; exit 0
  fi
  sleep 60
done
