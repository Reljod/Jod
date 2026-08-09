# Harness engineering — the field

**Date:** 2026-08-09 · **Analyst:** Jod

> The model is a stateless function. Everything that makes it feel like a
> colleague who knows you is the harness.

This is the frame for the two deep dives — [`HERMES.md`](HERMES.md) and
[`OPENCLAW-MEMORY.md`](OPENCLAW-MEMORY.md). It records what an agent harness
actually consists of, and the design tensions every serious one has had to
resolve.

---

## 1. What a harness is

An **agent harness** is the program that wraps a stateless LLM and supplies
everything the model cannot supply for itself: what it knows, what it can do,
where it runs, what it remembers, and when it stops.

The definitional test: **swap the model and the agent survives.** Hermes calls
this model-agnosticism explicitly (`hermes model` switches providers with "no
code changes, no lock-in"). If replacing the model breaks the product, the
product was a prompt, not a harness.

Jod is a harness *orchestrator* — `jod-core` delegates to Claude Code and
OpenCode, one tmux session each, and normalises their output into one event
stream. Understanding the layer it drives is a prerequisite for driving it well.
→ [`docs/jod-system.md`](../../docs/jod-system.md)

---

## 2. The seven subsystems

Every harness examined solves these seven, whether or not it names them.

| # | Subsystem | The question it answers | Hermes | OpenClaw |
|---|---|---|---|---|
| 1 | **Context assembly** | What goes in the prompt, in what order, and what is cacheable? | Frozen snapshot at session start | Bootstrap files + per-turn retrieval injection |
| 2 | **Memory** | What survives the session? | 2 capped Markdown files | 4 Markdown files + hybrid RAG index |
| 3 | **Session store** | What happened, and can we find it again? | SQLite + 3× FTS5, agent-queried | SQLite, optional search source |
| 4 | **Skills / procedural knowledge** | How do we do *this* thing here? | `SKILL.md` dirs, progressive disclosure | Plugins + skills |
| 5 | **Tools & gateway** | What can it touch, and who mediates? | One gateway, 20+ platforms | Plugin-based tool surface |
| 6 | **Execution substrate** | Where does the work physically run? | 7 backends (local → Modal → Vercel Sandbox) | Local-first, any OS |
| 7 | **Learning loop** | How is next week better than this week? | 4 loops (turn → task → week → offline) | Nightly "dreaming" consolidation |

Subsystems 2, 3, 4 and 7 are the memory story, and they are the ones that
differentiate harnesses. 1, 5 and 6 are converging into commodity.

---

## 3. The memory taxonomy

The useful split is the cognitive-science one, and both subjects arrive at it
independently:

| Type | Contents | Hermes | OpenClaw |
|---|---|---|---|
| **Semantic** — facts | "user prefers pnpm, never npm" | `MEMORY.md`, `USER.md` (capped) | `MEMORY.md`, `USER.md` |
| **Episodic** — what happened | "last Tuesday we tried X and it broke" | `state.db` + FTS5 | `memory/YYYY-MM-DD.md` + session index |
| **Procedural** — how to do things | "how to deploy this service, step by step" | Skills (`SKILL.md`) | Plugins, skills |

**Procedural memory is the one most harnesses miss**, and it is where growth
actually lives. Facts make an agent informed; procedures make it *better*.
Hermes' four skill-creation triggers include "the user corrected the approach" —
a correction becomes a procedure, not a remembered scolding. That single design
choice is most of what "grows with you" means in practice.

A fourth type is emerging: the **relational** model — not facts *about* the user
but a running representation *of* them. Honcho (used by both projects) maintains
a user representation, a peer card, and an AI self-representation, refreshed by
multi-pass dialectic reasoning. → [`HERMES.md` §4](HERMES.md#4-modelling-you-specifically-honcho)

---

## 4. The central tension: bounded vs unbounded

Every harness eventually faces the same wall — memory grows, context does not.
There are exactly two answers, and our two subjects are clean instances of each.

### Bounded (Hermes)

Cap long-term memory hard — 2,200 + 1,375 characters, error on overflow — and
push everything else into tiers that cost more to reach.

- **Pro:** memory is auditable by a human in thirty seconds; zero retrieval
  cost; no embedding bill; no wrong-chunk failures; the cap itself is the
  forcing function that keeps entries dense.
- **Con:** the agent forgets things that didn't fit. Consolidation quality is
  the whole ballgame.

### Unbounded (OpenClaw)

Let the corpus grow forever, index it, and buy back precision with ranking.

- **Pro:** nothing is lost; scales to years of notes; recall improves with
  corpus size.
- **Con:** an embedding bill, a nightly LLM bill, a ranking function to tune,
  and a new failure mode — confidently retrieving the *wrong* chunk. No human
  ever reads the whole memory again.

### What they agree on

The agreements are more instructive than the disagreement, because they're what
converged independently:

1. **Markdown is the source of truth; the database is a derived index.** Both
   projects can delete the SQLite file and rebuild it. Memory you cannot open in
   an editor is memory you cannot fix.
2. **SQLite, always.** No server, one file, ships with FTS5 and reaches vectors
   through `sqlite-vec`. Neither project runs a vector database.
3. **The prompt-resident layer stays small.** Even OpenClaw, with a full RAG
   stack, auto-loads only `USER.md` and `MEMORY.md`.
4. **Automated memory rewrites need a blast radius.** OpenClaw rejects a
   consolidation that drops >25% of prior entries; Hermes stages writes behind
   `write_approval` and errors rather than truncating.
5. **Provenance and trust are first-class.** OpenClaw stores origin class
   outside the chunk text so it can't be forged; Hermes scans memory writes for
   injection and exfiltration patterns. Anything an agent ingests is
   attacker-reachable.

---

## 5. Recurring design principles

Extracted from both codebases; these generalise.

**Cache-shaped context.** Hermes freezes the system prompt at session start
specifically so prefix caching survives; writes land on disk immediately but
appear next session. Context assembly is a caching problem wearing a prompt
costume.

**Tier by cost of access, not by topic.** Free (in prompt) · cheap (a ~3k-token
listing) · on demand (a search or file open). Progressive disclosure is what
lets a skill library grow without bound.

**Errors teach; truncation doesn't.** A cap that silently drops the oldest entry
produces no learning. A cap that fails the write forces consolidation *by the
model that best knows what's redundant*.

**Deterministic passes are free; run them always. LLM passes cost money; make
them opt-in.** Hermes' curator ages skills to `stale` at 30 days and `archived`
at 90 with no model call at all, and gates the LLM consolidation behind
`consolidate: false`. Aging by disuse requires no intelligence, so it doesn't
buy any.

**Let retrieval generate the training signal for retention.** OpenClaw's
dreaming ranks candidates 0.24 on frequency and 0.15 on query diversity — a fact
earns permanence by being *retrieved in varied contexts*, not by looking
important when written. Write-time importance is close to the worst available
predictor of future usefulness.

**Close the loop between writing and reading.** OpenClaw's consolidation writes
`<!-- importance: N -->`; its retrieval multiplies score by `0.75 + 0.05N`. Two
small functions, no ML, and the memory system tunes itself.

**Ship the sophistication off by default.** MMR re-ranking and temporal decay
are implemented and tested in OpenClaw, and both default to `false`.

**Idle-triggered beats scheduled.** The curator fires on *interval elapsed AND
agent idle ≥2h*, and forks its own agent with a separate prompt cache.
Maintenance should never contend with the user.

---

## 6. Evaluating or building a harness — the checklist

Questions to ask of any harness, ours included:

**Memory**
- [ ] Is long-term memory human-readable and hand-editable?
- [ ] Is it capped? What happens at the cap — error, truncate, or unbounded growth?
- [ ] Semantic, episodic and procedural memory — which of the three exist?
- [ ] Can the index be deleted and rebuilt from the source of truth?

**Growth**
- [ ] Does a user correction change future *behaviour*, or only get recorded?
- [ ] What deletes or archives learned artefacts? (Growth without GC is a landfill.)
- [ ] Is there a signal for what proved useful, distinct from what seemed important?

**Safety**
- [ ] Is provenance stored where ingested content cannot forge it?
- [ ] Is there a blast-radius bound on automated memory rewrites?
- [ ] Can the human gate writes (staging / approval) without disabling learning?
- [ ] Is the previous state recoverable after an automated rewrite?

**Engineering**
- [ ] Is the system prompt stable enough to cache?
- [ ] Is multi-process access to the store handled (timeouts, retry, WAL)?
- [ ] Does the model swap out cleanly?
- [ ] Do expensive passes default off?

---

## 7. Where Jod sits

Jod is unusual: it is a harness orchestrator whose own memory lives in the
Claude Code auto-memory (`MEMORY.md` + one file per fact) and whose procedural
knowledge lives in `.agents/skills/`. Mapped to the seven subsystems, Jod has
1, 4, 5 and 6; **it has semantic memory, procedural memory, and no episodic
memory at all** — nothing searches past sessions.

The concrete, ranked gaps, drawn from the deep dives:

1. **No cap and no overflow behaviour** on auto-memory. Cheapest possible fix,
   and Hermes shows the failure mode it prevents. → [`HERMES.md` §2.1](HERMES.md#21-prompt-resident-memory-semantic)
2. **No episodic layer.** Past sessions are unsearchable. SQLite + FTS5 is a
   weekend, and it's the tier Jod most obviously lacks.
3. **No usefulness signal.** Memories record what seemed important at write
   time. OpenClaw's retrieve-then-promote inverts this. → [`OPENCLAW-MEMORY.md` §4](OPENCLAW-MEMORY.md#dreaming--the-consolidation-algorithm)
4. **No provenance class on ingested content.** Jod reads Linear, Notion and the
   web into memory. A Notion page should not be able to assert that it is
   `owner`-trusted.
5. **No garbage collection** for skills or memories. Currently fine — the
   library is hand-written. It stops being fine the moment anything authors
   skills automatically.

Nothing here argues Jod should become Hermes or OpenClaw. It argues that four of
those five gaps close with small, reversible, deterministic additions — which is
the kind of change [`AGENTS.md`](../../AGENTS.md) already asks for.

---

## Sources

Beyond the per-document source lists in [`HERMES.md`](HERMES.md#sources) and
[`OPENCLAW-MEMORY.md`](OPENCLAW-MEMORY.md#sources), the framing here draws on:

- [Hermes Agent documentation](https://hermes-agent.nousresearch.com/docs/)
- [OpenClaw documentation](https://docs.openclaw.ai/)
- Carbonell & Goldstein, *The Use of MMR, Diversity-Based Reranking* (1998) —
  cited directly in OpenClaw's `mmr.ts`
