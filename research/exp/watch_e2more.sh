#!/bin/bash
# Wait for the added over-window trials, then say so once.
LOG=/home/reljod/.claude/jobs/95056dbd/tmp/drive_e2more.log
while true; do
  if grep -q "E2MORE CHAIN COMPLETE" "$LOG" 2>/dev/null; then
    echo "EXTRA OVER-WINDOW TRIALS COMPLETE"; exit 0
  fi
  sleep 60
done
