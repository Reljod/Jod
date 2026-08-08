# Dataset schema — `providers.json`

Every candidate is one flat record. Flat beats nested here: it maps 1:1 to a CSV
row, and a typo in a deeply nested blob is much harder to spot in review.

## Identity

| field | type | meaning |
|---|---|---|
| `id` | string | stable slug, used as the join key everywhere |
| `name` | string | display name |
| `hq` | string | company home country |
| `jurisdiction` | string | legal regime the *servers* sit under (may differ from `hq`) |
| `category` | string | `mainstream` · `eu-budget` · `lowend` · `offshore` · `hyperscaler` · `paas` |

## Reference plan

All providers are priced at the **same reference spec** so the comparison is
apples-to-apples: the cheapest plan meeting **≥2 vCPU, ≥4 GB RAM, ≥40 GB
SSD/NVMe, KVM (or better)**. That is Jod's realistic floor — orchestrator
process, a few MCP servers, and one or two headless Chrome instances.

| field | type | meaning |
|---|---|---|
| `plan_name` | string | the specific plan priced |
| `vcpu`, `ram_gb`, `disk_gb` | number | reference plan resources |
| `disk_type` | string | `NVMe` · `SSD` · `HDD` |
| `traffic_tb` | number | included egress per month; `9999` = unmetered |
| `port_gbps` | number | uplink speed |
| `price` / `currency` | number/string | native list price, monthly |
| `term` | string | what unlocks that price: `monthly` · `annual` · `promo-annual` |
| `virt` | string | `KVM` · `LXC` · `OpenVZ` · `container` · `bare-metal` |

## Can you actually buy it

The first version of this dataset scored advertised prices and never asked
whether the plan was orderable. Two of its top three picks turned out to be
unbuyable, so these three fields exist and `stock` is a **hard filter**, not a
score.

| field | type | meaning |
|---|---|---|
| `stock` | string | `in` · `partial` (some locations/sizes) · `out` · `unknown` (never checked) |
| `price_basis` | string | what the price *is*: `standing` (published catalogue), `promo` (time-limited SKU), `advertised-headline` (a "starting at" banner, not a plan), `unknown` |
| `stock_checked` | string/null | ISO date the order path was last looked at |

**`price_basis` is the field that catches the real failure mode.** A promo price
is not a lie, but it is not a price you can rely on either: promo SKUs sell out,
expire, and often carry different specs from the standing plan of the same name.
Rank on `standing` and record the promo in `notes`. Where the two diverge
sharply — RackNerd is $3.62/mo on promo and $24.59/mo standing — the gap *is*
the finding.

`unknown` passes the filter deliberately. Most rows here have never had their
order page opened, and dropping them all would distort the ranking far more than
admitting the gap does. Run `scripts/check_stock.py` to shrink it.

## Location & network

| field | type | meaning |
|---|---|---|
| `loc_count` | number | distinct datacenter locations |
| `regions` | array | continents served: `EU` `NA` `SA` `APAC` `ME` `AF` `OC` |
| `cities` | array | representative sites (not exhaustive) |
| `ipv6`, `ipv4_incl` | bool | address availability |
| `net_quality` | 1–5 | transit blend quality — 5 = strong tier-1 mix, low jitter |
| `ip_rep` | 1–5 | **1 = clean, 5 = heavily blocklisted.** How often the provider's ranges trip Cloudflare / DataDome / anti-bot. Lower is better; this is inverted in scoring. |

## Availability

| field | type | meaning |
|---|---|---|
| `sla_pct` | number | contractual SLA; `0` = none published |
| `uptime_rep` | 1–5 | observed real-world reliability reputation, 5 = best |
| `steal_risk` | 1–5 | CPU oversubscription risk, **1 = none, 5 = heavily oversold** (inverted) |
| `cpu_perf` | 1–5 | relative single-core performance, 5 = best |

## Permissiveness — "free to do anything"

| field | type | meaning |
|---|---|---|
| `aup_strict` | 1–5 | **1 = anything goes, 5 = suspends on the first complaint** (inverted) |
| `kyc` | 1–5 | **1 = email only, 5 = government ID + proof of address** (inverted) |
| `dmca` | string | `ignored` · `forwarded` · `responsive` · `strict` |
| `crypto` | bool | accepts cryptocurrency |
| `abuse_style` | string | how they handle a complaint in practice |

## Operations

`api`, `snapshots`, `hourly`, `docker_ok` — all bool. `compliance` is an array of
certifications (ISO 27001, SOC 2, GDPR, PCI-DSS…).

## Provenance — the part that matters

| field | type | meaning |
|---|---|---|
| `confidence` | `high` · `medium` · `low` | how much to trust *this row's* pricing and policy fields |
| `sources` | array | URLs backing the row |
| `notes` | string | anything a score can't express |
| `flags` | array | disqualifiers and hazards: `sanctions-risk`, `stock-limited`, `renewal-hike`, `no-sla`, `excluded:<reason>` |

`confidence` is not decoration. `low` means the price is a plausible market
figure I could not confirm against a live pricing page, and the report is
required to say so next to the number.
