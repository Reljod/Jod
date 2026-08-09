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

Neither half works alone. The answer is the stack.

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

### Buy ISP, not rotating residential

This is the cost finding of the study, and it inverts the obvious choice.

| | Cost/mo | Success | Layers |
|---|---:|---:|---|
| **Camoufox + Webshare ISP** | **~$3.00** | ~88% | 5/6 |
| Camoufox + home tunnel | $0.00 | ~93% | 5/6 |
| Residential proxy alone | $24.50 | ~72% | 1/6 |

ISP ("static residential") IPs are datacenter-hosted but registered to consumer
ISPs, so they **resolve as residential on the ASN lookup Cloudflare performs**
while staying stable and unmetered. Ten of them cost about $3/month flat. A
metered rotating-residential pool costs $24.50–$40 and rises with every page
fetched — for a *worse* success rate, because it fixes only the IP layer.

Rotating residential earns its premium only when you need many distinct
identities at once. Jod is one person's assistant; it needs to look like one
consistent person, which is the opposite requirement.

## `geoip` is not optional with a proxy

When a proxy is configured the wrapper sets `geoip=True`, which matches the
browser's locale, timezone and geolocation to where the proxy exits.

Without it the browser claims to be in one country while its packets arrive
from another. That contradiction is *cheaper* to detect than any of the things
the proxy was bought to hide — so an unconfigured `geoip` turns a $3 proxy into
a stronger signal than no proxy at all.

## What this does not do

- **It is not an unblocker API.** ~88% success, not 100%. Sites with server-side
  Turnstile or aggressive behavioural scoring will still refuse.
- **It does not make scraping polite.** Rate limits, robots.txt and terms of
  service are unaffected by any of this.
- **It shares one identity.** Every agent using it looks like the same person,
  which is correct for a personal assistant and wrong for anything else.
