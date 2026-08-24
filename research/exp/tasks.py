#!/usr/bin/env python3
"""Task definitions and objective scoring against generated ground truth."""

import json
import re

# Every task ends with a strict ANSWER line so parsing is deterministic and
# scoring never depends on a judge's reading of prose.
ANSWER_RULE = ("End your reply with exactly one line, and nothing after it:\n"
               "ANSWER: <answer>\n")

TASKS = {
    # Trivially parallel AND trivially greppable. Included on purpose: it is
    # the control that shows what delegation overhead costs when the task did
    # not need distributing.
    "t1_scan": {
        "shape": "breadth-easy",
        "objective": (
            "The notes in this directory tree contain lines of the form\n"
            "  FACT[alpha]: CODE=NUMBER\n"
            "Find every one of them.\n"),
        "answer_format": "ANSWER: CODE=NUM,CODE=NUM,... sorted by CODE ascending",
        "key": "scan_alpha",
    },
    # Parallel but needs per-file reasoning: a join between two lines that live
    # in the same file. No single grep answers it.
    "t2_join": {
        "shape": "breadth-join",
        "objective": (
            "Each note in this directory tree has a line 'POLICY: enabled' or\n"
            "'POLICY: disabled'. Some notes also contain lines of the form\n"
            "  FACT[alpha]: CODE=NUMBER\n"
            "Report every FACT[alpha] code and number that appears in a note\n"
            "whose POLICY is enabled. A fact in a disabled note must not be\n"
            "reported.\n"),
        "answer_format": "ANSWER: CODE=NUM,CODE=NUM,... sorted by CODE ascending",
        "key": "join_alpha_enabled",
    },
    # Strictly sequential: you cannot know file N+1 until you have read N.
    # Delegation should not help and may hurt.
    "t3_chain": {
        "shape": "depth",
        "objective": (
            "Start at the note named {start}. It contains a line 'NEXT: <path>'\n"
            "pointing at another note. Follow the chain of NEXT links, note by\n"
            "note, until you reach a note containing a line 'TERMINAL: SECRET-NNNNN'.\n"
            "Report that secret.\n"),
        "answer_format": "ANSWER: SECRET-NNNNN",
        "key": "chain_secret",
    },
    # Parallel gather plus a reduce step. Tests whether aggregation survives
    # being split across workers.
    "t4_sum": {
        "shape": "breadth-reduce",
        "objective": (
            "Each note has a line 'POLICY: enabled' or 'POLICY: disabled'.\n"
            "Some notes contain lines of the form FACT[alpha]: CODE=NUMBER.\n"
            "Add up the NUMBERs from every FACT[alpha] line that appears in a\n"
            "note whose POLICY is enabled. Report only the total.\n"),
        "answer_format": "ANSWER: <integer total>",
        "key": "aggregate_sum",
    },
    # Semantic classification over every file, decided by a COMBINATION of two
    # sentences, so it needs per-file judgement rather than one pattern match.
    "t5_classify": {
        "shape": "breadth-semantic",
        "objective": (
            "A note describes a module that is RESTART-SAFE only if BOTH of\n"
            "these are true of it:\n"
            "  - it owns no state of its own beyond its input arguments, and\n"
            "  - it does not hold any long-lived connection open.\n"
            "A note that keeps a durable journal on disk is NOT restart-safe.\n"
            "A note that holds a long-lived socket open is NOT restart-safe.\n"
            "List the paths of every note whose module is restart-safe.\n"),
        "answer_format": ("ANSWER: area/note_XXX.md,area/note_YYY.md,... "
                          "sorted ascending"),
        "key": "restart_safe",
    },
}


def prompt_for(task_id, ground):
    t = TASKS[task_id]
    obj = t["objective"]
    if task_id == "t3_chain":
        obj = obj.format(start=ground["chain"]["start"])
    return obj + "\n" + ANSWER_RULE.replace("<answer>", t["answer_format"]
                                            .replace("ANSWER: ", ""))


# ---------------------------------------------------------------- parsing


def extract_answer(text):
    """Last ANSWER: line wins - models sometimes restate it."""
    if not text:
        return ""
    hits = re.findall(r"^\s*ANSWER:\s*(.*)$", text, re.MULTILINE)
    if hits:
        return hits[-1].strip()
    # Fall back to the last non-empty line so a formatting slip is not scored
    # as a total failure; that would confound format-following with capability.
    lines = [l.strip() for l in text.strip().splitlines() if l.strip()]
    return lines[-1] if lines else ""


def parse_pairs(ans):
    out = {}
    for m in re.finditer(r"([A-Z]{2}-\d{4})\s*=\s*(\d+)", ans):
        out[m.group(1)] = int(m.group(2))
    return out


def parse_paths(ans):
    return set(re.findall(r"[a-z]+/note_\d{3}\.md", ans))


def parse_int(ans):
    m = re.findall(r"-?\d[\d,]*", ans.replace(" ", ""))
    if not m:
        return None
    try:
        return int(m[-1].replace(",", ""))
    except ValueError:
        return None


# ---------------------------------------------------------------- scoring


def _prf(got, want):
    tp = len(got & want)
    p = tp / len(got) if got else 0.0
    r = tp / len(want) if want else 0.0
    f = 2 * p * r / (p + r) if (p + r) else 0.0
    return p, r, f


def score(task_id, raw_text, ground):
    """Return dict with a 0..1 'score' plus diagnostics."""
    ans = extract_answer(raw_text)

    if task_id in ("t1_scan", "t2_join"):
        want = (ground["facts"]["alpha"] if task_id == "t1_scan"
                else ground["join_alpha_enabled"])
        got = parse_pairs(ans)
        # Credit only pairs whose NUMBER is also right; a right code with a
        # wrong number is a wrong answer, not a partial one.
        got_ok = {c for c, n in got.items() if want.get(c) == n}
        p, r, f = _prf(got_ok, set(want))
        return {"score": f, "precision": p, "recall": r,
                "n_got": len(got), "n_want": len(want),
                "exact": f == 1.0 and len(got) == len(want)}

    if task_id == "t3_chain":
        want = ground["chain"]["secret"]
        ok = want in ans.upper()
        return {"score": 1.0 if ok else 0.0, "exact": ok, "want": want}

    if task_id == "t4_sum":
        want = ground["aggregate_sum"]
        got = parse_int(ans)
        ok = got == want
        return {"score": 1.0 if ok else 0.0, "exact": ok,
                "got": got, "want": want}

    if task_id == "t5_classify":
        want = set(ground["restart_safe"])
        got = parse_paths(ans)
        p, r, f = _prf(got, want)
        return {"score": f, "precision": p, "recall": r,
                "n_got": len(got), "n_want": len(want),
                "exact": got == want}

    raise KeyError(task_id)


if __name__ == "__main__":
    import sys
    ground = json.load(open(sys.argv[1]))
    for tid in TASKS:
        print("=" * 70)
        print(tid, "|", TASKS[tid]["shape"])
        print(prompt_for(tid, ground))
