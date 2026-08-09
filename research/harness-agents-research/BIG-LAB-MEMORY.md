# How the big labs actually do memory

**Date:** 2026-08-09 · **Analyst:** Jod · **Companions:**
[`FUTURE-OF-MEMORY.md`](FUTURE-OF-MEMORY.md) ·
[`experiments/FINDINGS.md`](experiments/FINDINGS.md)

> **Question, as asked:** how do Claude, Gemini, Codex and the other big-company
> systems do memory?

Confidence markers: **[src]** primary/vendor reference · **[docs]** vendor
documentation · **[3p]** third-party, unverified.

---

## The answer

**All four labs converged on the same three-part shape, independently:**

1. **A small, always-loaded profile** — a curated set of durable facts injected
   into every session. Never a vector search.
2. **Background consolidation off the critical path** — a separate process that
   reads history and rewrites the profile while the user isn't waiting.
3. **Explicit retrieval as a distinct, second-class channel** — search over past
   sessions, invoked when needed rather than always-on.

Nobody's flagship product is a RAG-over-everything system. That is worth sitting
with, because it is the opposite of what the open-source memory category sells.

**And the single most striking number in this whole research area is OpenAI's
own:** across three generations of ChatGPT memory, factual recall went 41.5% →
67.9% → **82.8%**, while **time-sensitive** memory accuracy went 9.4% →
**75.1%**. **[3p]** The headline jump wasn't better retrieval — it was learning
to know *when* a fact stopped being true. Their example is exactly a
supersession: "You're going to Singapore in July" becoming "You went to
Singapore in July 2026." **[3p]**

That independently corroborates the finding from my own experiment
([`FINDINGS.md`](experiments/FINDINGS.md)): the control plane — knowing what is
currently true — is where the value is, not the ranker.

---

## 1. Anthropic — three separate surfaces

Worth separating, because "how Claude does memory" means three different things.

### 1.1 Consumer Claude (claude.ai)

Four mechanisms with different scopes: **[3p]**

| Mechanism | Scope | Behavior |
|---|---|---|
| **Chat memory** | Global, per-user | Claude writes and updates entries about you in real time — role, current work, formatting preferences, technical preferences — and organizes them into categories |
| **Project memory** | Per-project, isolated | Each Project keeps its own separate memory; no bleed between projects |
| **Chat search** | On demand | Retrieves specific past conversations when asked. Paid plans only |
| **Manual management** | Settings → Memory | Every entry listed by category, individually editable and deletable |

Chat memory reached all users including Free in March 2026. **[3p]**

The **per-project isolation** is the design choice worth noting: it treats
"which context am I in" as a hard partition rather than something retrieval
should figure out. That's a control-plane decision, not a ranking one.

### 1.2 Claude Code (the harness I'm running in)

The simplest of every system here, and I can describe it from the inside:
**[src]**

- One Markdown file per fact, in a per-project memory directory
- YAML frontmatter: `name`, `description` (used for recall relevance), and
  `metadata.type` — one of `user`, `feedback`, `project`, `reference`
- Facts link to each other with `[[wiki-style]]` references
- A `MEMORY.md` index — one line per memory — loaded into context every session
- Recalled memories arrive inside `<system-reminder>` blocks, explicitly marked
  as background context rather than user instructions

Two details are quietly the most interesting things in this document.

**The typed `metadata.type` field is a provenance system in disguise.** It
separates who the fact is about and where it came from — `user` facts, `feedback`
the user gave, `project` state, external `reference` — which is the same
distinction OpenClaw makes with origin classes, arrived at from a different
direction.

**And the guidance treats recalled memories as stale by default**: a memory that
names a file, function, or flag must be re-verified before being acted on. That
is a directly-stated answer to the staleness problem the survey literature calls
an open question — solved not by tracking validity intervals, but by declaring
memory non-authoritative about the world.

Alongside it sits `CLAUDE.md` / `AGENTS.md` — the hand-written, version-
controlled instruction layer. Same split as Codex: **authored** instructions and
**learned** memory are different files with different lifecycles.

### 1.3 The Claude API — the primitives everyone else builds on

This is the layer worth studying, because it's where the mechanisms are named.
**[docs]**

| Primitive | What it does |
|---|---|
| **Memory tool** (`memory_20250818`) | Client-side. Claude issues `view` / `create` / `str_replace` / `insert` / `delete` / `rename` against a `/memories` directory. **You implement the storage.** Anthropic ships helper base classes but no backend |
| **Context editing** (beta) | *Clears* stale content from the transcript — `clear_tool_uses_20250919` for old tool results, `clear_thinking_20251015` for thinking blocks. Pruning, not summarizing |
| **Compaction** (beta) | *Summarizes* earlier context server-side into a compaction block when the window fills |
| **Memory stores** (Managed Agents) | Server-side persistent store, mounted into the agent's container as a filesystem |

**Context editing versus compaction is a distinction the open-source category
mostly doesn't make.** Editing *deletes* what's no longer relevant; compaction
*compresses* what is. Different failure modes, different controls, both shipped.

Three details from the memory-store design stand out:

- **Every mutation writes an immutable version** (`created` / `modified` /
  `deleted`), each recording the actor that made it. That is a full audit trail
  — the thing no open-source memory system in [`FUTURE-OF-MEMORY.md`](FUTURE-OF-MEMORY.md) has.
- **A redaction endpoint** clears a version's content while preserving actor and
  timestamps — built for leaked secrets, PII, and deletion requests. Real
  forgetting, with the audit record intact.
- **Optimistic concurrency via content hash.** An update can carry a
  `content_sha256` precondition and fails with a conflict on mismatch — so
  read-modify-write can't silently clobber a concurrent writer.

And the security guidance is blunt: **never store credentials in memory**,
because memories are replayed verbatim into every future session that mounts the
store; validate every model-supplied path against traversal; use per-user
directories in multi-user systems. **[docs]**

**What Anthropic ships is a set of well-specified primitives with the policy
left to you.** No opinion about consolidation cadence, no ranking function, no
promotion heuristic — but versioning, redaction, and preconditions, which are
exactly the parts that are dangerous to get wrong.

---

## 2. OpenAI — two systems, very different philosophies

### 2.1 ChatGPT "Dreaming V3" (rolled out from 2026-06-04)

A memory architecture layer sitting between the user and the model that
**synthesizes** context from past chats in a background process, replacing the
manually-curated list of saved memories. It reads across years of conversation
and updates its model of you unprompted. **[3p]**

Measured across three generations: **[3p]**

| Generation | Factual recall | Time-sensitive accuracy |
|---|---:|---:|
| 2024 saved memories | 41.5% | 9.4% |
| 2025 system | 67.9% | — |
| **2026 Dreaming V3** | **82.8%** | **75.1%** |

**9.4% → 75.1% on time-sensitive memory is the most important number in this
document.** The 2024 system stored facts and never revisited them, so anything
with a date attached decayed into wrongness. The fix wasn't a better index. It
was temporal awareness — revising a memory when the world moves past it.

Users get a readable summary of what the system knows, can edit it, and can
control which topics it raises and when. **[3p]** Reporting at rollout noted the
audit trail became *less* granular than the old saved-memories list: you see a
synthesized summary rather than every discrete entry. Legibility traded for
coverage. **[3p]**

The name is not a coincidence — OpenClaw ships nightly "dreaming," Letta ships
"sleep-time compute," and Anthropic distils long-term-worthy information on a
roughly daily cadence. Four teams, same idea, same metaphor.

### 2.2 Codex CLI — the most transparent memory system any lab ships

Two layers, deliberately separated: **[3p]**

**Layer 1 — `AGENTS.md` (authored, static).** Hierarchical walkup from git root
to cwd, concatenated in path order, with `AGENTS.override.md` taking precedence
at any level. Reads `CLAUDE.md` and `.cursorrules` via fallback filenames — a
cross-tool convention. **32 KiB cap (~8,000 tokens), truncated silently.**

The honest framing in OpenAI's own docs: `AGENTS.md` "captures what you
remembered to write down, not what you actually encountered." Layer 2 is the fix.

**Layer 2 — `~/.codex/memories/` (learned, dynamic).** Plain Markdown on disk:

| File | Role |
|---|---|
| `memory_summary.md` | Consolidated view, read whole at startup and token-truncated to budget |
| `MEMORY.md` | Long-form merged repository — the agent **greps** it for detail |
| `raw_memories.md` | Pre-consolidation extraction output |
| `rollout_summaries/<slug>.md` | Per-session summaries feeding consolidation |
| `skills/<name>/SKILL.md` | Skill-scoped memories |

**Recall is not vector retrieval.** Read the summary whole; grep the long-form
file for more. No embeddings anywhere.

Two-phase writes: **phase 1** (per session) samples the conversation against a
strict-schema extraction prompt and **redacts secrets before anything hits
disk**; **phase 2** takes a global lock and runs a consolidation sub-agent that
writes a diff. Thresholds: sessions become consolidation-eligible after **6
hours idle**; consolidation considers at most **256 recent rollouts**; rollouts
unused for **30 days** age out, and **memories that go unrecalled for 30 days
are pruned**. Consolidation pauses when the user's API quota runs low.

Not available in the EEA, UK, or Switzerland at launch — those users get the
`AGENTS.md` layer only.

**Three things here are worth stealing outright.** Secret redaction at write
time is the write-time admission control from
[`OPENCLAW-MEMORY.md`](OPENCLAW-MEMORY.md), aimed at exfiltration instead of
poisoning. Idle-triggered consolidation is the same rule as Hermes' curator
([`HERMES.md`](HERMES.md)) — maintenance must never contend with the user. And
**pruning memories that go unrecalled** is retrieval-driven retention shipping
in production.

That last one deserves a caveat against my own experiment. I found
retrieval-driven *promotion* (using recall to rank) added zero accuracy for +62%
tokens. Codex uses recall for **eviction**, not ranking — and eviction is the
defensible use: "nothing has needed this in 30 days" is far better evidence for
deleting a fact than "this was retrieved often" is for ranking it higher.

---

## 3. Google — Gemini

### 3.1 Consumer: Personal Context

Ties memory to the Google Account rather than the session. Periodically
generates a compressed **user profile** / conversation summary through
LLM-driven summarization and selective recall, capturing themes, preferences,
stated facts, and recurring patterns. **[3p]**

Same shape as everyone else: a compact synthesized profile, refreshed in the
background, not a retrieval index.

### 3.2 Engineering: Agent Engine / Agent Platform **Memory Bank**

The most explicitly specified of the enterprise offerings. **[docs]**

**Generation** — three modes: LLM **extraction** ("only the most meaningful
information"), **consolidation** (new information merges with existing memories
so they evolve rather than accumulate), and **asynchronous** generation that
never blocks the agent. Continuous event ingestion triggers generation on
configurable batching rules.

**Scoping is a hard partition, not a ranking signal.** Every memory carries
`scope: {agent_name, user}`; sessions require a user ID; memories are strictly
isolated by scope.

**Retrieval, two ways:** scope-based (everything for this user+agent) or
similarity search *scoped to an identity*. Note the ordering — identity filter
first, similarity second. The vector search never crosses a scope boundary.

**Consolidation is the deduplication mechanism** — merging is what stops a store
becoming a pile of near-duplicate facts.

**Memory revisions** are maintained automatically so you can inspect how a
memory transformed as new information arrived — the same audit surface as
Anthropic's memory versions.

**TTL** is configurable at the instance level for automatic expiry of stale
information.

Google's own framing is the sharpest statement of the thesis in this whole
research area: agent memory in 2026 should mean **"a curated store of durable
facts that are safe to retain, easy to retrieve, and easy to delete, rather than
an unlimited archive."** **[3p]**

---

## 4. The IDEs — Cursor and the coding-agent tier

Cursor splits the same two layers as Codex: **[3p]**

- **Rules** — `.cursor/rules/` with multiple files, each scoped to file globs
  and tagged with metadata; only rules matching the current context load. This
  is progressive disclosure applied to instructions, the same idea as Hermes'
  skill listing.
- **Memories** (since v1.0, June 2025) — facts carried forward *within a
  project*.
- **Codebase index** — a semantic index over files, functions, types and
  dependencies, used to select context per suggestion.

The important structural point: **the code index and the memory are separate
systems.** Nobody puts "what this repo contains" and "what I learned about the
user" in the same store, because they have different lifecycles — one is
derived and rebuildable, the other is earned and irreplaceable.

---

## 5. Side by side

| | Always-loaded layer | Consolidation | Retrieval | Embeddings? | Audit / versioning |
|---|---|---|---|---|---|
| **Claude (consumer)** | Chat memory + project memory | Real-time entry updates | Chat search, on demand | Not disclosed | Per-entry edit/delete |
| **Claude Code** | `MEMORY.md` index + `CLAUDE.md` | Manual, agent-written | Read the fact file | **No** | Git |
| **Claude API** | Your `/memories` dir | Compaction + context editing | Your implementation | Your choice | **Memory versions + redaction** |
| **ChatGPT** | Synthesized profile | **Background "dreaming"** | Past-chat reference | Not disclosed | Summary view (coarser than before) |
| **Codex CLI** | `memory_summary.md` + `AGENTS.md` | Idle-triggered, 2-phase, locked | **Grep** `MEMORY.md` | **No** | Files on disk + 30-day pruning |
| **Gemini** | Personal context profile | Periodic summarization | Profile injection | Not disclosed | — |
| **Memory Bank** | Scope-based retrieval | Extraction + merge, async | Similarity **within scope** | Yes | **Memory revisions + TTL** |
| **Cursor** | Glob-scoped rules | — | Semantic code index | Yes (code) | Git (rules) |

---

## 6. What the convergence tells you

**1. The always-loaded layer is small and synthesized. Every time.** Nobody
ships "search your whole history on every turn" as the primary mechanism. The
profile is compact, curated, and injected; retrieval is the exception path. This
is Hermes' bet ([`HERMES.md`](HERMES.md)), not OpenClaw's — from four labs
independently.

**2. Two of the four production coding agents use no embeddings for memory at
all.** Codex greps. Claude Code reads files. Both are shipped by frontier labs
to enormous user bases. Semantic retrieval is not the load-bearing part.

**3. Consolidation is always off the critical path.** Background dreaming, idle-
triggered consolidation, async generation, ~daily distillation. Nobody makes the
user wait for it. This is now unanimous.

**4. Authored and learned memory stay in separate files.** `AGENTS.md` vs
`memories/`, rules vs Memories, `CLAUDE.md` vs the memory directory. Different
lifecycles, different owners, different trust. Merging them is a mistake nobody
made.

**5. Scope is enforced, not ranked.** Project memory is isolated. Memory Bank
filters by identity *before* similarity. Nobody trusts a ranker to keep one
user's or one project's facts out of another's.

**6. Temporal correctness is where the measured wins are.** OpenAI's 9.4% →
75.1% is the clearest evidence available anywhere that this is the high-value
axis — and it lines up with Graphiti's bi-temporal edges beating Mem0 by 15
points, with the +24 to +34.8 pp from deterministic freshness resolution
(arXiv:2606.01435), and with my own 0.17 → 0.73.

**7. The enterprise tiers have audit trails; the consumer tiers don't.**
Anthropic's memory versions plus redaction and Google's memory revisions plus
TTL exist because enterprises must answer "why does the agent believe this?" and
"delete everything about this person." Consumer surfaces mostly offer
edit-and-delete. ChatGPT's Dreaming rollout actually *reduced* audit granularity
in exchange for coverage.

**What nobody ships:** a trust or provenance class on ingested content. Claude
Code's `metadata.type` is the closest thing, and it's a categorisation of
purpose rather than a security boundary. Given memory poisoning is OWASP ASI06
with 80–99.8% attack success rates
([`FUTURE-OF-MEMORY.md` §4](FUTURE-OF-MEMORY.md#4-security-is-not-optional-any-more)),
and that these agents ingest web pages, repos and pasted text, this is the
clearest gap in the production landscape. OpenClaw — a community project — has
the best answer to it of anyone.

---

## 7. What I'd take from the labs specifically

Beyond the recommendations already in [`FINDINGS.md`](experiments/FINDINGS.md):

1. **Split authored from learned memory into different files.** Unanimous across
   every lab. Jod has `AGENTS.md` and an auto-memory directory already — the
   split is right; keep it.
2. **Redact secrets at extraction time, before anything touches disk** (Codex).
   Cheap, and the failure it prevents is unrecoverable — a leaked key replayed
   into every future session.
3. **Prune what goes unrecalled** (Codex, 30 days). Retrieval-driven *eviction*,
   which is better-founded than the retrieval-driven *ranking* my experiment
   found worthless.
4. **Filter by scope before you rank** (Memory Bank). Identity and project
   boundaries are partitions, not features fed to a scorer.
5. **Keep version history and add a redaction path** (Anthropic memory stores,
   Google revisions). Answers "why does it believe this?" and "delete this
   person's data" — and neither is retrofittable later.
6. **Use content-hash preconditions on memory writes** (Anthropic). One field,
   and concurrent read-modify-write stops silently clobbering.
7. **Treat recalled memory as stale until verified** (Claude Code). The cheapest
   available answer to staleness: don't try to keep memory true about the world,
   just refuse to let it be authoritative.

---

## Sources

**Anthropic** — [Claude chat search and memory](https://support.claude.com/en/articles/11817273-use-claude-s-chat-search-and-memory-to-build-on-previous-context) ·
[Memory tool](https://platform.claude.com/docs/en/agents-and-tools/tool-use/memory-tool) ·
[Context editing](https://platform.claude.com/docs/en/build-with-claude/context-editing) ·
[Compaction](https://platform.claude.com/docs/en/build-with-claude/compaction) ·
[Managed Agents memory](https://platform.claude.com/docs/en/managed-agents/memory.md) ·
`claude-api` skill reference (bundled) · Claude Code auto-memory: this session's own configuration

**OpenAI** — [How memory works in Codex CLI](https://mem0.ai/blog/how-memory-works-in-codex-cli) ·
[Memories in Codex (openai/codex discussion #12567)](https://github.com/openai/codex/discussions/12567) ·
[ChatGPT dreaming memory architecture](https://www.androidauthority.com/chatgpt-dreaming-memory-architecture-improved-3674780/) ·
[OpenAI rolls out new ChatGPT memory system](https://www.edtechinnovationhub.com/news/openai-rolls-out-new-chatgpt-memory-system-to-keep-personalization-current)

**Google** — [Agent Platform Memory Bank](https://docs.cloud.google.com/gemini-enterprise-agent-platform/scale/memory-bank) ·
[Vertex AI Agent Engine Memory Bank overview](https://cloud.google.com/vertex-ai/generative-ai/docs/agent-engine/memory-bank/overview) ·
[Enterprise agent memory in 2026 — what to keep, what to avoid](https://codimite.ai/blog/enterprise-agent-memory-in-2026-what-to-keep-what-to-avoid-google-adk-gemini/) ·
[Inside Gemini's memory](https://medium.com/@rushikeshchavan_99600/inside-geminis-memory-context-user-profiles-and-personalization-87bc1ae4ba18)

**IDEs** — [Context management strategies for Cursor](https://datalakehousehub.com/blog/2026-03-context-management-cursor/) ·
[Cursor rules and memory banks](https://www.lullabot.com/articles/supercharge-your-ai-coding-cursor-rules-and-memory-banks)

**Cross-lab** — [AI that remembers: ChatGPT, Gemini and Claude compared](https://www.notebookcheck.net/AI-that-remembers-ChatGPT-Gemini-and-Claude-compared.1336513.0.html) ·
[How AI agents actually remember](https://kenhuangus.substack.com/p/how-ai-agents-actually-remember-inside) ·
[Why AI agents are starting to dream](https://kenhuangus.substack.com/p/why-ai-agents-are-starting-to-dream)
