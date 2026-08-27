# Which model subscription should Jod run on

**Question.** Two jobs: an LLM that browses the web at volume, and a Claude Code
replacement for coding at limits comparable to a Max plan. It should be cheap,
and more importantly it should have as few guardrails as possible. Flat-rate
subscriptions with good limits are the priority, benchmarked against OpenCode.

**Answer.** **Run two flat plans side by side: OpenCode Go at $10/month as the
daily driver, and the Z.ai GLM Coding Plan at $18/month for the web job and as
the second opinion. Put a Z.ai pay-as-you-go key behind both as overflow.**
That is $28/month standing, it clears both jobs, and neither vendor runs a
moderation layer over your requests.

OpenCode Go finished in the top three under **all five** weightings, which no
other option managed. The GLM Coding Plan finished top three under four of five.
They are also complementary rather than redundant: Go has the deeper model bench
and the honest quota unit, GLM has the bundled web search and page reading that
Go completely lacks.

> 69 options surveyed → 4 hard filters → 15 survivors → 20 scored (15 flat-rate
> plus 5 pay-as-you-go carried forward as overflow) → ranked under 5 weightings.
> Full tables in [`out/RANKINGS.md`](out/RANKINGS.md).

The uncomfortable part of the answer is in §3: **almost nothing that markets
itself as an uncensored LLM can do either of these jobs**, and the genuinely
permissive options that can are ordinary Chinese frontier labs that nobody
describes as uncensored.

---

## 1. Why this question exists at all in 2026

It exists because of a specific date. On **9 January 2026** Anthropic began
rejecting subscription OAuth tokens used outside its own client, with the error
`This credential is only authorized for use with Claude Code`. Anthropic's
position is that Free, Pro and Max tokens are for Claude Code and Claude.ai
only, and that using them with any other product — explicitly including the
Agent SDK — violates the consumer terms.

That is not a pricing change, it is a change in what a subscription *is*. A Max
plan is no longer capacity you own and can point at whatever harness you like.
It is capacity you may spend inside one program. OpenCode Go exists because of
this; it was launched as a $10/month open-weight month-pass after the block cut
OpenCode off from subscription Claude.

This single fact eliminates more shortlist candidates than any other filter.
Claude Max, ChatGPT Pro's Codex quota, Google AI Ultra, Copilot, Cursor,
Windsurf, JetBrains AI, Amazon Q and Warp all fail the same way: the capacity
cannot leave the vendor's client. API keys still work everywhere — but an API
key is pay-as-you-go, which is exactly what the brief asked to avoid.

So the honest framing is: **you are not looking for a cheaper Claude Max. You
are looking for the thing that replaced the category Claude Max used to be in.**

---

## 2. The metric that actually separates these plans

Every vendor publishes a limit and almost every vendor publishes it in a
different unit. Collected from their own pages:

| vendor | unit the limit is sold in |
|---|---|
| OpenCode Go | dollars of provider-rate value ($12/5h, $30/week, $60/month) |
| Cerebras Code | tokens per day (24M / 120M) |
| MiniMax | requests per 5 hours (1,500 → 30,000) |
| Z.ai GLM | credits per week (10,000 / 60,000 / 140,000) |
| Synthetic | messages per 5 hours (500) |
| Chutes | a 5x multiple of pay-as-you-go value |
| Kimi Code | agent credits plus concurrent task count |
| Xiaomi MiMo | credits, plus an estimated task count |
| Claude Max | a multiplier of Pro, with no published token figure at all |

These are not comparable, and that is not an accident. The only unit you can
verify before paying is **dollars of provider-rate value per dollar of
subscription**, because it can be reconstructed from the vendor's own token
rates. Where it can be computed:

| plan | monthly fee | value returned | multiple |
|---|---|---|---|
| **OpenCode Go** | $10 | $60 | **6.0x** |
| Chutes Plus / Pro | $10 / $20 | $50 / $100 | 5.0x |
| LLM Gateway DevPass | $29 / $179 | ~$87 / ~$537 | ~3.0x |
| Cursor Ultra | $200 | ~$400 | 2.0x |
| Nous Portal Plus | $20 | $22 | 1.1x |
| Venice Pro | $18 | $1 of API credit | 0.06x |

Two things fall out of this table.

**Venice's headline is about a different product.** Pro at $18 advertises
unlimited text prompts, and that is true — of the chat app. The API side of the
same subscription includes 100 credits, and 100 credits is one dollar. This is
the most commonly misread price in the survey and it is worth stating plainly
before recommending Venice for anything.

**Chutes told everyone why request-count plans die.** In February 2026 it
abandoned token-agnostic request pricing and replaced it with an explicit 5x
value cap, saying directly that the old model was not viable. Every plan in this
survey still quoting a flat request count — MiniMax, StepFun, Synthetic — is
running the model Chutes just proved doesn't survive contact with agents.

### The burst rate is not the budget

The most important arithmetic in this report, and the one every plan's marketing
obscures. OpenCode Go advertises **$12 per rolling 5-hour window**. A week
contains 33.6 such windows. Spending $12 in every one would cost $403/week. The
weekly cap is **$30**.

So the $12 figure is a burst allowance you can reach **2.5 times a week**. If
you code eight hours a day, five days a week — eight windows — your real budget
is $30 ÷ 8 = **$3.75 per window**, not $12.

The same shape applies to GLM (a ~80 prompts/5h burst against a ~400/week total
on Lite) and to Kimi Code (a 5-hour window that refreshes on a 7-day cycle).
**Plan against the weekly number. The 5-hour number tells you how fast you may
go, not how much you get.**

### What that means in requests per week

OpenCode Go is the only vendor that publishes this directly, which is itself the
strongest argument for it:

| model on OpenCode Go | requests per 5h | per week |
|---|---|---|
| DeepSeek V4 Flash | 31,650 | 79,050 |
| MiMo-V2.5 | 30,100 | 75,200 |
| Hy3 | 4,300 | 10,750 |
| DeepSeek V4 Pro | 3,450 | 8,550 |
| MiniMax M3 | 3,200 | 8,000 |
| GLM-5.2 | 880 | 2,150 |
| Qwen3.8 Max | 160 | 400 |
| Grok 4.5 | 120 | 300 |
| Kimi K3 | 110 | 250 |

Set that against the GLM Coding Plan, whose Lite tier is roughly 400 prompts a
week, Pro roughly 2,400 and Max roughly 5,600:

> **OpenCode Go at $10/month delivers about 2,150 GLM-5.2 requests a week.
> The GLM Pro plan at $80/month delivers about 2,400 GLM-5.3 requests a week.**
> Roughly 90% of the volume for 12.5% of the price.

That comparison is the core finding of this report. It is not quite an
apples-to-apples result — GLM Pro serves the newer 5.3, and it bundles web
tooling Go does not have — but a 8x price gap is not explained by half a version
number.

The catch, and it is a real one: on Go, the top-end models are *thin*. 250 Kimi
K3 requests a week is perhaps two serious sessions. Go is excellent at mid-tier
volume and rationed at the frontier. That is precisely why the recommendation
pairs it with a second plan rather than treating it as sufficient.

---

## 3. What "minimal guardrails" turns out to mean

This is where the research changed its own mind.

The intuitive move is to search for uncensored model hosting, and that market is
large and easy to find — Featherless with a 40,000-model catalogue and dedicated
`uncensored` and `abliterated` filters, Arli, Infermatic, uncensored.chat,
abliteration.ai, unfil.ai, plus the fine-tune families everyone means by the
word: TheDrummer, Sao10K's Euryale, Anthracite's Magnum, MythoMax.

Measured against OpenRouter's own catalogue metadata, here is what that market
can do:

| uncensored fine-tune | context | tool calling |
|---|---|---|
| TheDrummer Cydonia 24B v4.1 | 131K | **no** |
| Dolphin Mistral 24B Venice | 128K | **no** |
| TheDrummer Skyfall 36B v2 | 32K | **no** |
| Sao10K Euryale 70B (L3.3) | 131K | **no** |
| Anthracite Magnum v4 72B | 32K | **no** |
| Nous Hermes 4 405B | 131K | **no** |
| Gryphe MythoMax L2 13B | 8K | **no** |
| Undi95 ReMM SLERP 13B | 6K | **no** |

**Essentially none of it can call a tool.** A model that cannot call a tool
cannot browse the web and cannot edit a repository, however little it refuses.
The entire self-described uncensored segment fails the brief on capability, not
on ethics or legality — it is simply built for a different job, which is
conversation and roleplay.

Meanwhile, the actual moderation landscape looks like this. Of OpenRouter's 417
catalogued models, **291 are served with no provider moderation layer** — and
that unmoderated majority is where all the good agentic models live:

| | tool-capable | not tool-capable |
|---|---|---|
| **moderated** | 112 | 9 |
| **unmoderated** | **224** | 60 |

So the finding inverts the premise:

> **Low-guardrail, tool-capable, long-context model access is not a niche you
> have to hunt for. It is the mainstream.** GLM-5.3, Kimi K3, DeepSeek V4,
> Qwen3.8, MiniMax M3 and Grok 4.6 are all served without a provider-side
> classifier. The models with a filter bolted on top are the minority, and they
> are almost entirely OpenAI's and Anthropic's.

### Four different things get called a guardrail

Separating them matters, because only two of the four are worth optimising away.

1. **A provider-side classifier** that inspects requests and responses and can
   block them. OpenAI, Anthropic, Bedrock and the enterprise gateways run one.
   The Chinese labs, xAI and every open-weight host do not. *This is the one
   worth avoiding, because you cannot see it, tune it, or appeal it.*
2. **The model's own refusal training.** Present everywhere, varies enormously.
   A published safety audit across 19 frontier models found strict refusal rates
   spanning **0.1% to 94.6% on identical prompts**, with DeepSeek and Qwen
   characterised as permissive ecosystems that preserve helpfulness, and Llama
   as a conservative one that over-refuses.
3. **Chinese political filtering.** Real, documented, and confined to topics
   like Tiananmen, Taiwan and the Uyghurs. For writing code and scraping web
   pages it is irrelevant. This is why `cn-political` scores nearly as well as
   `none` in the ranking, and the report says so rather than hiding it in a
   weight.
4. **Account and policy enforcement.** Terms of service, org verification, usage
   review. Note that Google explicitly warns that applications using less
   restrictive safety settings *may be subject to review* — a loosened filter is
   not the same as an absent policy.

Google is the interesting outlier on point 1: Gemini is the only major Western
model whose filters the developer can lower per request, across four harm
categories, on a five-step scale that goes down to `OFF`. Only child-safety
protections are fixed. That control lives on the API, not the subscription —
which is the recurring shape of this whole market: **the permissive knob and the
flat rate are sold separately.**

### The one vendor doing something genuinely different

Nous Research is the only vendor here whose flagship treats low refusal as a
*training objective* rather than as the absence of a filter. Hermes 4 is
explicitly trained for steerability, lower refusal rates and neutral,
user-directed behaviour. Nous Portal also ships the only first-party browser
automation gateway in the survey — search, browsing, code sandboxes — which is
directly on target for the web job.

It still doesn't win, for two honest reasons: the credits are near face value
(1.1x, so you're buying the gateway rather than capacity), and Hermes 4 is a
Llama-3.1-derived model well below the current coding frontier, with its
OpenRouter endpoints advertising no tool support. It is the most philosophically
aligned option in the survey and the one whose model is furthest behind. Worth
$20 to keep an eye on; not worth building on yet.

---

## 4. The price picture

The mainstream floor for a capable API is now about **$0.20 per million input
tokens**, and the frontier brands have moved down to meet it — GPT-5.6 Luna sits
at $0.20/$1.20 with a 1.05M context after OpenAI's July 2026 cut.

Prices that matter for these two jobs, from vendor pages on 2026-08-27:

| model | in $/M | cache read | out $/M | context | max out | moderated |
|---|---|---|---|---|---|---|
| inclusionAI Ling 3.0 Flash | 0.021 | — | 0.063 | 262K | 32K | no |
| Qwen3.7 Flash | 0.030 | — | 0.130 | 1.00M | 65K | no |
| DeepSeek V4 Flash | 0.080 | 0.007 | 0.159 | 1.05M | 384K | no |
| **GLM-5.3-Flash** | **0.075** | 0.015 | **0.250** | **1.31M** | 131K | no |
| Poolside Laguna-S 2.1 | 0.090 | — | 0.180 | 1.05M | 131K | no |
| Xiaomi MiMo-V2.5 | 0.140 | 0.003 | 0.280 | 1.05M | 131K | no |
| GPT-5.6 Luna | 0.200 | 0.020 | 1.200 | 1.05M | 128K | **yes** |
| MiniMax M3 | 0.300 | 0.060 | 1.200 | 1.05M | 512K | no |
| Gemini 3.7 Flash | 0.375 | 0.037 | 1.880 | 1.05M | 65K | configurable |
| DeepSeek V4 Pro | 0.870 | 0.072 | 1.740 | 1.05M | 384K | no |
| **GLM-5.3** | **1.400** | 0.260 | **4.400** | 1.05M | 131K | no |
| Grok 4.6 | 2.000 | 0.500 | 6.000 | 500K | 450K | no |
| Qwen3.8 Max | 2.000 | 0.250 | 6.000 | 1.00M | 131K | no |
| Kimi K3 | 3.000 | 0.300 | 15.000 | 1.05M | 943K | no |
| Claude Sonnet 5 | 2.000 | 0.200 | 10.000 | 1.00M | 128K | **yes** |
| Claude Opus 5 | 5.000 | 0.500 | 25.000 | 1.00M | 128K | **yes** |
| Claude Fable 5 | 10.000 | 1.000 | 50.000 | 1.00M | 128K | no |

Three observations worth carrying:

**GLM-5.3-Flash is the best price-to-context ratio anywhere** — $0.075 input
against a 1.31M window, the largest context in the survey. For the web job,
where you are stuffing scraped pages into a prompt and asking simple questions,
this is close to unbeatable, and Z.ai additionally serves GLM-4.7-Flash and
GLM-4.5-Flash for free.

**Kimi K3 is a trap at pay-as-you-go rates.** At $3/$15 it is the most expensive
open-weight model here — dearer per token than Claude Sonnet 5. If you want K3,
buy it inside a subscription; paying list price for it costs more than paying
Anthropic.

**Caching is where the real money is, and only Anthropic makes it count twice.**
Most vendors discount cache reads by 80–95%. Anthropic goes further and excludes
cache reads from the rate limit entirely, so at an 80% hit rate a 2M
input-tokens-per-minute limit passes 10M tokens/minute. No Chinese lab matches
this. It is the single best piece of limit design in the survey and it belongs
to the vendor this report is recommending against.

### The same model is not the same model

Pulled from OpenRouter's endpoint data for **DeepSeek V4 Pro**, one model across
seventeen hosts:

| host | in $/M | out $/M | max out | quantisation | 30-day uptime |
|---|---|---|---|---|---|
| GMICloud | 0.79 | 2.38 | 943K | fp8 | 99.16% |
| StreamLake | 0.87 | 1.74 | 384K | fp8 | 99.99% |
| DigitalOcean | 0.87 | 1.74 | 943K | unknown | 99.89% |
| Ionstream | 1.13 | 2.26 | 393K | **fp4** | 99.99% |
| DeepInfra | 1.30 | 2.60 | **16K** | fp8 | 99.57% |
| Together | 1.74 | 3.48 | 460K | unknown | **92.62%** |
| Fireworks | 1.74 | 3.48 | 943K | unknown | — |
| Azure | 1.91 | 3.83 | 384K | unknown | 98.63% |

A **2.4x price spread**, quantisation ranging from fp4 to fp8, max output
ranging from 16K to 943K, and uptime from 92.6% to 100% — all for a model with
one name. Routing to the cheapest endpoint is silently a quality decision. Note
in particular that DeepInfra is among the cheapest and caps output at 16K, which
would quietly break long agent turns, and that Together is both the most
expensive of the mid-tier and the least available.

This is the strongest argument for OpenRouter as an instrument even if you buy
elsewhere: it is the only place that publishes `is_moderated`, quantisation and
uptime per endpoint, and it exposes all of it through a public API with no auth.

---

## 5. The recommendation

### Buy this

| # | what | cost | why |
|---|---|---|---|
| 1 | **OpenCode Go** | **$10/mo** | 6x value multiple, the only honest quota unit, 18 models including Kimi K3 and Grok 4.5, Anthropic-compatible, top-3 under every weighting |
| 2 | **Z.ai GLM Coding Plan (Lite)** | **$18/mo** | GLM-5.3 at frontier coding quality, and the only plan that bundles web search and page reading at no extra cost |
| 3 | **Z.ai pay-as-you-go key** | $0 standing | overflow when either weekly cap lands; GLM-5.3-Flash at $0.075/$0.25 with a 1.31M context, and two Flash models free |

**$28/month standing.** Both plans speak the Anthropic API, so both drop into
Claude Code, OpenCode, Cline, Roo or Kilo by changing a base URL. Neither vendor
runs a moderation layer over your requests.

### Assign the jobs like this

- **Web browsing at volume** → GLM Coding Plan. Its bundled MCP search and page
  reading is the only native retrieval included in a flat fee anywhere in this
  survey. Everyone else bills retrieval separately: Anthropic $10 per thousand
  searches, Google $14 per thousand after a 5,000/month free allowance, Venice
  $10 per thousand searches and $10 per thousand URLs scraped. Drop to
  GLM-5.3-Flash on the pay-as-you-go key for bulk page-stuffing.
- **Coding** → OpenCode Go, defaulting to GLM-5.2 or DeepSeek V4 Pro for
  ordinary work and spending the thin Kimi K3 allowance (250/week) only on hard
  problems. When the $30 weekly cap lands, fall through to the GLM plan.

### If the budget is bigger

Only one single plan plausibly matches Claude Max 20x on volume at
frontier-adjacent quality: **GLM Coding Plan Max at $168/month**, roughly 5,600
prompts a week against Max 20x's $200. It is 16% cheaper than Anthropic and it
is portable across harnesses, which the Anthropic plan no longer is. If
throughput matters more than intelligence, **Cerebras Code Max at $200** buys
120M tokens a day at ~2,000 tokens/second — the largest published ceiling in the
survey — but on a 131K context and a 32K output cap, and on Qwen3-Coder rather
than a frontier model.

### Do not buy

- **Anything IDE-locked** (Copilot, Cursor, Windsurf, JetBrains, Amazon Q, Warp).
  The capacity cannot leave the vendor's client, which fails the brief outright.
- **Gateways that resell frontier models under a flat rate** (DevPass, Factory
  Droid). They inherit the upstream's refusal behaviour exactly. A wrapper
  cannot be more permissive than what it wraps, and this is the single most
  common way to spend money and get nothing on the guardrail axis.
- **The uncensored-hosting segment** (Featherless, Arli, Infermatic,
  uncensored.chat, abliteration.ai, Wiro). No tool calling, short contexts, and
  in several cases neither limits nor retention are published.
- **Venice as a coding plan.** Excellent privacy story — local-only storage,
  zero retention in private mode, hardware-attested TEE, end-to-end encryption —
  and the API entitlement on Pro is one dollar. Buy it for the privacy or the
  metered scraping, not for volume.
- **Self-hosting, at this scale.** One rented 96GB card is about $1,300/month
  continuous, more than the most expensive subscription here, and still will not
  hold a 2.8T-parameter model.

---

## 6. Model limits, for reference

The numbers most likely to bite, since context is usually quoted and max output
usually is not:

| model | context | max output | tools | notes |
|---|---|---|---|---|
| GLM-5.3-Flash | **1,310,720** | 131,072 | yes | largest context in the survey |
| Kimi K3 | 1,048,576 | **943,718** | yes | largest output ceiling; 2.8T MoE |
| DeepSeek V4 (all) | 1,048,576 | 384,000 | yes | sold as concurrency: 2,500 concurrent on Flash, 500 on Pro |
| MiniMax M3 | 1,048,576 | 512,000 | yes | $0.30/$1.20 is a promotional 50%-off rate |
| Xiaomi MiMo-V2.5 | 1,050,000 | 131,072 | yes | $0.003 cache reads |
| Qwen3.8 Max | 1,000,000 | 131,072 | yes | |
| Grok 4.6 | 500,000 | 450,000 | yes | AA intelligence 60.9, unmoderated |
| Claude Opus 5 | 1,000,000 | 128,000 | yes | AA intelligence 63, top of the leaderboard |
| Nemotron 3 Ultra 550B | 1,000,000 | 460,000 | yes | US-origin, unmoderated, free variants exist |
| Cerebras Qwen3-Coder | 131,072 | 32,768 | yes | the ceiling that makes 120M tokens/day spendable |
| Venice (most models) | 1,000,000 | 32,768 | yes | output cap hurts long agent turns |

**Rate limits on the pay-as-you-go tiers**, for when overflow matters:

- **Anthropic** — Start tier 1,000 RPM / 2M input TPM / 400K output TPM on Opus
  5, rising to 10,000 / 10M / 2M at Scale. Monthly spend caps $500 / $1,000 /
  $200,000. Cache reads do not count toward the input limit.
- **OpenAI** — five tiers gated on cumulative spend from $5 to $1,000, with
  monthly usage limits from $100 to $200,000.
- **Google** — spend-based tiers, but the actual numbers are account-specific
  and visible only in AI Studio. Google does not publish them as tables.
- **DeepSeek** — concurrency rather than tokens per minute, and rates *double*
  during peak hours (01:00–04:00 and 06:00–10:00 UTC, Mon–Fri). Overnight batch
  work is effectively half price.
- **Nous Portal** — 180 RPM / 720K TPM on unsubscribed keys.

---

## 7. What the losers taught

Every candidate record in [`data/candidates.json`](data/candidates.json) carries
a `lesson` field that survives the option losing. The ones that changed this
report:

- **Chutes** — publicly abandoned request-count subscriptions in February 2026
  because they don't survive agent traffic. Treat every remaining request-count
  plan as temporary.
- **Alibaba** — killed the Qwen Code CLI's 2,000-requests/day free tier and the
  OAuth free tier in April 2026, and closed its Lite plan to new signups in
  March. Free tiers are marketing, not infrastructure.
- **Thinking Machines Inkling** — a 1.05M-context, 262K-output, tool-capable,
  unmoderated model available right now at zero cost. Genuinely excellent, and
  no free tier of this shape has ever survived sustained agent traffic.
- **Perplexity Sonar** — the only vendor pricing retrieval into the token rate
  rather than per query, which is strictly cheaper for read-only browsing. It
  cannot call tools, so it cannot act.
- **Morph / Relace** — edit-application is now a purchasable model class. Pairing
  a cheap planner with a dedicated fast-apply model is the cost structure the IDE
  vendors actually run, and a self-assembled harness can copy it.
- **Phala** — trusted-execution inference is the only *technical* answer to "the
  host can read my prompts", as opposed to the contractual answer everyone else
  sells, and it is priced at parity with Moonshot's own endpoint.
- **Poolside Laguna** — a US lab serving a purpose-built coding model at
  $0.09/$0.18 with a 1M context, unmoderated, with a free tier. The
  cheap-and-permissive tier is no longer exclusively Chinese, which matters if
  jurisdiction is the objection.

---

## 8. How much to trust this

**This is a desk study, not a benchmark.** That is the honest difference between
this report and [`research/agent-db-2026`](../agent-db-2026), which measured
nine engines with real concurrent processes and found that the received wisdom
was wrong. Nothing here was measured on this machine. Every score in
[`data/scores.json`](data/scores.json) is marked `documented`, `derived` or
`judged`, and there is no `measured` value in the file.

Specifically:

- **No refusal testing was run.** The guardrail scores rest on provider
  documentation, OpenRouter's `is_moderated` metadata, and published third-party
  audits. Nobody here sent the same 200 prompts to twenty endpoints and counted
  refusals. That experiment is cheap and would settle the central question
  properly; it is the obvious next step.
- **No throughput or quality testing was run.** Speed figures come from
  Artificial Analysis and vendor claims. Coding-power scores come from
  leaderboard positions, not from running a task on a real repository.
- **Quota multiples are arithmetic on published rates**, and vendors choose
  which rate to publish. A 6x multiple assumes you spend it on the model mix the
  vendor priced it against.
- **Prices move monthly and sometimes weekly.** Within this survey's own sources:
  Z.ai Lite went $7 → $10 → $18 during 2026, Synthetic went $20 → $30 and
  removed its $60 tier, Chutes replaced its entire pricing model in one
  announcement, and MiniMax M3's headline rate is explicitly a 50%-off promotion.
  Treat every number as read on 2026-08-27.
- **Third-party aggregators disagree with vendors.** Two coding-plan comparison
  sites gave materially different tiers for GLM, Kimi and MiniMax on the same
  day. Where sources conflicted, the vendor's own page won; where only
  aggregators had a number, `confidence` is `med` or `low`.
- **Some vendors could not be read directly.** Several pricing pages are
  JavaScript-gated or returned 404 or 403. Those records are marked `low`
  confidence and are concentrated in the least important part of the ranking.

### What would overturn the recommendation

- A refusal benchmark showing GLM or the OpenCode Go bundle refuses ordinary
  security, scraping or automation work at a materially higher rate than the
  Western frontier models. The whole guardrail argument rests on this not being
  true, and it is currently an inference rather than a measurement.
- OpenCode Go re-pricing or narrowing its model bench. It is a young plan at an
  aggressive multiple, and this market has already shown what happens to those.
- Anthropic reversing the January 2026 OAuth block, which would put Claude Max
  back in the comparison and probably back at the top of the coding job.
- Jurisdiction becoming a hard constraint. Four of the top six options process
  prompts in China. If that is unacceptable, the answer changes to Nemotron 3
  Ultra or Poolside on a US host, at a worse price.

---

## 9. Read next

- [`out/RANKINGS.md`](out/RANKINGS.md) — the generated tables, all five
  weightings, per-criterion scores, and every elimination with its reason
- [`data/candidates.json`](data/candidates.json) — all 69 options, each with a
  lesson that outlives the option
- [`data/profiles.json`](data/profiles.json) — the hard filters and the five
  weight vectors. Disagree by editing these and re-running `scripts/score.py`
