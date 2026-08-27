# Dataset schema

## `candidates.json`

One record per way of buying model capacity. 69 records. A few records cover a
family of near-identical vendors, because the transferable lesson is the same
for all of them — `gpu-host-commodity` stands for Parasail, GMICloud,
AtlasCloud, Ionstream, StreamLake, DigitalOcean Gradient and CoreWeave, which
resell the same open weights within about 2x of each other.

| field | meaning |
|---|---|
| `id` | stable slug, used to join against `scores.json` and `gates.json` |
| `kind` | what the thing is: `lab-subscription`, `aggregator-subscription`, `gpu-host`, `permissive-host`, `ide-subscription`, `model-family`, `self-host`, `benchmark`, … |
| `access` | `subscription`, `payg`, `both`, `oauth-locked` (bound to the vendor's own client), `ide-locked`, `local` |
| `plan_floor_usd` / `plan_top_usd` | cheapest and dearest flat monthly tier; `null` when there is no flat plan |
| `quota` | the limit **as the vendor states it**, verbatim where possible. The unit matters more than the number and the report argues why |
| `quota_multiple` | dollars of provider-rate value returned per dollar of subscription, where the vendor publishes enough to compute it; `null` otherwise |
| `payg_in` / `payg_out` | USD per 1M tokens for the flagship, overflow rate for subscription vendors |
| `best_model` | the strongest model the plan or endpoint unlocks |
| `context` / `max_output` | tokens. `max_output` is the more discriminating of the two and is routinely capped far below the model's real ceiling by budget hosts |
| `tools` | whether the best model supports tool/function calling. This single boolean eliminates most of the uncensored-hosting market |
| `moderation` | see the vocabulary block in `_meta`: `none`, `model-only`, `configurable`, `provider-layer`, `strict`, `cn-political` |
| `harness` | `anthropic+openai`, `openai`, `oauth-only`, `ide-only`, `local` |
| `web_tools` | native retrieval, and whether the plan includes it or bills it separately |
| `retention` | data retention and training policy as published |
| `disqualifiers` | concrete reasons it fails; empty list if none |
| `lesson` | **the point of the record** — the transferable idea, kept even when the option loses |
| `confidence` | `high` = read on the vendor's own page or computed from its published rates; `med` = reputable secondary source; `low` = single source or vendor claim only |

## `gates.json`

Pass/fail against the four hard filters, applied to all 69. `fails` lists every
filter missed; empty means the candidate reaches scoring. Hard filters run
**before** scoring, because a weighted average otherwise lets a brilliant model
paper over "the quota is bound to a client you cannot use".

## `scores.json`

Only the 20-option shortlist is scored. Each criterion is 0–5 and carries
provenance:

- `documented` — the number is on the vendor's own pricing or docs page, read on 2026-08-27
- `derived` — computed here from published rates
- `judged` — an assessment from documentation, third-party measurement and reported behaviour

There is no `measured` value in this dataset, and that is the honest difference
between this study and [`research/agent-db-2026`](../../agent-db-2026), which
ran a real benchmark. See "How much to trust this" in `REPORT.md`.

`tier` `A` passed every hard filter. `tier` `B` is pay-as-you-go, carried
forward because the recommendation uses it as overflow capacity. Tier B entries
are scored under the same rubric with two fixed values, which is simply what
pay-as-you-go means: `quota_value` 1 (you pay face value) and `quota_ceiling` 5
(there is no ceiling).

| score | reading |
|---|---|
| 5 | best in the set, by a clear margin |
| 4 | strong, no practical concern at this scale |
| 3 | adequate; a known limit exists but is reachable only later |
| 2 | works, but you will feel it |
| 1 | technically possible, practically painful |
| 0 | structurally absent |

## `profiles.json`

`hard_filters` are applied first and are pass/fail. `profiles` are named weight
vectors. The ranking is a function of the weights, and the weights are data;
changing them and re-running is the intended way to disagree with the result.
