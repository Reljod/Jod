#!/usr/bin/env bash
# Reproduce the entire analysis from scratch.
#
# Validate -> measure -> score -> report. Stops at the first failure, because a
# ranking built on a dataset that failed validation is worse than no ranking.
#
# Usage:  ./scripts/run_all.sh  [--skip-net]

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$HERE")"
cd "$ROOT"

SKIP_NET=0
[[ "${1:-}" == "--skip-net" ]] && SKIP_NET=1

echo "==> 1/3  validating dataset"
python3 scripts/validate.py | tail -3

if [[ "$SKIP_NET" -eq 0 ]]; then
  echo
  echo "==> 2/3  measuring network latency (this takes a minute)"
  python3 scripts/netcheck.py --samples 5 --workers 6 | tail -5
else
  echo
  echo "==> 2/3  skipping network measurement (--skip-net)"
fi

echo
echo "==> 3/3  scoring and generating tables"
python3 scripts/report.py --trials 20000

echo
echo "done."
echo "  narrative + recommendation : REPORT.md"
echo "  generated tables           : out/RANKINGS.md"
echo "  per-profile CSVs           : out/scores-*.csv"
