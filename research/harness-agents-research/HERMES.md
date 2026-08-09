# Hermes Agent — how it remembers, and how it grows with you

**Date:** 2026-08-09 · **Analyst:** Jod · **Subject:**
[NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) ·
**Companion:** [`OPENCLAW-MEMORY.md`](OPENCLAW-MEMORY.md)

> **Question, as asked:** how does Hermes remember, and how does it grow with
> the user?

---

## The answer in one paragraph

Hermes does **not** do retrieval-augmented memory. Its long-term memory is two
Markdown files totalling **~1,300 tokens**, injected verbatim into the system
prompt at session start and frozen there for the whole session. That is a
deliberate, aggressive bound — when the agent tries to write past it, the tool
*errors* rather than truncating, forcing consolidation. Everything that doesn't
fit goes into one of two other stores with different economics: **skills**
(procedural knowledge, unbounded on disk, loaded on demand through progressive
disclosure) and the **session database** (every message ever, SQLite + FTS5,
searched explicitly by the agent when it needs to). Growth is then four separate
loops layered on top: a per-turn self-improvement review, autonomous skill
authoring, a background *curator* that garbage-collects the skill library, and
an offline DSPy+GEPA evolutionary optimizer that rewrites skills from execution
traces and ships them as pull requests.

The slogan "the agent that grows with you" cashes out as: **memory stays small
and human-readable; capability grows without bound.**

---

## 1. What the harness is

A model-agnostic, multi-platform agent harness in Python. One gateway process
serves CLI plus Telegram, Discord, Slack, WhatsApp and Signal simultaneously,
against seven execution backends — `local`, Docker, SSH, Singularity, Modal,
Daytona and Vercel Sandbox, the serverless ones hibernating when idle. **[docs]**

Home is `~/.hermes/` (overridable via `HERMES_HOME`;
`%LOCALAPPDATA%\hermes` on Windows), with the managed checkout at
`~/.hermes/hermes-agent`. **[docs]**

```
~/.hermes/
├── memories/
│   ├── MEMORY.md          # agent's notes   — 2,200 chars
│   └── USER.md            # user profile    — 1,375 chars
├── skills/<category>/<name>/SKILL.md
├── skill-bundles/<slug>.yaml
├── pending/skills/        # staged writes when write_approval: true
├── state.db               # every session, every message (SQLite + FTS5)
├── memory_store.db        # only if the Holographic provider is enabled
└── config.yaml
```

That the profile root is a single relocatable directory matters more than it
looks: it's what makes multi-profile isolation and container backends work
without a database migration story.

---

## 2. How it remembers — three stores, three economics

Hermes splits memory by **cost of access**, not by content type. This is the
central design idea and it's worth stating plainly:

| Store | Holds | Cost to read | Bound |
|---|---|---|---|
| `MEMORY.md` + `USER.md` | Declarative facts | **Free** — already in the prompt | Hard char cap |
| Skills | Procedures | ~3k tokens to list, more to open | Unbounded on disk |
| `state.db` | Raw episodic history | A tool call + ~20 ms | Unbounded |

### 2.1 Prompt-resident memory (semantic)

Two files in `~/.hermes/memories/`: **[docs]**

- **`MEMORY.md`** — "Agent's personal notes — environment facts, conventions,
  things learned." **2,200 characters ≈ 800 tokens.** Environment facts (OS,
  tools, project structure), project conventions, completed-task diary entries.
- **`USER.md`** — "User profile — your preferences, communication style,
  expectations." **1,375 characters ≈ 500 tokens.** Name, role, timezone;
  communication preferences; pet peeves and things to avoid.

Both are "injected into the system prompt as a frozen snapshot at session
start." **[docs]**

**The frozen-snapshot rule is a caching decision, not laziness.** "The system
prompt injection is captured once at session start and never changes
mid-session." Writes during a session hit disk immediately but only surface in
the *next* session's prompt. Mutating the system prompt mid-session would
invalidate the LLM prefix cache on every write; Hermes trades one session of
staleness for a cache that holds all day. **[docs]**

The `memory` tool takes three actions: **[docs]**

| Action | Mechanics |
|---|---|
| `add` | Append a new entry |
| `replace` | Substring match on `old_text`, then swap |
| `remove` | Substring match, then delete |

Substring matching rather than line numbers or IDs is the right call for a file
the human is also expected to hand-edit.

**Overflow is a hard error.** "When memory would overflow, the `memory` tool
returns an error instead of silently dropping entries" — the agent must
consolidate or delete before retrying. Exact duplicates are rejected, and
entries are scanned for "injection and exfiltration patterns" before landing.
**[docs]**

That error is the whole growth mechanism for declarative memory. A cap that
truncates teaches nothing; a cap that *fails the write* forces the model to
re-read what it already knows and rewrite it denser. The docs even grade
entries: a strong `MEMORY.md` line packs "multiple related facts," a weak one is
"too vague" or "too verbose" with unnecessary narrative. **[docs]**

```yaml
memory:
  memory_char_limit: 2200
  user_char_limit: 1375
  write_approval: false     # true → stage writes for human approval
```

`write_approval` governs both foreground turns and the background review
uniformly, across every platform. **[docs]**

### 2.2 Session history (episodic)

Every CLI and messaging session lands in **SQLite at `~/.hermes/state.db`**, WAL
mode, schema **version 23**. **[docs]**

- **`sessions`** — id, source, user_id, model, system prompt, full token
  accounting (`input_tokens`, `output_tokens`, `cache_read_tokens`,
  `cache_write_tokens`, `reasoning_tokens`), cost fields
  (`estimated_cost_usd`, `actual_cost_usd`, `cost_status`), workspace context
  (`cwd`, `git_branch`, `git_repo_root`), and `parent_session_id`.
- **`messages`** — role, content, timestamp, `tool_calls`, `tool_name`,
  `tool_call_id`, reasoning fields, plus an `api_content` sidecar preserving
  exact wire bytes when they differ from `content`.

`parent_session_id` is how **compression-triggered session splits** stay
navigable: a compacted session becomes a child, and recursive CTEs walk the full
lineage. Context compaction doesn't destroy history here, it forks it.

**Three** FTS5 virtual tables index messages — standard, **trigram** (substring
and CJK), and a **CJK-unicode61** tokenizer shipped as hand-written C in
`native/fts5_cjk/`. Triggers keep them in sync on INSERT/UPDATE/DELETE, guarded
by `fts_rebuild_high_water` markers so background rebuilds don't double-index.
**[docs, src]**

The `session_search` tool exposes this in three modes — discovery, scrolling,
browsing within a found session. Results carry `>>>match<<<` snippet markers,
surrounding context and parent-session metadata. Query strings are sanitised
(unmatched quotes stripped, hyphenated terms wrapped, dangling boolean operators
removed) before hitting FTS5. Stated performance: **~20 ms FTS5 query, ~1 ms
scroll.** **[docs]**

The memory doc is emphatic that this is *raw recall*: "Search queries return
actual messages from the DB — no LLM summarization, no truncation." **[docs]**
(The README claims the opposite — see [§ Open questions](#open-questions-and-source-conflicts).)

Multi-process contention — gateway and CLI both writing — is handled with a
short **1-second** SQLite timeout instead of the 30-second default, application-
level retry with **20–150 ms jitter up to 15 attempts**, `BEGIN IMMEDIATE` to
surface lock conflicts early, and a PASSIVE WAL checkpoint **every 50 successful
writes**. **[docs]** Worth stealing verbatim for anything multi-process on
SQLite.

### 2.3 Skills (procedural)

Skills are where Hermes puts everything too big for 2,200 characters. They live
in `~/.hermes/skills/`, grouped by category, one directory each: **[docs]**

```
~/.hermes/skills/<category>/<skill-name>/
├── SKILL.md          # required
├── references/  templates/  scripts/  examples/  assets/
```

`SKILL.md` carries YAML frontmatter — `name`, `description` (**≤60 chars** by
house standard), `version` (semver), optional `platforms`,
`metadata.hermes.{tags,category,config}`, and `required_environment_variables`
with secure prompting. The body follows a fixed shape: **When to Use ·
Procedure · Pitfalls · Verification.** **[docs]**

**Progressive disclosure** is what keeps an unbounded library affordable:

> "Level 0: `skills_list()` → `[{name, description, category}, ...]` (~3k tokens)"

Full `SKILL.md` content loads only when actually needed; individual reference
files load on demand via `skill_view(name, path)`. **[docs]**

Every installed skill also becomes a slash command (`/skill-name`), chainable
(`/github-pr-workflow /test-driven-development fix issue #123`), and groupable
into bundles at `~/.hermes/skill-bundles/<slug>.yaml`. **[docs]**

---

## 3. How it grows — four loops

This is the part that distinguishes Hermes from every harness that merely
persists a file. Growth runs at four different timescales.

### Loop 1 — per turn: the self-improvement review

A background review runs **after a turn** and may "quietly save a memory or
update a skill" — what the docs call a **"consent-aware learning loop."** It
obeys the same `write_approval` gate as foreground writes. **[docs]**

No cron, no user prompt. The unit of learning is the turn.

### Loop 2 — per task: autonomous skill authoring

The agent writes its own skills through the `skill_manage` tool, with actions
`create`, `patch` (targeted text replacement, token-efficient), `edit`
(structural rewrite), `delete`, and `write_file` / `remove_file` for supporting
docs. **[docs]**

Documented trigger conditions: **[docs]**

- a task took **5+ tool calls** and succeeded
- a workaround was found after errors
- **the user corrected the approach**
- a non-trivial workflow was discovered

The third one is the "grows with *you*" clause. A correction isn't logged as a
fact in `MEMORY.md` — it's compiled into a procedure that changes how the next
attempt is executed. That's the difference between an agent that remembers you
scolded it and an agent that stops needing to be scolded.

`/learn` runs the same machinery on demand, converting existing docs or a
described procedure into a house-standard `SKILL.md`.

Safety: `skills.write_approval: true` stages every modification under
`~/.hermes/pending/skills/` for `/skills approve|reject <id>`. Hub installs are
scanned for exfiltration, injection and destructive patterns; trust tiers run
**builtin → official → trusted → community** with widening policy, and a
`dangerous` verdict is unoverridable even with `--force`. **[docs]**

### Loop 3 — per week: the curator (garbage collection)

Autonomous skill creation without deletion produces a landfill. The **curator**
exists so "skills created via the self-improvement loop don't pile up forever."
**[docs]**

It is **inactivity-triggered, not scheduled** — checked at session start and
periodically in the gateway, it fires only when *both* hold:

1. `interval_hours` since the last run (default **168** = 7 days), **and**
2. the agent has been idle `min_idle_hours` (default **2**).

Then "it spawns a background fork of `AIAgent`" with its own prompt cache, so
curation never pollutes the live conversation. Two phases:

- **Phase 1 — deterministic, no LLM, always runs.** Unused skills go `active` →
  `stale` after **30 days** → `archived` after **90 days**.
- **Phase 2 — LLM consolidation, opt-in** (`curator.consolidate: true`). An
  auxiliary model reviews agent-created skills and may "consolidate overlapping
  ones into class-level umbrellas" or propose patches.

```yaml
curator:
  enabled: true
  interval_hours: 168
  min_idle_hours: 2
  stale_after_days: 30
  archive_after_days: 90
  consolidate: false      # LLM pass is opt-in
  prune_builtins: true
auxiliary:
  curator:
    provider: openrouter          # or `auto` → your main chat model
    model: google/gemini-3-flash-preview
```

Note the shape: **the free, deterministic pass always runs; the expensive,
lossy, LLM pass is off by default.** Aging by disuse needs no intelligence, so
it doesn't buy any.

### Loop 4 — offline: DSPy + GEPA evolution

[NousResearch/hermes-agent-self-evolution](https://github.com/NousResearch/hermes-agent-self-evolution)
optimises skills by evolutionary search rather than hand-tuning: **[docs]**

1. **Read** an existing skill, prompt or tool description
2. **Generate** an evaluation dataset
3. **Evolve** candidate variants with DSPy + GEPA
4. **Evaluate** each variant against **execution traces**
5. **Gate** on tests, size limits, cache compatibility, semantic preservation
6. **Deploy** the winner as a pull request

The interesting property is trace-awareness: the optimiser "reads execution
traces to understand *why* things fail (not just that they failed), then
proposes targeted improvements." Mutation guided by post-mortem, not random
search. **~$2–10 per run, no GPU** — it's all API calls.

Phase 1 (`SKILL.md` files) is working today; tool descriptions, system prompts,
tool implementation code (via a "Darwinian Evolver") and continuous pipelines
are planned. **[docs]**

Beyond this, the README describes "batch trajectory generation, trajectory
compression for training the next generation of tool-calling models" — Hermes
usage becomes training data for future Nous models. **[docs]** Growth at the
weight level, not just the file level.

### The four loops, side by side

| Loop | Timescale | Writes | LLM? | Default |
|---|---|---|---|---|
| Self-improvement review | per turn | memory, skills | yes | on |
| Skill authoring | per task | skills | yes | on |
| Curator phase 1 | ~weekly | skill state | **no** | on |
| Curator phase 2 | ~weekly | skill content | yes | **off** |
| DSPy/GEPA evolution | manual/offline | skills, via PR | yes | separate repo |

---

## 4. Modelling *you* specifically: Honcho

Built-in `USER.md` is hand-curated and capped at 500 tokens. For a real user
model, Hermes integrates **Honcho** — "an AI-native memory backend that adds
dialectic reasoning and deep user modeling," maintaining "a running model of who
the user is — their preferences, communication style, goals, and patterns — by
reasoning about conversations after they happen." **[docs]**

**Dialectic reasoning** is multi-pass, cadenced by `dialecticCadence`:

- **Pass 1** — identify assessment gaps, synthesise evidence from recent sessions.
- **Pass 2** — check for contradictions against prior passes, produce final
  synthesis.

Early passes run at lighter reasoning levels, escalating with depth. The system
picks a **cold-start** query ("Who is this person? What are their preferences,
goals, and working style?") versus a warm-session query based on existing
context — the profile literally starts as a question and becomes an answer.

Context injection is two concurrent layers, concatenated and truncated to a
`contextTokens` budget:

1. **Base** (per `contextCadence`): session summary, user representation, user
   peer card, AI self-representation, AI identity card.
2. **Dialectic supplement** (per `dialecticCadence`): LLM-synthesised reasoning
   about the user's current state and needs.

Five tools: `honcho_profile` (read/update peer card facts), `honcho_search`
(semantic search over conclusions, returning raw excerpts), `honcho_context`,
`honcho_reasoning` (synthesised answer at configurable depth), `honcho_conclude`
(create/delete conclusions — the PII escape hatch).

**Query-adaptive reasoning level**: depth increments by one at **≥120
characters** of query, by two at **≥400**, clamped to a configured cap. Longer
questions get more thinking, automatically.

Note the AI *self*-representation and identity card sitting alongside the user
model. Honcho models both sides of the relationship, which is what "grows with
you" implies if taken literally.

### The wider provider table

Nine external providers; **exactly one** may be active, and "the built-in memory
is always active alongside it" — never replaced. Activate with
`hermes memory setup` or `memory.provider: <name>`. **[docs]**

| Provider | Storage | Algorithm / database |
|---|---|---|
| **Honcho** | Cloud or self-hosted | Dialectic reasoning + semantic search |
| **OpenViking** | Self-hosted | Filesystem hierarchy + tiered retrieval |
| **Mem0** | Cloud / self-hosted / in-process | LLM fact extraction + semantic search |
| **Hindsight** | Cloud or local PostgreSQL | Knowledge graph + entity resolution |
| **Holographic** | Local SQLite | FTS5 + **HRR algebra** |
| **RetainDB** | Cloud | Hybrid: vector + BM25 + reranking |
| **ByteRover** | Local or cloud sync | Hierarchical knowledge tree |
| **Supermemory** | Cloud or self-hosted | Semantic similarity + graph ingest |
| **Memori** | Cloud | Structured memory with tool awareness |

An active provider automatically injects its context into the system prompt,
**prefetches relevant memories before turns**, and **syncs conversation turns
after each response**. Cloud credentials live in `~/.hermes/.env`; config under
`$HERMES_HOME/` for profile isolation. **[docs]**

**Holographic** deserves a callout as the most unusual algorithm in the set — a
local, dependency-light fact store at `$HERMES_HOME/memory_store.db` combining
FTS5 with **Holographic Reduced Representations**: facts encoded as
high-dimensional vectors (default **1024 dims**) that compose through binding
and unbinding, so related concepts can be inferred algebraically rather than
merely matched. NumPy is optional. New facts start at **trust score 0.5**,
trained up or down by user helpful/unhelpful feedback. The `fact_store` tool has
nine actions: `add`, `search`, `probe`, `related`, `reason`, `contradict`,
`update`, `remove`, `list` — `contradict` being the one almost no other memory
system exposes. **[docs]**

---

## 5. What's worth stealing for Jod

1. **Bound declarative memory and error on overflow.** Jod's auto-memory
   (`MEMORY.md` + one file per fact) has no cap. A cap that *fails the write* is
   the cheapest possible forcing function for consolidation, and it costs one
   `if`.
2. **Freeze the prompt snapshot at session start.** Writes go to disk
   immediately, the prompt doesn't move, prefix cache survives. Jod injects
   memory per session already; making the no-mid-session-mutation rule explicit
   is free.
3. **Split memory by cost of access, not by topic.** Free (in-prompt) ·
   cheap (a listing) · on-demand (a search). Jod's `.agents/` skills are already
   the middle tier; the bottom tier — searchable session history — doesn't exist
   yet.
4. **The curator's two-phase design.** Deterministic aging always on, LLM
   consolidation opt-in. Directly applicable if Jod's skills ever get authored
   automatically.
5. **The SQLite contention recipe** — 1 s timeout, jittered retry ×15,
   `BEGIN IMMEDIATE`, checkpoint every 50 writes — is a solved-problem answer
   for any multi-process store Jod grows. `jod-core` runs one tmux session per
   harness, so this is a *when*, not an *if*.
6. **"Skill on user correction."** Of the four skill triggers, this is the one
   that converts a Reljod correction into a durable behaviour change instead of
   a remembered scolding. It fits Jod's charter — *extend by writing it down* —
   almost word for word.

---

## Open questions and source conflicts

1. **Does `session_search` summarise?** The README advertises "FTS5 session
   search with LLM summarization for cross-session recall"; the memory doc says
   "Search queries return actual messages from the DB — **no LLM summarization**,
   no truncation." Most likely raw FTS5 in the tool, with summarisation as a
   separate layer or stale README copy. **Unresolved — verify in source before
   relying on either.**
2. **Terminal backend count.** README enumerates seven (local, Docker, SSH,
   Singularity, Modal, Daytona, Vercel Sandbox); other pages say six. Cosmetic.
3. **Nudge mechanics.** Hermes is repeatedly described as having "agent-curated
   memory with **periodic nudges**," but the memory doc describes only reactive
   saving plus the post-turn background review, with no nudge cadence
   documented. The nudge is probably a system-prompt instruction rather than a
   scheduled event. Not confirmed.
4. **Not yet read in source:** `tools/memory_tool.py`, `agent/agent_init.py`,
   `hermes_cli/config_defaults.py`. Every number above is from docs; the file
   paths are confirmed to exist.

## Sources

- [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) — README, `website/docs/user-guide/features/{memory,memory-providers,skills,curator,honcho}.md`, `website/docs/developer-guide/session-storage.md`, `plugins/memory/holographic/README.md`
- [NousResearch/hermes-agent-self-evolution](https://github.com/NousResearch/hermes-agent-self-evolution)
- [Hermes Agent documentation](https://hermes-agent.nousresearch.com/docs/)
- [Inside Hermes Agent: What "Self-Improving AI Agent" Actually Means in Production](https://saulius.io/blog/hermes-agent-self-improving-ai-architecture) — the four-orthogonal-mechanisms framing
- [Hermes Agent Memory Architecture: Code Walkthrough](https://www.mmntm.net/articles/hermes-memory-architecture)
