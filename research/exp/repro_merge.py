#!/usr/bin/env python3
"""Reproduce the merge step in isolation.

The fan-out workers returned correct answers and the merging agent replied by
asking for those answers. Either the prompt was built wrong (my bug, and the
fan-out scores are invalid) or the merging agent failed to read a prompt that
plainly contained them (a genuine finding about the synthesis step). This
prints the exact prompt so the two can be told apart.
"""
import json
import subprocess
import sys

sys.path.insert(0, "/home/reljod/repo/Jod/.claude/worktrees/research+main-agent-pattern/research/exp")
import tasks as T  # noqa: E402

CORPUS = "/home/reljod/.claude/jobs/95056dbd/tmp/bench/corpus"
GT = "/home/reljod/.claude/jobs/95056dbd/tmp/bench/ground_truth.json"

WORKERS = [
    "identity/note_013.md,storage/note_039.md,telemetry/note_053.md,transport/note_014.md",
    "telemetry/note_011.md,transport/note_020.md",
    "billing/note_012.md,transport/note_026.md",
    "billing/note_018.md,identity/note_007.md,storage/note_009.md,storage/note_057.md",
]

ground = json.load(open(GT))
objective = T.prompt_for("t5_classify", ground)
parts = ["--- worker %d ---\n%s" % (i + 1, w) for i, w in enumerate(WORKERS)]
merge_prompt = (
    "You are the lead agent. You split a task across %d workers, each of\n"
    "which saw a different slice of the files. Their replies follow.\n\n"
    "%s\n\n"
    "Merge them into one final answer for the whole task. Remove duplicates.\n"
    "Do not re-read the files; work only from what the workers reported.\n\n"
    "THE ORIGINAL TASK:\n%s\n"
) % (len(WORKERS), "\n\n".join(parts), objective)

print("=" * 72)
print("PROMPT ACTUALLY SENT (%d chars):" % len(merge_prompt))
print("=" * 72)
print(merge_prompt)
print("=" * 72)

p = subprocess.run(["claude", "-p", merge_prompt, "--model", "haiku",
                    "--allowedTools", "Read", "--output-format", "json"],
                   cwd=CORPUS, capture_output=True, text=True, timeout=600)
d = json.loads(p.stdout)
print("REPLY:", repr(d.get("result", ""))[:800])
print("scored:", T.score("t5_classify", d.get("result", ""), ground))
