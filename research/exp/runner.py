#!/usr/bin/env python3
"""Run one orchestration configuration against one task and record everything.

Each configuration is a different answer to the question the research is about:
when should the main agent do the work itself, and when should it hand a slice
of the work to a worker that has its own context window?

Modes
  solo          one agent, no delegation available
  deleg         one agent, delegation available and encouraged (agent decides)
  fanout        the script partitions the work across k workers, then a
                synthesis call merges their answers (orchestrator decides)
  verify        solo, then n critic passes over its own answer
  fanout_vague  fanout with a deliberately under-specified worker brief
  fanout_fat    fanout where workers return full reasoning, not just an answer

Every model call goes through `claude -p --output-format json`, so token
counts, cost and wall time come from the harness rather than being estimated.
"""

import argparse
import concurrent.futures as cf
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import tasks as T  # noqa: E402

BASE_TOOLS = ["Read", "Grep", "Glob", "Bash"]


def run_claude(prompt, cwd, model="haiku", tools=None, timeout=600,
               extra_args=None, attempts=3):
    """One headless agent call, retried on infrastructure failure.

    A call that fails to return parseable output has told us nothing about the
    orchestration strategy under test, so retrying it is not cherry-picking -
    scoring it as zero would be the distortion. A call that returns a bad
    answer is kept as-is.
    """
    last = None
    for i in range(attempts):
        last = _run_once(prompt, cwd, model, tools, timeout, extra_args)
        if last["ok"] or not str(last.get("error", "")).startswith(
                ("unparseable", "exit", "timeout")):
            return last
        last["retries"] = i + 1
        time.sleep(2 * (i + 1))
    return last


def _run_once(prompt, cwd, model="haiku", tools=None, timeout=600,
              extra_args=None):
    tools = tools or BASE_TOOLS
    cmd = ["claude", "-p", prompt, "--model", model,
           "--output-format", "json", "--allowedTools"] + tools
    if extra_args:
        cmd += extra_args
    t0 = time.time()
    try:
        proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True,
                              timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": "timeout", "text": "", "cost": 0.0,
                "wall": time.time() - t0, "usage": {}, "turns": 0}
    wall = time.time() - t0
    if proc.returncode != 0:
        return {"ok": False, "error": "exit%d: %s" % (proc.returncode,
                                                      proc.stderr[-300:]),
                "text": "", "cost": 0.0, "wall": wall, "usage": {}, "turns": 0}
    try:
        d = json.loads(proc.stdout)
    except json.JSONDecodeError:
        # Keep the head of stdout/stderr. Without it an infrastructure failure
        # is indistinguishable from a model that answered badly, and the two
        # call for completely different conclusions.
        return {"ok": False,
                "error": "unparseable stdout: out=%r err=%r"
                         % (proc.stdout[:200], proc.stderr[:200]),
                "text": "", "cost": 0.0, "wall": wall, "usage": {}, "turns": 0}
    u = d.get("usage", {}) or {}
    return {
        "ok": not d.get("is_error", False),
        "error": d.get("api_error_status"),
        "text": d.get("result", "") or "",
        "cost": d.get("total_cost_usd", 0.0) or 0.0,
        "wall": wall,
        "turns": d.get("num_turns", 0),
        "usage": {
            "in": u.get("input_tokens", 0),
            "out": u.get("output_tokens", 0),
            "cache_read": u.get("cache_read_input_tokens", 0),
            "cache_create": u.get("cache_creation_input_tokens", 0),
        },
        "denials": d.get("permission_denials", []),
        # Whether the agent actually took up the offer to delegate. Without
        # this the "delegation allowed" condition is unfalsifiable: a run that
        # looks like solo may be one that simply declined to delegate.
        "spawned": (d.get("subagent_stats") or {}).get("spawned", 0),
    }


def shard(files, k):
    out = [[] for _ in range(k)]
    for i, f in enumerate(sorted(files)):
        out[i % k].append(f)
    return out


def list_notes(corpus):
    got = []
    for root, _, fns in os.walk(corpus):
        for fn in fns:
            if fn.endswith(".md"):
                got.append(os.path.relpath(os.path.join(root, fn), corpus))
    return sorted(got)


# ------------------------------------------------------------------ modes


def mode_solo(objective, corpus, model, allow_task=False, **kw):
    tools = list(BASE_TOOLS) + (["Task"] if allow_task else [])
    pre = ""
    if allow_task:
        pre = ("You may delegate parts of this work to subagents with the Task\n"
               "tool if that helps. Each subagent has its own context window.\n\n")
    r = run_claude(pre + objective, corpus, model, tools)
    return {"text": r["text"], "calls": [r]}


def mode_fanout(objective, corpus, model, workers=4, brief="rich",
                ret="thin", **kw):
    notes = list_notes(corpus)
    shards = shard(notes, workers)

    def worker(i):
        fl = "\n".join("  " + f for f in shards[i])
        if brief == "rich":
            head = (
                "You are worker %d of %d on a larger task. Another agent will\n"
                "merge your answer with the other workers' answers.\n\n"
                "YOUR SLICE - examine ONLY these files and no others:\n%s\n\n"
                "Do not look at files outside your slice; another worker owns\n"
                "them and duplicated work is wasted.\n\n"
                "THE TASK, applied only to your slice:\n%s\n"
            ) % (i + 1, workers, fl, objective)
        else:  # deliberately under-specified, to test MAST's claim that
               # most multi-agent failure is bad task specification
            head = ("Help with this task. Your part is roughly these files:\n%s\n\n%s\n"
                    ) % (fl, objective)
        if ret == "thin":
            head += ("\nReply with ONLY the single ANSWER line. No explanation,\n"
                     "no preamble, no reasoning.\n")
        elif ret == "labelled":
            # Same content as 'thin', but the worker states what its list
            # MEANS. Without this, a worker whose answer type matches its input
            # type - a list of file paths, when it was assigned a list of file
            # paths - returns something the merger cannot tell apart from its
            # own assignment.
            head += ("\nReply with ONLY the single ANSWER line, and make the line\n"
                     "self-describing: it must say that these are the items you\n"
                     "CONFIRMED satisfy the task, not the items you were asked to\n"
                     "examine. Write it as:\n"
                     "ANSWER: confirmed matches in my slice = <items>\n")
        else:
            head += ("\nExplain your reasoning fully, file by file, showing what\n"
                     "you found in each, then give the ANSWER line.\n")
        return run_claude(head, corpus, model)

    calls = []
    with cf.ThreadPoolExecutor(max_workers=min(workers, 4)) as ex:
        futs = [ex.submit(worker, i) for i in range(workers)]
        results = [f.result() for f in futs]
    calls.extend(results)

    parts = []
    for i, r in enumerate(results):
        body = r["text"] if ret == "fat" else T.extract_answer(r["text"])
        head = ("--- worker %d: ITEMS THIS WORKER CONFIRMED MATCH THE TASK ---"
                if ret == "labelled" else "--- worker %d ---")
        parts.append((head % (i + 1)) + "\n" + body)
    merge_prompt = (
        "You are the lead agent. You split a task across %d workers, each of\n"
        "which saw a different slice of the files. Their replies follow.\n\n"
        "%s\n\n"
        "Merge them into one final answer for the whole task. Remove duplicates.\n"
        "Do not re-read the files; work only from what the workers reported.\n\n"
        "THE ORIGINAL TASK:\n%s\n"
    ) % (workers, "\n\n".join(parts), objective)
    m = run_claude(merge_prompt, corpus, model, tools=["Read"])
    calls.append(m)
    # Keep each worker's own answer so we can measure whether workers stayed
    # inside their slice. Overlap between workers is the observable signature
    # of a brief that failed to draw a boundary.
    wa = [T.extract_answer(r["text"]) for r in results]
    return {"text": m["text"], "calls": calls, "worker_answers": wa,
            "shard_sizes": [len(s) for s in shards]}


def mode_verify(objective, corpus, model, passes=1, clean=True, **kw):
    first = run_claude(objective, corpus, model)
    calls = [first]
    text = first["text"]
    for _ in range(passes):
        if clean:
            # Clean-context critic: sees the answer, not the work that made it.
            crit = (
                "Another agent produced the answer below for the task that\n"
                "follows. You did not see how it worked. Check it against the\n"
                "actual files yourself. If it is wrong or incomplete, produce\n"
                "the corrected answer; if it is right, repeat it unchanged.\n\n"
                "THEIR ANSWER:\n%s\n\nTHE TASK:\n%s\n"
            ) % (T.extract_answer(text), objective)
        else:
            # Shared-context self-review: the trap condition in the literature.
            crit = (
                "Here is your own earlier working and answer. Review it and\n"
                "produce a final answer.\n\nYOUR EARLIER WORK:\n%s\n\nTHE TASK:\n%s\n"
            ) % (text[-4000:], objective)
        r = run_claude(crit, corpus, model)
        calls.append(r)
        text = r["text"]
    return {"text": text, "calls": calls}


MODES = {
    "solo": lambda *a, **k: mode_solo(*a, allow_task=False, **k),
    "deleg": lambda *a, **k: mode_solo(*a, allow_task=True, **k),
    "fanout": mode_fanout,
    "verify": mode_verify,
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--ground", required=True)
    ap.add_argument("--task", required=True, choices=list(T.TASKS))
    ap.add_argument("--mode", required=True)
    ap.add_argument("--model", default="haiku")
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--brief", default="rich", choices=["rich", "vague"])
    ap.add_argument("--ret", default="thin",
                    choices=["thin", "fat", "labelled"])
    ap.add_argument("--passes", type=int, default=1)
    ap.add_argument("--clean", default="1")
    ap.add_argument("--trial", type=int, default=0)
    ap.add_argument("--label", default="")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()

    ground = json.load(open(a.ground))
    objective = T.prompt_for(a.task, ground)

    t0 = time.time()
    res = MODES[a.mode](objective, a.corpus, a.model,
                        workers=a.workers, brief=a.brief, ret=a.ret,
                        passes=a.passes, clean=(a.clean == "1"))
    wall = time.time() - t0

    sc = T.score(a.task, res["text"], ground)
    calls = res["calls"]
    rec = {
        "label": a.label or "%s/%s" % (a.mode, a.task),
        "task": a.task, "shape": T.TASKS[a.task]["shape"], "mode": a.mode,
        "model": a.model, "workers": a.workers, "brief": a.brief,
        "ret": a.ret, "passes": a.passes, "clean": a.clean,
        "trial": a.trial, "corpus": os.path.basename(os.path.dirname(a.corpus)),
        "wall_s": round(wall, 1),
        "n_calls": len(calls),
        "cost_usd": round(sum(c["cost"] for c in calls), 5),
        "out_tokens": sum(c["usage"].get("out", 0) for c in calls),
        "in_tokens": sum(c["usage"].get("in", 0) for c in calls),
        "cache_read": sum(c["usage"].get("cache_read", 0) for c in calls),
        "cache_create": sum(c["usage"].get("cache_create", 0) for c in calls),
        "turns": sum(c["turns"] for c in calls),
        "spawned": sum(c.get("spawned", 0) for c in calls),
        "errors": [c["error"] for c in calls if not c["ok"]],
        "answer": T.extract_answer(res["text"])[:1200],
    }
    if "worker_answers" in res:
        wa = res["worker_answers"]
        rec["worker_answers"] = [w[:400] for w in wa]
        rec["shard_sizes"] = res["shard_sizes"]
        # Duplication = items claimed by more than one worker, as a fraction of
        # all items claimed. Zero means the boundary held.
        seen, dup, total = {}, 0, 0
        for i, w in enumerate(wa):
            items = set(T.parse_pairs(w)) | T.parse_paths(w)
            for it in items:
                total += 1
                if it in seen and seen[it] != i:
                    dup += 1
                seen.setdefault(it, i)
        rec["dup_rate"] = round(dup / total, 4) if total else 0.0
    rec.update(sc)
    with open(a.out, "a") as fh:
        fh.write(json.dumps(rec) + "\n")
    print("%-28s score=%.3f cost=$%.4f wall=%.0fs calls=%d %s"
          % (rec["label"], rec["score"], rec["cost_usd"], rec["wall_s"],
             rec["n_calls"], "ERR:" + str(rec["errors"]) if rec["errors"] else ""))


if __name__ == "__main__":
    main()
