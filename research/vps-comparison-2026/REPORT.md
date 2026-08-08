# The best VPS for hosting Jod — 60 providers, analysed

**Date:** 2026-08-08 · **Analyst:** Jod · **Method:** scripted, reproducible ·
**Numbers:** [`out/RANKINGS.md`](out/RANKINGS.md)

> **Goal, as stated:** a VPS to host the Jod LLM-agent orchestrator — free to do
> anything on the internet, no compliance friction, good availability, good
> internet access, good global location coverage, and the cheapest of them all.

---

## The answer

**Buy [Advin Servers](https://advinservers.com/) — $6.00/mo, 4 GB / 2 vCPU / 60 GB NVMe, 6 locations.**

Take the 8 GB tier if Jod will run more than two headless Chrome instances.

The model's top-scoring option is actually HostHatch at **$2.42/mo**, and it is a
genuinely excellent number. I am recommending the $6 option over the $2.42 one,
and you should know exactly why before you accept that:

| | HostHatch | Advin Servers |
|---|---|---|
| Score / stability | **73.0** · 100% top-5 | 71.0 · **100% top-5** |
| Price | **$2.42/mo** ($29/yr) | $6.00/mo |
| Billing | **Annual prepay only** | Monthly, no lock-in |
| CPU | 0.5 dedicated + 1.5 shared | 2 vCPU AMD, ECC RAM |
| Port | 10 Gbps | 10 Gbps |
| Locations | 7 (compute) | 6 |
| Known weakness | **Support responsiveness** | Small operator |

Both sit at 100% top-5 stability across 20,000 perturbed trials, so choosing the
second costs you nothing in robustness — it costs $43/year. What that buys is
monthly billing instead of a year prepaid to a host whose most consistent
criticism is that support does not answer, plus two real vCPUs with ECC memory
instead of half a dedicated core. For the machine that *is* your assistant, I
would rather be able to walk away in 30 days.

**If the $43/year matters more than that, take HostHatch.** It is the correct
answer to "cheapest that isn't a trap", and its 3-year prepay doubles RAM, disk
and bandwidth — 8 GB for around $2.42/mo equivalent is the single best value on
this list.

### Runners-up, by what you might actually want

| If you want… | Buy | Why |
|---|---|---|
| **Maximum freedom, no questions asked** | **IncogNET** — $12/mo | Requires no personally identifiable information, takes Monero, 6 US/EU locations, AMD EPYC. Scored **100/100** on permissiveness. Independently listed as no-KYC. |
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

Meanwhile the small, boring hosts — **Advin, Webdock, 1984, IncogNET, Melbicom,
Servarica** — have comparatively clean ranges purely because nobody has abused
them at scale yet.

**So: do not plan for Jod to browse the open web from its own VPS IP.** No
provider choice fixes this. The working architecture is:

```
Jod orchestrator on a cheap, permissive VPS   ← this report picks the box
        │
        ├── LLM API calls ────────────→ direct from the VPS (never blocked)
        ├── Your own services/webhooks → direct from the VPS
        └── Open-web browsing/scraping → through a residential/mobile proxy
```

Budget roughly $6/mo for the box and expect proxy costs to exceed it if Jod does
serious browsing. **The VPS is the cheap part of this problem.** Picking a $2.42
host over a $6 host is noise next to getting the egress path right.

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

## Four different questions, four different winners

This is the most useful output of the whole exercise. The scripts ran the same
60 providers through four weightings:

| Rank | Your stated goal | Pure cost | Max permissiveness | Reliability-first |
|---:|---|---|---|---|
| 1 | **HostHatch** ($2) | Oracle Cloud ($0) | **BuyVM** ($30) | **UpCloud** ($18) |
| 2 | **Advin Servers** ($6) | HostHatch ($2) | FlokiNET ($24) | phoenixNAP ($30) |
| 3 | IncogNET ($12) | VirMach ($4) | AbeloHost ($16) | Latitude.sh ($110) |
| 4 | GreenCloudVPS ($5) | RackNerd ($4) | Shinjiru ($25) | Vultr ($20) |
| 5 | Melbicom ($13) | Contabo ($5) | OrangeWebsite ($20) | Hivelocity ($12) |

No provider appears in all four columns. There is no universally best VPS, only
a best one for a stated weighting — which is why the weights are a data file you
can edit rather than a judgement buried in prose.

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
| Fly.io | Container/microVM, not a root VPS; metered egress; strict AUP |
| Railway | Managed container PaaS; no root VM; strictest AUP class |

57 of 60 survived. The three that didn't are all providers that would otherwise
have scored respectably — which is exactly why the filter exists rather than
being a low weight.

---

## How much to trust this

Honest answer: **the method is sound; the data is uneven.** That distinction
matters more than any single ranking.

| Confidence | Count | What it means |
|---|---:|---|
| **high** | 8 | Confirmed against the provider's own live page |
| medium | 17 | Confirmed against a reputable secondary source or partial fetch |
| _low_ | 35 | Plausible market figure, **not** confirmed — verify before buying |

Verification during this research **changed the ranking four times**, which is
the strongest available evidence that unverified rows are not safe to trust:

- HostHatch went from an assumed $6.00/mo to a verified **$2.42/mo** — and from
  15 locations to **7** for compute. It moved to #1 *and* got worse on reach.
- RackNerd went from $2.74 to **$3.62**, but gained 3 locations (12, not 9) and
  a third vCPU.
- BuyVM's qualifying plan went from $15 to **$30** once the 1-core detail
  surfaced, dropping it from #4 to #12.
- Advin went from $5.00 to **$6.00**, with a 10 Gbps port confirmed.

The Monte Carlo already penalises unverified rows: low-confidence prices are
perturbed ±25% per trial against ±4% for verified ones, so an unverified bargain
must survive being wrong about its own price before it can rank. That is why
HostHatch and Advin hold 100% top-5 stability while GreenCloudVPS — nominally
4th — holds only 35%.

**Before you buy anything below the top two, confirm the live price yourself.**

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
python3 scripts/score.py --profile jod         # the stated goal
python3 scripts/score.py --profile freedom     # if permissiveness is everything
python3 scripts/netcheck.py                    # measure latency from your network
python3 scripts/report.py                      # regenerate all tables

./scripts/run_all.sh                           # or just do all of it
```

To change what "best" means, edit the weights in
[`data/profiles.json`](data/profiles.json) and rerun. To correct a price, edit
[`data/providers.json`](data/providers.json), raise its `confidence`, and the
Monte Carlo will automatically trust it more.

---

## Sources

Pricing and policy verified against provider pages:
[Advin Servers](https://advinservers.com/) ·
[BuyVM slice plans](https://buyvm.net/kvm-dedicated-server-slices/) ·
[RackNerd](https://www.racknerd.com/) ·
[HostHatch](https://hosthatch.com/) ·
[IncogNET KVM](https://incognet.io/kvm-vps) ·
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
