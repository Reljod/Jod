# Browser access

Agents need to read the web, and most of the web Jod cares about is behind
Cloudflare. This is how Jod browses without being blocked.

→ the measured groundwork: [`research/ip-blocking-2026`](../research/ip-blocking-2026/REPORT.md)

## The one thing to understand first

**Cloudflare does not check your IP first. It checks it fifth.**

```
1. TLS ClientHello  → JA3/JA4 fingerprint     ← inspected at the edge,
2. HTTP/2 SETTINGS  → frame + priority order     before your request is
3. JS challenge     → Turnstile                  even routed
4. Behavioural signals
5. IP reputation    → datacenter vs residential ASN
6. Bot Score        → composite
```

A proxy fixes **line 5 only**. An agent that fetches a page with `requests`
announces "I am not a browser" in the TLS handshake, before the IP is
considered at all — so paying for a residential IP and keeping the Python
client means paying to be blocked with a nicer address.

| Approach | Layers fixed |
|---|---|
| Datacenter proxies | 0 / 6 |
| Residential / ISP proxies alone | 1 / 6 |
| Stealth browser alone | 3.7 / 6 |
| **Stealth browser + ISP proxy** | **5 / 6** |
| Paid unblocker API | 5.7 / 6 |

Neither half works alone. The answer is the stack — and the study calls the
proxy-alone belief the single most expensive misconception in this market.

## What is installed

| Piece | Where | Status |
|---|---|---|
| Camoufox 0.5.4 (patched Firefox 152) | `~/.cache/camoufox` | **installed** |
| Python venv + Playwright 1.60 | `~/.jod/browser-venv` | **installed** |
| Firefox runtime libs (`libgtk-3-0` et al) | system | **installed** |
| Wrapper | [`browser/jodbrowser.py`](../browser/jodbrowser.py) | **installed** |
| Webshare ISP proxy | `~/.jod/browser.env` | **needs credentials** |

Verified working headless: loads pages, reports a Windows Firefox user agent,
and `navigator.webdriver` is `false`.

## Using it

```sh
~/.jod/browser-venv/bin/python browser/jodbrowser.py https://example.com
~/.jod/browser-venv/bin/python browser/jodbrowser.py https://example.com --html
~/.jod/browser-venv/bin/python browser/jodbrowser.py https://example.com --screenshot /tmp/shot.png
```

It prints `innerText` by default rather than the DOM, because an agent reading
the output pays for every token and the markup is rarely what it needs.

Agents reach it by running that command. Nothing in `jod-core` knows about the
browser — it stays a tool an agent may use, not a capability Jod itself has.

## Wiring the proxy

The box currently egresses from its own datacenter ASN, which is the single
worst thing about its reputation. Fix it by putting credentials in
`~/.jod/browser.env` (mode `600`, never committed):

```sh
JOD_PROXY_SERVER=http://p.webshare.io:80
JOD_PROXY_USERNAME=<username>
JOD_PROXY_PASSWORD=<password>
```

Then confirm the exit address changed:

```sh
~/.jod/browser-venv/bin/python browser/jodbrowser.py https://ipinfo.io/json
```

The `org` field should no longer name a hosting company.

Credentials are read from that file or the environment and never passed on the
command line, because argv is world-readable through `/proc`.

### Buy the *static ISP* plan, not "Webshare Residential"

Webshare sells three products and the names are misleading. The one to buy is
**Webshare Static Residential (ISP)**, 10 IPs at $0.30/IP/month:

| Webshare product | Cost/mo | Rank of 50 | Note |
|---|---:|---:|---|
| **Static Residential (ISP)** | **$3.00 flat** | **#3** (in the stack) | buy this |
| Residential (rotating) | ~$15.00 metered | #46 | 5× the price, 43 places worse |
| Datacenter | — | filtered out | fixes 0 of 6 layers |

ISP IPs are datacenter-hosted but registered to consumer ISPs, so they **resolve
as residential on the ASN lookup Cloudflare performs** while staying stable and
unmetered. Rotating residential is metered per GB and rises with every page
fetched, for a *worse* result — it still fixes only the IP layer.

Rotating earns its premium only when you need many distinct identities at once,
or when per-IP rate limits are the binding constraint. Jod is one person's
assistant: it needs to look like one consistent person, which is the opposite
requirement. Static IPs are also sticky by construction, which the workload
needs — roughly a fifth of fetches want a persistent session.

Sourcing matters too. Proxy pools are ranked by how the IPs were obtained, and
June 2026 reporting found proxy SDKs embedded in free Smart TV apps whose
consent dialogs were written by the app publisher. Static ISP sits in the
cleanest tier; running the study's `ethical` weighting changes the winner not
at all.

## How good is this, really

**~88% is a composed estimate, not a measurement.** The study benchmarked
Camoufox alone (72%) and ISP proxies alone (62%), then reasoned about the
pairing — no benchmark tested the two together. The study says so plainly, and
calls the recommended stacks "simultaneously my central recommendation and my
least certain numbers." 60% of its rows are low-confidence, and it declares a
shelf life of about six months from 2026-08-08.

Treat the number as a plan, not a promise. The settling experiment it names is
cheap and worth doing once credentials exist: **pick the ten sites Jod actually
needs, run this against them for a week, and count.** That replaces every
estimate here with a fact about the real workload.

For calibration, the one high-confidence figure in the whole study belongs to a
paid unblocker API — 98.4% in an independent 11-provider benchmark, at
$1.50/1,000 requests. Which leads to the tier below.

## The tiers — the fallback is what makes this cheap

Not every fetch needs a stealth browser, and the ~12% this stack misses does not
need to be solved by making the stack heavier.

| Target | Tool | Cost |
|---|---|---|
| Unprotected site | plain HTTP with a correct TLS fingerprint (`curl_cffi`) | free, ~50 ms |
| Cloudflare-protected | **Camoufox + ISP proxy** (this document) | ~$3/mo flat |
| Still blocked | a paid unblocker API, per request | pay only for failures |

Routing the easy majority around the browser keeps it fast, and paying per
request only for what actually failed keeps the last few percent from costing
more than everything else combined. Neither tier is installed yet; the middle
one is.

## `geoip` is not optional with a proxy

When a proxy is configured the wrapper sets `geoip=True`, which matches the
browser's locale, timezone and geolocation to where the proxy exits.

Without it the browser claims to be in one country while its packets arrive
from another. That contradiction is *cheaper* to detect than any of the things
the proxy was bought to hide — so an unconfigured `geoip` turns a $3 proxy into
a stronger signal than no proxy at all.

## Do not install these

They are the common answers and they are worse than nothing:

- **`playwright-stealth` / `puppeteer-extra-stealth`** — deprecated since
  February 2025 and down to 18% success. Its JavaScript monkey-patches are
  themselves a detection signal now. It is still the top search result.
- **Stock Playwright or Puppeteer from the VPS** — 12%. A plain HTTP client is 3%.
- **Datacenter proxies** — 0 of 6 layers. One datacenter ASN is no better than
  another.
- **Tor** — actively worse than doing nothing. It moves you from "datacenter" to
  "known anonymizer", which Cloudflare challenges by default.

The reason Camoufox is the pick over JavaScript-patching tools is the same
reason those fail: it patches Firefox at the C++ level, below the layer where
the patches themselves become the fingerprint.

## Still to do: give the browser its own sandbox

The browser is the part of Jod most likely to be attacked, because a prompt
injection on a scraped page becomes input to a trusted process. The OS cannot
prevent that; it decides how far the damage goes.

The [host research](../research/agent-host-os-2026/REPORT.md) is specific, and
none of it is done yet:

- Run agents as a non-root user that is **not** in `sudo`. The agent must never
  be one `sudo` away from root.
- A hardened systemd unit — `ProtectSystem=strict`, empty
  `CapabilityBoundingSet`, `SystemCallFilter=@system-service`, and an
  `IPAddressAllow` egress allowlist. Target a `systemd-analyze security` score
  in the low single digits.
- **The browser gets its own unit.** An egress allowlist cannot survive a worker
  that must reach arbitrary sites, and the answer is a second unit with its own
  wider policy — not widening the orchestrator's to `any` to make one job work.

Two omissions there are deliberate and should not be "fixed":
`RestrictNamespaces=~CLONE_NEWUSER`, because browser sandboxes need user
namespaces, and `MemoryDenyWriteExecute=yes`, because JIT compilation breaks
under W^X.

Related and already visible: this box has
`kernel.apparmor_restrict_unprivileged_userns=1`, and Camoufox logs
`CanCreateUserNamespace() unshare(CLONE_NEWPID): EPERM` at startup. It runs
anyway, with its own sandbox degraded. The documented fix is an AppArmor profile
granting `userns` to the browser binary — never `--no-sandbox`, which is
strictly worse.

## What this does not do

- **It is not an unblocker API.** Sites with server-side Turnstile or aggressive
  behavioural scoring will still refuse.
- **It does not make scraping polite.** Rate limits, robots.txt and terms of
  service are unaffected by any of this.
- **It shares one identity.** Every agent using it looks like the same person,
  which is correct for a personal assistant and wrong for anything else.
