#!/usr/bin/env bash
# Full experiment, start to finish. Pure stdlib Python 3, no network, no keys.
# Runtime: about two minutes on a laptop.
set -euo pipefail

cd "$(dirname "$0")/.."
export PYTHONPATH=scripts
# Python randomises string hashing per process, so set iteration order — and
# therefore float summation order in the dense channel — varies run to run.
# That flips the occasional near-tie and makes the artefacts non-reproducible.
# Pinning the seed removes the whole class; the sorted() iterations in
# retrieval.py cover the paths that matter even without it.
export PYTHONHASHSEED=0

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

echo "==> round 2: multi-workspace corpus"
python3 scripts/corpus_scoped.py

echo "==> round 2: scoring the lab mechanisms"
python3 scripts/evaluate_lab.py

echo "==> round 2: security sweep + seed stability"
python3 scripts/sweep_lab.py

echo "==> rendering out/RANKINGS-2.md"
python3 scripts/report_lab.py

echo "done. round 1: out/RANKINGS.md + FINDINGS.md"
echo "      round 2: out/RANKINGS-2.md + FINDINGS-2.md"
