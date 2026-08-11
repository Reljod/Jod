#!/usr/bin/env python3
"""Survey the candidate crates on crates.io.

Answers two questions the report needs evidence for: is a crate maintained
(last release date), and how widely is it used (recent downloads). Run:

    python3 research/transports-2026/bench/crate_survey.py
"""

import json
import urllib.request

CRATES = [
    "hmac",
    "sha2",
    "subtle",
    "constant_time_eq",
    "ring",
    "hex",
    "teloxide",
    "teloxide-core",
    "frankenstein",
    "telegram-bot-raw",
    "telegram-bot",
    "reqwest",
    "ureq",
    "hyper-rustls",
    "rustls",
]


def fetch(name):
    url = f"https://crates.io/api/v1/crates/{name}"
    req = urllib.request.Request(url, headers={"User-Agent": "jod-research/0.1"})
    with urllib.request.urlopen(req, timeout=30) as fh:
        return json.load(fh)


def main():
    print(f"{'crate':<18} {'latest':<14} {'released':<12} {'recent 90d':>12} {'all-time':>14}")
    for name in CRATES:
        try:
            data = fetch(name)
        except Exception as exc:  # network is the only failure that matters here
            print(f"{name:<18} ERROR {exc}")
            continue
        crate = data["crate"]
        live = [v for v in data["versions"] if not v["yanked"]]
        latest = live[0] if live else {"num": "-", "created_at": "-"}
        print(
            f"{name:<18} {latest['num']:<14} {latest['created_at'][:10]:<12} "
            f"{crate.get('recent_downloads') or 0:>12,} {crate['downloads']:>14,}"
        )


if __name__ == "__main__":
    main()
