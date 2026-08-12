# An agent that earns: the wallet is the easy part, and the wrong part to start with

**Date:** 2026-08-12 · **Analyst:** Jod
**Track:** what setup Reljod needs for Jod to earn money from the internet
**Builds on:** [`core/src/monitor.rs`](../../core/src/monitor.rs) (long-running
task heartbeat monitoring), [`core/src/ledger.rs`](../../core/src/ledger.rs)
(the delivery ledger), migration `0008_monitors_and_ledger`

> **Provenance.** This is **desk research against public sources**, not a
> measured benchmark track. No wallet was created, no endpoint was monetised,
> and no money moved. Every load-bearing number below is a citation, and §12
> lists what is unverified. Where the house standard ([`research/GOALS.md`](../GOALS.md)
> §G2) asks for ten graded iterations, the iterations here are **passes over the
> option space**, each scored against a rubric fixed before the evidence was
> gathered — not ten runs of an experiment. That substitution is deliberate and
> is called out again in §2 so nobody reads this as measured.

---

## The answer in one paragraph

Do not build an autonomously-earning agent. The rails you would reach for are
real, standardised and backed by Google, Visa, AWS, Circle, Anthropic and
Cloudflare — and they carry **about $28,000 a day globally, roughly half of it
wash-traded, at an average payment of $0.20**
([CoinDesk/Artemis, March 2026](https://www.coindesk.com/markets/2026/03/11/coinbase-backed-ai-payments-protocol-wants-to-fix-micropayment-but-demand-is-just-not-there-yet)).
The single most-publicised autonomous earning path — bounty hunting — measured
**$500 across 84 pull requests in 30 days**
([DEV, 2026](https://dev.to/zeroknowledge0x/the-agent-economy-how-ai-agents-are-earning-real-money-in-open-source-and-why-most-fail-9j2)),
which is ~$6 a PR against a token bill that is certainly larger. Meanwhile the
capability you would be granting is the one with the worst safety record in the
industry: **$150k–200k drained from a Grok/Bankr wallet by a Morse-code prompt
injection**, a **$204k first-documented injection drain**, and **$40M lost at
Step Finance, which shut down permanently**. So the recommendation is a
**receive-only** setup, staged: instrument Jod's *cost* first using the ledger
you already have, then add an address and a Stripe account that can **only take
money in and hold no spend key at all**, and treat spend authority as a separate
decision you may never make. The asymmetry is the whole design: every documented
2026 incident required an agent that could *send*. An agent that can only
receive cannot be talked out of anything.

---

## 1. Hypotheses, fixed before the evidence

Written first, graded after. Grades: **held** / **refuted** / **partly**.

| # | Hypothesis | Grade | What decided it |
|---|---|---|---|
| H1 | The blocker is wallet infrastructure — pick a wallet and the agent can earn. | **refuted** | Wallet infra is commodity and excellent (§5). The blocker is demand (§4) and the unit economics (§3.3). |
| H2 | Agent-native payment rails carry enough real volume to be worth selling into. | **refuted** | $28k/day globally, ~half artificial, $0.20 median payment (§4.1). |
| H3 | An agent can hold money in its own right if the custody is good enough. | **refuted** | KYC/AML/BSA are written for human actors; no identity, intent or accountability hook exists for an agent (§7). |
| H4 | Autonomous bounty/gig work is net-positive for a competent coding agent. | **refuted** | ~$6/PR gross at 70% acceptance into a market where high-value issues draw 8–158 attempts in hours (§3.3). |
| H5 | Giving an LLM payment authority is the dominant risk, above anything else. | **held** | Four documented 2026 incidents, three of them total losses (§6). |
| H6 | Jod's existing monitor + ledger are the right substrate for this. | **held** | Watch mode is the cost-correct opportunity scanner; the three-checkpoint obligation is already the correct shape for a money row (§8). |
| H7 | Receiving money is dramatically safer than spending it, and can be separated. | **held** | Every 2026 incident required spend authority. Receive-only is a real, buildable boundary (§9). |
| H8 | There is a near-term path where Jod earns more than it costs. | **partly** | Not autonomously and not on agent rails. Yes, if Jod *operates* a business Reljod owns (§3.4) — but that is a product decision, not a wallet decision. |

The headline reversal: **H1 was the question as asked, and it is the one that
did not survive.** "Set up a wallet" is a two-hour task with a good answer. It
is not the thing standing between Jod and revenue.

---

## 2. Iteration log

### Rubric, fixed before passes 3–10

Each candidate *earning path* scored 0–5 on six axes. Chosen before any
option was priced, so a path cannot be rescued by inventing an axis it wins on.

| Axis | Question |
|---|---|
| **R1 Demand** | Does anyone actually pay for this today, in money that clears? |
| **R2 Margin** | Does revenue exceed the token + infra cost of earning it? |
| **R3 Blast radius** | If Jod is prompt-injected mid-task, what is the worst case? |
| **R4 Legality** | Can this be done cleanly with Reljod as the legal earner? |
| **R5 Build cost** | What does Jod have to grow to do this? |
| **R6 Compounding** | Does doing it once make the next one easier, or is it piecework? |

Max 30.

### The ten passes

| # | What the pass changed | Result |
|---|---|---|
| 1 | Framed as asked: "which wallet?" | Produced a shortlist and no revenue. Reframed. |
| 2 | Reframed to "what would pay Jod?" | Surfaced the option space in §3. Rubric fixed here. |
| 3 | Scored x402 API-selling | Rails superb, demand absent. R1 collapses the score. |
| 4 | Scored bounty/gig hunting | First real numbers; R2 goes negative. |
| 5 | Scored agent marketplaces (toku.agency et al.) | Real USD via Stripe, but thin and unproven. |
| 6 | Added the threat model as a scoring input | R3 re-scored everything downward that needs a spend key. |
| 7 | Added the legal layer | R4 eliminated "agent as principal" entirely. |
| 8 | Scored "Jod operates a business Reljod owns" | Wins on R1/R2/R6; loses on R5. Highest total. |
| 9 | Split every path into receive-only vs spend-capable | The decisive move — see reversal below. |
| 10 | Re-scored against Jod's actual codebase | Monitor + ledger already cover most of the substrate (§8). |

### Ranking

| Rank | Path | R1 | R2 | R3 | R4 | R5 | R6 | **Total** |
|---|---|---|---|---|---|---|---|---|
| **1** | **Jod operates a business/product Reljod owns** | 5 | 5 | 4 | 5 | 2 | 5 | **26** |
| 2 | Sell a Jod capability as a paid endpoint (MPP/Stripe) | 2 | 4 | 5 | 5 | 4 | 4 | **24** |
| 3 | Instrument cost first, earn nothing yet | 0* | 5 | 5 | 5 | 5 | 5 | **25*** |
| 4 | Sell a Jod capability via x402/USDC | 1 | 4 | 5 | 3 | 4 | 4 | **21** |
| 5 | Agent marketplace listing (toku-style) | 2 | 3 | 4 | 4 | 3 | 2 | **18** |
| 6 | Autonomous bounty hunting | 3 | 1 | 3 | 4 | 3 | 1 | **15** |
| 7 | Autonomous trading / DeFi / arbitrage | 4 | 1 | 0 | 1 | 2 | 1 | **9** |

\* Path 3 scores 0 on R1 by construction — it earns nothing. It ranks where it
does because it is the **precondition** for honestly scoring R2 on everything
else, and it is the only path with no downside. Read it as "do this *and* one
of the others", not as a competitor.

### Reversals — where a later pass refuted an earlier one

- **Pass 3 → pass 4.** x402 initially looked like the answer: elegant, one line
  of middleware, Anthropic and Visa on the foundation. Pass 4's numbers made
  the elegance irrelevant. A perfect toll booth on an empty road.
- **Pass 6 → pass 9.** Pass 6 scored the threat model as a penalty applied to
  each path. Pass 9 found it is not a penalty but an **axis of separation**:
  the same path scores 5 or 0 on R3 depending only on whether the agent holds a
  spend key. That reframing is the report's main contribution, and it is why
  the recommendation is a *sequence* rather than a *choice*.
- **Pass 8 stands against the framing of the request.** The highest-scoring
  path involves no agent-native payment rail at all.

---

## 3. What "earning" can actually mean

### 3.1 The four honest categories

1. **Jod sells its own compute/capability** — an endpoint other agents or people
   pay per call to use.
2. **Jod does piecework** — bounties, gigs, marketplace tasks.
3. **Jod trades** — markets, arbitrage, DeFi.
4. **Jod operates a business Reljod owns** — it is the staff, not the earner.

Only (4) has the property that revenue is decoupled from Jod's token burn.

### 3.2 What the rails were built for

Worth stating plainly because it inverts the request: **x402, ACP, UCP, AP2 and
MPP are all, primarily, protocols for an agent to *spend*.**
([ATXP protocol comparison, 2026](https://atxp.ai/blog/agent-payment-protocols-compared/))
The industry is building the buy side. The sell side — an agent that *receives*
— is the thin end, and that is exactly why demand is missing: the merchants
these protocols serve are still rare, in the words of one analyst quoted by
CoinDesk.

### 3.3 The unit economics, which is where most of this dies

The measured autonomous-bounty result:

- **$500+ across 84 PRs in 30 days**, ~70% acceptance *after* filtering for
  high-probability targets — ~$6 per merged PR, gross.
- High-value issues attract **8–158 attempts within hours**, so speed is a
  commodity and reputation is the real moat.
- Agents exhibit "confident hallucination" — e.g. writing passing tests against
  files that do not exist on the branch.
  ([DEV, 2026](https://dev.to/zeroknowledge0x/the-agent-economy-how-ai-agents-are-earning-real-money-in-open-source-and-why-most-fail-9j2))

Against that, near-frontier inference runs roughly **$2/M input and $10/M output
tokens** ([Asanify, July 2026](https://asanify.com/blog/news/ai-agent-unit-economics-july-1-2026/)),
and 84 PRs — each of them a clone, a read, a fix, a test loop and a review
round — is not a $500 token bill. The published conclusion is a hybrid: the AI
does the grunt work, a human picks the targets and talks to maintainers. That is
not an autonomous earner. That is a tool.

The macro number agrees: **over 40% of autonomous agent projects face
cancellation over escalating cost and unclear value**, and **77% never reach
production** ([Company of Agents](https://www.companyofagents.ai/blog/en/ai-agent-unit-economics-scaling),
[Malik, 2026](https://umesh-malik.com/blog/autonomous-ai-agents-production-gap-2026)).

### 3.4 The path that actually scores highest

Jod is already a competent chief of staff for one person. The version of
"earning" with real margin is **Jod running the parts of a business Reljod owns
that would otherwise need hiring** — support, triage, content, ops, monitoring,
follow-up. Revenue comes from customers paying for a product; Jod's cost is
compared against a salary, not against a bounty. This needs a product, which is
why it loses on R5 (build cost) and why it is out of scope for a wallet
question. But it should be said clearly, because it is the answer that survives
scrutiny, and the wallet setup below serves it perfectly well.

---

## 4. The rails, compared

### 4.1 x402 — the one everyone means

HTTP 402, finally activated after 30 years dormant. Coinbase + Cloudflare
founding; **Google, Visa, AWS, Circle, Anthropic and Vercel** on the x402
Foundation with the Linux Foundation, launched **2 April 2026**
([blockeden summary](https://blockeden.xyz/forum/t/is-x402-the-most-important-protocol-of-2026-google-stripe-aws-and-visa-just-made-http-payments-real/4699)).
**166M+ transactions**, 100M of them on Base inside three quarters
([Chainalysis](https://www.chainalysis.com/blog/x402-agentic-payments-adoption/)).

And the demand side, which is the number that matters:

| Metric | Value | Source |
|---|---|---|
| Daily volume | **~$28,000** | Artemis via CoinDesk |
| Daily transactions | ~131,000 | " |
| Average payment | **~$0.20** | " |
| Share artificial (self-dealing / wash) | **~50%** | " |
| Ecosystem "valuation" | ~$7B, inflated by counting Chainlink's $6.3B cap | " |

The mechanics for a seller are genuinely one line of middleware: wrap the
endpoint, set a price, return 402 with price + address, the facilitator (CDP
hosts the primary one, **fee-free USDC settlement on Base mainnet**) verifies
and settles, revenue lands in a seller wallet you withdraw from
([Coinbase](https://www.coinbase.com/developer-platform/discover/launches/monetize-apis-on-x402),
[Circle](https://www.circle.com/blog/turn-your-api-into-a-storefront-for-agents)).
Nothing about the engineering is hard. There is simply almost nobody on the
other end.

### 4.2 Stripe MPP — the one that matters more for receiving

**Machine Payments Protocol**, co-authored by Stripe and Tempo, launched
**18 March 2026** ([Stripe](https://stripe.com/blog/machine-payments-protocol)).
This is the important one for Jod, because it is the rail where *receiving* is
first-class and boring:

- Accept agent payments through the ordinary **PaymentIntents API**.
- Settles in **stablecoins or fiat**; lands in your default currency on your
  normal payout schedule.
- Comes with fraud protection, **tax calculation**, reporting and refunds —
  i.e. the compliance surface is Stripe's problem, not Reljod's.
- Live users cited: Browserbase (per-session browser infra), PostalForm,
  Prospect Butcher Co.

Stripe also previewed **Connect payouts in stablecoins to 160+ countries**
([Sessions 2026](https://stripe.com/blog/everything-we-announced-at-sessions-2026)),
which matters if the off-ramp is not a US bank.

### 4.3 The rest, briefly

| Protocol | Backer | For | Status |
|---|---|---|---|
| **ACP** | Stripe + OpenAI | agent→merchant checkout | live in ChatGPT early 2026; PayPal, Shopify, Salesforce adopting |
| **UCP** | Google + Shopify | discovery→fulfilment | announced Jan 2026, 20+ partners, 5M+ merchants |
| **AP2** | Google | authorisation/trust layer — spend policy, audit trail | announced Sept 2025, enterprise |

Source: [ATXP comparison](https://atxp.ai/blog/agent-payment-protocols-compared/).
All three are buy-side. **AP2 is worth reading anyway** — not to adopt, but
because its question ("how does an entity authorise an agent to spend on its
behalf, with policy and audit") is precisely the question §8 argues Jod's ledger
already half-answers.

---

## 5. Wallets and custody, if it comes to that

Commodity, and good. The 2026 field splits cleanly
([Crossmint](https://www.crossmint.com/learn/agent-wallets-compared),
[Openfort](https://www.openfort.io/blog/best-agent-wallets-for-developers)):

**Purpose-built agent wallets** — Coinbase Agentic Wallets, Crossmint, thirdweb.
**Signing infrastructure** — Turnkey, Privy, Alchemy.

| Option | Shape | Controls | Note |
|---|---|---|---|
| **Coinbase Agentic Wallets** | launched **11 Feb 2026**, on AgentKit + x402 | **TEE-enforced** session caps, per-tx caps, contract allowlists | keys in Coinbase enclaves; gasless on Base; installable via `npx awal` or an **MCP server that already speaks to Claude Code** |
| **Privy** | server wallets, TEE + Shamir | off-chain policy: transfer limits, approved protocols, recipient restrictions, **operating time windows** | build-your-own agent layer |
| **Turnkey** | low-level signing | policy engine | most control, most work |
| **Crossmint** | wallets + orchestration, 50+ chains | on/off-ramps, KYC/AML/Travel Rule, **card rails** (Visa/Mastercard via Lobster Cash) | only one covering both stablecoin and card networks |
| **Plain AgentKit CDP wallet** | SDK for Base | **no built-in spending limits** — you add them at the app layer | see §6 for why that is disqualifying |

The one distinction that matters: **TEE-enforced limits versus app-layer
limits.** A cap the model cannot reach because it lives in an enclave is a
control. A cap enforced by code the agent is editing, or by an instruction in
its prompt, is a suggestion. AgentKit's bare CDP wallet shipping with *no*
built-in limits is the single most important line in the comparison.

**If Jod ever needs a spend key: Coinbase Agentic Wallets.** TEE caps, native
x402, and an MCP server that plugs into the harnesses Jod already drives. But
see §9 — that is stage 3, and it may never arrive.

---

## 6. The threat model, which decides the architecture

This is not theoretical, and 2026 supplied the case law.

| Incident | Loss | Mechanism |
|---|---|---|
| **Grok / Bankrbot**, May 2026 | **$150k–200k** (3B DRB tokens) | Attacker sent a membership NFT to the wallet, then posted a **Morse-code** message on X and asked the agent to translate it. It decoded an instruction to move funds and executed. ([OECD.AI incident record](https://oecd.ai/en/incidents/2026-05-04-4a73), [Giskard](https://www.giskard.ai/knowledge/how-grok-got-prompt-injected-an-x-user-drained-150-000-from-an-ai-wallet)) |
| **First documented injection drain** | **$204,000** | Prompt injection against an agent with live financial capability. ([MetaMask](https://metamask.io/news/agentic-wallet-security)) |
| **Step Finance** | **$40M**, unrecovered | AI agent treasury exploit. **Protocol shut down permanently.** |
| **Gitcoin Owockibot**, 8 Feb 2026 | key exposure | Agent exposed its own hot-wallet private key in multiple locations. |

Two things follow, and they point straight at Jod's existing code.

**First: Jod already ingests exactly the input class that caused these.**
[`monitor.rs`](../../core/src/monitor.rs) polls URLs a stranger writes, and its
own documentation is admirably clear about the limit of its defence:

> *"Not a security control — a model can be talked out of any instruction. It is
> the cheapest layer available and the one that makes the intent legible to
> whoever reads the transcript afterwards."* — `MONITOR_PREAMBLE`

That is the correct assessment, and it is fatal to any design where the same
session holds a spend key. `MONITOR_PREAMBLE` is doing the same job the Grok
agent's system prompt was doing when someone asked it to translate Morse code.

**Second: the boundary has to be a capability, not a sentence.** The industry
consensus is stated well by MetaMask: the goal is a wallet *"where being fooled
doesn't turn into unlimited financial authority, where the worst a compromised
agent can do is bounded by a spend cap, an allowlist, and a signature it was
never able to reach on its own."* The strongest version of "a signature it was
never able to reach" is **no signing key in the process at all**.

---

## 7. The legal layer: the agent is never the earner

Unambiguous in the sources, and it closes off a whole branch of the design:

> Every layer of the existing compliance stack — **KYC, AML, the Bank Secrecy
> Act** — was written for human actors, and an AI agent making autonomous
> decisions **does not satisfy the identity, intent, or accountability
> requirements** built into those frameworks.
> ([Prompthalo](https://www.prompthalo.ai/feeds/blog/ai-agent-compliance-banking),
> [KYC-Chain](https://kyc-chain.com/ai-compliance-agents-kyc-aml/))

So:

- **Reljod (or an entity he owns) is the earner.** Jod is a delegated operator
  with bounded authority. Every account, every KYC, every tax obligation
  attaches to him.
- **Income is income** whether it arrives as USDC or as a Stripe payout.
  Stablecoin receipts are taxable and need a cost basis and a record. This is
  the strongest single argument for Stripe MPP over raw x402 for anything
  material: Stripe does tax calculation and reporting; a Base address does not.
- **`domains/finance/README.md` is the right place to write the authority
  boundary down**, and it is currently `TBD` with a note that says exactly this:
  *"what it's allowed to do autonomously... versus what always requires
  confirmation (e.g. moving money)"*. That note was written before this research
  and anticipates its conclusion.

---

## 8. How this builds on heartbeat monitoring

The request was "build on top of long-running task heartbeat monitoring", and
the connection turns out to be structural rather than decorative. Two modules
already in `0008_monitors_and_ledger` are the substrate.

### 8.1 `monitor.rs` is the opportunity scanner, and it is the reason earning can be net-positive

`monitor.rs` exists because of an economic argument its own header makes:

> *"most scheduled work should not wake a model. A watchdog is a script and a
> hash. For an agent Reljod pays per token to run around the clock, that is the
> difference between a scheduler and a bill."*

That argument is *more* true for earning than for watching. Any earning path is
a loop of the form **watch for an opportunity → evaluate → act**, and the watch
step runs constantly while the act step runs rarely. An earning agent that wakes
a model on every tick has already lost — this is precisely the §3.3 failure,
where 84 model-driven attempts chase $500.

`Mode::Watch` is the correct shape unchanged: probe, hash the exact bytes,
suppress on unchanged, inject a diff and wake the model only on change. A new
bounty on a board, a new inbound request, a changed price — all of them are
`digest()` over bytes. **The scanner is already written.** And `Mode::NoAgent`
covers the case where the decision needs no model at all.

The one caveat is §6: the diff arriving from that probe is attacker-controlled
text, and it arrives *in the prompt*. Today the mitigation is
`MONITOR_PREAMBLE`, which the code correctly says is not a control. Adding a
spend key to that session would convert a candid limitation into the Bankr
incident.

### 8.2 `ledger.rs` is already the right shape for a money ledger

This is the closest correspondence in the report. `ledger.rs` solves "prove a
message Jod owed somebody was actually sent". A payment ledger solves "prove
money Jod owed somebody actually moved". **These are the same problem**, and the
existing design is already correct for the harder one:

| `ledger.rs` today | The money analogue |
|---|---|
| Row written **before** the send | Row written **before** the transfer — the only way a crash mid-payment is recoverable |
| `pending` → never reached transport → replay safely | Never broadcast → safe to retry, duplicate impossible |
| `attempting` → **in flight, genuinely unknowable** | Broadcast but unconfirmed — the exact double-spend-risk state |
| `RECOVERED_MARKER` — ambiguity is **labelled**, never silently resent | A payment that may have gone twice must be flagged for a human, not quietly retried |
| `Owner{machine, pid}` — only a dead process's rows are claimable | Two Jods must never both send the same payment |
| `sweep_recoverable` claims in the same transaction as it selects | Identical requirement, higher stakes |

The ethic in the module docstring transfers verbatim: *"delivery is honestly
at-least-once, and ambiguity is labelled rather than silently resent. Dropping
the message would be a lie of omission; resending it unmarked would be a lie of
commission; saying 'this may be a duplicate' is neither."* Replace "message"
with "payment" and that is a better treasury policy than most.

The rule that a sweep may only be invoked by a process that can actually send
maps to a rule that only a process holding the payment credential may claim a
payment row — with the same failure mode if violated.

### 8.3 What that means concretely

The first thing to build is neither a wallet nor an endpoint. It is a **cost
row**: extend the `0008` schema with a money ledger that, at first, records only
what Jod *spends* on itself per run. Jod already has runs, an event stream and a
ledger idiom in one SQLite file. Without the cost denominator, every claim about
whether earning is net-positive is a guess — and §3.3 says the guess is usually
wrong.

---

## 9. The recommendation: four stages, and you may stop at any of them

**Stage 0 — Earn nothing. Measure the bill.**
Add a cost ledger alongside `ledger.rs` in the `0008` idiom: per run, per
harness, tokens and dollars. Surface it in the TUI next to the run. Nothing
external, no keys, no accounts. *This is the whole of the first PR.* It is also
the only stage that is unambiguously worth doing regardless of what you decide
about the rest.

**Stage 1 — Receive-only. No spend key exists.**
- A **Stripe account in Reljod's name/entity**, MPP-capable, as the primary
  receiving rail — because it settles to fiat, calculates tax, and reports.
- Optionally a **USDC receiving address on Base** whose private key is **not on
  the VPS and not reachable by any Jod process** — a hardware wallet or a
  Coinbase account address. Jod knows the address as a string. Nothing more.
- The invariant, written into `domains/finance/README.md`: **no Jod process
  holds a key that can move money.** A prompt-injected Jod at this stage can at
  worst tell someone the wrong address to pay — bad, recoverable, and not
  $200,000.

**Stage 2 — Monetise one thing Jod already does well.**
Wrap a single capability as a paid endpoint behind Stripe MPP (PaymentIntents),
and optionally mirror it on x402 to learn the rail. Set expectations from §4.1:
**revenue here will be approximately zero**, and that is fine — the purpose is
to have the receiving path built and proven before there is demand, not to make
money in 2026. Use `Mode::Watch` monitors for inbound signals so the endpoint
costs nothing while idle.

**Stage 3 — Spend authority. Only if a concrete need arrives, and probably never.**
If Jod must pay for something (an API it needs mid-task), then and only then:
**Coinbase Agentic Wallets**, TEE-enforced per-transaction and session caps,
contract allowlist, funded with an amount you would shrug at losing. Never the
bare AgentKit CDP wallet, which ships with no built-in limits. Never a key in
the same process that reads monitor diffs.

The staging is the recommendation. Stage 0 is a week of work with certain value;
stage 3 is the one that can lose $200,000, and nothing in the evidence justifies
reaching it yet.

---

## 10. What to build in Jod, in value order

1. **Cost ledger** (`0008` idiom, alongside `ledger.rs`) — per-run token and
   dollar spend. No external dependency. Precondition for every other claim.
2. **`domains/finance/README.md`, written properly** — the authority boundary:
   Reljod is the legal earner, Jod may read balances and draft invoices, Jod
   holds no key that moves money, and what would have to change for that to
   change. This is a charter edit, not code.
3. **A `docs/decisions.md` entry** — "Jod receives, never sends", with the §6
   incidents as the reason. Per the charter's *extend by writing it down*.
4. **Stripe MPP receiving path** — one endpoint, PaymentIntents, receipts
   recorded as ledger rows.
5. **A money ledger** reusing the three-checkpoint state machine, initially
   recording only *inbound* settlements (which have no ambiguity problem — the
   hard `attempting` state only exists for sends). This is the honest order:
   build the easy half first, and only design the ambiguous half if stage 3 ever
   arrives.
6. **Opportunity monitors** — `Mode::Watch` probes against whatever inbound
   signal matters, reusing the existing scanner unchanged.

Items 1–3 need no wallet, no account, and no counterparty, and they are where
the value is.

---

## 11. Direct answer to the question asked

> *"what setup do I need — maybe a wallet, etc."*

- **A wallet is not the blocker,** and if you set one up first you will have
  built the safest, most standardised toll booth on a road with $28k/day of
  traffic, half of it fake.
- **The minimum viable setup is: a Stripe account in your name, and a receiving
  address whose key Jod cannot touch.** That is stage 1, it takes an afternoon,
  and it is sufficient for any real revenue Jod is likely to see in the next
  year.
- **The thing worth building this week is the cost ledger,** because you
  currently cannot answer "is this net-positive" about anything.
- **The path with real margin is Jod operating something you own,** not Jod
  freelancing on the internet. That is a product question, and the stage-1
  setup serves it without modification.
- **If you want one sentence for `domains/finance/README.md`:** *Jod may see
  money and reason about it; Jod may never move it.*

---

## 12. What is explicitly not verified

- **No wallet was created, no endpoint monetised, no payment sent or received.**
  Nothing here is measured.
- **Jod's actual token cost per run is unknown** — that is stage 0's entire
  point. The §3.3 margin argument is an inference from published pricing against
  a published earnings figure, not a measurement of Jod.
- **Reljod's jurisdiction, tax residency and entity status are unknown to this
  report.** §7 states the shape of the obligation, not its content. Off-ramp
  availability, stablecoin tax treatment and whether an entity is worth forming
  all depend on that and need a human professional, not an agent.
- **The bounty figure ($500/84 PRs) is a single blogged run**, not a
  distribution. It is directionally corroborated by the saturation figures and
  the 40%-cancellation macro number, but it is one data point.
- **x402 volume is one analytics provider (Artemis) via one outlet.** The
  transaction *counts* are corroborated by Chainalysis; the *dollar volume* and
  the ~50% artificial share are not independently corroborated here.
- **Fee schedules were not priced.** CDP advertises fee-free USDC settlement on
  Base mainnet; Stripe's MPP pricing was not confirmed.
- **`agentbounty.org` and `toku.agency` were surfaced but not evaluated.** If
  path 5 is ever revisited, they are the starting point.
- **No claim is made that any of these rails will still be the right ones in
  12 months.** Four of the five protocols in §4 were announced within the last
  14 months.

---

## Sources

- [Chainalysis — Inside x402: 100M Agentic Payments on Base](https://www.chainalysis.com/blog/x402-agentic-payments-adoption/)
- [CoinDesk — Coinbase-backed AI payments protocol wants to fix micropayments but demand is just not there yet](https://www.coindesk.com/markets/2026/03/11/coinbase-backed-ai-payments-protocol-wants-to-fix-micropayment-but-demand-is-just-not-there-yet)
- [ATXP — Every Agent Payment Protocol Compared: x402, ACP, UCP, AP2](https://atxp.ai/blog/agent-payment-protocols-compared/)
- [blockeden — Is x402 the Most Important Protocol of 2026?](https://blockeden.xyz/forum/t/is-x402-the-most-important-protocol-of-2026-google-stripe-aws-and-visa-just-made-http-payments-real/4699)
- [Coinbase — Monetize APIs on x402](https://www.coinbase.com/developer-platform/discover/launches/monetize-apis-on-x402)
- [Coinbase — Introducing Agentic Wallets](https://www.coinbase.com/developer-platform/discover/launches/agentic-wallets)
- [Circle — Turn your API into a storefront for agents](https://www.circle.com/blog/turn-your-api-into-a-storefront-for-agents)
- [Stripe — Machine Payments Protocol](https://stripe.com/blog/machine-payments-protocol)
- [Stripe — Everything we announced at Sessions 2026](https://stripe.com/blog/everything-we-announced-at-sessions-2026)
- [Crossmint — Agent Wallets Compared](https://www.crossmint.com/learn/agent-wallets-compared)
- [Openfort — Best AI Agent Wallets for Developers in 2026](https://www.openfort.io/blog/best-agent-wallets-for-developers)
- [MetaMask — What actually keeps an AI agent from draining your wallet](https://metamask.io/news/agentic-wallet-security)
- [OECD.AI — AI Prompt Injection Exploit Drains Grok-Linked Crypto Wallet](https://oecd.ai/en/incidents/2026-05-04-4a73)
- [Giskard — How Grok got prompt-injected](https://www.giskard.ai/knowledge/how-grok-got-prompt-injected-an-x-user-drained-150-000-from-an-ai-wallet)
- [DEV — The Agent Economy: How AI Agents Are Earning Real Money in Open Source (And Why Most Fail)](https://dev.to/zeroknowledge0x/the-agent-economy-how-ai-agents-are-earning-real-money-in-open-source-and-why-most-fail-9j2)
- [Asanify — AI Agent Unit Economics, July 2026](https://asanify.com/blog/news/ai-agent-unit-economics-july-1-2026/)
- [Company of Agents — AI Agent Unit Economics: Scaling Your Agentic Fleet](https://www.companyofagents.ai/blog/en/ai-agent-unit-economics-scaling)
- [Umesh Malik — Why 77% Never Reach Production](https://umesh-malik.com/blog/autonomous-ai-agents-production-gap-2026)
- [Prompthalo — AI Agent Compliance in Banking](https://www.prompthalo.ai/feeds/blog/ai-agent-compliance-banking)
- [KYC-Chain — AI Compliance Agents for KYC/AML in 2026: Hype vs Reality](https://kyc-chain.com/ai-compliance-agents-kyc-aml/)
