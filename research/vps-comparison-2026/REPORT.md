# The best VPS for hosting Jod — 60 providers, analysed

**Date:** 2026-08-08 · **Corrected:** 2026-08-09 · **Analyst:** Jod ·
**Method:** scripted, reproducible · **Numbers:** [`out/RANKINGS.md`](out/RANKINGS.md)

> **Goal, as stated:** a VPS to host the Jod LLM-agent orchestrator — free to do
> anything on the internet, no compliance friction, good availability, good
> internet access, good global location coverage, and the cheapest of them all.

> ### ⚠️ This report was wrong, and the correction is the most useful thing in it
>
> The first version recommended **Advin Servers at $6.00/mo**. Advin has nothing
> to sell. Every plan on its cloud page reads *"Out of Stock"* and all six
> regions report *"No Available Plans"* — and there was never a 4 GB / 2 vCPU
> plan to begin with; that price came off a *"Starting at $6.00/month"* banner.
>
> Checking that one claim exposed a systematic fault. **This dataset priced
> advertisements, not purchases.** Six rows have now had their order page
> actually opened. **All six were wrong.** Corrections below; the recommendation
> has changed.

---

## The answer

**Buy [HostHatch NVMe 12 GB](https://hosthatch.com/products) — $12.00/mo, 12 GB
RAM / 4 cores / 50 GB NVMe / 3 TB, 7 compute locations.**

This is the best *verified* option, and that qualifier is doing real work — see
[How much to trust this](#how-much-to-trust-this). Three rows in this dataset
have a price confirmed against a standing catalogue. HostHatch is the cheapest
of them and carries 3× the RAM of the next, which for an orchestrator running
Claude Agent SDK processes, MCP servers and headless Chrome is the resource that
actually binds.

| | HostHatch | IncogNET | RackNerd |
|---|---|---|---|
| Price | **$12.00/mo** | $20.00/mo | $24.59/mo |
| Spec | 4 core / **12 GB** / 50 GB | 2 vCPU / 4 GB / 50 GB | 4 vCPU / 4 GB / 130 GB |
| $ per GB RAM | **$1.00** | $5.00 | $6.15 |
| Score | 65.7 | **66.7** | 54.3 |
| Permissiveness | 84 | **100** | 84 |
| Orderable | Not confirmed (panel login) | **3 of 6 locations** | **Yes, all plans** |
| Locations | 7 compute | 6 | **12** |

**One thing to check before you pay:** HostHatch publishes per-location stock
only behind its panel, so I could not confirm the 12 GB plan is orderable in the
location you want. That is a two-minute check you can do and I cannot — and
given that this report already recommended a sold-out host once, do it first.

**If it is sold out, buy IncogNET at $20/mo** — it scores marginally higher
anyway (66.7 vs 65.7, a gap well inside the noise), it is the most permissive
host measured at 100/100, it needs no personally identifiable information, it
takes Monero, and its stock is published openly per plan per location: at the
4 GB tier, Liberty Lake, Stockholm and Sofia are in stock today.

**What I am no longer recommending, and why:**

| Was | Claimed | Actually |
|---|---|---|
| Advin Servers | $6.00/mo, buy this | **Sold out everywhere**; no such plan exists |
| HostHatch | $2.42/mo, top score | $2.42 was a withdrawn promo; standing price for a spec-meeting plan is **$12.00** |
| RackNerd | $3.62/mo | **$24.59/mo** standing — and its annual term carries *no* discount |

### Runners-up, by what you might actually want

| If you want… | Buy | Why |
|---|---|---|
| **Maximum freedom, no questions asked** | **IncogNET** — $20/mo | Requires no personally identifiable information, takes Monero, 6 US/EU locations, AMD EPYC. Scored **100/100** on permissiveness. Independently listed as no-KYC. Verified in stock in 3 locations. |
| **Freedom + proven reliability** | **BuyVM** — $15/mo (1 core) or $30/mo (2 core) | Luxembourg jurisdiction, genuinely unmetered 1 Gbps, no overselling. Owner-operated. The most permissive host that is also unambiguously legitimate. |
| **It must never go down** | **UpCloud** — $18/mo | Published **100% uptime SLA**, fastest storage in class, 13 locations. Wins the reliability-first profile outright. |
| **Most locations for the money** | **PQ.Hosting** — $7.52/mo | ~40 sites; scored 89/100 on location coverage. Verify the corporate structure yourself first. |
| **Best raw price/performance, freedom aside** | **Hetzner** — $4.13/mo | Ranks #30 here *only* because of permissiveness. On price and reliability alone it beats nearly everything. |

---

## The finding that should change your architecture

**The IP address is the real constraint, not the server.**

Every provider on this list sells CPU, RAM and bandwidth. None of them sell what
a browsing agent actually needs, which is *an IP address that websites will
answer*. Anti-bot vendors — Cloudflare, DataDome, PerimeterX — score by ASN
before a request is ever evaluated. Datacenter ASNs get default-low trust, and
the well-known hosting ranges get worse than that.

This inverts the usual advice. The "best" infrastructure is the worst here:

- **AWS / GCP / Azure** — the most aggressively blocked ranges of anything
  analysed. AWS maintains a public `HostingProviderIPList`; being on it is the
  default state for Lightsail.
- **Hetzner / OVH / Contabo** — repeatedly named in abuse-mitigation writeups as
  major sources of malicious traffic, and filtered accordingly.
- **Cloudzy / M247 / Zomro** — ranges shared with VPN resellers, which is
  precisely the signal anti-bot vendors train on.

Meanwhile the small, boring hosts — **Webdock, 1984, IncogNET, Melbicom,
HostHatch** — have comparatively clean ranges purely because nobody has abused
them at scale yet. (This list originally opened with Advin and Servarica. Both
are sold out, which is its own lesson: the hosts with the cleanest IP ranges are
frequently the ones too small to have capacity.)

**So: do not plan for Jod to browse the open web from its own VPS IP.** No
provider choice fixes this. The working architecture is:

```
Jod orchestrator on a cheap, permissive VPS   ← this report picks the box
        │
        ├── LLM API calls ────────────→ direct from the VPS (never blocked)
        ├── Your own services/webhooks → direct from the VPS
        └── Open-web browsing/scraping → through a residential/mobile proxy
```

Budget roughly $12–20/mo for the box. **The VPS is the cheap part of this
problem.** Picking a $12 host over a $20 host is noise next to getting the
egress path right — and, as the correction pass showed, most of the prices below
$10 that made this look like a $5 decision were not real.

> **Follow-up, 2026-08-08:** the egress path is now its own study —
> [`research/ip-blocking-2026`](../ip-blocking-2026/REPORT.md), 55 solutions
> compared. It partly **corrects** the advice above: a residential proxy fixes
> only one of the six checks Cloudflare runs, and the IP is checked *fifth*,
> after the TLS handshake has already given a plain HTTP client away. The
> working answer is a stealth browser plus ~10 static ISP proxy IPs for about
> **$3/month** — cheaper than the residential proxy assumed here, not more
> expensive.

---

## What "free to do anything" actually costs

The permissiveness spread is the widest of any criterion measured — 100/100 down
to 22/100 — and it maps almost perfectly onto price and reliability in the wrong
direction.

| Tier | Score | Examples | The trade |
|---|---:|---|---|
| **Offshore / no-KYC** | 100 | IncogNET, BuyVM, FlokiNET, AbeloHost, Shinjiru, PRQ | Genuinely unrestricted. 2-4× the price, few locations, often no API and no SLA. |
| **Low-end market** | 84 | Advin, HostHatch, RackNerd, GreenCloud, VPSDime | Complaints forwarded, not acted on. Cheap. Support and reliability vary. |
| **EU/mainstream** | 32-68 | Hetzner, OVH, Contabo, Vultr | Real abuse desks. Hetzner is the strictest of the budget hosts. |
| **Hyperscaler** | ~22 | AWS, GCP, Azure, Alibaba | Strict AUP, heavy KYC, and the worst IP reputation. Structurally wrong for this. |

The important nuance: **you probably do not need tier 1.** Offshore hosting is
priced for people who need takedown resistance. Running an LLM orchestrator that
calls APIs and automates your own accounts is ordinary, legal computing. The
low-end tier at 84/100 already means "nobody will bother you", and it costs a
quarter as much. Pay for tier 1 only if you specifically need a host that
ignores legal process — and on the evidence here, you don't.

---

## Five different questions, five different winners

This is the most useful output of the whole exercise. The scripts ran the same
60 providers through five weightings:

| Rank | Your stated goal | **Verified rows only** | Pure cost | Max permissiveness | Reliability-first |
|---:|---|---|---|---|---|
| 1 | GreenCloudVPS ($5) | **IncogNET** ($20) | Oracle Cloud ($0) | **BuyVM** ($30) | **UpCloud** ($18) |
| 2 | Melbicom ($13) | **HostHatch** ($12) | VirMach ($4) | FlokiNET ($24) | phoenixNAP ($30) |
| 3 | PQ.Hosting ($8) | RackNerd ($25) | Contabo ($5) | AbeloHost ($16) | Latitude.sh ($110) |
| 4 | IncogNET ($20) | — | Hetzner Cloud ($4) | Shinjiru ($25) | Vultr ($20) |
| 5 | Hivelocity ($12) | — | GreenCloudVPS ($5) | OrangeWebsite ($20) | Hivelocity ($12) |

No provider appears in all five columns. There is no universally best VPS, only
a best one for a stated weighting — which is why the weights are a data file you
can edit rather than a judgement buried in prose.

**The `verified` column is new, and it only has three rows.** That is not a bug.
It is every candidate whose price was read off a standing catalogue rather than
inferred from a listicle, a promo, or a banner. The `jod` column above it is
topped by GreenCloudVPS, PQ.Hosting and Hivelocity — all *low* confidence, none
ever checked. Given that six for six of the rows checked so far were wrong, the
honest reading is that the left column is a list of *candidates to verify*, and
only the `verified` column is a list of things to buy.

Note the price spread across winners: **$0 → $30**, a 12× range, for the same
reference spec. Almost all of that is buying permissiveness or buying an SLA.

---

## Things that will cost you money if nobody tells you

**Oracle's free tier is being dismantled — this month.** The famous "free 24 GB
ARM box" is over. The Always Free Ampere allocation was halved to 2 OCPU / 12 GB
on 2026-06-15, and instances above the new limits are **terminated from
2026-08-18** — ten days from this report. It also has no SLA, is almost always
out of capacity, and reclaims instances idling below 20% CPU. It wins the
pure-cost profile at $0 and is still the wrong place to run something you depend
on.

**BuyVM's famous $15 plan has one core.** The widely-quoted SLICE 4096 is 4 GB
with a single (dedicated, 3.5 GHz+) core, so it fails a 2-vCPU bar. The cheapest
2-core plan is $30/mo. If one fast dedicated core is enough for Jod, the $15
slice is the best permissiveness-per-dollar on this entire list — verify against
your actual workload.

**HostHatch's "15 locations" is 7 for compute.** The APAC sites (Sydney, Hong
Kong, Singapore) are storage VMs only. If Jod needs to be near Asia, this
matters and the marketing number will mislead you.

**Melbicom's "unmetered" is 5 TB.** Then it throttles 1 Gbps → 100 Mbit/s.
Fujairah and Mumbai are hard-capped at 2-5 TB, and users report Asian links as
low as 10 Mbps.

**Aeza Group is OFAC-sanctioned.** Sanctioned by the US Treasury in July 2025 as
a bulletproof hosting provider. It appears in "permissive VPS" listicles
constantly. Excluded automatically by the sanctions filter — transacting creates
real legal exposure and payment rails will reject you.

**Contabo's headline RAM comes with a 200 Mbit/s port** on entry tiers, and
well-documented CPU oversubscription. The RAM-per-dollar figure is real; the
performance behind it is not comparable.

**DigitalOcean locks accounts at signup, after payment.** A recurring,
well-documented pattern. For a $24/mo box that scores 53/100 on permissiveness,
there is no reason to accept that risk here.

---

## What got disqualified, and why that's the point

Hard filters ran *before* scoring, because a weighted average will happily let a
great price paper over a disqualifying flaw:

| Provider | Disqualified because |
|---|---|
| Aeza Group | `sanctions-risk` — OFAC-designated |
| **Advin Servers** | **`stock: out` — nothing orderable in any region** |
| **Servarica** | **`stock: out` — every tier under $10 sold out** |
| VPSDime | Below the disk floor (30 GB, not the 60 GB recorded) |
| Fly.io | Container/microVM, not a root VPS; metered egress; strict AUP |
| Railway | Managed container PaaS; no root VM; strictest AUP class |

54 of 60 survived. **Stock is a filter, not a score, and that is a deliberate
design decision.** Advin scores 70.7 — it would sit at or near the top of the
ranking on merit. No weighting should be able to average away "you cannot buy
this", so it never reaches the scoring stage at all. The same logic that keeps
an OFAC-sanctioned host out keeps a sold-out one out.

`stock: unknown` deliberately does **not** disqualify. 54 rows have never had
their order page opened, and silently dropping them would distort the ranking
far more than admitting the gap does.

---

## How much to trust this

Honest answer: **the method is sound; the data is uneven.** That distinction
matters more than any single ranking.

| Confidence | Count | What it means |
|---|---:|---|
| **high** | 13 | Confirmed against the provider's own live page |
| medium | 14 | Confirmed against a reputable secondary source or partial fetch |
| _low_ | 33 | Plausible market figure, **not** confirmed — verify before buying |

But confidence was measuring the wrong thing. A row could be `high` confidence
because a price was read off a page, while that price was a promo, or a banner,
or a smaller plan than the one the row claimed. So there is now a second axis:

| Order path opened | Count |
|---|---:|
| Priced from the standing catalogue | **4** |
| Confirmed unbuyable (`stock: out`) | 2 |
| **Never checked at all** | **54** |

### Six checked, six wrong

On 2026-08-09 I opened the order page for six providers. Not one row survived
intact. This is the finding:

| Provider | Recorded | Reality | Error |
|---|---|---|---|
| **Advin Servers** | $6.00/mo, 4 GB/2 vCPU | **Out of stock in all 6 regions**; no such plan — $6 was a *"starting at"* banner | Unbuyable |
| **RackNerd** | $3.62/mo | **$24.59/mo** standing; annual is $295.08, i.e. no discount | **6.8×** |
| **IncogNET** | $12.00/mo, 4 GB/2 vCPU | $12 is the *2 GB* plan; the 4 GB plan is **$20.00/mo**, in stock in 3 of 6 sites | **1.7×** |
| **HostHatch** | $2.42/mo, 4 GB/50 GB | Withdrawn promo. Standing 4 GB ships 20 GB disk (fails spec); cheapest qualifying plan is **$12.00/mo** | **5.0×** |
| **VPSDime** | $7.00/mo, 4 GB/60 GB | $7 buys 6 GB but only **30 GB** — below the disk floor, now correctly filtered | Spec wrong |
| **Servarica** | $6.00/mo, 4 GB | Every tier under $10 reads **"Out of stock"**, and none is a 4 GB tier | Unbuyable |

Two failure modes, and they compound:

1. **Promo prices recorded as standing prices.** RackNerd's $3.62 and
   HostHatch's $2.42 were real offers once. They are time-limited SKUs that sell
   out and vanish, and the specs differ from the standing plan of the same name.
2. **The cheapest advertised tier priced against the reference spec's specs.**
   IncogNET's $12 belonged to a 1 vCPU / 2 GB / 30 GB plan; the row claimed
   2 vCPU / 4 GB / 60 GB. Nothing caught this, because the hard filters check
   the *recorded* specs — and the recorded specs were the right ones. Only the
   price came from a different plan.

The second is the nastier bug: **the reference spec disciplined the columns but
was never enforced against the source.** A validator cannot catch it. Only
opening the page can.

Both failure modes bias the same direction — **downward on price** — so the
original ranking was not noisy, it was systematically tilted toward whichever
providers advertise most aggressively. Advin and HostHatch did not reach the top
two by being good. They reached it by being advertised.

### What that means for the 54 unchecked rows

The Monte Carlo penalises unverified rows — low-confidence prices are perturbed
±25% per trial against ±4% for verified ones — but a ±25% error bar does not
survive contact with a **6.8× error**. The machinery was modelling the wrong
magnitude of doubt entirely.

So treat the `jod` ranking as a **queue, not a verdict**. GreenCloudVPS ($5),
PQ.Hosting ($8) and LiteServer ($5) currently outrank the recommendation, and
any of them might genuinely win once checked — or evaporate the way Advin did.
Two of the three could not be checked here: GreenCloudVPS sits behind a
Cloudflare challenge, and IncogNET's own site rejected a plain fetch.

**Run `python3 scripts/check_stock.py` before trusting any row, and buy nothing
whose order page you have not personally seen.**

### What this analysis does not establish

- **No provider was benchmarked on real hardware.** `cpu_perf`, `steal_risk` and
  `net_quality` are researched ratings, not measurements. Renting the top three
  for a month and running identical workloads would settle it properly.
- **The latency measurements are weak evidence.** They are TCP-connect times to
  *corporate websites*, most behind a CDN, not to purchasable datacenters. They
  show reachability and packet loss, nothing about your future VPS's speed. Two
  runs agreed closely on medians (Hetzner 193→227 ms, PRQ 274→272 ms), and the
  1000 ms+ jitter spikes on FlokiNET, IncogNET, 1984 and AbeloHost are TCP SYN
  retransmits — genuine loss on those paths, worth noting but not damning.
- **AUP strictness is a judgement, not a document diff.** It is drawn from
  published policies plus reported enforcement behaviour. Enforcement in
  practice varies by customer and by complaint volume.
- **FX rates are assumptions** (1 EUR = 1.09 USD). Roughly half the dataset is
  euro-priced. Update `data/profiles.json` before trusting absolute dollars.

---

## Reproduce or re-weight it yourself

Nothing here is a static opinion — disagree with the weighting and rerun it:

```bash
cd research/vps-comparison-2026

python3 scripts/validate.py                    # check the dataset first
python3 scripts/check_stock.py                 # can these still be ordered?
python3 scripts/score.py --profile jod         # the stated goal
python3 scripts/score.py --profile verified    # only rows with a confirmed price
python3 scripts/score.py --profile freedom     # if permissiveness is everything
python3 scripts/netcheck.py                    # measure latency from your network
python3 scripts/report.py                      # regenerate all tables

./scripts/run_all.sh                           # or just do all of it
```

`check_stock.py` is honest about being weak evidence: it fetches each recorded
source page and greps for sold-out language, which misses any host that renders
its catalogue client-side — Advin returns *no markers* while being entirely sold
out. Treat a hit as "go look" and a miss as "learned nothing". The part that is
load-bearing is its exit code: it **fails** if a row claiming `stock: "in"`
starts showing sold-out language, so a stale dataset cannot quietly pass.

To change what "best" means, edit the weights in
[`data/profiles.json`](data/profiles.json) and rerun. To correct a price, edit
[`data/providers.json`](data/providers.json), raise its `confidence`, and the
Monte Carlo will automatically trust it more.

---

## Sources

Order pages opened and read on 2026-08-09 (the corrections above):
[Advin cloud plans](https://advinservers.com/cloud) ·
[HostHatch products](https://hosthatch.com/products) ·
[RackNerd KVM store](https://my.racknerd.com/index.php?rp=/store/kvm-vps) ·
[IncogNET KVM](https://incognet.io/kvm-vps) ·
[VPSDime](https://vpsdime.com/) ·
[Servarica](https://servarica.com/)

Pricing and policy verified against provider pages:
[BuyVM slice plans](https://buyvm.net/kvm-dedicated-server-slices/) ·
[Hetzner Cloud](https://www.hetzner.com/cloud/) ·
[Oracle Always Free](https://docs.oracle.com/en-us/iaas/Content/FreeTier/freetier_topic-Always_Free_Resources.htm)

Corroborating research:
[VPSBenchmarks — HostHatch](https://www.vpsbenchmarks.com/hosters/hosthatch) ·
[VPSBenchmarks — Melbicom](https://www.vpsbenchmarks.com/hosters/melbicom) ·
[ServerHunter — RackNerd 4GB](https://www.serverhunter.com/offer/racknerd-4gb-kvm-vps/) ·
[KYCnot.me — IncogNET](https://kycnot.me/service/incognet) ·
[LearnWithHasan — BuyVM](https://learnwithhasan.com/self-hosting-hub/vps-providers/buyvm) ·
[Contabo pricing analysis](https://cybernews.com/best-web-hosting/contabo-review/pricing/) ·
[Netcup vs Hetzner 2026](https://www.vpsbenchmarks.com/compare/hetzner_vs_netcup) ·
[Melbicom reviews](https://hostadvice.com/hosting-company/melbicom-reviews/)

IP reputation and anti-bot behaviour:
[Why AI agents get blocked on datacenter IPs](https://www.joinmassive.com/blog/why-ai-agents-get-blocked-on-datacenter-ips-and-how-to-fix-it) ·
[Cloudflare Error 1009 / ASN blocking](https://decodo.com/blog/error-1009) ·
[Cloudflare IP Access rules](https://developers.cloudflare.com/waf/tools/ip-access-rules/) ·
[Anti-bot IP reputation](https://www.proxies.sx/data-works/anti-bot)

Permissiveness and offshore hosting:
[DMCA-ignored hosting comparison](https://www.websiteplanet.com/blog/best-dmca-ignored-hosting-services/) ·
[DMCA-ignored VPS](https://hostingrevelations.com/best-dmca-ignored-vps-hosting/) ·
[Monero / no-KYC VPS index](https://criptovps.com/best-monero-vps-2026/) ·
[Tor ISP correspondence (Hetzner)](https://trac.torproject.org/projects/tor/wiki/doc/ISPCorrespondence) ·
[DigitalOcean KYC lockouts](https://news.ycombinator.com/item?id=41662147) ·
[US Treasury — Aeza Group designation](https://home.treasury.gov/news/press-releases/sb0189)

Agent-hosting context:
[Best VPS for automation](https://cybernews.com/vps/best-vps-for-automation/) ·
[Best VPS for AI agents](https://cybernews.com/vps/best-vps-for-ai-agents/) ·
[Cheap VPS providers 2026](https://sliplane.io/blog/top-5-cheap-vps-providers) ·
[Cheapest VPS providers](https://www.hostingadvice.com/how-to/cheapest-vps-providers/)
