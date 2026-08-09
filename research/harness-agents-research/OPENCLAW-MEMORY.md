# OpenClaw — how it remembers

**Date:** 2026-08-09 · **Analyst:** Jod · **Subject:**
[openclaw/openclaw](https://github.com/openclaw/openclaw) (~385k ★) ·
**Companion:** [`HERMES.md`](HERMES.md)

> **Question, as asked:** how does OpenClaw do the remembering part — what
> database, what algorithm?

Scope is memory only. Nothing here about OpenClaw's gateway, channels, or tools.

---

## The answer in one paragraph

**Database:** per-agent **SQLite**, one file, with **FTS5** for keyword search
and the **`sqlite-vec`** extension for vector search — no server, no external
vector DB, embeddings and text in the same file.
**Algorithm:** Markdown files are the source of truth; a background indexer
chunks them at **400 tokens with 80-token overlap**, embeds each chunk (default
OpenAI `text-embedding-3-small`, 1536 dims), and answers queries with **hybrid
search scored `0.7 × vector + 0.3 × BM25`**, then multiplies by a
per-entry **importance** factor, optionally applies **exponential temporal
decay** and optional **MMR diversity re-ranking**, and returns the top **6**
chunks above a **0.35** score floor. Overnight, a three-phase **"dreaming"**
process re-reads recall telemetry, scores candidate facts on six weighted
signals, and promotes only the qualified ones into the durable `MEMORY.md`.

The retrieval weights, BM25 normalisation, MMR and decay constants below are
read directly from source, not from documentation.

---

## 1. Storage layer — files are the truth

Everything durable is plain Markdown in the workspace (default
`~/.openclaw/workspace`). **[docs]**

| File | Role | Auto-loaded? |
|---|---|---|
| **`USER.md`** | Stable user preferences and communication style | **Yes** — separate token budget |
| **`MEMORY.md`** | "Durable non-profile facts and decisions" — the curated long-term layer | **Yes** — bootstrap prompt |
| **`memory/YYYY-MM-DD.md`** | Daily working notes, detailed observations | Today + yesterday on `/new` or `/reset` only |
| **`DREAMS.md`** | Consolidation diary for human review | No — audit artefact |

Only `USER.md` and `MEMORY.md` enter the bootstrap prompt. Daily notes are
**indexed but not injected** — they reach the model through search. Material
migrates from daily notes up into `MEMORY.md` over time via dreaming (§4).
**[docs]**

This is a genuinely different bet from Hermes: OpenClaw lets the corpus grow
without limit and pays for it with retrieval, where Hermes caps the corpus and
pays for it with consolidation pressure.

---

## 2. The database

**Path:** `~/.openclaw/agents/<agentId>/agent/openclaw-agent.sqlite` — one
SQLite file per agent. **[docs]** "SQLite WAL sidecars are bounded with periodic
and shutdown checkpoints." **[docs]**

### Tables **[3p, corroborated by source references]**

| Table | Purpose |
|---|---|
| `files` | path, source, content hash, `updated_at`, size — lets the indexer skip unchanged files |
| `chunks` | `id, path, start_line, end_line, hash, model, text, embedding, updated_at` |
| `chunks_fts` | FTS5 virtual table — keyword search |
| `memory_index_chunks_vec` (`vec0`) | `sqlite-vec` virtual table — binary float vectors **[src]** |
| `embedding_cache` | `(provider, model, hash) → embedding`, cross-file dedupe |

Chunks are addressed as `(path, start_line, end_line)`, so every retrieved
snippet cites an exact source range. That's what makes dreaming's
`Source: path#Lx-Ly` requirement (§4) enforceable.

### Provenance is stored out-of-band

Each chunk carries an **origin class** — `owner`, `agent`, `untrusted`, or
`system` — plus session kind, observation time, and an optional supersession
key, "separately from content to prevent trust classification tampering."
**[docs]**

Storing the trust label outside the text is the correct defence: if provenance
lived *in* the chunk, injected text could claim to be `owner`. Downstream, the
dreaming deep phase drops `untrusted` and `system` candidates before any
promotion.

### Indexing and reindex triggers **[docs]**

- **Sources:** `MEMORY.md`, a root `USER.md` if present, `memory/*.md`, plus
  anything in `memory.search.extraPaths`.
- **Chunking:** 400 tokens, 80-token overlap, line-aware (~4 chars ≈ 1 token).
- **File watching:** changes trigger a **debounced reindex, 1.5 s default**.
- **Full reindex** when the embedding provider, model, chunking config,
  configured sources, or scope changes — or manually via
  `openclaw memory index --force`.
- **Sessions** are an opt-in source (`memory.search.sources: ["memory","sessions"]`),
  delta-synced after **100 KB of new session data or 50 new messages**, with a
  **5 s** debounce. **[3p]**

### Embeddings **[docs, 3p]**

Default provider `openai`, model `text-embedding-3-small` (**1536 dims**).
Eleven providers supported — Bedrock, DeepInfra, Gemini
(`gemini-embedding-001`, 768 dims), GitHub Copilot, Mistral, Ollama, Voyage, and
**local GGUF** (`embeddinggemma-300M-Q8_0.gguf`, ~600 MB, ~50 tok/s on an M1 vs
~1,000 tok/s for remote APIs).

Batch embedding is available for OpenAI, Gemini and Voyage — 50% cheaper, off by
default, max 2 concurrent jobs, 8,000 tokens per batch, 3 retries. The
`embedding_cache` keys on a **SHA-256 of chunk content** plus provider and
model, so re-chunking overlapping text costs nothing.

Index density is roughly **5 KB per 1,000 tokens** at 1536 dims. **[3p]**

---

## 3. The retrieval algorithm

This is the part worth reading closely. Verified in
`extensions/memory-core/src/memory/{hybrid,mmr,temporal-decay,importance}.ts`
and `src/agents/memory-search.ts`. **[src]**

### Step 0 — build the FTS query

```ts
export function buildFtsQuery(raw: string): string | null {
  const tokens = normalizeStringEntries(raw.match(/[\p{L}\p{N}_]+/gu) ?? []);
  if (tokens.length === 0) return null;
  const quoted = tokens.map((t) => `"${t.replaceAll('"', "")}"`);
  return quoted.join(" AND ");
}
```

Unicode-aware tokenisation, every token quoted, joined with `AND` — conjunctive,
so BM25 acts as a precision filter while the vector side supplies recall.

### Step 1 — two independent searches

- **Vector:** `vec_distance_cosine(v.embedding, ?) AS dist` inside SQLite via
  `sqlite-vec`. **[src]** If the extension fails to load, the system degrades to
  computing cosine similarity in Node over candidate chunks. **[3p]**
- **Keyword:** FTS5 BM25, tokenizer `unicode61` (or `trigram`). **[docs]**

### Step 2 — normalise BM25 into `[0,1]`

```ts
export function bm25RankToScore(rank: number): number {
  if (!Number.isFinite(rank)) return 1 / (1 + 999);
  if (rank < 0) { const relevance = -rank; return relevance / (1 + relevance); }
  return 1 / (1 + rank);
}
```

Two regimes, because SQLite's `bm25()` returns *negative* relevance while
positional rank is non-negative — handling both is why the same function serves
both call sites.

### Step 3 — weighted fusion

Results merge by **union of chunk IDs** (a chunk found by only one side keeps
its other score at zero), then:

```
score = vectorWeight × vectorScore + textWeight × textScore
```

Defaults, from `src/agents/memory-search.ts`: **[src]**

```ts
const DEFAULT_MIN_SCORE               = 0.35;
const DEFAULT_HYBRID_VECTOR_WEIGHT    = 0.7;
const DEFAULT_HYBRID_TEXT_WEIGHT      = 0.3;
const DEFAULT_MMR_LAMBDA              = 0.7;
```

Both weights are clamped to `[0,1]` and **renormalised to sum to 1**, so
configuring `0.9 / 0.9` yields `0.5 / 0.5` rather than inflated scores.

### Step 4 — importance multiplier

```ts
function importanceMultiplier(importance: number | null | undefined): number {
  if (importance === null || importance === undefined) return 1;
  const bounded = Math.max(1, Math.min(10, Math.floor(importance)));
  return 0.75 + bounded * 0.05;
}
```

Importance **1–10** maps to a multiplier of **0.80–1.25**; absent importance is
a neutral `1`. Deliberately gentle — importance re-ranks near-ties, it can't
drag an irrelevant chunk into the results.

**This is the elegant bit.** That `importance` value is not hand-set: it's the
`<!-- importance: N -->` annotation that the *dreaming* process writes into
`MEMORY.md` (§4). The nightly consolidation pass and the per-query retrieval
pass are two halves of one loop — writing tunes reading.

### Step 5 — temporal decay (default **off**)

```ts
export const DEFAULT_TEMPORAL_DECAY_CONFIG = { enabled: false, halfLifeDays: 30 };
// multiplier = exp(-(ln2 / halfLifeDays) * ageInDays)
```

Standard exponential half-life. Crucially, it only applies to **dated** files
matching `memory/YYYY-MM-DD.md`; `MEMORY.md`, `USER.md` and undated `memory/*`
files are **evergreen** and never decay. Age comes from the *filename*, not
mtime — so re-saving a daily note doesn't make it young again.

### Step 6 — MMR diversity re-ranking (default **off**)

```ts
export const DEFAULT_MMR_CONFIG = { enabled: false, lambda: 0.7 };
// MMR = λ × relevance − (1 − λ) × max_similarity_to_already_selected
```

Textbook Maximal Marginal Relevance, citing Carbonell & Goldstein (1998), with
inter-chunk similarity computed as **Jaccard over tokens** — cheap, no second
embedding pass. λ = 0.7 leans toward relevance. Its job is to stop 80-token
overlap from returning six near-identical windows of the same paragraph.

### Step 7 — cut and return

Drop anything below `minScore` **0.35**, return at most `maxResults` **6**, each
with up to **700 characters** of snippet. **[docs, 3p]**

Target latency: **<100 ms for hybrid search across 10K chunks.** **[3p]**

### The pipeline in one line

```
FTS5(BM25) ∪ sqlite-vec(cosine)
  → 0.7·vec + 0.3·bm25
  → × importance(0.80–1.25)
  → × exp(−ln2·age/30)   [off by default, dated files only]
  → MMR λ=0.7            [off by default]
  → minScore 0.35, top 6, 700-char snippets
```

Both optional stages default **off** — OpenClaw ships the simple thing and lets
you opt into sophistication.

---

## 4. Writing and consolidation

### Memory flush before compaction **[3p]**

When the context window is nearly full, a flush runs so nothing important dies
in compaction:

```
flush when: currentTokens ≥ contextWindow − reserveTokensFloor − softThresholdTokens
```

For a 200K window with a 20,000-token reserve floor and 4,000-token soft
threshold, that fires at **≥176,000 tokens**. It's silent (`NO_REPLY`) when
there's nothing to save, runs **once per compaction cycle**, and is skipped in
read-only sandbox mode.

### Dreaming — the consolidation algorithm **[docs]**

Enabled by default, on cron `0 3 * * *`. Each sweep runs three phases in
sequence: **light → REM → deep**. Only **deep** writes to `MEMORY.md`.

- **Light** — read recent short-term recall signals, daily files and session
  transcripts; dedupe; stage candidates. No writes.
- **REM** — build theme summaries from recent traces; record reinforcement
  signals for deep ranking. No writes.
- **Deep** — rank, gate, and promote.

**Deep ranking weights** — six base signals, summing to 1.00:

| Signal | Weight | Meaning |
|---|---|---|
| Relevance | **0.30** | retrieval quality |
| Frequency | **0.24** | accumulated short-term recall signals |
| Query diversity | **0.15** | distinct contexts that surfaced the entry |
| Recency | **0.15** | time-decayed freshness |
| Consolidation | **0.10** | multi-day recurrence strength |
| Conceptual richness | **0.06** | concept-tag density |

Light and REM hits add "a small recency-decayed boost" on top.

Frequency + query diversity together are the interesting pair: **an entry earns
promotion by being retrieved, in varied contexts** — not by looking important
when written. The read path generates the training signal for the write path.

**Three gates, all must pass:** `minScore`, `minRecallCount`, `minUniqueQueries`.
Then snippets are "rehydrate[d] from live daily files before writing, so
stale/deleted snippets are skipped" — you can delete a daily note and it will
not resurrect at 3am.

**What gets written into `MEMORY.md`:** the entry plus trailing recall metadata
— up to three concept tags as `<!-- trigger: phrase one, phrase two -->` and a
bounded `<!-- importance: N -->` (1–10). Existing annotated entries are kept
**byte-for-byte** unless explicitly merged or superseded.

The trigger tags feed the FTS5 side of retrieval; the importance value feeds
step 4. Dreaming is literally writing next month's ranking function.

**Safety guards on the rewrite:**

- Candidates marked `untrusted` or `system` are removed first.
- Prior entries must be preserved within `maxPriorEntryLossFraction` —
  **default 0.25**, so a rewrite losing >25% of existing entries is rejected.
- Every promoted candidate must carry its `Source: path#Lx-Ly` reference.
- The result must fit the bootstrap-safe file budget and parse as the expected
  structured response.
- The previous `MEMORY.md` is stored in SQLite before the change.

That loss-fraction guard is the single most important line in the design: it's
what stops an LLM rewrite from quietly deleting your memory. Cap the blast
radius of the model's own edit, and consolidation becomes safe to automate.

`DREAMS.md` receives added / merged / superseded counts plus "short diff-style
highlights," written by a configurable subagent model. The diary itself is
**excluded from promotion** — only grounded snippets qualify, so the agent
cannot bootstrap its own narrative into fact.

```yaml
# plugins.entries.memory-core.config.dreaming
enabled: true
frequency: "0 3 * * *"
phases.deep.maxPromotedSnippetTokens: 160
phases.deep.maxPriorEntryLossFraction: 0.25
```

---

## 5. Configuration reference **[docs]**

| Key | Default |
|---|---|
| `memory.search.enabled` | `true` |
| `memory.search.provider` | `"openai"` |
| `memory.search.model` | provider default |
| `memory.search.fallback` | `"none"` |
| `memory.search.query.maxResults` | `6` |
| `memory.search.query.minScore` | `0.35` |
| `memory.search.store.vector.enabled` | `true` |
| `memory.search.store.fts.tokenizer` | `"unicode61"` (or `"trigram"`) |
| `memory.search.cache.enabled` | `true` |
| `memory.search.sources` | `["memory"]` — add `"sessions"` for transcripts |
| `memory.search.rememberAcrossConversations` | on for personal; off with DM isolation |
| `memory.search.extraPaths` | — |
| `memory.search.multimodal.enabled` | `false` |
| `memory.search.multimodal.maxFileBytes` | `10485760` |
| `memory.search.remote.nonBatchConcurrency` | `4` |
| `memory.search.remote.batch.enabled` | `false` |
| `memory.backend` | `builtin` |

Multimodal indexing requires `gemini-embedding-2-preview` and applies only to
files in `extraPaths`.

### Alternative backends **[docs]**

| Backend | What it adds |
|---|---|
| **QMD** | Local-first sidecar with reranking — `maxResults 4`, `maxSnippetChars 450`, `timeoutMs 4000` |
| **Honcho** | "AI-native cross-session memory with user modeling" (same provider Hermes uses) |
| **LanceDB** | Plugin backend with local embedding support |
| **Memory Wiki** | Converts memory into a structured vault with "deterministic page structure, structured claims and evidence" |

---

## 6. Hermes vs OpenClaw — the two philosophies

| | **Hermes** | **OpenClaw** |
|---|---|---|
| Long-term memory size | **Capped** (2,200 + 1,375 chars) | Uncapped |
| Overflow behaviour | Tool **errors**, agent must consolidate | Grows; dreaming curates later |
| Embeddings in core | **No** | Yes (`sqlite-vec`) |
| Memory retrieval | Agent explicitly calls `session_search` (FTS5 only) | Auto hybrid retrieval per turn |
| Consolidation | Human-legible rewrite under a hard cap | Nightly scored promotion (`0 3 * * *`) |
| Scaling answer | **Skills** + progressive disclosure | **RAG** + ranking |
| Cost of memory | Fixed tokens, zero API | Embedding calls + nightly LLM passes |
| Failure mode | Forgets what won't fit | Retrieves the wrong chunk |

Neither is wrong. Hermes optimises for a memory a *human* can read and audit in
thirty seconds; OpenClaw optimises for never losing anything. Both converge on
the same two structural choices — **Markdown as source of truth, SQLite as
derived index** — and both keep the human-readable layer authoritative so the
index can always be thrown away and rebuilt.

---

## 7. What's worth stealing for Jod

1. **Markdown truth + disposable SQLite index.** Jod's auto-memory is already
   Markdown files plus a hand-maintained `MEMORY.md` index. Adding a rebuildable
   SQLite index is additive and reversible — nothing is lost if it's deleted.
2. **Provenance stored outside the content.** `owner` / `agent` / `untrusted` /
   `system` on every chunk, unforgeable from inside the text. Jod ingests from
   Linear, Notion and the web; the trust label must not be something a Notion
   page can assert about itself.
3. **`maxPriorEntryLossFraction`.** Bound how much an automated rewrite may
   delete and reject the rewrite otherwise. One number, and LLM-driven memory
   compaction stops being a footgun.
4. **Promote on retrieval, not on write.** Frequency + query-diversity ranking
   means facts earn permanence by proving useful. Jod's memory currently records
   what seemed important at write time — which is exactly the signal least
   correlated with future usefulness.
5. **The importance-annotation loop.** Consolidation writes
   `<!-- importance: N -->`; retrieval multiplies by `0.75 + 0.05N`. A closed
   loop in two small pieces of code, no ML.
6. **Ship the optional sophistication off.** MMR and temporal decay are
   implemented, tested, and default-`false`. Good discipline for a repo whose
   charter says *reversible by default*.

---

## Open questions and source conflicts

1. **Database path.** Docs say
   `~/.openclaw/agents/<agentId>/agent/openclaw-agent.sqlite`; two third-party
   write-ups say `~/.openclaw/memory/{agentId}.sqlite`. Probably a rename the
   blogs missed. **Trust the docs path; verify on a live install.**
2. **Embedding column format.** Third-party sources say embeddings are
   JSON-serialised in `chunks` *and* stored as binary floats in the `vec0`
   virtual table. Likely both — JSON as the portable fallback for when
   `sqlite-vec` is unavailable, vectors for the fast path. Not confirmed in
   source.
3. **Which score reaches `minScore`.** The 0.35 floor is applied after fusion,
   but whether it is checked before or after the importance multiplier and decay
   changes which chunks survive. Not established; requires reading the
   `mergeHybridResults` tail and its caller.
4. **Dreaming's gate values.** `minRecallCount` and `minUniqueQueries` are named
   in the docs but their defaults are not published. Unread in source.
5. **Not yet read in source:** `extensions/memory-core/src/memory/manager.ts`,
   `manager-search.ts` (both confirmed to exist and reference the constants
   above), and the dreaming implementation.

## Sources

- [openclaw/openclaw](https://github.com/openclaw/openclaw) — `extensions/memory-core/src/memory/{hybrid,mmr,temporal-decay,importance}.ts`, `src/agents/memory-search.ts` **(all `[src]` figures read from these)**
- [Builtin memory engine · OpenClaw docs](https://docs.openclaw.ai/concepts/memory-builtin)
- [Memory overview · OpenClaw docs](https://docs.openclaw.ai/concepts/memory)
- [Memory configuration reference · OpenClaw docs](https://docs.openclaw.ai/reference/memory-config)
- [Dreaming · OpenClaw docs](https://docs.openclaw.ai/concepts/dreaming)
- [Deep Dive: How OpenClaw's Memory System Works](https://snowan.gitbook.io/study-notes/ai-blogs/openclaw-memory-system-deep-dive) — most `[3p]` numbers
- [Local-First RAG: Using SQLite for AI Agent Memory with OpenClaw](https://www.pingcap.com/blog/local-first-rag-using-sqlite-ai-agent-memory-openclaw/) — table schema
