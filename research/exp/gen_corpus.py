#!/usr/bin/env python3
"""Generate a synthetic 'needle farm' with known ground truth.

The point of a synthetic corpus is that scoring becomes objective. We know
exactly which facts exist, so an agent's answer is graded by set overlap rather
than by another model's opinion. Judges are reserved for genuinely subjective
questions.

Task shapes deliberately differ in how parallelisable they are, because the
central claim under test is that task shape - not model quality - decides
whether delegation pays:

  T1 breadth-scan   find all FACT[alpha] lines        trivially parallel, also
                                                      trivially greppable
  T2 join           FACT[alpha] but only where the    parallel, needs per-file
                    same file has POLICY: enabled     reasoning (a join)
  T3 depth-chain    follow NEXT: links to a secret    strictly sequential
  T4 aggregate      sum the T2 numbers                parallel + a reduce step
  T5 classify       which notes are restart-safe      parallel, semantic

Line formats:
  FACT[topic]: CODE=NUMBER
  POLICY: enabled | disabled
  NEXT: <relative path>
  TERMINAL: SECRET-NNNNN
"""

import argparse
import json
import os
import random

TOPICS = ["alpha", "beta", "gamma", "delta"]
AREAS = ["billing", "identity", "transport", "storage", "scheduler", "telemetry"]

FILLER = [
    "Configuration is read once at start-up and cached for the lifetime of the process.",
    "Callers must not assume ordering between independent writes to this component.",
    "A background reaper removes entries whose lease has expired.",
    "Errors are surfaced to the caller rather than swallowed, so a failure is visible.",
    "The queue is bounded; producers block when it is full rather than dropping work.",
    "Metrics are emitted per operation and aggregated by the collector every minute.",
    "Schema migrations run at open time and are idempotent across repeated starts.",
    "The lock is advisory and a stale holder is detected by heartbeat rather than timeout.",
    "Retries use exponential backoff with jitter to avoid synchronised thundering herds.",
    "Callers receive a typed error rather than a string so failures can be matched on.",
]

# T5: restart-safety is decided by the COMBINATION of two properties, and each
# property is expressed by one of several paraphrases. That combination is what
# makes the task genuinely semantic: no single grep answers it, and an agent
# cannot know the paraphrase set in advance to grep for all of them.
STATE_SENTENCES = {
    True: [
        "This module owns no state of its own beyond its input arguments.",
        "Nothing is persisted here; every value is derived on demand from the request.",
        "The component is purely functional and keeps nothing between invocations.",
        "All working data is discarded once the call returns.",
        "It stores nothing locally and defers every write to the caller.",
        "Between requests this component retains no information whatsoever.",
    ],
    False: [
        "This module keeps a durable write-ahead journal on local disk.",
        "Progress is checkpointed to a local file so a restart can resume.",
        "The component maintains an on-disk index that outlives the process.",
        "State accumulates in a local database file under the data directory.",
        "A spool directory on disk holds pending items until they are acknowledged.",
        "It persists its cursor locally so that it can pick up where it left off.",
    ],
}
HANDLE_SENTENCES = {
    True: [
        "All external connections are opened per request and closed on return.",
        "Every outbound call uses a fresh connection that is torn down afterwards.",
        "No file descriptor is retained once the operation completes.",
        "Connections are borrowed from the caller and never cached here.",
        "The component holds nothing open while it is idle.",
        "Sockets are short-lived and scoped to a single exchange.",
    ],
    False: [
        "A long-lived socket to the upstream service is held open by this module.",
        "It maintains a persistent connection pool for the lifetime of the process.",
        "A watch is registered upstream and kept open until shutdown.",
        "The component pins an open file handle for as long as it runs.",
        "A streaming subscription stays connected between requests.",
        "It keeps a session open upstream rather than reconnecting each time.",
    ],
}


def filler_block(rng, n):
    return "\n\n".join(rng.choice(FILLER) for _ in range(n))


def build(outdir, n_files, facts_per_topic, chain_len, filler_paras, seed):
    rng = random.Random(seed)
    os.makedirs(outdir, exist_ok=True)

    files = []
    for i in range(n_files):
        area = AREAS[i % len(AREAS)]
        files.append(os.path.join(area, "note_%03d.md" % i))

    contents = {p: [] for p in files}
    ground = {"n_files": n_files}

    # --- POLICY flag on every file (the join key for T2/T4) -------------
    policy = {p: rng.random() < 0.5 for p in files}
    for p in files:
        contents[p].append("POLICY: %s" % ("enabled" if policy[p] else "disabled"))

    # --- T5 restart safety: compound condition -------------------------
    safe_map = {}
    for p in files:
        stateless = rng.random() < 0.5
        no_handle = rng.random() < 0.5
        contents[p].append(rng.choice(STATE_SENTENCES[stateless]))
        contents[p].append(rng.choice(HANDLE_SENTENCES[no_handle]))
        safe_map[p] = stateless and no_handle
    ground["restart_safe"] = sorted(p for p in files if safe_map[p])

    # --- planted facts (T1) --------------------------------------------
    ground["facts"] = {}
    ground["fact_file"] = {}
    used = set()
    for topic in TOPICS:
        chosen = rng.sample(files, facts_per_topic)
        ground["facts"][topic] = {}
        for p in chosen:
            while True:
                code = "%s-%04d" % (topic[:2].upper(), rng.randint(1000, 9999))
                if code not in used:
                    used.add(code)
                    break
            number = rng.randint(10, 999)
            contents[p].append("FACT[%s]: %s=%d" % (topic, code, number))
            ground["facts"][topic][code] = number
            ground["fact_file"][code] = p

    # --- T2 join: alpha facts in POLICY-enabled files -------------------
    join = {c: n for c, n in ground["facts"]["alpha"].items()
            if policy[ground["fact_file"][c]]}
    ground["join_alpha_enabled"] = join
    ground["aggregate_sum"] = sum(join.values())   # T4

    # --- T3 planted chain ----------------------------------------------
    chain_files = rng.sample(files, chain_len + 1)
    for i in range(chain_len):
        contents[chain_files[i]].append("NEXT: %s" % chain_files[i + 1])
    secret = "SECRET-%05d" % rng.randint(10000, 99999)
    contents[chain_files[chain_len]].append("TERMINAL: %s" % secret)
    ground["chain"] = {"start": chain_files[0], "length": chain_len,
                       "secret": secret, "path": chain_files}

    # --- write ----------------------------------------------------------
    for p in files:
        full = os.path.join(outdir, p)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        # Scatter the planted lines through the filler rather than leaving them
        # in one block. A contiguous block would let an agent find everything
        # from a single hit's surrounding lines.
        blocks = [rng.choice(FILLER) for _ in range(filler_paras * 2)]
        for line in contents[p]:
            blocks.insert(rng.randint(0, len(blocks)), line)
        body = ["# %s" % os.path.basename(p).replace(".md", "")] + blocks
        with open(full, "w") as fh:
            fh.write("\n\n".join(body) + "\n")

    gt_path = os.path.join(os.path.dirname(outdir.rstrip("/")), "ground_truth.json")
    with open(gt_path, "w") as fh:
        json.dump(ground, fh, indent=2, sort_keys=True)
    return ground, gt_path


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--files", type=int, default=60)
    ap.add_argument("--facts-per-topic", type=int, default=8)
    ap.add_argument("--chain-len", type=int, default=5)
    ap.add_argument("--filler-paras", type=int, default=6)
    ap.add_argument("--seed", type=int, default=7)
    a = ap.parse_args()
    g, gt = build(a.out, a.files, a.facts_per_topic, a.chain_len,
                  a.filler_paras, a.seed)
    print("files=%d  alpha=%d  join=%d  sum=%d  safe=%d  chain=%d  secret=%s"
          % (a.files, len(g["facts"]["alpha"]), len(g["join_alpha_enabled"]),
             g["aggregate_sum"], len(g["restart_safe"]), g["chain"]["length"],
             g["chain"]["secret"]))
    print("ground truth -> %s" % gt)
