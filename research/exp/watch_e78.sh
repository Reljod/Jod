#!/bin/bash
# Wait for the last chained experiment set and say so once.
LOG=/home/reljod/.claude/jobs/95056dbd/tmp/drive_e78.log
while true; do
  if grep -q "E78 CHAIN COMPLETE" "$LOG" 2>/dev/null; then
    echo "E7/E8 COMPLETE - all experiments finished"; exit 0
  fi
  sleep 60
done
