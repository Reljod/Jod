#!/usr/bin/env python3
"""Pull the load-bearing GitHub webhook facts from primary sources.

Two kinds of evidence: sentences from docs.github.com (quoted verbatim in the
report), and the live `GET /meta` response, which is what an IP allowlist would
actually have to track. `/meta` needs no credential. Run:

    python3 research/transports-2026/bench/gh_facts.py
"""

import html
import ipaddress
import json
import re
import urllib.request

UA = {"User-Agent": "jod-research/0.1", "Accept": "application/vnd.github+json"}

DOCS = [
    (
        "best practices",
        "https://docs.github.com/en/webhooks/using-webhooks/best-practices-for-using-webhooks",
        [
            "10 seconds",
            "asynchronous",
            "at least once",
            "redeliver",
            "X-GitHub-Delivery",
            "order",
        ],
    ),
    (
        "troubleshooting",
        "https://docs.github.com/en/webhooks/testing-and-troubleshooting-webhooks/troubleshooting-webhooks",
        ["10 seconds", "timed out", "redeliver"],
    ),
    (
        "validating deliveries",
        "https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries",
        ["secure_compare", "constant time", "raw", "sha256="],
    ),
]


def sentences(url, needles):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=60) as fh:
        raw = fh.read().decode("utf-8", "replace")
    text = html.unescape(re.sub(r"<[^>]+>", " ", raw))
    text = re.sub(r"\s+", " ", text)
    out = []
    for needle in needles:
        for m in re.finditer(re.escape(needle), text):
            start = text.rfind(". ", 0, m.start()) + 2
            end = text.find(". ", m.end())
            out.append((needle, text[start : end + 1 if end > 0 else len(text)].strip()))
            break
    return out


def meta():
    req = urllib.request.Request("https://api.github.com/meta", headers=UA)
    with urllib.request.urlopen(req, timeout=60) as fh:
        return json.load(fh)


def main():
    for label, url, needles in DOCS:
        print(f"\n===== {label} — {url}")
        try:
            for needle, sentence in sentences(url, needles):
                print(f"  [{needle}] {sentence[:400]}")
        except Exception as exc:
            print(f"  ERROR {exc}")

    print("\n===== GET https://api.github.com/meta")
    try:
        data = meta()
    except Exception as exc:
        print(f"  ERROR {exc}")
        return
    hooks = data.get("hooks", [])
    v4 = [c for c in hooks if ipaddress.ip_network(c).version == 4]
    v6 = [c for c in hooks if ipaddress.ip_network(c).version == 6]
    addresses = sum(ipaddress.ip_network(c).num_addresses for c in v4)
    print(f"  hooks CIDRs: {len(hooks)}  (v4 {len(v4)}, v6 {len(v6)})")
    print(f"  distinct IPv4 addresses covered: {addresses:,}")
    print(f"  v4 list: {v4}")
    print(f"  v6 list: {v6}")
    print(f"  other keys present: {sorted(k for k in data if k != 'hooks')}")


if __name__ == "__main__":
    main()
