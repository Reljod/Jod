#!/bin/bash
# The short run showed no positional effect at ~41k tokens, which is the
# expected result: context rot is reported to bite well above that. Re-run the
# same design at roughly 120k tokens, where an effect should appear if the
# recitation technique earns its keep at all.
set -u
EXP_DIR="$(dirname "$0")"
LOG=/home/reljod/.claude/jobs/95056dbd/tmp/pos_long.log

while pgrep -f "pos_test.py --corpus" >/dev/null 2>&1; do sleep 20; done
cd "$EXP_DIR" || exit 1
echo "starting long-context position test" >> "$LOG"
python3 pos_test.py \
  --corpus /home/reljod/.claude/jobs/95056dbd/tmp/bench_huge/corpus \
  --out /home/reljod/.claude/jobs/95056dbd/tmp/pos_long.jsonl \
  --words 90000 --trials 6 --concurrency 1 >> "$LOG" 2>&1
echo "POS LONG COMPLETE" >> "$LOG"
