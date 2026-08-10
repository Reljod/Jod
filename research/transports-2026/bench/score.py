#!/usr/bin/env python3
"""Score the design iterations against the fixed rubric.

The rubric is fixed BEFORE the iterations are scored (see REPORT.md §Rubric),
and the arithmetic is done here rather than by hand so the ranking cannot be
quietly nudged toward whichever design was written last.

    python3 research/transports-2026/bench/score.py > research/transports-2026/out/05-scores.txt
"""

# name -> (weight, what a 5 means)
RUBRIC = {
    "S1 signature / spoof resistance": (15, "raw-byte HMAC, constant time, every negative case refused"),
    "S2 prompt-injection containment": (15, "untrusted text can never occupy an instruction position"),
    "S3 secrets + exposure surface": (10, "no inbound port, secrets off-database, 0600, fingerprint only"),
    "D1 delivery correctness": (15, "dedupe, at-least-once, crash-safe across a restart"),
    "D2 ack within GitHub's 10s": (10, "ack in single-digit ms, work strictly afterwards"),
    "O  operator simplicity (1 VPS)": (10, "one process, one config file, nothing else to run"),
    "W  dependency weight": (8, "no new crates"),
    "T  testability, no live creds": (9, "every claim asserted offline against fixtures"),
    "U  Telegram UX": (4, "streaming, chunking, keyboards, completion notifications"),
    "M  memory integration": (4, "writes to jod.db with the correct trust class"),
}

CRITS = list(RUBRIC)

# 0-5 per criterion, in RUBRIC order.
ITERATIONS = [
    ("I1  naive synchronous handler",        [0, 0, 1, 0, 0, 5, 5, 2, 0, 0]),
    ("I2  async ack + in-memory queue",      [0, 0, 1, 2, 5, 5, 5, 3, 0, 0]),
    ("I3  + HMAC over raw bytes",            [5, 0, 2, 2, 5, 4, 5, 5, 0, 0]),
    ("I4  + durable row, dedupe, sweep",     [5, 0, 2, 5, 5, 4, 5, 5, 0, 2]),
    ("I5  + rule expression language",       [5, 1, 2, 5, 5, 2, 4, 2, 0, 2]),
    ("I6  fixed condition vocabulary",       [5, 2, 2, 5, 5, 4, 5, 5, 0, 2]),
    ("I7  + injection containment",          [5, 4, 3, 5, 5, 4, 5, 5, 0, 4]),
    ("I8  + exposure hardening",             [5, 4, 5, 5, 5, 3, 5, 5, 0, 4]),
    ("I9  + Telegram long-poll loop",        [5, 4, 5, 5, 5, 3, 4, 4, 3, 4]),
    ("I10 + Telegram rendering, trimmed",    [5, 5, 5, 5, 5, 4, 4, 5, 5, 5]),
    ("A   GitHub Actions + Tailscale",       [5, 3, 4, 4, 5, 5, 5, 3, 0, 0]),
]


def total(scores):
    return sum(RUBRIC[c][0] * s / 5 for c, s in zip(CRITS, scores))


def main():
    print("# Rubric\n")
    print(f"{'criterion':<34}{'weight':>8}   a 5 means")
    for name, (w, meaning) in RUBRIC.items():
        print(f"{name:<34}{w:>8}   {meaning}")
    print(f"\n{'total':<34}{sum(w for w, _ in RUBRIC.values()):>8}")

    print("\n\n# Scores (0-5 per criterion)\n")
    head = "".join(f"{c.split()[0]:>5}" for c in CRITS)
    print(f"{'iteration':<36}{head}{'TOTAL':>9}")
    for name, scores in ITERATIONS:
        row = "".join(f"{s:>5}" for s in scores)
        print(f"{name:<36}{row}{total(scores):>9.1f}")

    print("\n\n# Ranking, by score\n")
    print(f"{'rank':<6}{'iteration':<36}{'score':>8}   {'delta vs prev iteration':>10}")
    ranked = sorted(ITERATIONS, key=lambda kv: total(kv[1]), reverse=True)
    prev = {}
    for i, (name, scores) in enumerate(ITERATIONS):
        prev[name] = total(scores) - (total(ITERATIONS[i - 1][1]) if i > 0 else 0)
    for rank, (name, scores) in enumerate(ranked, 1):
        d = prev[name]
        marker = "" if name.startswith("A") else f"{d:+.1f}"
        print(f"{rank:<6}{name:<36}{total(scores):>8.1f}   {marker:>10}")

    win, ws = ranked[0]
    print(f"\nWinner by score: {win.strip()} at {total(ws):.1f}/100.")

    # A dip is not automatically a mistake. I5 was a wrong turn and was undone;
    # I9's dip is the honest price of a dependency and a harder-to-test loop,
    # paid deliberately because the transport is the point.
    REVERTED = {"I5"}
    for n, _ in ITERATIONS:
        if prev[n] >= 0 or n.startswith("A"):
            continue
        tag = n.split()[0]
        verdict = "reverted at the next iteration" if tag in REVERTED else "kept anyway, see the note"
        print(f"Dip: {n.strip()} scored {prev[n]:+.1f} against its predecessor — {verdict}.")


if __name__ == "__main__":
    main()
