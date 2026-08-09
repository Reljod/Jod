"""One-line console summary of a single harness result, read from stdin."""

import json
import sys

try:
    r = json.loads(sys.stdin.read())
except Exception:
    print("NO OUTPUT")
    sys.exit()

if "setup_error" in r:
    print("setup failed:", r["setup_error"][:80])
    sys.exit()
if "skipped" in r:
    print("skipped:", r["skipped"])
    sys.exit()
if "error" in r:
    print("failed:", r["error"][:80])
    sys.exit()

if r["workload"] == "vector":
    print(
        "load={}s idx={}s({}) p50={}ms p95={}ms recall@1={}% recall@10={}%".format(
            r.get("load_s"),
            r.get("index_build_s"),
            r.get("index"),
            r.get("p50_ms"),
            r.get("p95_ms"),
            r.get("recall_at_1_pct"),
            r.get("recall_at_10_pct"),
        )
    )
else:
    w = r["write"]
    c = r.get("correctness", {})
    line = "{:>9} ops/s  p50={:<8} p99={:<9} err={:<6}% {}".format(
        w["throughput_ops_s"], w["p50_ms"], w["p99_ms"], w["error_rate_pct"], c.get("verdict", "")
    )
    if c.get("lost_updates"):
        line += " (-{} lost)".format(c["lost_updates"])
    if c.get("lost_writes"):
        line += " (-{} lost)".format(c["lost_writes"])
    if r.get("read"):
        line += "  | read p50={}ms p99={}ms".format(r["read"]["p50_ms"], r["read"]["p99_ms"])
    print(line)
