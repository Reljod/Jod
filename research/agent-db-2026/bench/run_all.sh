#!/usr/bin/env bash
# Benchmark sweep. Everything runs inside one Docker network so the embedded
# engines and the server engines are measured on the same kernel, the same
# filesystem and the same CPU budget.
#
#   ./run_all.sh                       full sweep (truncates out/raw-results.jsonl)
#   ./run_all.sh --quick               shorter durations, smaller vector set
#   ./run_all.sh vector mixed          run only these stages, appending results
#   ./run_all.sh variance              repeat one cell N times to measure noise
#
# Stages: rmw append mixed vector variance
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/../out"
NET=jodbench
VOL=jodbench-data
RESULTS="$OUT/raw-results.jsonl"

DURATION=10
VECTORS=30000
RMW_OPS=200
REPS=3

ARGS=()
for a in "$@"; do
  case "$a" in
    --quick) DURATION=5; VECTORS=10000; RMW_OPS=100 ;;
    *) ARGS+=("$a") ;;
  esac
done

if [[ ${#ARGS[@]} -eq 0 ]]; then
  STAGES=(rmw append mixed vector)
  mkdir -p "$OUT"; : > "$RESULTS"      # full run starts clean
else
  STAGES=("${ARGS[@]}")
  mkdir -p "$OUT"; touch "$RESULTS"    # partial run appends
fi

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

require_docker() {
  if ! docker info >/dev/null 2>&1; then
    echo "docker daemon is not responding — start Docker and retry" >&2
    exit 1
  fi
}

cleanup() {
  say "tearing down"
  docker rm -f jod-pg jod-redis jod-qdrant >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
trap cleanup EXIT

require_docker

say "provisioning"
docker network create "$NET" >/dev/null 2>&1 || true
docker volume rm "$VOL" >/dev/null 2>&1 || true
docker volume create "$VOL" >/dev/null

docker rm -f jod-pg jod-redis jod-qdrant >/dev/null 2>&1 || true
docker run -d --name jod-pg --network "$NET" \
  -e POSTGRES_USER=jod -e POSTGRES_PASSWORD=jod -e POSTGRES_DB=jod \
  pgvector/pgvector:pg18 \
  -c shared_buffers=512MB -c max_connections=100 -c synchronous_commit=on >/dev/null
docker run -d --name jod-redis --network "$NET" redis:8 \
  redis-server --appendonly yes --appendfsync everysec >/dev/null
docker run -d --name jod-qdrant --network "$NET" qdrant/qdrant:latest >/dev/null

say "building bench image"
docker build -q -t jod-bench "$HERE" >/dev/null || exit 1

say "waiting for servers"
for _ in $(seq 1 60); do docker exec jod-pg pg_isready -U jod >/dev/null 2>&1 && break; sleep 1; done
for _ in $(seq 1 60); do docker exec jod-redis redis-cli ping >/dev/null 2>&1 && break; sleep 1; done
for _ in $(seq 1 60); do
  docker run --rm --network "$NET" curlimages/curl:latest -sf http://jod-qdrant:6333/readyz >/dev/null 2>&1 && break
  sleep 1
done
echo "servers up"

# record what else was competing for the CPU — this machine is not dedicated
say "co-tenant load at start (affects every number below)"
docker stats --no-stream --format '{{.Name}} {{.CPUPerc}}' | grep -v '^jod-' | head -8 \
  | tee "$OUT/co-tenants.txt"

bench() {  # bench <db> <workload> [extra args...]
  local db="$1" wl="$2"; shift 2
  printf '  %-16s %-8s ' "$db" "$wl"
  docker run --rm --network "$NET" -v "$VOL:/data" \
    -e PG_DSN=postgresql://jod:jod@jod-pg:5432/jod \
    -e REDIS_HOST=jod-redis -e QDRANT_HOST=jod-qdrant -e BENCH_DATA=/data \
    jod-bench --db "$db" --workload "$wl" "$@" 2>/tmp/bench.err | tee -a "$RESULTS" \
    | python3 "$HERE/summarize.py"
  [[ -s /tmp/bench.err ]] && head -2 /tmp/bench.err | sed 's/^/      ! /'
  return 0
}

has() { [[ " ${STAGES[*]} " == *" $1 "* ]]; }

if has rmw; then
  say "rmw — contended read-modify-write, 8 writers x $RMW_OPS ops, 4 hot keys"
  for db in sqlite sqlite-naive postgres postgres-naive redis redis-naive lancedb qdrant duckdb; do
    bench "$db" rmw --writers 8 --ops "$RMW_OPS"
  done
fi

if has append; then
  say "append — write throughput vs writer count, ${DURATION}s per cell"
  for w in 1 4 8 16; do
    echo "-- $w writers --"
    for db in sqlite postgres redis duckdb; do
      bench "$db" append --writers "$w" --duration "$DURATION"
    done
    # LanceDB is orders of magnitude slower here; cap its cost
    bench lancedb append --writers "$w" --duration 5
  done
  say "append — the same engine configured naively (8 writers)"
  bench sqlite-naive append --writers 8 --duration "$DURATION"
fi

if has mixed; then
  say "mixed — 4 writers appending while 4 readers query, ${DURATION}s"
  for db in sqlite postgres redis lancedb; do
    bench "$db" mixed --writers 4 --readers 4 --duration "$DURATION"
  done
fi

if has vector; then
  say "vector — $VECTORS x 384d, top-10, recall measured against exact cosine"
  for db in sqlite postgres qdrant lancedb redis; do
    bench "$db" vector --vectors "$VECTORS"
  done
fi

if has variance; then
  say "variance — $REPS repeats of one cell, to size the noise on this machine"
  for _ in $(seq 1 "$REPS"); do
    for db in sqlite postgres redis; do
      bench "$db" append --writers 8 --duration "$DURATION"
    done
  done
fi

say "done -> $RESULTS"
wc -l < "$RESULTS"
