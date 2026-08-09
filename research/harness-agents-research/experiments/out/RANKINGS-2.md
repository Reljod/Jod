# Round 2 rankings — generated, do not edit

Seed `20260809` · 1149 chunks · 171 queries · workspaces `ws-alpha`, `ws-beta` · evidence budget 400 tokens.

Regenerate with `bash scripts/run_all.sh`. Interpretation lives in [`../FINDINGS-2.md`](../FINDINGS-2.md).

## Headline

`leak` = evidence set contained any chunk from another workspace. `leakF` = it contained another workspace's *fact*. `wrong` = the top-ranked item was a confidently wrong answer.

| # | strategy | composite | leak | leakF | wrong | tokens |
|---|---|---:|---:|---:|---:|---:|
| 1 | `redact_history` | **0.756** | 0.00 | 0.00 | 0.02 | 90 |
| 2 | `control_scoped` | **0.695** | 0.00 | 0.00 | 0.06 | 90 |
| 3 | `abstain_ambiguous` | **0.695** | 0.00 | 0.00 | 0.06 | 90 |
| 4 | `authored_core` | **0.694** | 0.00 | 0.00 | 0.06 | 204 |
| 5 | `append_supersede` | **0.677** | 0.00 | 0.00 | 0.06 | 71 |
| 6 | `codex_grep` | **0.602** | 0.00 | 0.00 | 0.05 | 390 |
| 7 | `rewrite_inplace` | **0.571** | 0.00 | 0.00 | 0.06 | 72 |
| 8 | `bounded_importance` | **0.513** | 0.00 | 0.00 | 0.03 | 263 |
| 9 | `bounded_unrecalled` | **0.479** | 0.00 | 0.00 | 0.02 | 265 |
| 10 | `hybrid_scope_filter` | **0.476** | 0.00 | 0.00 | 0.14 | 88 |
| 11 | `hybrid_scope_boost` | **0.432** | 0.99 | 0.79 | 0.39 | 87 |
| 12 | `hybrid_noscope` | **0.388** | 1.00 | 0.80 | 0.38 | 85 |
| 13 | `bounded_lru` | **0.360** | 0.00 | 0.00 | 0.04 | 256 |
| 14 | `abstain_on_noscope` | **0.304** | 0.41 | 0.29 | 0.16 | 35 |

## Per query type (budgeted)

| strategy | stable | current | historic | authored | retract | redacted | poison | composite |
|---|---|---|---|---|---|---|---|---|
| `redact_history` | 0.68 | 0.68 | 0.73 | 0.56 | 0.88 | 0.94 | 0.83 | 0.756 |
| `control_scoped` | 0.68 | 0.68 | 0.80 | 0.56 | 0.88 | 0.44 | 0.83 | 0.695 |
| `abstain_ambiguous` | 0.68 | 0.68 | 0.80 | 0.56 | 0.88 | 0.44 | 0.83 | 0.695 |
| `authored_core` | 0.60 | 0.62 | 0.80 | 0.94 | 1.00 | 0.31 | 0.58 | 0.694 |
| `append_supersede` | 0.68 | 0.68 | 0.80 | 0.44 | 0.88 | 0.44 | 0.83 | 0.677 |
| `codex_grep` | 0.53 | 0.62 | 0.07 | 1.00 | 0.81 | 0.69 | 0.50 | 0.602 |
| `rewrite_inplace` | 0.68 | 0.68 | 0.00 | 0.44 | 0.88 | 0.50 | 0.83 | 0.571 |
| `bounded_importance` | 0.30 | 0.39 | 0.00 | 0.81 | 1.00 | 1.00 | 0.08 | 0.513 |
| `bounded_unrecalled` | 0.30 | 0.43 | 0.00 | 0.62 | 1.00 | 1.00 | 0.00 | 0.479 |
| `hybrid_scope_filter` | 0.45 | 0.18 | 0.20 | 0.69 | 0.56 | 0.50 | 0.75 | 0.476 |
| `hybrid_scope_boost` | 0.28 | 0.18 | 0.13 | 0.62 | 0.69 | 0.62 | 0.50 | 0.432 |
| `hybrid_noscope` | 0.25 | 0.14 | 0.13 | 0.56 | 0.75 | 0.62 | 0.25 | 0.388 |
| `bounded_lru` | 0.12 | 0.39 | 0.00 | 0.00 | 1.00 | 1.00 | 0.00 | 0.360 |
| `abstain_on_noscope` | 0.15 | 0.07 | 0.13 | 0.00 | 0.88 | 0.81 | 0.08 | 0.304 |

## Attack success rate vs evidence width

Does 0% attack success without a defence survive a wider evidence set?

| k | no admission (`hybrid_scope_filter`) | write-time admission (`control_scoped`) |
|---|---:|---:|
| 4 | 0.17 | 0.00 |
| 8 | 0.17 | 0.00 |
| 16 | 0.17 | 0.00 |
| 32 | 0.17 | 0.00 |
| 64 | 0.25 | 0.00 |

## Seed stability

Seeds: 20260809, 606060, 771177. `rank` is best-worst placement across seeds.

| strategy | mean | min | max | rank |
|---|---:|---:|---:|---:|
| `redact_history` | 0.722 | 0.675 | 0.756 | 1-2 |
| `authored_core` | 0.689 | 0.662 | 0.711 | 1-4 |
| `abstain_ambiguous` | 0.636 | 0.586 | 0.695 | 3-4 |
| `control_scoped` | 0.636 | 0.586 | 0.695 | 2-3 |
| `append_supersede` | 0.617 | 0.568 | 0.677 | 5 |
| `codex_grep` | 0.577 | 0.555 | 0.602 | 6 |
| `rewrite_inplace` | 0.523 | 0.463 | 0.571 | 7-8 |
| `bounded_importance` | 0.515 | 0.488 | 0.544 | 7-8 |
| `bounded_unrecalled` | 0.462 | 0.438 | 0.479 | 9 |
| `hybrid_scope_filter` | 0.418 | 0.357 | 0.476 | 10-12 |
| `hybrid_scope_boost` | 0.413 | 0.389 | 0.432 | 11 |
| `bounded_lru` | 0.395 | 0.360 | 0.416 | 10-13 |
| `hybrid_noscope` | 0.385 | 0.350 | 0.416 | 12-13 |
| `abstain_on_noscope` | 0.322 | 0.304 | 0.351 | 14 |

