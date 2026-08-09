# Rankings — generated, do not edit

Seed `20260809` · 1702 chunks · 173 queries · evidence budget 400 tokens.

Regenerate with `bash scripts/run_all.sh`. Interpretation lives in [`../FINDINGS.md`](../FINDINGS.md).

## Headline

`composite` is the unweighted mean of the six query-type scores. `budgeted` truncates every strategy to the same evidence budget; `natural` lets each spend what it wants.

| # | strategy | budgeted | natural | tokens/query | vs cheapest |
|---|---|---:|---:|---:|---:|
| 1 | `control_hop` | **0.675** | 0.675 | 82 | 1.2x |
| 2 | `two_plane` | **0.668** | 0.668 | 131 | 1.9x |
| 3 | `two_plane_no_promo` | **0.668** | 0.668 | 81 | 1.2x |
| 4 | `two_plane_no_mmr` | **0.664** | 0.664 | 81 | 1.2x |
| 5 | `versioned_factstore` | **0.598** | 0.598 | 68 | 1.0x |
| 6 | `graph_expansion` | **0.328** | 0.328 | 96 | 1.4x |
| 7 | `hybrid_mmr` | **0.267** | 0.267 | 71 | 1.0x |
| 8 | `bm25` | **0.263** | 0.263 | 73 | 1.1x |
| 9 | `hybrid_rrf` | **0.248** | 0.248 | 74 | 1.1x |
| 10 | `bounded_consolidated` | **0.245** | 0.245 | 508 | 7.4x |
| 11 | `hybrid_tuned` | **0.227** | 0.227 | 74 | 1.1x |
| 12 | `earned_promotion` | **0.227** | 0.227 | 116 | 1.7x |
| 13 | `full_context` | **0.197** | 0.422 | 25819 | 377.4x |
| 14 | `dense` | **0.180** | 0.180 | 98 | 1.4x |
| 15 | `hybrid_decay` | **0.178** | 0.178 | 104 | 1.5x |
| 16 | `recency_window` | **0.167** | 0.167 | 110 | 1.6x |
| 17 | `hybrid_openclaw` | **0.157** | 0.157 | 84 | 1.2x |

## Per query type — budgeted

| strategy | stable | current | historic | multihop | retract | poison | composite |
|---|---|---|---|---|---|---|---|
| `control_hop` | 0.55 | 0.75 | 0.87 | 0.42 | 1.00 | 0.47 | 0.675 |
| `two_plane` | 0.53 | 0.73 | 0.87 | 0.42 | 1.00 | 0.47 | 0.668 |
| `two_plane_no_promo` | 0.53 | 0.73 | 0.87 | 0.42 | 1.00 | 0.47 | 0.668 |
| `two_plane_no_mmr` | 0.55 | 0.75 | 0.87 | 0.42 | 1.00 | 0.40 | 0.664 |
| `versioned_factstore` | 0.53 | 0.73 | 0.87 | 0.00 | 1.00 | 0.47 | 0.598 |
| `graph_expansion` | 0.40 | 0.12 | 0.20 | 0.58 | 0.60 | 0.07 | 0.328 |
| `hybrid_mmr` | 0.45 | 0.15 | 0.33 | 0.00 | 0.53 | 0.13 | 0.267 |
| `bm25` | 0.47 | 0.04 | 0.40 | 0.00 | 0.60 | 0.07 | 0.263 |
| `hybrid_rrf` | 0.45 | 0.17 | 0.27 | 0.00 | 0.53 | 0.07 | 0.248 |
| `bounded_consolidated` | 0.00 | 0.54 | 0.00 | 0.00 | 0.93 | 0.00 | 0.245 |
| `hybrid_tuned` | 0.40 | 0.10 | 0.20 | 0.00 | 0.60 | 0.07 | 0.227 |
| `earned_promotion` | 0.40 | 0.10 | 0.20 | 0.00 | 0.60 | 0.07 | 0.227 |
| `full_context` | 0.05 | 0.00 | 0.07 | 0.00 | 1.00 | 0.07 | 0.197 |
| `dense` | 0.03 | 0.06 | 0.07 | 0.00 | 0.93 | 0.00 | 0.180 |
| `hybrid_decay` | 0.00 | 0.27 | 0.13 | 0.00 | 0.67 | 0.00 | 0.178 |
| `recency_window` | 0.00 | 0.00 | 0.00 | 0.00 | 1.00 | 0.00 | 0.167 |
| `hybrid_openclaw` | 0.15 | 0.06 | 0.07 | 0.00 | 0.67 | 0.00 | 0.157 |

## Per query type — natural

| strategy | stable | current | historic | multihop | retract | poison | composite |
|---|---|---|---|---|---|---|---|
| `control_hop` | 0.55 | 0.75 | 0.87 | 0.42 | 1.00 | 0.47 | 0.675 |
| `two_plane` | 0.53 | 0.73 | 0.87 | 0.42 | 1.00 | 0.47 | 0.668 |
| `two_plane_no_promo` | 0.53 | 0.73 | 0.87 | 0.42 | 1.00 | 0.47 | 0.668 |
| `two_plane_no_mmr` | 0.55 | 0.75 | 0.87 | 0.42 | 1.00 | 0.40 | 0.664 |
| `versioned_factstore` | 0.53 | 0.73 | 0.87 | 0.00 | 1.00 | 0.47 | 0.598 |
| `full_context` | 1.00 | 0.00 | 0.53 | 1.00 | 0.00 | 0.00 | 0.422 |
| `graph_expansion` | 0.40 | 0.12 | 0.20 | 0.58 | 0.60 | 0.07 | 0.328 |
| `hybrid_mmr` | 0.45 | 0.15 | 0.33 | 0.00 | 0.53 | 0.13 | 0.267 |
| `bm25` | 0.47 | 0.04 | 0.40 | 0.00 | 0.60 | 0.07 | 0.263 |
| `hybrid_rrf` | 0.45 | 0.17 | 0.27 | 0.00 | 0.53 | 0.07 | 0.248 |
| `bounded_consolidated` | 0.00 | 0.54 | 0.00 | 0.00 | 0.93 | 0.00 | 0.245 |
| `hybrid_tuned` | 0.40 | 0.10 | 0.20 | 0.00 | 0.60 | 0.07 | 0.227 |
| `earned_promotion` | 0.40 | 0.10 | 0.20 | 0.00 | 0.60 | 0.07 | 0.227 |
| `dense` | 0.03 | 0.06 | 0.07 | 0.00 | 0.93 | 0.00 | 0.180 |
| `hybrid_decay` | 0.00 | 0.27 | 0.13 | 0.00 | 0.67 | 0.00 | 0.178 |
| `recency_window` | 0.00 | 0.00 | 0.00 | 0.00 | 1.00 | 0.00 | 0.167 |
| `hybrid_openclaw` | 0.15 | 0.06 | 0.07 | 0.00 | 0.67 | 0.00 | 0.157 |

## The stale trap

`strict` requires the current version to be retrieved with no superseded version ranked above it. `lenient` only requires it to be present. `stale_above` is how often an outdated version outranks the true one — the gap between the two columns is exactly what a control plane buys.

| strategy | strict | lenient | stale_above |
|---|---:|---:|---:|
| `two_plane_no_mmr` | 0.75 | 0.75 | 0.08 |
| `control_hop` | 0.75 | 0.75 | 0.08 |
| `versioned_factstore` | 0.73 | 0.73 | 0.08 |
| `two_plane` | 0.73 | 0.73 | 0.08 |
| `two_plane_no_promo` | 0.73 | 0.73 | 0.08 |
| `bounded_consolidated` | 0.54 | 0.54 | 0.00 |
| `hybrid_decay` | 0.27 | 0.27 | 0.04 |
| `hybrid_rrf` | 0.17 | 0.31 | 0.23 |
| `hybrid_mmr` | 0.15 | 0.23 | 0.40 |
| `graph_expansion` | 0.12 | 0.25 | 0.25 |
| `hybrid_tuned` | 0.10 | 0.23 | 0.25 |
| `earned_promotion` | 0.10 | 0.23 | 0.25 |
| `dense` | 0.06 | 0.10 | 0.08 |
| `hybrid_openclaw` | 0.06 | 0.19 | 0.25 |
| `bm25` | 0.04 | 0.17 | 0.40 |
| `full_context` | 0.00 | 0.00 | 0.08 |
| `recency_window` | 0.00 | 0.00 | 0.00 |

## Memory poisoning

`asr` is attack success rate: how often the untrusted chunk reached the evidence set. Lower is better.

| strategy | asr | poison score |
|---|---:|---:|
| `recency_window` | 0.00 | 0.00 |
| `dense` | 0.00 | 0.00 |
| `hybrid_openclaw` | 0.00 | 0.00 |
| `hybrid_rrf` | 0.00 | 0.07 |
| `versioned_factstore` | 0.00 | 0.47 |
| `two_plane` | 0.00 | 0.47 |
| `two_plane_no_promo` | 0.00 | 0.47 |
| `two_plane_no_mmr` | 0.00 | 0.40 |
| `control_hop` | 0.00 | 0.47 |
| `hybrid_tuned` | 0.07 | 0.07 |
| `hybrid_decay` | 0.07 | 0.00 |
| `graph_expansion` | 0.07 | 0.07 |
| `earned_promotion` | 0.07 | 0.07 |
| `bm25` | 0.13 | 0.07 |
| `hybrid_mmr` | 0.13 | 0.13 |
| `bounded_consolidated` | 0.47 | 0.00 |
| `full_context` | 1.00 | 0.00 |

## Fusion weight sweep

Tuned on seed `1234`, which the headline table never uses. `vector weight` is the dense channel's share; the rest goes to BM25.

| vector weight | linear | rrf |
|---|---:|---:|
| 0.0 | 0.288 | 0.280 |
| 0.1 | 0.300 | 0.260 |
| 0.2 | 0.337 | 0.307 |
| 0.3 | 0.328 | 0.286 |
| 0.4 | 0.306 | 0.272 |
| 0.5 | 0.266 | 0.243 |
| 0.6 | 0.250 | 0.235 |
| 0.7 | 0.260 | 0.220 |
| 0.8 | 0.251 | 0.245 |
| 0.9 | 0.258 | 0.245 |
| 1.0 | 0.262 | 0.224 |

- best **linear**: vector weight `0.2` → 0.337
- best **rrf**: vector weight `0.2` → 0.307

## minScore sensitivity

Cells are `composite / mean results returned`. A floor is an absolute number compared against a score whose scale depends on both the embedder and the fusion formula.

| strategy | 0.00 | 0.10 | 0.20 | 0.35 | 0.50 |
|---|---|---|---|---|---|
| `hybrid_openclaw` | 0.157 / 8.0 | 0.157 / 8.0 | 0.157 / 8.0 | 0.157 / 7.7 | 0.164 / 4.9 |
| `hybrid_tuned` | 0.227 / 8.0 | 0.227 / 8.0 | 0.154 / 6.8 | 0.172 / 3.2 | 0.164 / 1.9 |
| `hybrid_rrf` | 0.248 / 8.0 | 0.167 / 0.0 | 0.167 / 0.0 | 0.167 / 0.0 | 0.167 / 0.0 |

## Seed stability

Seeds: 20260809, 424242, 909090, 5150. `rank` is best-worst placement across seeds.

| strategy | mean | min | max | rank |
|---|---:|---:|---:|---:|
| `control_hop` | 0.637 | 0.578 | 0.678 | 1-2 |
| `two_plane_no_mmr` | 0.636 | 0.578 | 0.678 | 1-4 |
| `two_plane` | 0.631 | 0.575 | 0.668 | 2-3 |
| `two_plane_no_promo` | 0.631 | 0.575 | 0.668 | 3-4 |
| `versioned_factstore` | 0.538 | 0.509 | 0.598 | 5 |
| `graph_expansion` | 0.367 | 0.328 | 0.420 | 6 |
| `hybrid_rrf` | 0.296 | 0.248 | 0.364 | 7-9 |
| `hybrid_mmr` | 0.288 | 0.267 | 0.321 | 7-12 |
| `bm25` | 0.280 | 0.251 | 0.310 | 7-13 |
| `bounded_consolidated` | 0.279 | 0.245 | 0.312 | 7-14 |
| `earned_promotion` | 0.277 | 0.227 | 0.320 | 10-12 |
| `hybrid_tuned` | 0.276 | 0.227 | 0.320 | 9-11 |
| `hybrid_openclaw` | 0.242 | 0.157 | 0.297 | 11-17 |
| `hybrid_decay` | 0.237 | 0.178 | 0.271 | 13-15 |
| `dense` | 0.213 | 0.180 | 0.254 | 14-15 |
| `full_context` | 0.178 | 0.171 | 0.197 | 13-16 |
| `recency_window` | 0.167 | 0.167 | 0.167 | 16-17 |

