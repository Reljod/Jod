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
    JOD_PERSIST_PROFILE 1 by default; 0 = throwaway profile per launch
    JOD_BLOCK_IMAGES    1 by default; 0 = load images (needed for screenshots
                        where pictures matter, or image search)
    JOD_BLOCK_MEDIA     1 by default; 0 = load video/audio/fonts as normal.
                        Aborts <video>/<audio>/font requests at the route
                        level, which is most of the bytes on shops like Shopee.

Credentials are never passed on the command line, because argv is world-
readable through /proc on a shared box.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

ENV_FILE = Path.home() / ".jod" / "browser.env"

# Resource types aborted at the Playwright route level when JOD_BLOCK_MEDIA=1.
# Images are handled one level lower (Firefox pref, never even requested).
BLOCKED_RESOURCE_TYPES = frozenset({"media", "font"})

# Trackers / beacons that cost proxy round-trips and never affect readable text.
BLOCKED_HOSTS = (
    "google-analytics.com",
    "googletagmanager.com",
    "facebook.net",
    "hotjar.com",
    "fullstory.com",
    "mixpanel.com",
    "segment.io",
)


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


def _flag(name: str, default: str = "1") -> bool:
    load_env()
    return os.environ.get(name, default).strip() != "0"


def block_images() -> bool:
    return _flag("JOD_BLOCK_IMAGES", "1")


def block_media() -> bool:
    return _flag("JOD_BLOCK_MEDIA", "1")


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


def profile_dir() -> str | None:
    """A persistent Camoufox profile directory, or None to stay ephemeral.

    A fresh launch has no cookies or localStorage, so a login is lost as soon as
    the browser process exits. Pointing Camoufox at a persistent `user_data_dir`
    makes the whole profile (cookies, logins, site data) survive restarts — the
    difference between "logged in during one run" and "logged in across runs".

    Set `JOD_PERSIST_PROFILE=0` to go back to a throwaway profile per launch
    (the old behaviour). Default is on.
    """
    load_env()
    if os.environ.get("JOD_PERSIST_PROFILE", "1").strip() == "0":
        return None
    return str(Path(ENV_FILE).parent / "profile")  # ~/.jod/profile


def browser_options() -> dict:
    """Arguments for `Camoufox(**opts)`."""
    load_env()
    proxy = proxy_config()
    opts: dict = {"headless": True}
    profile = profile_dir()
    if profile:
        opts["persistent_context"] = True
        opts["user_data_dir"] = profile
    if proxy:
        opts["proxy"] = proxy
        # Match locale, timezone and geolocation to where the proxy exits.
        # Without this the browser claims a fingerprint from one country while
        # its packets arrive from another — a contradiction that is cheaper to
        # detect than any of the things the proxy was bought to hide.
        opts["geoip"] = os.environ.get("JOD_PROXY_GEOIP", "1") != "0"
    # Images are most of the bytes on shopping/image pages, and they travel
    # through a slow residential proxy. Blocking them at the Firefox-pref
    # level means they are never requested at all. innerText reads are
    # unaffected. Screenshots that need pictures: JOD_BLOCK_IMAGES=0.
    if block_images():
        opts["block_images"] = True
        # Deliberate: shopping pages are MBs of images over a slow residential
        # proxy, and text extraction never needs them. Silence the upstream
        # WAF-detection nag accordingly.
        opts["i_know_what_im_doing"] = True
    return opts


def install_blockers(page) -> None:
    """Abort media/font/tracker requests on an already-open page.

    Call once per page, right after creating it. Cheap insurance on top of
    `block_images`: a Shopee search page pulls several MB of video previews,
    webfonts and analytics beacons through the proxy, none of which changes
    the text an agent reads.
    """
    if not block_media():
        return

    def _route(route) -> None:
        req = route.request
        if req.resource_type in BLOCKED_RESOURCE_TYPES:
            return route.abort()
        url = req.url
        if any(h in url for h in BLOCKED_HOSTS):
            return route.abort()
        return route.continue_()

    page.route("**/*", _route)


def describe() -> str:
    proxy = proxy_config()
    if not proxy:
        return "direct (no proxy configured — the VPS IP is exposed)"
    who = proxy.get("username", "anonymous")
    return f"{proxy['server']} as {who}"


def main(argv: list[str]) -> int:
    """Fetch one URL and print it. Agents call this; it needs no library."""
    if len(argv) < 2 or argv[1] in ("-h", "--help"):
        print(__doc__, file=sys.stderr)
        print(f"\nproxy: {describe()}", file=sys.stderr)
        print(
            "\nusage: jodbrowser.py <url> [--html] [--screenshot PATH]"
            " [--wait SELECTOR] [--wait-ms MS] [--timeout SECS]"
            " [--wait-until MODE] [--full]",
            file=sys.stderr,
        )
        print(
            "\n  --wait SELECTOR   wait for a CSS selector before reading."
            " Essential for SPAs (Shopee, React apps) where domcontentloaded"
            " fires long before the content an agent wants exists.",
            file=sys.stderr,
        )
        print("  --wait-ms MS      extra settle time after load (default 0).", file=sys.stderr)
        print("  --timeout SECS    navigation timeout (default 30).", file=sys.stderr)
        print(
            "  --wait-until MODE domcontentloaded (default), load, networkidle.",
            file=sys.stderr,
        )
        print(
            "  --full            load images/media/fonts (undoes blocking).",
            file=sys.stderr,
        )
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

    def _opt(name: str, default: str | None = None) -> str | None:
        if name in argv:
            i = argv.index(name)
            if i + 1 < len(argv):
                return argv[i + 1]
        return default

    wait_selector = _opt("--wait")
    wait_ms = int(_opt("--wait-ms", "0") or "0")
    timeout_s = int(_opt("--timeout", "30") or "30")
    wait_until = _opt("--wait-until", "domcontentloaded") or "domcontentloaded"
    if "--full" in argv:
        os.environ["JOD_BLOCK_IMAGES"] = "0"
        os.environ["JOD_BLOCK_MEDIA"] = "0"

    from camoufox.sync_api import Camoufox

    print(f"[jod] via {describe()}", file=sys.stderr)
    with Camoufox(**browser_options()) as browser:
        page = browser.new_page()
        install_blockers(page)
        page.goto(url, timeout=timeout_s * 1000, wait_until=wait_until)
        if wait_selector:
            page.wait_for_selector(wait_selector, timeout=timeout_s * 1000)
        if wait_ms:
            page.wait_for_timeout(wait_ms)
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
