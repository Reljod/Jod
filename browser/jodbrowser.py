"""Camoufox behind a proxy, configured once and shared.

Why this exists: an agent that fetches a page with `requests` announces "I am
not a browser" in the TLS handshake, before its IP is ever considered. Routing
that through a residential proxy buys a better-looking IP attached to the same
instantly-detectable client. The proxy fixes one detection layer out of six;
the browser fingerprint is the load-bearing half.

So: a real Firefox build with patched fingerprints, egressing through a static
ISP proxy. → research/ip-blocking-2026/REPORT.md

Configuration is environment-only, read from ~/.jod/browser.env if present:

    JOD_PROXY_SERVER    http://p.webshare.io:80   (omit to browse direct)
    JOD_PROXY_USERNAME
    JOD_PROXY_PASSWORD
    JOD_PROXY_GEOIP     1 by default; 0 disables

Credentials are never passed on the command line, because argv is world-
readable through /proc on a shared box.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

ENV_FILE = Path.home() / ".jod" / "browser.env"


def load_env(path: Path = ENV_FILE) -> None:
    """Read KEY=value lines. Anything already in the environment wins."""
    if not path.exists():
        return
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        os.environ.setdefault(key.strip(), value.strip().strip("'\""))


def proxy_config() -> dict | None:
    """The proxy Camoufox should egress through, or None to browse direct.

    Loads the env file itself rather than trusting a caller to have done it.
    It used to depend on `browser_options()` having run first, which made the
    answer depend on call order: `describe()` before the first fetch reported
    "direct" while the very next fetch went through the proxy. Harmless in the
    one-shot CLI, where `browser_options()` always ran first — and a lie in the
    MCP server's `browser_status`, whose entire job is to say whether traffic is
    proxied. `load_env` uses `setdefault`, so this is idempotent and a real
    environment variable still wins.
    """
    load_env()
    server = os.environ.get("JOD_PROXY_SERVER", "").strip()
    if not server:
        return None
    proxy = {"server": server}
    username = os.environ.get("JOD_PROXY_USERNAME", "").strip()
    password = os.environ.get("JOD_PROXY_PASSWORD", "").strip()
    if username:
        proxy["username"] = username
    if password:
        proxy["password"] = password
    return proxy


def browser_options() -> dict:
    """Arguments for `Camoufox(**opts)`."""
    load_env()
    proxy = proxy_config()
    opts: dict = {"headless": True}
    if proxy:
        opts["proxy"] = proxy
        # Match locale, timezone and geolocation to where the proxy exits.
        # Without this the browser claims a fingerprint from one country while
        # its packets arrive from another — a contradiction that is cheaper to
        # detect than any of the things the proxy was bought to hide.
        opts["geoip"] = os.environ.get("JOD_PROXY_GEOIP", "1") != "0"
    return opts


def describe() -> str:
    proxy = proxy_config()
    if not proxy:
        return "direct (no proxy configured — the VPS IP is exposed)"
    who = proxy.get("username", "anonymous")
    return f"{proxy['server']} as {who}"


def main(argv: list[str]) -> int:
    """Fetch one URL and print it. Agents call this; it needs no library."""
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        print(f"\nproxy: {describe()}", file=sys.stderr)
        print("\nusage: jodbrowser.py <url> [--html] [--screenshot PATH]", file=sys.stderr)
        return 2

    url = argv[1]
    want_html = "--html" in argv
    shot = None
    if "--screenshot" in argv:
        i = argv.index("--screenshot")
        if i + 1 >= len(argv):
            print("--screenshot needs a path", file=sys.stderr)
            return 2
        shot = argv[i + 1]

    from camoufox.sync_api import Camoufox

    print(f"[jod] via {describe()}", file=sys.stderr)
    with Camoufox(**browser_options()) as browser:
        page = browser.new_page()
        page.goto(url, timeout=90_000, wait_until="domcontentloaded")
        if shot:
            page.screenshot(path=shot, full_page=True)
            print(f"[jod] screenshot → {shot}", file=sys.stderr)
        if want_html:
            print(page.content())
        else:
            # innerText rather than the DOM: an agent reading this pays for
            # every token, and the markup is almost never the thing it needs.
            print(page.evaluate("document.body.innerText"))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
