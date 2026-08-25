#!/usr/bin/env python3
"""Independent ranking of the surveyed approaches by a panel of other models.

The distillation to a top five is a judgement call, and a judgement call made
by the same model that wrote the survey is worth little. This puts the same
question to several models from other families through `agy`, with the
approaches presented in a shuffled order per voter so that list position cannot
drive the result, and tallies the votes with a Borda count.
"""

import argparse
import json
import os
import random
import re
import subprocess
import sys
import time
import concurrent.futures as cf
import threading
from collections import defaultdict

APPROACHES = [
    ("A1", "Orchestrator-worker: a lead agent plans, spawns several specialised subagents in parallel, and synthesises their findings"),
    ("A2", "Agent-as-tool: a specialist is called like an ordinary tool and control returns to the main agent"),
    ("A3", "Handoff: control transfers permanently to a specialist, carrying conversation state"),
    ("A4", "Single-threaded linear agent: no delegation at all, invest everything in context engineering"),
    ("A5", "Single-writer multi-reader: many agents contribute analysis but only one main loop holds state and performs writes"),
    ("A6", "Fan-out scatter-gather: parallel independent workers over a partitioned input, then a merge"),
    ("A7", "Sequential pipeline: each stage transforms the previous stage's output"),
    ("A8", "Debate: several agents argue to consensus"),
    ("A9", "Swarm: peer agents hand control to each other with no supervisor"),
    ("A10", "Hierarchical nesting: supervisors of supervisors, more than one level deep"),
    ("A11", "Capability router: the main agent escalates a hard sub-problem to a stronger model"),
    ("A12", "Blackboard: a shared workspace that specialists read from and contribute to"),
    ("B1", "Compaction: summarise the conversation and restart from the summary when the window fills"),
    ("B4", "Sub-agent context isolation: the delegated task runs in a fresh window and only a short summary returns"),
    ("B5", "Filesystem as context: notes and intermediate results live on disk, not in the context window"),
    ("B6", "Todo recitation: continually rewrite the objective at the end of the context to re-anchor attention"),
    ("B7", "KV-cache prefix stability: never mutate the prompt prefix, so the cache keeps hitting"),
    ("B9", "Tool-result clearing: drop stale large tool outputs rather than summarising them"),
    ("B10", "Just-in-time retrieval: load identifiers and fetch bodies only when needed"),
    ("B11", "Progressive disclosure: describe a capability in one line and load its full instructions only on use"),
    ("C1", "Ambient operation: the agent runs continuously on events rather than waiting for a chat turn"),
    ("C2", "Durable execution: persist execution history so a crash or interruption resumes instead of restarting"),
    ("C4", "Agent inbox: a priority queue in front of the main loop so a human can enqueue, interrupt or redirect"),
    ("C7", "Sleep-time compute: a background agent reorganises memory while the primary agent is idle"),
    ("C8", "DAG planner: the planner emits a dependency graph and ready tasks dispatch in parallel"),
    ("C10", "Supervision tree: restart failed workers under a declared policy rather than trying to prevent failure"),
    ("D1", "Self-editing memory blocks the agent updates through ordinary tool calls"),
    ("D3", "Reflection: periodically synthesise observations into higher-level insights and store those"),
    ("D4", "Externalised structured note-taking that survives a change of harness"),
    ("E1", "Evaluator-optimiser loop: a generator and a critic iterate"),
    ("E2", "Clean-context reviewer: the reviewer sees the artefact but not the history that produced it"),
    ("E4", "Veto-only review: reviewers may block but never approve"),
    ("F4", "Agent-computer interface design: few tools, unambiguous names, errors that teach"),
]

QUESTION = """You are helping evaluate architectures for one specific system.

THE SYSTEM: a single always-on personal agent. The user talks to exactly one
agent. That agent must manage its own context window, delegate work to other
agents, run asynchronously for hours without supervision, and handle several
tasks at once. The user interrupts it occasionally and expects it to keep the
thread.

Below is a list of candidate techniques, in no particular order.

%s

Pick the FIVE techniques that matter most for THIS system, ranked 1 to 5 where
1 is most important. Judge by how much the system degrades without the
technique, not by how novel or interesting it is.

Reply with ONLY five lines, nothing else, in this exact form:
1. <ID>
2. <ID>
3. <ID>
4. <ID>
5. <ID>
"""

MODELS = ["gemini-3.7-flash-high", "gemini-3.1-pro-high", "claude-sonnet-4-6",
          "gemini-3.6-flash-high", "gpt-oss-120b-medium"]

_lock = threading.Lock()


def ask(model, seed, out):
    rng = random.Random(seed)
    items = list(APPROACHES)
    rng.shuffle(items)
    listing = "\n".join("  %s. %s" % (i, d) for i, d in items)
    prompt = QUESTION % listing
    t0 = time.time()
    try:
        p = subprocess.run(
            ["agy", "-p", prompt, "--model", model, "--output-format", "json"],
            capture_output=True, text=True, timeout=900)
    except subprocess.TimeoutExpired:
        with _lock:
            print("  %-24s seed=%d TIMEOUT" % (model, seed), flush=True)
        return
    if p.returncode != 0:
        with _lock:
            print("  %-24s seed=%d exit=%d %s"
                  % (model, seed, p.returncode, p.stderr[-160:]), flush=True)
        return
    try:
        text = json.loads(p.stdout).get("response", "")
    except json.JSONDecodeError:
        with _lock:
            print("  %-24s seed=%d unparseable" % (model, seed), flush=True)
        return

    valid = {i for i, _ in APPROACHES}
    picks = []
    for line in text.splitlines():
        m = re.match(r"\s*([1-5])[.)]\s*([A-F]\d+)", line.strip())
        if m and m.group(2) in valid and m.group(2) not in picks:
            picks.append(m.group(2))
    if len(picks) < 5:  # tolerate a looser reply rather than discarding a vote
        for tok in re.findall(r"\b([A-F]\d{1,2})\b", text):
            if tok in valid and tok not in picks:
                picks.append(tok)
            if len(picks) == 5:
                break
    rec = {"model": model, "seed": seed, "picks": picks[:5],
           "wall_s": round(time.time() - t0, 1)}
    with _lock:
        with open(out, "a") as fh:
            fh.write(json.dumps(rec) + "\n")
        print("  %-24s seed=%d -> %s" % (model, seed, picks[:5]), flush=True)


def tally(path):
    votes = [json.loads(l) for l in open(path)]
    votes = [v for v in votes if len(v["picks"]) == 5]
    borda = defaultdict(int)
    firsts = defaultdict(int)
    appear = defaultdict(int)
    for v in votes:
        for rank, pid in enumerate(v["picks"]):
            borda[pid] += 5 - rank      # 1st = 5 points, 5th = 1 point
            appear[pid] += 1
            if rank == 0:
                firsts[pid] += 1
    desc = dict(APPROACHES)
    print()
    print("### Independent panel tally (%d valid ballots)" % len(votes))
    print()
    hdr = "%-6s %6s %7s %7s  %s" % ("id", "borda", "ballots", "firsts", "technique")
    print(hdr)
    print("-" * 100)
    for pid in sorted(borda, key=lambda k: (-borda[k], k)):
        print("%-6s %6d %7d %7d  %.62s"
              % (pid, borda[pid], appear[pid], firsts[pid], desc[pid]))
    return borda


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--concurrency", type=int, default=3)
    ap.add_argument("--tally-only", action="store_true")
    a = ap.parse_args()
    if not a.tally_only:
        jobs = [(m, r) for m in MODELS for r in range(a.rounds)]
        print("asking %d ballots (%d models x %d rounds), order shuffled per ballot"
              % (len(jobs), len(MODELS), a.rounds), flush=True)
        with cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
            list(ex.map(lambda j: ask(j[0], j[1], a.out), jobs))
    if os.path.exists(a.out):
        tally(a.out)


if __name__ == "__main__":
    main()
