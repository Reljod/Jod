#!/usr/bin/env bash
# Poll the NEWEST run in a JOD_HOME until it leaves `running`.
#
# `jod ls --json` is not ordered newest-first, so an earlier version of this
# script took element [0] and cheerfully reported "settled" while the run under
# test was still going. Sort by created_at_ms and take the last.
home="$1"
jod="$2"
for _ in $(seq 1 80); do
  s=$(JOD_HOME="$home" "$jod" ls --json | python3 -c \
    "import json,sys; rs=json.load(sys.stdin); print(sorted(rs,key=lambda r:r['created_at_ms'])[-1]['status'])")
  if [ "$s" != "running" ]; then
    echo "settled: $s"
    exit 0
  fi
  sleep 15
done
echo "still running after 20 minutes"
