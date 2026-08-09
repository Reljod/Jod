# What Jod should use for memory — the decision

**Date:** 2026-08-09 · **Analyst:** Jod · **Evidence:**
[`experiments/FINDINGS.md`](experiments/FINDINGS.md) ·
[`experiments/FINDINGS-2.md`](experiments/FINDINGS-2.md) ·
[`BIG-LAB-MEMORY.md`](BIG-LAB-MEMORY.md) ·
[`FUTURE-OF-MEMORY.md`](FUTURE-OF-MEMORY.md) ·
**Companion:** DB engine choice → branch `research/memory-db`

> **Question:** which memory system should Jod use?

---

## The recommendation

**Adopt no memory product. Build a control plane in `jod-core`, over the
Markdown files Jod already writes.**

That is not a default-to-DIY answer — it is what 31 measured architectures and
four shipped lab systems point at:

- **Buying a memory layer buys cost reduction and a benchmark claim, and costs
  accuracy.** A controlled comparison has long-context at 92.85% on LoCoMo where
  Mem0 scores 57.68% — a 35-point gap caused by compressing ~102k tokens into
  ~2,909 retrieved. Memory products win on cost after ~10 turns, not on being
  right.
- **The part that isn't substitutable is governance** — knowing what is
  currently true, deleting so it stays deleted, and not believing a web page
  over Reljod. That was worth +0.34 composite in round 1 and +0.22 in round 2,
  and no amount of context or retrieval quality substitutes for it.
- **Jod already has most of the storage layer**, and it happens to match what
  every lab converged on.

**The specific composition:** Claude Code's file model for storage (already
present), Codex's authored-vs-learned split (already present), Memory Bank's
scope-before-similarity partitioning (`domains/` already is one), Graphiti's
bi-temporal validity (**build**), OpenClaw's consolidation *guards* but not its
promotion scoring, and write-time trust admission (**build** — this is the gap
that matters most).

---

## Component-by-component

| Component | Decision | Measured evidence |
|---|---|---|
| **Markdown as source of truth** | **Keep** — already have it | Every lab and both OSS systems converged on it; the index must be rebuildable |
| **Authored / learned split** (`AGENTS.md` + `.agents/skills` vs auto-memory) | **Keep** — already have it | Merging them loses ~⅓ of conventions to extraction (0.94 → 0.56, R2) |
| **Scope partition by `domains/`** | **Build** — the directories exist, the enforcement doesn't | Scope-as-a-ranking-signal leaks 79% of the time; as a hard filter, 0% (R2) |
| **Versioned facts with validity intervals** | **Build — highest value** | current-value 0.17 → 0.73 (R1); OpenAI's own time-sensitive accuracy 9.4% → 75.1% |
| **Deletion purges every version** | **Build — free** | Tombstoning only the head leaks the withdrawn fact on 56% of historical queries (R2) |
| **Write-time trust admission** | **Build — free, and the biggest safety gap** | Attack success 0.17–0.25 → 0.00 at every evidence width (R2). Jod ingests Linear, Notion and the web |
| **Episodic layer** (searchable session history) | **Build** | Jod has none. Hermes' `state.db` + FTS5 is a weekend, and it's the tier most obviously missing |
| **Second retrieval hop** | **Build** | multi-hop 0.00 → 0.42 for ~14 tokens (R1) — reserve slots, never displace round one |
| **Eviction: importance or recall** | **Build if you ever cap** | LRU evicts 100% of authored conventions (R2). Anything beats recency |
| **Consolidation guards** (bounded loss on rewrite) | **Copy from OpenClaw** | Its 25% `maxPriorEntryLossFraction` is the piece most implementations lack |
| **Hybrid dense+lexical retrieval** | **Defer** | Two frontier coding agents (Codex, Claude Code) ship *no embeddings* for memory. Add when lexical demonstrably fails |
| **Earned promotion** | **Skip** | Zero accuracy gain for +62% tokens (R1) |
| **Temporal decay for freshness** | **Skip** | Destroyed long-tail recall 0.40 → 0.00 (R1). The control plane gives freshness free |
| **Destructive in-place rewrite** | **Skip** | Historical value → 0.00 (R2). Append and supersede instead |
| **Abstention on conflict** | **Skip** *if* the three above are built | An exact no-op once supersession and scope have run (R2) |

---

## What Jod already has, and what's actually missing

```
AGENTS.md + .agents/skills/   →  authored tier            ✅ (labs' AGENTS.md split)
auto-memory *.md + MEMORY.md  →  learned tier, file truth ✅ (Claude Code model)
domains/{finance,second-brain,tasks}  →  scope partition  ⚠️  exists as folders, unenforced
git                           →  audit trail + rollback   ✅ (better than most labs ship)
metadata.type in frontmatter  →  purpose class            ⚠️  not a trust boundary
—                             →  temporal validity        ❌
—                             →  real deletion            ❌
—                             →  trust admission          ❌
—                             →  episodic search          ❌
```

Four gaps. All four are write-path, which is exactly where both rounds found the
value. **Jod does not need a new memory system; it needs a control plane over
the one it has.**

---

## Build order

Each phase is independently useful and independently reversible.

**Phase 1 — the control plane (`crates/jod-core`).** A fact store over the
existing Markdown: `(scope, subject, predicate) → [versions]`, each version
carrying `valid_from`, the source file, and an origin class
(`owner` / `agent` / `untrusted` / `system`). Conflict resolution is
`max(valid_from)` in Rust — never a prompt. Deletion purges every version and
writes a tombstone. Files stay authoritative; the store is derived and
rebuildable. *Payoff: the largest measured effect in either round.*

**Phase 2 — trust admission at ingest.** Anything arriving from Linear, Notion,
a fetched page or a pasted block is written with `origin: untrusted`, stored
outside the fact text so it cannot be forged, and excluded from the answer set
by default. Redact secrets before anything touches disk, as Codex does.
*Payoff: attack success 0.17–0.25 → 0.00. Jod's blast radius here is real —
it holds Reljod's finance and task data.*

**Phase 3 — scope enforcement.** `domains/finance` must not answer with a
`domains/tasks` fact. Filter by domain *before* ranking, never as a score bonus.
*Payoff: cross-domain leakage 0.79 → 0.00.*

**Phase 4 — the episodic layer.** Every delegated harness session (Claude Code,
OpenCode) already streams through `jod-core`. Persist it — SQLite + FTS5, one
row per message — and expose a search tool. *Payoff: the tier Jod entirely
lacks; Hermes measures ~20 ms queries over it.*

**Phase 5 — consolidation, off the critical path.** Idle-triggered like Hermes'
curator, not scheduled. Bound the blast radius: reject any rewrite that drops
more than a quarter of prior entries. *Payoff: keeps the file layer legible as
it grows.*

Retrieval quality — hybrid search, embeddings, MMR — is deliberately last. It
was worth 0.02–0.07 in the experiments. Phases 1–3 are worth 0.2–0.5 and are
mostly deterministic code.

---

## Why not the alternatives

| Option | Why not |
|---|---|
| **Mem0** (62.8k ★) | Easiest adoption, largest community, and the 2026 rewrite is directionally right (ADD-only, entity linking, temporal rerank). But it is a *retrieval* layer — it solves the part Jod's problem isn't, and published numbers don't survive replication (93.4% claimed, 73.8% observed) |
| **Zep / Graphiti** (29.7k ★) | Architecturally the most serious — bi-temporal validity on every edge is exactly the right primitive, and it independently beats Mem0 by 15 points. **Copy the model, not the dependency**: it wants a graph service, Jod wants a local file + SQLite footprint |
| **Letta / MemGPT** (24.2k ★) | Best ideas in the category (tiered memory, sleep-time compute), but it is a full agent framework. Jod already *is* the harness — adopting Letta means running two |
| **Hermes / OpenClaw wholesale** | Both are complete agents, not memory libraries. Jod delegates *to* agents; it can't be one |
| **A hosted memory API** | Reljod's finance and second-brain data leaving the machine, to buy a retrieval layer the measurements say is the least valuable component |

---

## The one thing to change first

If only one thing gets built: **make deletion real, and make it purge history.**

It is free, it is a few dozen lines, it closes a defect I found in my own design
(a withdrawn fact resurfacing on 56% of historical queries), and it is the
difference between "Jod forgot that" and "Jod says it forgot that." For a system
holding someone's finances and private notes, that is the property that has to
be true before any of the accuracy numbers matter.

---

## Open question: which engine backs the index

Phases 1 and 4 need a local store — embedded, Rust-friendly, rebuildable,
shipping inside a Tauri app. SQLite with FTS5 is the obvious default and what
every system studied here uses, but the options (rusqlite vs libsql vs redb,
FTS5 vs tantivy, sqlite-vec if embeddings ever land) deserve their own
evaluation against Jod's actual constraints.

That is the subject of the companion research on branch **`research/memory-db`**.
