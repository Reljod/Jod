# What should hold Jod's state on a VPS

**Question.** Several agent processes share one VPS and all read and write the
same state. Markdown files are the incumbent and the assumption going in was
that they are not the answer. What is?

**Answer.** **One SQLite database in WAL mode, holding everything — events,
state, vectors, full-text — with markdown files kept as the source of truth for
prose and the database as a rebuildable index over them.** Not because SQLite is
a safe conservative default, but because on this workload it measured *fastest*,
*most correct*, and *cheapest to operate*, and it was the only engine that
refused unsafe work instead of silently losing it.

The instinct behind the question was half right. Markdown alone genuinely fails
— it has no atomic read-modify-write, so two agents updating one note is
last-writer-wins. But the fix is not to throw the files away. It is to stop
asking files to be a *database* while continuing to let them be *files*.

> 46 options surveyed → 5 hard filters → 16 survivors → 9 engines benchmarked
> with real concurrent OS processes → SQLite wins all four weightings.
> Full tables in [`out/RANKINGS.md`](out/RANKINGS.md).

---

## 1. What the question actually is

Jod does not have one storage problem. It has five, and the temptation is to buy
a specialist for each. From [`docs/jod-system.md`](../../docs/jod-system.md):

| # | Shape | Access pattern | Today |
|---|---|---|---|
| 1 | **Agent event streams** | append-only, many writers, tail-follow | `~/.jod/runs/<id>/stream.jsonl` |
| 2 | **Agent state** | contended read-modify-write (who owns this task) | `agent.json` |
| 3 | **A2A inbox/outbox** | append + durable read cursor | planned JSONL |
| 4 | **Brain nodes** | prose, human-editable, diffable | planned markdown |
| 5 | **Brain connections + recall** | graph traversal, vector, full-text | planned SQLite/GraphQLite |

Shape 2 is the one that breaks files, and it is the one almost every "best
vector database" comparison ignores entirely. An agent claiming a task is a
`SELECT` then an `UPDATE`, and if two agents interleave, both think they own it.

So the benchmark led with contention, not throughput.

---

## 2. What the measurements found

Test bed: Docker Linux containers on 4 vCPU / 8 GB. Note that this repo's own
[VPS research](../vps-comparison-2026/REPORT.md) recommends a **2 vCPU / 4 GB**
box — the test bed is *double* the real target, which is generous to the
server-based options, not to SQLite. Error bar on an idle host is **±5–6%**;
methodology and caveats in §7.

### Finding 1 — The discriminator is correctness, and most engines fail it silently

8 writer processes, 4 hot keys, read-modify-write `+1`, 1,600 operations:

| engine | acknowledged | actually stored | errors | verdict |
|---|---|---|---|---|
| `sqlite` (`BEGIN IMMEDIATE`) | 1,600 | 1,600 | 0% | **correct** |
| `postgres` (`FOR UPDATE`) | 1,600 | 1,600 | 0% | **correct** |
| `redis` (`INCR`) | 1,600 | 1,600 | 0% | **correct** |
| `sqlite` (deferred, no timeout) | 664 | 664 | **58.5%** | correct, but refused most work |
| `redis` (`GET`/`SET`) | 1,600 | 1,437 | 0% | **lost 163** |
| `qdrant` (`set_payload`) | 1,600 | 863 | 0% | **lost 737** |
| `postgres` (READ COMMITTED, no lock) | 1,600 | 843 | 0% | **lost 757** |
| `lancedb` (`update()`) | 1,600 | 778 | 0% | **lost 822** |

**Every engine that lost data reported a 0% error rate.** Postgres — the
engine everyone reaches for precisely because it is safe — silently discarded
47% of updates when used the obvious way. LanceDB lost 51%.

Meanwhile the one configuration that *looks* like a disaster, SQLite throwing
`database is locked` at 58% of calls, lost nothing. It refused work it could
not do safely.

> **This is the single most important result here.** "Is it safe?" is not a
> property of the engine. It is a property of the primitive you reach for, and
> the engines differ enormously in whether the *obvious* primitive is the safe
> one. SQLite's much-mocked lock error is a feature: it fails loudly. A
> zero-error run is not evidence of correctness — nobody ever gets paged for
> the updates that were never stored.

### Finding 2 — The single-writer bottleneck is not real at agent scale

The received wisdom is that SQLite's one-writer-at-a-time rule disqualifies it
for multi-process work. Measured, on an idle host:

| engine | 1 writer | 4 | 8 | 16 | p50 |
|---|---|---|---|---|---|
| `sqlite` | 45,360 | 45,677 | 43,904 | 38,566 | **0.011 ms** |
| `redis` | 8,965 | 25,731 | 44,202 | 42,049 | 0.101 ms |
| `postgres` | 4,734 | 10,220 | 14,770 | 18,184 | 0.177 ms |
| `duckdb` | 815 | 592 | 803 | 722 | 1.118 ms |
| `lancedb` | 322 | 125 | 61 | 53 | 2.873 ms |

SQLite is essentially **flat at ~40,000 appends/second from 1 to 16 writer
processes**, at 11 microseconds per write. Serialization is real, but
serializing an operation that costs 11 µs is not a bottleneck — it is a queue
that never forms.

For scale: a busy agent emits maybe 10–50 events/second. Twenty agents is
perhaps 1,000 events/second. SQLite has **~40x headroom** over that, and even on
the busy host (§7) it held ~19,000/s — still 19x.

The curves diverge in shape, and that is the honest caveat: SQLite is flat-to-
declining while Postgres and Redis rise. SQLite has no headroom *above* ~45k; it
simply starts far above the requirement. Postgres is the one with room to grow.

### Finding 3 — Two pragmas separate 98% failure from 0%

The same SQLite binary, same workload, 8 writers appending:

- `busy_timeout=5000` + `BEGIN IMMEDIATE` → **43,904 ops/s, 0% errors**
- no busy timeout + deferred transactions → **7,659 ops/s, 98.4% errors**

Almost every "SQLite can't handle concurrency" report is measuring the second
configuration. The difference is two lines of setup.

### Finding 4 — Reads do not notice writers

4 writers appending while 4 readers query:

| engine | read ops/s | read p50 | read p99 |
|---|---|---|---|
| `sqlite` | 153,872 | 0.013 ms | **0.063 ms** |
| `postgres` | 22,515 | 0.13 ms | 0.722 ms |
| `redis` | 9,470 | 0.333 ms | 1.309 ms |
| `lancedb` | 630 | 4.803 ms | 26.58 ms |

WAL's promise — readers never block on the writer — holds precisely.

### Finding 5 — Vector search barely matters at this scale, and ANN defaults are a trap

30,000 × 384-dim, clustered data, top-10:

| engine | index | p50 | recall@1 | recall@10 |
|---|---|---|---|---|
| `redis` | HNSW (defaults) | 1.46 ms | 70.5% | 33.7% |
| `postgres` | HNSW, `ef_search=40` | 1.51 ms | **43.0%** | 19.4% |
| `qdrant` | HNSW (defaults) | 3.44 ms | **98.5%** | 80.1% |
| `lancedb` | IVF-PQ | 5.62 ms | 73.0% | 22.95% |
| `postgres` | HNSW, `ef_search=400` | 7.75 ms | 96.5% | 71.85% |
| `postgres` | exact (control) | 15.00 ms | 100% | 100% |
| `sqlite` | brute force | 18.98 ms | **100%** | **100%** |

Two things fall out.

**pgvector's defaults are not safe.** At `ef_search=40` it found the true
nearest neighbour 43% of the time. Tuning to 400 recovers 96.5% — but costs 5x
the latency, at which point it is only 2.4x faster than brute force. Qdrant got
98.5% out of the box. If you deploy pgvector without tuning `ef_search` and
measuring recall against an exact control, you are silently shipping a broken
memory. (The exact-scan control returning 100% is what proves this is pgvector's
index and not my harness.)

**Brute force is fine here.** sqlite-vec scans all 30,000 vectors in 19 ms with
perfect recall. At 0.63 µs/vector that stays under 100 ms up to roughly
**150,000 memories** — far beyond Jod's brain for a long time. The exact-scan
Postgres control took 15 ms for the same work, confirming brute force is
memory-bandwidth-bound and roughly engine-independent.

The engine that won recall outright, Qdrant, is also the engine that lost 737
updates in Finding 1. It is an excellent index and cannot be your state store.

---

## 3. Hypothesis scorecard

Predictions were committed in [`HYPOTHESES.md`](HYPOTHESES.md) before running
anything. Reporting the misses is the point.

| | Prediction | Outcome |
|---|---|---|
| **H1** | SQLite >2,000 append/s at 8 writers, p99 <50 ms, 0% errors | ✅ **Confirmed**, by 20x — 43,904/s, p99 0.98 ms |
| **H2** | Naive config >5% errors; correct config ~0% | ✅ **Confirmed** — 98.4% vs 0.0% |
| **H3** | Postgres higher p50, better scaling, overtakes SQLite at 8–32 writers | ⚠️ **Half wrong.** Latency and scaling direction correct; the crossover never happened — at 16 writers SQLite was still 2.1x ahead |
| **H4** | Correct primitives safe; naive variants lose updates; Redis loses most | ⚠️ **Half wrong.** First two right. Redis lost *fewest* (163); Postgres lost 757. And naive SQLite lost **none** — it errored instead, a failure mode I did not predict |
| **H5** | LanceDB >20% error rate or >10x collapse | ❌ **Wrong in form, right in conclusion.** 0% errors and 6.1x collapse — but it silently lost 51% of updates, which is worse than either predicted failure |
| **H6** | DuckDB fails categorically | ✅ **Confirmed** — 7 of 8 processes could not open the file |
| **H7** | All engines p95 <50 ms; sqlite-vec within 5x of HNSW | ⚠️ **Half wrong.** All under 50 ms ✅, but sqlite-vec was 13x slower than the fastest HNSW, not 5x — though at 100% recall against their 43–98% |
| **H8** | Reads inflate <2x under write load | ⛔ **Untested.** I measured reads under load but never measured a no-write baseline, so the *ratio* is unavailable. The absolute numbers are in Finding 4 |

Three clean confirmations, four partial misses, one hypothesis I failed to
design a control for. None of the misses changes the recommendation, and the
stated overturn conditions (SQLite under 300 ops/s, or above 1% errors when
configured correctly) were missed by two orders of magnitude.

---

## 4. Best of each — what to steal from the 30 that lost

The survey's real output. Each of these is portable to the recommendation.

| Source | Idea worth taking | How it lands in Jod |
|---|---|---|
| **Dolt** | An agent's write is a *proposal*, not a fact | Shared knowledge lands in a `proposed` state; Jod promotes it. Jod already does this for code via git worktrees — apply the same discipline to data |
| **Zep / Graphiti** | Facts get *invalidated*, not deleted | Two columns: `valid_from`, `valid_to`. Contradiction resolution is impossible without them, and it costs nothing to add now |
| **XTDB / Datomic** | Bitemporality — *when it was true* vs *when I learned it* | A third column, `recorded_at`. "What did I believe last Tuesday" becomes a `WHERE` clause |
| **Letta** | Tiered memory: small always-in-context core, large searchable archive | A schema decision, not a product to buy |
| **DuckLake** | Catalog in a transactional store, data in immutable files | Exactly Jod's markdown-plus-index shape. Validates the hybrid rather than replacing it |
| **Milvus** | Index building belongs off the write path | Never rebuild an embedding index inside the transaction that inserts the note |
| **Redis** | Single-threaded execution makes read-modify-write atomic for free | Where an atomic primitive exists (`INCR`, `UPDATE … SET n = n + 1`), use it instead of read-then-write |
| **NATS JetStream** | Durable consumers with cursors and acks beat tailing a file | The A2A inbox wants a read cursor and an ack, not `tail -f` |
| **Elasticsearch** | *Anti-lesson*: near-real-time indexing breaks read-your-own-write | An agent that writes a fact and immediately re-reads it must see it. Rules out refresh-interval designs |
| **CRDTs** | *Anti-lesson*: convergence is not correctness | Task claims need a *winner*. Use CRDTs for prose, never for coordination |
| **Kùzu** | *Anti-lesson*: the best engine in a thin niche got acquired and archived | Weight project survival as heavily as benchmarks |
| **Qdrant** | Best-in-class *filtered* vector search | The documented escape hatch if recall-under-filter ever becomes the bottleneck |
| **Turso** | Row-level MVCC with SQLite ergonomics | The successor to watch. Concurrent writes hit early preview 2026-08-03 — too young today, plausibly the answer in a year |

---

## 5. The recommended design

One file: `~/.jod/jod.db`. SQLite, WAL.

```sql
PRAGMA journal_mode = WAL;      -- readers never block the writer
PRAGMA busy_timeout = 5000;     -- wait for the lock instead of failing
PRAGMA synchronous = NORMAL;    -- durable across crashes, not across power loss
PRAGMA foreign_keys = ON;
```

Three rules, each earned by a measurement above:

1. **Every write transaction is `BEGIN IMMEDIATE`.** Deferred transactions
   upgrade late and collide — that is the 98% failure mode in Finding 3.
2. **Never hold a write transaction across an LLM call.** The whole argument
   rests on write transactions costing microseconds. A transaction held open
   across a 30-second model call converts SQLite's single writer from a
   non-issue into a hard outage for every other agent.
3. **Markdown stays the source of truth for prose.** The database indexes it and
   can be deleted and rebuilt by rescanning. This keeps the charter's "plain
   files at the boundaries" rule and gives you `grep` when the system is down.

Schema sketch, carrying the stolen ideas:

```sql
-- shape 1+3: events and A2A, append-only, many writers
CREATE TABLE events (
  id         INTEGER PRIMARY KEY,      -- monotonic, assigned by SQLite
  run_id     TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  kind       TEXT NOT NULL,            -- Started|Thinking|Message|ToolCall|...
  recorded_at REAL NOT NULL,
  payload    TEXT NOT NULL             -- the raw harness line, never dropped
);
CREATE INDEX ix_events_run ON events(run_id, seq);

-- shape 2: contended state. Claim with BEGIN IMMEDIATE, or better, atomically:
--   UPDATE tasks SET owner = ?, claimed_at = ?
--    WHERE id = ? AND owner IS NULL;   -- 0 rows changed == you lost the race
CREATE TABLE tasks (
  id TEXT PRIMARY KEY, owner TEXT, claimed_at REAL, status TEXT NOT NULL
);

-- shape 5: facts, bitemporal (XTDB) + proposable (Dolt) + invalidated (Zep)
CREATE TABLE facts (
  id           INTEGER PRIMARY KEY,
  subject      TEXT NOT NULL,
  predicate    TEXT NOT NULL,
  object       TEXT NOT NULL,
  source_note  TEXT,                  -- path to the markdown that asserted it
  valid_from   TEXT,                  -- when it became true in the world
  valid_to     TEXT,                  -- NULL = still believed
  recorded_at  REAL NOT NULL,         -- when Jod learned it
  state        TEXT NOT NULL DEFAULT 'proposed',  -- proposed | accepted
  invalidated_by INTEGER REFERENCES facts(id)
);

-- recall: derived index over the markdown, rebuildable
CREATE VIRTUAL TABLE notes_fts USING fts5(path, title, body);
CREATE VIRTUAL TABLE notes_vec USING vec0(id INTEGER PRIMARY KEY, embedding float[384]);
```

Graph queries start as recursive CTEs. Add GraphQLite only when a traversal
becomes genuinely painful to express — it ranked #3 overall, but it is at
v0.3.7 with a single maintainer, and that bus factor is the reason not to make
it load-bearing before it has to be.

### When to change your mind

Each trigger is a measured number, not a feeling:

| Trigger | Threshold | Move to |
|---|---|---|
| Sustained write rate | > 5,000 events/s (≈ 100+ busy agents) | Postgres — it is the only engine measured with headroom above its own curve |
| Vector corpus | > ~150,000 memories (brute force passes 100 ms at 0.63 µs/vector) | pgvector tuned + verified, or Qdrant beside SQLite |
| Filtered recall | "search only domain X" becomes common | Qdrant — sqlite-vec's metadata filtering is its weakest area |
| More than one machine | an iOS client or a second VPS needs writes | libSQL server mode, or Postgres |
| Write txn cannot stay short | some operation must hold a write open for seconds | Postgres (MVCC), immediately |

Until one of those fires, a second daemon buys nothing and costs RAM the agents
need — especially on the 2 vCPU / 4 GB box this repo already chose.

---

## 6. Rankings

SQLite ranks first under all four weightings, including `throughput`, where it
wins on measurement rather than on simplicity. Full tables and the 30
eliminations are in [`out/RANKINGS.md`](out/RANKINGS.md).

| Profile | 1st | 2nd | 3rd |
|---|---|---|---|
| **charter** (Jod's charter as written) | SQLite **4.71** (100% stable) | libSQL 4.60 | GraphQLite 4.37 |
| **throughput** (20+ agents) | SQLite **4.62** (100%) | libSQL 4.52 | GraphQLite 4.28 |
| **simplicity** (fewest parts) | SQLite **4.74** (100%) | libSQL 4.62 | GraphQLite 4.38 |
| **recall-first** (millions of notes) | SQLite **4.47** (99%) | libSQL 4.38 | **ParadeDB 4.28** |

Only under `recall-first` does a Postgres variant reach the podium — ParadeDB,
for BM25 and vector fused in one query. That is the shape of the future
migration if one comes.

The top-3 stability column matters: SQLite holds the podium in 99–100% of 2,000
runs with weights jittered ±1 and judged scores ±0.5. Postgres sits at 2.6%
under `charter` — not because it is bad, but because that profile weights
`local_first` and `ops_burden` at 5, and a daemon cannot win there. Under
`throughput` it climbs to 20%. **The ranking is a function of the weights, and
the weights are in [`data/profiles.json`](data/profiles.json).**

---

## 7. How much to trust this

**Solid.** The correctness results — who loses updates and who does not — are
categorical, reproducible, and independent of hardware and load. They are the
findings the recommendation actually rests on. The DuckDB and LanceDB
disqualifications are likewise structural.

**Reasonably solid.** The throughput *shapes* — SQLite flat, Postgres and Redis
rising, LanceDB collapsing — held across two independent passes on different
load conditions, and the idle-host error bar is ±5–6%.

**Treat with care.**

- **One machine, one architecture.** aarch64 Docker on macOS, 4 vCPU / 8 GB.
  Real hardware, a real Linux kernel, and the 2 vCPU box actually recommended
  will all differ. Absolute ops/s should be read as an order of magnitude.
- **The host was not dedicated.** An unrelated container held ~177% CPU of 4
  cores during the first pass, and the Docker daemon crashed once under that
  load. Those runs are labelled `contended` in the raw data and excluded from
  headline tables. Throughput fell 2–3x on the busy host **without reordering
  the engines**, which is itself a useful result for a cheap oversubscribed VPS.
- **Durability settings are not identical.** Postgres ran `synchronous_commit=on`,
  SQLite `synchronous=NORMAL`, Redis `appendfsync everysec`. SQLite and Redis are
  therefore doing measurably less fsync work than Postgres. This favours them,
  and a like-for-like `synchronous=FULL` comparison was not run.
- **Synthetic embeddings.** Clustered Gaussian vectors, not real sentence
  embeddings. The first attempt used uniform random vectors and produced
  meaningless recall (16% for pgvector); that run was discarded and the
  generator fixed. Real embeddings would likely score somewhat better than these
  numbers for every ANN engine — the *ordering* is the transferable part.
- **30,000 vectors, not millions.** The brute-force extrapolation to ~150,000 is
  linear arithmetic from a measured per-vector cost, not a measured point.
- **Turso Database was not benchmarked.** Its concurrent-write mode entered
  early preview six days before this was written. It is the most interesting
  candidate on the list and the only one whose absence is a real gap here.
- **Single-node only.** Nothing here says anything about replication, failover,
  or backup restore times.

**Reproduce it:** `./bench/run_all.sh`, then `python3 scripts/report.py`.

---

## 8. The one-paragraph version

Keep the markdown. Add one SQLite file in WAL mode next to it and put the
events, the agent state, the task claims, the vectors and the full-text index
in it. Set `busy_timeout`, use `BEGIN IMMEDIATE` for every write, and never hold
a write transaction across a model call. It measured ~40,000 writes/second flat
from 1 to 16 concurrent processes with zero lost updates, needs no daemon, fits
a $6 VPS, and is the only engine tested that refused unsafe work rather than
silently dropping it. Revisit when you pass 5,000 writes/second or 150,000
memories — and watch Turso, which is building exactly the right thing and is
about a year from being trustworthy.
