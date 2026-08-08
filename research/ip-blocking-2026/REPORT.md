# The IP problem — how to stop Cloudflare blocking Jod

**Date:** 2026-08-08 · **Analyst:** Jod · **Method:**
[`compare-options`](../../.agents/skills/compare-options/SKILL.md) ·
**Numbers:** [`out/RANKINGS.md`](out/RANKINGS.md)

> Follow-on from the [VPS study](../vps-comparison-2026/REPORT.md), which found
> that the server is not the constraint — the IP address is. This one asks what
> to actually do about it. **55 solutions compared.**

---

## The premise was half wrong, and that matters

The VPS report told you to route Jod's browsing through a residential proxy.
That advice was incomplete in a way worth correcting, because acting on it
alone would have cost you money and still got you blocked.

**Cloudflare does not check your IP first. It checks it fifth.**

```
1. TLS ClientHello  → JA3/JA4 fingerprint     ← inspected at the edge,
2. HTTP/2 SETTINGS  → frame + priority order     before your request is
3. JS challenge     → Turnstile                  even routed
4. Behavioural signals
5. IP reputation    → datacenter vs residential ASN
6. Bot Score (0-99) → composite; >30 gets challenged
```

A residential proxy fixes **line 5 only**. If Jod calls a protected site with
Python's `requests`, the TLS handshake announces "I am not a browser" before
the IP is ever considered — so a $8/GB residential proxy buys you a
better-looking IP attached to the same instantly-detectable client. You pay,
and you are still blocked.

This is the single most expensive misconception in this market, and the
data shows it plainly:

| Category | Layers fixed | What that means |
|---|---:|---|
| Datacenter proxies | **0 / 6** | Swapping one datacenter ASN for another changes nothing |
| Residential / ISP / mobile proxies | **1 / 6** | The IP only — the handshake still gives you away |
| Self-hosted stealth browsers | **3.7 / 6** | The client fingerprint — but from your flagged IP |
| Managed browser infra | **4.6 / 6** | Client fingerprint managed, IP often billed separately |
| Unblocker APIs | **5.7 / 6** | The whole chain, priced per request |

Neither half works alone. **The answer is a stack.**

---

## The answer

**Run Camoufox (or Patchright) on Jod's VPS, egressing through 10 static ISP
proxy IPs. About $3/month.**

That covers 5 of the 6 layers — correct browser TLS and HTTP/2 fingerprints, JS
challenges, plausible behaviour, and a residential-looking IP — for roughly
half what the VPS itself costs.

| | Cost/mo | Success | Layers | Top-5 stability |
|---|---:|---:|---:|---:|
| **Patchright + Decodo ISP** | **$2.70** | ~84% | 5/6 | **92%** |
| **Camoufox + Webshare ISP** | **$3.00** | ~88% | 5/6 | 77% |
| Camoufox + home tunnel | $0.00 | ~93% | 5/6 | 92% |
| Bright Data Web Unlocker | $15.00 | **98%** | **6/6** | 28% |
| Residential proxy alone (IPRoyal) | $24.50 | ~72% | 1/6 | 0% |

**Pick between the two paid stacks on integration cost, not price:**

- Already driving Playwright? → **Patchright + Decodo ISP ($2.70)**. It is a
  drop-in fork; you change an import and add a proxy string.
- Starting fresh, or Patchright is getting caught? → **Camoufox + Webshare ISP
  ($3.00)**. It patches Firefox at the C++ level rather than injecting
  JavaScript, which is why it outperforms CDP-patched Chrome forks against
  Cloudflare Enterprise in the 651-verdict benchmark.

### Why ISP proxies and not residential

This is the cost finding of the study. ISP ("static residential") IPs are
registered to consumer ISPs but hosted in datacenters — so they **read as
residential on the ASN lookup that Cloudflare performs**, while being stable,
unmetered, and fully consented.

Ten of them cost **$2.70–$3.00/month flat**. Five GB through a rotating
residential pool costs **$24.50–$40** and rises with every page you fetch.

Rotating residential is worth its premium when you need many distinct
identities or per-IP rate limits are the binding constraint. For an assistant
browsing on your behalf, ten stable IPs is the right shape and roughly a tenth
of the price.

---

## When to reach for an unblocker API instead

The stack tops the balanced ranking, but it does **not** win the
"never get blocked" profile. That goes to **Bright Data Web Unlocker at 98.44%**
— the highest independently-measured success rate in the field, and the only
option covering all six layers including CAPTCHA solving.

At this workload it costs **$15/month** for 10,000 fetches, pay-per-success.

So the honest recommendation is tiered, not singular:

```
Jod needs a page
  │
  ├─ unprotected site ─────→ curl_cffi direct from the VPS   (free, ~50 ms)
  │                          correct TLS, no browser at all
  │
  ├─ Cloudflare-protected ─→ Camoufox/Patchright + ISP proxy  (~$3/mo)
  │                          handles the ~85% case
  │
  └─ still blocked ────────→ Bright Data Web Unlocker         ($1.50/1k)
                             pay only for what the stack couldn't get
```

The fallback is what makes this cheap. You are not buying 98% reliability for
every request — you are buying it for the fraction that fails, which is where
per-success pricing genuinely helps.

---

## Findings that will save you money

**Plan floors dominate at this volume.** ZenRows measures well (~95%) but
carries a **$69.99/month minimum** against $46.20 of actual usage — so you pay
a 51% premium for capacity you don't use, making it *more expensive than the
98%-scoring Bright Data option at $15*. ScrapFly and Scrape.do sit at $30
floors, ScraperAPI and Oxylabs at $49. **Scrappey and Bright Data have no
floor**, which is why they lead the API tier for a 10k-request workload. At
1M requests/month this table would invert completely.

**Beware multiplier pricing.** ZenRows' advertised request counts carry a **5×
multiplier for JavaScript rendering and 10× for premium proxies** — and
Cloudflare-protected pages need both. A "1M request" plan can be 100,000 real
fetches. Always price the multiplied rate.

**The best-known managed browser is the worst-performing one measured.**
Browserbase scored **42%** on the Browser Use stealth benchmark across 71
anti-bot-protected sites — against 81% for Browser Use Cloud on the identical
test. Browserbase also bills proxies separately on top of browser-hours. It is
excellent session infrastructure; it is not an anti-bot solution.

**Tor is worse than doing nothing.** It moves your IP from "datacenter" to
"known anonymizer", a category Cloudflare challenges by default. Filtered out
as counterproductive.

**`playwright-stealth` is deprecated and now self-defeating.** Maintenance
stopped in February 2025, and its JavaScript monkey-patches are themselves a
detection signal. It remains the top search result for this problem — it is the
most common wrong answer, which is why it's in the dataset as a disqualified row.

**Datacenter proxies solve nothing here.** At $0.05/IP they are the cheapest
thing that looks like a solution and they fix **zero** layers. Cloudflare
scores the ASN, and one datacenter ASN is no better than another.

---

## The ethics question you should answer deliberately

Residential proxy pools get their IPs from real people's home connections. How
those people were asked varies enormously, and it is not a footnote — it
determines whether Jod's traffic is riding on a connection whose owner
understood the deal.

In **June 2026**, reporting showed SDKs shipped inside free Smart TV apps
turning consumer devices into scraping proxies. The consent dialogs were
written by the app publisher, not the proxy network, and typically said
something like "allow the app to use your device's free resources" — never that
third-party scraping traffic would exit through the viewer's home IP. Bright
Data states its SDK is fully consent-based and says it terminated two partner
developers supplying over 10% of its peer network.

The cleaner sourcing models, in rough order:

1. **Your own connection** (home tunnel) — perfect consent by construction
2. **ISP proxies** — datacenter-hosted, ISP-registered; no third party's home involved
3. **Direct-payment pools** (IPRoyal Pawns, PacketStream) — users opt in explicitly and are paid
4. **ISP-partnership sourcing** (NetNut) — commercial agreements, not consumer SDKs
5. **Bundled-SDK pools** — consent mediated by an unrelated app's dialog

**The recommended stack sits in the top two.** That is not a coincidence — ISP
proxies are cheaper *and* cleaner, so this is one of the rare cases where the
ethical option is also the frugal one. Running the `ethical` profile changes
the winner not at all, which is the strongest form of that claim.

---

## Four questions, four winners

| Rank | Jod's goal | Pure cost | Never blocked | Consented IPs only |
|---:|---|---|---|---|
| 1 | **Camoufox + home tunnel** ($0) | curl_cffi ($0) | **Bright Data Unlocker** ($15) | **Camoufox + home tunnel** ($0) |
| 2 | **Patchright + Decodo ISP** ($3) | Camoufox ($0) | ScrapFly ($30) | **Camoufox + Webshare ISP** ($3) |
| 3 | Camoufox + Webshare ISP ($3) | Patchright ($0) | ZenRows ($70) | Patchright + Decodo ISP ($3) |
| 4 | Patchright ($0) | nodriver ($0) | Scrape.do ($30) | Camoufox ($0) |
| 5 | Camoufox ($0) | rebrowser ($0) | Oxylabs Unblocker ($49) | home tunnel ($0) |

The stacks top three of four columns. That convergence — cheapest, most
ethical, and best balanced all pointing the same way — is why the
recommendation is a stack rather than a product.

**A note on the home tunnel.** It tops two columns and I am *not* leading with
it. Routing Jod's egress through your house gives a genuinely residential IP
for free, but it is one IP (rate-limitable and trivially correlatable), it
runs at your home uplink's speed, it may breach your ISP's terms, and it ties
your assistant's ability to browse to your router staying up. Excellent as a
free experiment or a fallback; a fragile foundation for something you depend on.
The $3 ISP option removes all four problems.

---

## How much to trust this

**The layer analysis is the solid part. The success percentages are the soft part.**

| Confidence | Rows | Share |
|---|---:|---:|
| **high** | 2 | 4% |
| medium | 20 | 36% |
| _low_ | 33 | 60% |

That is a weaker evidence base than the VPS study, and the reason is structural:
anti-bot success rates are **adversarial and perishable**. A number measured in
March may be wrong in August because Cloudflare shipped a detection update.
Vendors publish their own benchmarks, on targets they chose, and every one of
them leads its category by its own measurement.

So the Monte Carlo here perturbs **success rate rather than price** — in this
market prices are published and stable while pass rates are the contested
claim. Vendor self-reports are marked `low` and perturbed ±25%.

### What this analysis does not establish

- **Nothing was tested.** Every success figure is reported by a vendor or a
  third-party benchmark, not measured by me against your actual targets. The
  next step that would settle it: pick your ten real target sites, run
  Camoufox+ISP against them for a week, and count.
- **The stack success rates are composed, not measured.** ~84-88% comes from
  combining a stealth-browser benchmark with an IP-reputation assumption. **No
  benchmark tested these pairings.** They are simultaneously my central
  recommendation and my least certain numbers — flagged `composed-estimate` in
  the dataset and perturbed hardest.
- **The two halves of the dataset aren't measured the same way.** Stealth-browser
  benchmarks generally run from datacenter IPs; proxy vendors quote rates
  assuming a competent client. Comparing them directly slightly flatters the
  proxies and slightly penalises the browsers.
- **Success rate is not uniform across sites.** An 85% average hides the fact
  that one target may be 100% and another 0%. Your specific targets dominate.
- **This decays fast.** Treat anything here as stale after ~6 months and re-run
  it. The dataset and scripts exist so that re-running is cheap.

---

## Reproduce or re-weight

```bash
cd research/ip-blocking-2026

python3 ../../.agents/skills/compare-options/scripts/validate.py .
python3 ../../.agents/skills/compare-options/scripts/score.py . --all-profiles
python3 report.py                      # regenerate every table
```

Disagree with the weighting? Edit `data/profiles.json`. Think a success rate is
wrong? Edit `data/dataset.json`, set the `confidence` honestly, and the Monte
Carlo adjusts how much it trusts you.

---

## Sources

Detection mechanics:
[How Cloudflare detects bots](https://scrapfly.io/blog/posts/how-cloudflare-detects-bots) ·
[Bot detection in 2026 — JA4 & HTTP/2](https://krowdev.com/article/bot-detection-2026/) ·
[Cloudflare Bot Management, Bot Score & JA4](https://brixio.io/blog/cloudflare-bot-management-production-guide/) ·
[JA4 fingerprinting](https://webdecoy.com/blog/ja4-fingerprinting-ai-scrapers-practical-guide/) ·
[Cloudflare IP Access rules](https://developers.cloudflare.com/waf/tools/ip-access-rules/) ·
[Error 1009 / ASN blocking](https://decodo.com/blog/error-1009)

Benchmarks:
[Anti-detect browser benchmark — 7 tools, 31 Cloudflare targets, 651 verdicts](https://ianlpaterson.com/blog/anti-detect-browser-benchmark-patchright-nodriver-curl-cffi/) ·
[Browser Use stealth benchmark, 71 sites](https://browser-use.com/posts/web-scraping-guide-2026) ·
[Scrape.do 11-provider benchmark](https://scrape.do/blog/zenrows-alternatives/) ·
[ZenRows benchmark](https://www.zenrows.com/blog/best-web-scraping-apis-in-2026-benchmarked/) ·
[Independent scraping-API benchmarks](https://webscraping.cc/)

Tooling:
[curl-impersonate](https://scrapfly.io/blog/posts/curl-impersonate-scrape-chrome-firefox-tls-http2-fingerprint) ·
[curl_cffi & tls-client](https://www.jibaoproxy.com/blog/bypass-tls-fingerprinting-curl-cffi.html) ·
[Playwright stealth alternatives](https://humanbrowser.cloud/blog/playwright-stealth-not-working-2026) ·
[Bypassing Cloudflare in 2026](https://scrapfly.io/blog/posts/how-to-bypass-cloudflare-anti-scraping) ·
[Browser infrastructure comparison](https://apiscout.dev/guides/browserbase-vs-steel-vs-hyperbrowser-browser-infrastructure-2026)

Proxy pricing:
[Residential proxy pricing per GB](https://proxidize.com/blog/residential-proxy-pricing/) ·
[Static residential (ISP) proxies](https://aimultiple.com/isp-proxies) ·
[Oxylabs ISP proxies](https://oxylabs.io/products/isp-proxies) ·
[Proxy pricing comparison](https://aimultiple.com/proxy-pricing) ·
[Cheapest residential proxies](https://dataimpulse.com/blog/cheapest-proxies/) ·
[Residential proxy providers compared](https://aimultiple.com/residential-proxy-providers)

Sourcing ethics:
[Free apps turning Smart TVs into scraping proxies](https://thehackernews.com/2026/06/free-apps-are-quietly-turning-smart-tvs.html) ·
[Hidden nodes: AI scraping SDKs as attack vectors (CSA)](https://labs.cloudsecurityalliance.org/research/ai-data-supply-chain-residential-proxy-risk-v1-0-csa-styled/) ·
[Where residential proxy IPs come from](https://dataimpulse.com/blog/where-do-residential-proxies-come-from/) ·
[Web scraping ethics checklist](https://www.privateproxyreviews.com/web-scraping-ethics/) ·
[Why AI agents get blocked on datacenter IPs](https://www.joinmassive.com/blog/why-ai-agents-get-blocked-on-datacenter-ips-and-how-to-fix-it) ·
[Why mobile IPs pass anti-bot](https://www.proxies.sx/blog/why-cloudflare-blocks-residential-proxies-mobile-ips-difference)
