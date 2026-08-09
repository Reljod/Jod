# harness-agents-research

**Date:** 2026-08-09 · **Analyst:** Jod

Research area for **harness engineering** — the discipline of building the
program that wraps an LLM and turns it into an agent that persists, learns, and
survives its own context window.

Jod is itself a harness orchestrator (`jod-core` drives Claude Code and OpenCode
in tmux sessions), so how the best open harnesses solve memory is directly load-
bearing for us. → [`docs/jod-system.md`](../../docs/jod-system.md)

## Documents

| Doc | Scope |
|---|---|
| [`HARNESS-ENGINEERING.md`](HARNESS-ENGINEERING.md) | The field. What a harness is, its seven subsystems, and the two rival answers to "how does an agent remember?" |
| [`HERMES.md`](HERMES.md) | **Focus.** Nous Research's Hermes Agent — how it remembers, and how it *grows with the user*. |
| [`OPENCLAW-MEMORY.md`](OPENCLAW-MEMORY.md) | **Focus.** OpenClaw's remembering only — database, schema, embeddings, ranking algorithm, consolidation. |
| [`FUTURE-OF-MEMORY.md`](FUTURE-OF-MEMORY.md) | Where LLM memory is going — 2026 papers and repos, the benchmark credibility problem, and a per-situation verdict on whether any of it is worth adopting. |
| [`BIG-LAB-MEMORY.md`](BIG-LAB-MEMORY.md) | How Anthropic, OpenAI, Google and the IDEs actually ship memory — Claude's three surfaces, ChatGPT "dreaming", Codex CLI's on-disk memories, Gemini Memory Bank, Cursor. |
| [`RECOMMENDATION.md`](RECOMMENDATION.md) | **The decision.** What Jod should build for memory, component by component, with the measured evidence and a five-phase build order. |
| [`experiments/`](experiments/) | A runnable comparison of 31 memory architectures across two rounds — the open-source designs, then the mechanisms the big labs ship. Pre-registered predictions, measured results, scorecards, combined conclusion. |

Read `HARNESS-ENGINEERING.md` first if you want the frame; go straight to the
two deep dives if you want the mechanics; read
[`experiments/FINDINGS.md`](experiments/FINDINGS.md) if you want measurements
rather than claims.

## The through-line

The two subjects sit at opposite ends of one axis — Hermes caps memory at ~1,300
tokens and errors on overflow; OpenClaw lets the corpus grow and buys precision
back with hybrid retrieval and nightly consolidation. `FUTURE-OF-MEMORY.md` asks
which bet the field is converging on; the experiment tests it directly.

What emerged: **both are optimising recall, and recall is not where the failures
are.** The highest-value component measured was a deterministic control plane —
versioned facts, real deletion, write-time trust admission — which lifted
current-value accuracy from 0.17 to 0.73 while being the cheapest strategy
tested. Raw long context scored 1.00 on recall, 0.00 on freshness, 0.00 on
deletion, and had a 100% poisoning attack success rate, at 314× the tokens.

Round 2 then tested the mechanisms the labs actually ship, and sharpened it into
one sentence: **memory quality is a write-path property, and the industry
measures the read path.** Across both rounds every large effect came from what
gets stored, partitioned, admitted and deleted — control plane, redaction,
scope, trust admission, eviction policy — while ranking, fusion, diversity and
promotion moved the needle by a few points. The one read-path exception worth
keeping is a second retrieval hop for multi-hop questions.

## Subjects at a glance

| | **Hermes Agent** | **OpenClaw** |
|---|---|---|
| Owner | Nous Research | openclaw/openclaw (community, ~385k ★) |
| Language | Python | TypeScript |
| Tagline | "The agent that grows with you" | "Your own personal AI assistant. Any OS. Any platform." |
| Memory thesis | **Bounded prompt-resident memory + procedural skills** | **Unbounded corpus + hybrid RAG + nightly consolidation** |
| Long-term store | `MEMORY.md` / `USER.md`, hard char caps | `MEMORY.md` / `USER.md` / `memory/YYYY-MM-DD.md`, uncapped |
| Index | SQLite `state.db`, FTS5 only (sessions) | SQLite per-agent, FTS5 **+ sqlite-vec** (memory + sessions) |
| Retrieval | Full-text over past sessions, agent-invoked | Hybrid vector+BM25, auto-injected per turn |
| Growth loop | Auto-authored skills + curator GC + offline DSPy/GEPA evolution | "Dreaming" — nightly scored promotion into `MEMORY.md` |

## Source confidence

Findings are marked inline:

- **[src]** — read directly from the project's source code (highest confidence).
- **[docs]** — the project's own documentation.
- **[3p]** — third-party write-up, not independently confirmed. Treat as a lead.

Where docs and third-party accounts disagree, both are recorded and the conflict
is flagged rather than smoothed over. Two such conflicts exist — see
[`HERMES.md` § Open questions](HERMES.md#open-questions-and-source-conflicts) and
[`OPENCLAW-MEMORY.md` § Open questions](OPENCLAW-MEMORY.md#open-questions-and-source-conflicts).

Everything here reflects `main` as of **2026-08-09**. Both projects move fast;
re-verify numbers before depending on them.
