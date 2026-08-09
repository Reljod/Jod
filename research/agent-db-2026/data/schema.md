# Dataset schema

## `candidates.json`

One record per storage option considered. 46 records — the survey is meant to be
exhaustive about *families*, so a few records cover a group of near-identical
engines (e.g. `rust-embedded-kv` covers redb, sled, fjall and RocksDB).

| field | meaning |
|---|---|
| `id` | stable slug, used to join against `scores.json` |
| `family` | how the thing is shaped: `embedded-relational`, `server-vector`, `event-log`, `versioned`, `memory-framework`, … |
| `deployment` | `embedded` (in-process), `server` (a daemon), `cloud`, `library` |
| `writer_model` | the one sentence that decides whether it survives the hard filter |
| `concurrency` | what actually happens when two processes write at once |
| `vector` / `fulltext` / `graph` | retrieval capabilities, `none` when absent |
| `txn` | transactional guarantee offered |
| `rust_client` | `first-party`, `community`, `none` — matters because `jod-core` is Rust |
| `idle_mb` | approximate resident memory doing nothing; `0` for in-process engines |
| `maturity` | `ga`, `beta`, `experimental`, `dormant`, `archived` |
| `status_2026` | what changed recently enough that older comparisons are wrong |
| `disqualifiers` | concrete reasons it fails, empty list if none |
| `lesson` | **the point of the record** — the transferable idea, kept even when the option loses |
| `confidence` | `high` = verified in docs or measured here; `med` = reputable secondary source; `low` = single source |

## `scores.json`

Only the candidates that survive the hard filters get scored. Each criterion is
0–5, and each carries provenance:

- `measured` — the number comes from `out/raw-results.jsonl`, produced by this
  repo's benchmark on this machine
- `judged` — an assessment from documentation and reported behaviour, not
  measured here

A score of 0 means "structurally cannot do this", not "does it badly".

| score | reading |
|---|---|
| 5 | best in the set, by a clear margin |
| 4 | strong, no practical concern at Jod's scale |
| 3 | adequate; a known limit exists but is reachable only later |
| 2 | works, but you will feel it |
| 1 | technically possible, practically painful |
| 0 | structurally absent |

## `profiles.json`

`hard_filters` are applied first and are pass/fail. `profiles` are named weight
vectors; the ranking is a function of the weights, and the weights are data.
Changing them and re-running is the intended way to disagree with the result.

## `out/raw-results.jsonl`

One JSON object per benchmark run, appended by `bench/harness.py`.

| field | meaning |
|---|---|
| `db`, `workload`, `writers`, `readers` | what was run |
| `write.throughput_ops_s` | successful operations per second across all writers |
| `write.error_rate_pct` | share of attempted ops that raised |
| `write.p50_ms` / `p95_ms` / `p99_ms` | per-operation latency as the caller sees it |
| `read.*` | same shape, for reader processes in the `mixed` workload |
| `correctness.verdict` | `CORRECT`, `LOST UPDATES` or `LOST WRITES` |
| `correctness.lost_updates` | acknowledged operations that did not survive |
| `recall_at_10_pct` | vector workload: overlap with exact cosine top-10 |
| `index` | which index the engine actually used, as reported by the engine |

Latency is measured around the client call, so it includes driver and IPC
overhead. That is deliberate: it is the number the agent waits on.
