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

echo "== out-degree hubs (100k)"
python3 "$here/outdegree.py" "$dbdir/g100k.db" \
    --out "$root/out/outdegree-100k.json" > /dev/null

# These two want `kuzu`, `duckdb` and `sqlite_vec` — point PY at a venv that
# has them, e.g.  PY=/path/to/venv/bin/python bench/run.sh
PY="${PY:-python3}"
echo "== engine comparison (100k)"
"$PY" "$here/engines.py" "$dbdir/g100k.db" \
    --out "$root/out/engines-100k.json" > /dev/null \
    || echo "  (skipped: needs kuzu + duckdb)"

echo "== vectors"
"$PY" "$here/vectors.py" --out "$root/out/vectors.json" > /dev/null \
    || echo "  (skipped: needs sqlite-vec)"

# The GraphQLite facts need its prebuilt .so:
#   pip download graphqlite  /  cargo add graphqlite, then
#   GQL_EXT=…/libs/graphqlite-linux-x86_64.so bench/run.sh
if [ -n "${GQL_EXT:-}" ]; then
  echo "== GraphQLite"
  python3 "$here/graphqlite_facts.py" --ext "$GQL_EXT" \
      > "$root/out/graphqlite-facts.txt" 2>&1 || true
fi

# And the check on the engine Jod actually ships (SQLite 3.50.2, not the
# system 3.46.1 the sweep above ran on).
if command -v cargo > /dev/null; then
  echo "== rust check (rusqlite bundled)"
  ( cd "$here/rust-check" && cargo run --release --quiet -- \
      "$dbdir/g100k.db" ) > "$root/out/rust-check-100k.txt" 2>&1 || true
fi

python3 "$here/summarize.py" "$root/out" > "$root/out/SUMMARY.md"
echo "wrote $root/out/SUMMARY.md"
