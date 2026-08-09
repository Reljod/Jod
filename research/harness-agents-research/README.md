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

Read `HARNESS-ENGINEERING.md` first if you want the frame; go straight to the
two deep dives if you want the mechanics.

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
