#!/usr/bin/env python3
"""Re-check whether the plans in providers.json can still be ordered.

The first version of this study scored advertised prices without ever opening an
order page. Two of its top three picks were unbuyable: Advin Servers showed "Out
of Stock" on every plan in all six regions, and HostHatch's headline price was a
withdrawn promo. Stock is also the fastest-decaying field in the dataset — a
figure verified today is a guess in a month — so it needs a script, not a
one-off manual pass.

What this does: fetch each provider's recorded source page and look for
out-of-stock language. What it deliberately does NOT do: write to the dataset.
The signal is too weak for that, for reasons worth being explicit about.

  - Many hosts render their catalogue client-side (Advin is a Nuxt app, HostHatch
    a Vue app), so the phrase lives in an XHR response this never sees. Those
    come back `no-markers` while being entirely sold out.
  - Real stock usually lives behind a cart or a panel login.
  - "Out of stock" in a FAQ or a footer is not a sold-out plan. BuyVM trips this.

So treat a hit as "go look", and a miss as "learned nothing". The one thing here
that is genuinely load-bearing is the exit code: if a row currently claiming
`stock: "in"` starts showing sold-out language, this fails, which is a check a
stale dataset cannot quietly pass.

Usage:
    python3 check_stock.py
    python3 check_stock.py --only advinservers,racknerd
    python3 check_stock.py --json ../out/stock-check.json
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DATA = os.path.join(ROOT, "data", "providers.json")

UA = ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/126.0 Safari/537.36")

# Phrases that mean "you cannot buy this right now". Kept narrow on purpose:
# "unavailable" alone matches far too much boilerplate to be useful.
SOLD_OUT = re.compile(
    r"out of stock|sold out|no available plans|currently unavailable|"
    r"get notified|notify me when|join the waitlist|back in stock",
    re.I,
)

TAGS = re.compile(r"<script.*?</script>|<style.*?</style>", re.S | re.I)


def strip_markup(raw):
    return re.sub(r"<[^>]+>", " ", TAGS.sub(" ", raw))


def page_text(url, timeout=20):
    """Fetch a page, preferring urllib and falling back to curl.

    A stock Python built without a CA bundle (the python.org macOS installer,
    unless you run its Install Certificates command) fails every HTTPS request
    with CERTIFICATE_VERIFY_FAILED. curl uses the system trust store, so it
    works where urllib does not. The fallback keeps verification ON — the point
    is to find a working trust store, not to skip the check.
    """
    try:
        req = urllib.request.Request(url, headers={"User-Agent": UA})
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, strip_markup(resp.read(2_000_000).decode("utf-8", "ignore"))
    except urllib.error.URLError as exc:
        if "CERTIFICATE_VERIFY_FAILED" not in str(exc) or not shutil.which("curl"):
            raise
    proc = subprocess.run(
        ["curl", "-sL", "--max-time", str(timeout), "-A", UA, "-w", "\n%{http_code}", url],
        capture_output=True, text=True, timeout=timeout + 10,
    )
    if proc.returncode != 0:
        raise OSError(f"curl exit {proc.returncode}: {proc.stderr.strip()[:60]}")
    body, _, code = proc.stdout.rpartition("\n")
    return int(code or 0), strip_markup(body)


def check(provider):
    sources = provider.get("sources") or []
    row = {
        "id": provider["id"],
        "name": provider["name"],
        "recorded_stock": provider.get("stock", "unknown"),
        "price_basis": provider.get("price_basis", "unknown"),
        "checked": provider.get("stock_checked"),
        "url": sources[0] if sources else None,
        "status": None,
        "markers": [],
        "result": "no-source",
    }
    if not row["url"]:
        return row

    try:
        status, text = page_text(row["url"])
    except (urllib.error.URLError, urllib.error.HTTPError, OSError, ValueError,
            subprocess.SubprocessError) as exc:
        row["result"] = "unreachable"
        row["status"] = str(exc)[:80]
        return row

    row["status"] = status
    hits = SOLD_OUT.findall(text)
    row["markers"] = sorted({h.lower() for h in hits})
    row["result"] = "sold-out-language" if hits else "no-markers"
    return row


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--only", help="comma-separated provider ids")
    ap.add_argument("--json", dest="json_out", help="write the full result here")
    ap.add_argument("--workers", type=int, default=6)
    args = ap.parse_args()

    with open(DATA) as fh:
        providers = json.load(fh)["providers"]

    if args.only:
        wanted = {s.strip() for s in args.only.split(",")}
        providers = [p for p in providers if p["id"] in wanted]

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        rows = list(pool.map(check, providers))

    rows.sort(key=lambda r: (r["result"] != "sold-out-language", r["id"]))

    print(f"{'provider':<22} {'recorded':<9} {'basis':<20} {'result':<18} markers")
    print("-" * 100)
    for r in rows:
        print(f"{r['name'][:21]:<22} {r['recorded_stock']:<9} {r['price_basis']:<20} "
              f"{r['result']:<18} {', '.join(r['markers'])[:34]}")

    checked = sum(1 for p in providers if p.get("stock_checked"))
    print()
    print(f"{len(providers)} providers · {checked} have ever had an order path checked "
          f"· {len(providers) - checked} still unknown")

    # The actual gate: a row asserting it is buyable while its own page says
    # otherwise. Everything above is advisory; this is the part that can fail.
    contradictions = [r for r in rows
                      if r["recorded_stock"] == "in" and r["result"] == "sold-out-language"]

    if args.json_out:
        path = args.json_out if os.path.isabs(args.json_out) else os.path.join(HERE, args.json_out)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as fh:
            json.dump({"rows": rows, "contradictions": [r["id"] for r in contradictions]},
                      fh, indent=2)
        print(f"wrote {path}")

    if contradictions:
        print()
        for r in contradictions:
            print(f"CONTRADICTION  {r['name']} is recorded as in stock but {r['url']} "
                  f"says: {', '.join(r['markers'])}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
