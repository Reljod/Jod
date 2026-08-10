#!/usr/bin/env bash
# Regenerate every graph and run the whole sweep. ~15 minutes at 1M edges.
#
#   bench/run.sh [DBDIR]
#
# Results land in out/. Nothing here depends on the repo: it is plain
# python3 + sqlite3, so the numbers can be reproduced anywhere.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$here")"
dbdir="${1:-${TMPDIR:-/tmp}/jod-graph-bench}"
mkdir -p "$dbdir" "$root/out"

for scale in 1k:1000 10k:10000 100k:100000 1m:1000000; do
  label="${scale%%:*}"
  edges="${scale##*:}"
  db="$dbdir/g$label.db"
  if [ ! -f "$db" ]; then
    echo "== generating $label ($edges edges)"
    python3 "$here/gen.py" "$db" --edges "$edges"
  fi
  echo "== benchmarking $label"
  python3 "$here/bench.py" "$db" --label "$label" \
      --out "$root/out/sqlite-$label.json" > /dev/null
done

echo "== concurrency (100k)"
python3 "$here/concurrency.py" "$dbdir/g100k.db" \
    --out "$root/out/concurrency-100k.json"

echo "== engine comparison (100k)"
python3 "$here/engines.py" "$dbdir/g100k.db" \
    --out "$root/out/engines-100k.json" || echo "  (skipped: see engines.py)"

echo "== vectors"
python3 "$here/vectors.py" --out "$root/out/vectors.json" \
    || echo "  (skipped: sqlite-vec not installed)"

python3 "$here/summarize.py" "$root/out" > "$root/out/SUMMARY.md"
echo "wrote $root/out/SUMMARY.md"
