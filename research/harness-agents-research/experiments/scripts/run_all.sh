#!/usr/bin/env bash
# Full experiment, start to finish. Pure stdlib Python 3, no network, no keys.
# Runtime: about two minutes on a laptop.
set -euo pipefail

cd "$(dirname "$0")/.."
export PYTHONPATH=scripts

echo "==> generating corpus"
python3 scripts/corpus.py

echo "==> sweeping fusion weights (separate tuning seed)"
python3 scripts/sweep.py

echo "==> scoring every strategy"
python3 scripts/evaluate.py

echo "==> minScore sensitivity"
python3 scripts/sensitivity.py

echo "==> seed stability"
python3 scripts/stability.py

echo "==> rendering out/RANKINGS.md"
python3 scripts/report.py

echo "done. see out/RANKINGS.md and FINDINGS.md"
