#!/bin/bash
# Emit a progress line every 15 completed cells; stop when the sweep ends,
# and say so loudly if the driver dies instead of finishing.
R="$1"; L="$2"; TOTAL="${3:-87}"
prev=0
while true; do
  n=$(wc -l < "$R" 2>/dev/null || echo 0)
  if [ $((n / 15)) -gt $((prev / 15)) ]; then
    echo "sweep progress: $n/$TOTAL cells done"
  fi
  prev=$n
  if grep -q "ALL DONE" "$L" 2>/dev/null; then
    echo "SWEEP COMPLETE: $n cells recorded"; exit 0
  fi
  if ! pgrep -f "drive.py --out $R" >/dev/null 2>&1; then
    echo "DRIVER EXITED early at $n/$TOTAL cells - inspect $L"; exit 1
  fi
  sleep 45
done
