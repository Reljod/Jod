#!/usr/bin/env python3
"""Measure what Jod's `events` table actually keeps of a real Claude Code run.

The adapters pass every tool result through `summarize(value, 400)`, which is
lossy at *write* time — the discarded bytes never reach SQLite and cannot be
recovered from it. This counts, over real transcripts on this machine, how much
of the conversation that throws away, and how many of the other things a
faithful replay needs are missing entirely.

Run: python3 lossiness.py
"""

import glob
import json
import os
import statistics

MAX = 400  # core/src/event.rs::summarize, as called by every adapter


def main():
    files = sorted(
        glob.glob(os.path.expanduser("~/.claude/projects/*/*.jsonl")),
        key=lambda p: -os.path.getsize(p),
    )[:12]

    kept = dropped = 0
    results = truncated = 0
    sizes = []
    thinking_blocks = signed = 0
    tool_calls = 0
    images = 0
    sidechain_msgs = 0
    user_human = 0
    system_prompt_records = 0

    for f in files:
        for line in open(f, errors="replace"):
            try:
                o = json.loads(line)
            except Exception:
                continue
            if o.get("isSidechain"):
                sidechain_msgs += 1
            t = o.get("type")
            msg = o.get("message") or {}
            content = msg.get("content")
            if t == "user":
                if isinstance(content, str):
                    user_human += 1
                elif isinstance(content, list):
                    for b in content:
                        if b.get("type") == "tool_result":
                            results += 1
                            c = b.get("content")
                            s = c if isinstance(c, str) else json.dumps(c)
                            n = len(s.strip())
                            sizes.append(n)
                            if n > MAX:
                                truncated += 1
                                kept += MAX
                                dropped += n - MAX
                            else:
                                kept += n
                        elif b.get("type") == "text":
                            user_human += 1
            elif t == "assistant" and isinstance(content, list):
                for b in content:
                    bt = b.get("type")
                    if bt == "thinking":
                        thinking_blocks += 1
                        if b.get("signature"):
                            signed += 1
                    elif bt == "tool_use":
                        tool_calls += 1
                    elif bt == "image":
                        images += 1
            elif t == "system" and o.get("subtype") == "init":
                system_prompt_records += 1

    total = kept + dropped
    # What would a bigger cap keep, and what would it cost? The question the
    # 400-char limit answers wrongly: it was chosen so the *UI* stream stays
    # readable, and then reused as the storage budget.
    caps = {}
    for cap in (400, 4096, 16384, 65536, 262144):
        k = sum(min(n, cap) for n in sizes)
        caps[cap] = {
            "bytes_kept": k,
            "pct_kept": round(100 * k / max(1, total), 1),
            "results_truncated": sum(1 for n in sizes if n > cap),
        }

    out = {
        "cap_sweep": caps,
        "transcripts_scanned": len(files),
        "tool_results": results,
        "tool_results_over_400_chars": truncated,
        "pct_results_truncated": round(100 * truncated / max(1, results), 1),
        "tool_result_bytes_total": total,
        "tool_result_bytes_kept_by_jod": kept,
        "tool_result_bytes_dropped_by_jod": dropped,
        "pct_tool_output_bytes_lost": round(100 * dropped / max(1, total), 1),
        "tool_result_size_p50": int(statistics.median(sizes)) if sizes else 0,
        "tool_result_size_p95": int(sorted(sizes)[int(len(sizes) * 0.95) - 1]) if sizes else 0,
        "tool_result_size_max": max(sizes) if sizes else 0,
        "tool_calls": tool_calls,
        "thinking_blocks": thinking_blocks,
        "thinking_blocks_with_signature": signed,
        "image_blocks": images,
        "sidechain_records": sidechain_msgs,
        "human_authored_user_messages": user_human,
        "system_init_records": system_prompt_records,
    }
    print(json.dumps(out, indent=2))
    with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "lossiness.json"), "w") as fh:
        json.dump(out, fh, indent=2)


if __name__ == "__main__":
    main()
