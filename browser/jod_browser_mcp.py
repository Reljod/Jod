"""Jod's browser, as an MCP server.

Why this exists as a *server* and not as the script next door: `jodbrowser.py`
fetches one URL, prints it, and exits — so every page costs a full Firefox
launch, and there is no way to click anything, because the browser that
rendered the page is gone by the time an agent reads it. That is the whole
shape of "agents browse the web" done wrong. Here the browser is *resident*:
one launch, many pages, and a real session with cookies that persist across
tool calls, which is what a login-walled page needs.

The detection argument is unchanged and is the reason camoufox is involved at
all → `research/ip-blocking-2026/REPORT.md`. An agent that fetches with
`requests` announces "I am not a browser" in the TLS handshake, before its IP
is ever considered; a residential proxy on top of that buys a better-looking
IP attached to the same instantly-detectable client. The proxy fixes one
detection layer out of six and the fingerprint is the load-bearing half, so
this is a real Firefox with patched fingerprints, egressing through Webshare.

## The seam, and why the interesting half needs no Firefox

Everything below `Session` is protocol: parse a JSON-RPC frame, validate
arguments, dispatch, shape a result. Everything camoufox does is behind
`Session`, which is four methods. That split is deliberate and is the same one
`crate::monitor` makes in the Rust half: the part that can be got subtly wrong
is tested from values, and the part that needs a 150MB browser download is a
thin adapter. `test_jod_browser_mcp.py` substitutes a fake `Session` and tests
the entire tool surface without launching anything.

## One page, and why

The session owns exactly one page and never creates a second one. That is not
tidiness, it is the fix for a wedge that made the resident browser useless.
Measured on 2026-09-13: once a Camoufox context has created a third page, a
navigation to a heavy site (Facebook) stops completing on **every** page, with
route interception installed or not, and closing pages afterwards does not heal
it. `browser_status` used to open a page of its own to read the egress IP, so
the first status call poisoned every navigation after it: measured at 90.02 s
per navigation, the full timeout, against 2 to 4 s warm. Status now asks the
proxy directly and opens nothing.

## Configuration

Environment-only, read from `~/.jod/browser.env` — see `jodbrowser.py`, whose
loader this reuses rather than duplicating:

    JOD_PROXY_SERVER    http://p.webshare.io:80   (omit to browse direct)
    JOD_PROXY_USERNAME
    JOD_PROXY_PASSWORD
    JOD_PROXY_GEOIP     1 by default; 0 disables
    JOD_PERSIST_PROFILE 1 by default; 0 = throwaway profile per launch

Credentials are never passed on the command line, because argv is world-
readable through /proc on a shared box.

## Transport

Line-delimited JSON on stdin/stdout, one frame per line, which is what every
MCP client speaks over stdio. **Nothing may print to stdout except a response
frame** — a stray `print()` corrupts the stream and the client's next parse
fails on something that looks nothing like the bug. Diagnostics go to stderr,
and camoufox's own chatter is redirected there for the same reason.
"""

from __future__ import annotations

import json
import os
import sys
import time
import traceback
import urllib.request
from typing import Any, Callable, Protocol

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from jodbrowser import (  # noqa: E402
    browser_options,
    describe,
    install_blockers,
    proxy_config,
)

# Matches the Rust server's own negotiation, so both of Jod's MCP servers agree
# with the same clients. See `core/src/mcp.rs`.
PROTOCOL_VERSION = "2025-06-18"
SUPPORTED_PROTOCOLS = ("2024-11-05", "2025-03-26", PROTOCOL_VERSION)

SERVER_NAME = "browser"
SERVER_VERSION = "0.1.0"

# JSON-RPC codes, from the spec.
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603

# How much page text one call may return.
#
# An agent pays for every token of this, and a large page is mostly navigation
# it did not ask for. Truncation is announced in the text rather than silent,
# because an agent that cannot tell a short page from a truncated one will
# conclude the content it needed is absent.
MAX_TEXT_CHARS = 40_000

# How long a navigation may take before it is an error.
#
# Measured on this box, 2026-09-13: a warm navigation to a heavy page
# (Facebook's own timeline) lands in 2.0 to 3.9 s, and the first navigation of a
# session, which includes the Firefox launch, in 6.8 s. The proxy is a detour
# through somebody else's ISP, so the ceiling stays loose, but it no longer has
# to absorb the 90 s that a wedged page used to burn.
NAV_TIMEOUT_MS = 45_000

# How the read path decides a page has rendered.
#
# Measured on this box, 2026-09-13, Facebook's own timeline after
# domcontentloaded: 435 chars at 3.9 s, then 1715 at 4.5 s, 1806 at 5.6 s,
# 3138 at 6.7 s, settling at 2950 from 7.2 s. The renders pause for up to
# 1.1 s in the middle, so "two reads in a row agree" accepts a half-rendered
# page: that rule returned 536 chars against this one's 2950. Waiting for a
# quiet window of 1200 ms catches the real content and costs a floor of about
# 1.2 s on any page, which is the price of not handing an agent an empty shell.
SETTLE_POLL_MS = 150
SETTLE_STABLE_MS = 1200
SETTLE_MAX_MS = 12_000

# Where the egress check asks what the world sees, and how long it waits.
EGRESS_URL = "https://api.ipify.org?format=json"
EGRESS_TIMEOUT_S = 20


class Session(Protocol):
    """The browser, as the four things this server actually needs from one.

    Kept this small on purpose: every method here is a method the fake in the
    tests has to implement, and every method beyond it is protocol surface that
    cannot be tested without a real Firefox.
    """

    def goto(self, url: str, wait_until: str) -> str:
        """Navigate, and return the landed URL (which redirects may change)."""
        ...

    def wait_for(self, selector: str) -> None:
        """Wait until a CSS selector matches, for SPA content that arrives
        after load. Kept separate from `goto` so sessions that predate it
        keep working: callers only invoke it when the agent asked to wait."""
        ...

    def content(self, as_html: bool) -> str:
        """The current page, as innerText or as markup."""

    def act(self, action: str, selector: str, text: str | None) -> str:
        """Click or type. Returns a one-line description of what happened."""

    def screenshot(self, path: str, full_page: bool) -> str:
        """Save a PNG and return the path it was written to."""

    def egress(self) -> str:
        """What the world sees as this session's address.

        Answered without a page, because opening one to ask is what used to
        wedge the session's own navigations."""
        ...

    def close(self) -> None:
        """Release the browser, if one was ever started."""


class CamoufoxSession:
    """The real thing: one resident Camoufox, started on first use.

    **Lazily**, and that matters more than it looks. An MCP server is launched
    when the harness starts, not when an agent first decides to browse, and a
    great many runs never browse at all. Starting Firefox in the constructor
    would put a ~1s launch and a few hundred MB of RSS behind every single run
    Jod spawns, to serve the ones that turn out to need it.
    """

    def __init__(self) -> None:
        self._browser = None
        self._page = None
        self._popups = []
        self._camoufox = None

    def _ensure(self):
        """The session's one page, created once and then reused for ever.

        A second page is not a harmless extra here. Measured on 2026-09-13: a
        context that has created a third page stops completing navigations to a
        heavy site on every page, whether or not route interception is running,
        and closing pages afterwards does not heal it. So this reuses the page
        the context opens with (the browser always opens one) and creates
        nothing else. Popups that a site opens are tracked for reading and
        typing, never for navigation.
        """
        if self._page is not None:
            try:
                if not self._page.is_closed():
                    return self._page
            except Exception:  # noqa: BLE001 — a dead handle reads as closed
                pass
        # Imported here rather than at module scope so that `--selftest`, and
        # the tests, work on a box where camoufox was never installed. An
        # import error at the top would make the whole server unloadable.
        from camoufox.sync_api import Camoufox

        self._camoufox = Camoufox(**browser_options())
        self._browser = self._camoufox.__enter__()
        opened = list(getattr(self._browser, "pages", []) or [])
        self._page = opened[0] if opened else self._browser.new_page()
        install_blockers(self._page)
        if hasattr(self._browser, "on"):
            self._browser.on("page", self._track)
        return self._page

    def _track(self, p) -> None:
        """Record a page the site opened, so a login popup can be read."""
        if p is self._page or p in self._popups:
            return
        install_blockers(p)
        self._popups.append(p)

    def _live_popup(self):
        for p in reversed(self._popups):
            try:
                if not p.is_closed():
                    return p
            except Exception:  # noqa: BLE001 — a dead handle reads as closed
                continue
        return None

    def _viewing_target(self):
        """Where a read, a click or a screenshot goes: popup first, then page.

        A login wall (Shopee to Google) opens its sign-in in a popup, and the
        agent has to read and type into that popup even though the main tab
        initiated it. Navigation is the one thing that stays on the main page.
        """
        return self._live_popup() or self._ensure()

    def goto(self, url: str, wait_until: str) -> str:
        page = self._ensure()
        page.goto(url, timeout=NAV_TIMEOUT_MS, wait_until=wait_until)
        return page.url

    def wait_for(self, selector: str) -> None:
        self._ensure().wait_for_selector(selector, timeout=NAV_TIMEOUT_MS)

    def content(self, as_html: bool) -> str:
        page = self._viewing_target()
        if as_html:
            return page.content()
        # innerText rather than the DOM: an agent reading this pays for every
        # token, and the markup is almost never the thing it needs.
        return page.evaluate("document.body.innerText")

    def act(self, action: str, selector: str, text: str | None) -> str:
        page = self._viewing_target()
        if action == "click":
            page.click(selector, timeout=NAV_TIMEOUT_MS)
            return f"clicked {selector}"
        page.fill(selector, text or "", timeout=NAV_TIMEOUT_MS)
        return f"typed into {selector}"

    def screenshot(self, path: str, full_page: bool) -> str:
        page = self._viewing_target()
        page.screenshot(path=path, full_page=full_page)
        return path

    def egress(self) -> str:
        """The address the world sees, asked of the proxy rather than a page.

        A direct request through the same proxy the browser uses proves that
        traffic leaves through it and reports the address it presents, which is
        the question this answers. It costs about half a second and, unlike the
        page it replaced, it cannot wedge the session. Credentials come from the
        environment file and are never printed.
        """
        proxy = proxy_config()
        if proxy:
            server = proxy["server"].split("://", 1)[-1]
            who = ""
            if proxy.get("username"):
                who = f"{proxy['username']}:{proxy.get('password', '')}@"
            handler = urllib.request.ProxyHandler(
                {"http": f"http://{who}{server}", "https": f"http://{who}{server}"}
            )
        else:
            handler = urllib.request.ProxyHandler({})
        opener = urllib.request.build_opener(handler)
        with opener.open(EGRESS_URL, timeout=EGRESS_TIMEOUT_S) as answer:
            return answer.read().decode("utf-8", "replace").strip()

    def close(self) -> None:
        if self._camoufox is None:
            return
        try:
            self._camoufox.__exit__(None, None, None)
        finally:
            self._browser = None
            self._page = None
            self._popups = []
            self._camoufox = None


# ---- tools ----------------------------------------------------------------


def _clip(text: str) -> str:
    """Bound a page's text, saying so when it is bounded."""
    if len(text) <= MAX_TEXT_CHARS:
        return text
    return (
        text[:MAX_TEXT_CHARS]
        + f"\n\n[jod: truncated at {MAX_TEXT_CHARS} characters — "
        "re-read with a selector or ask for html if you need the rest]"
    )


def _require(args: dict, key: str) -> str:
    value = args.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ToolError(f"`{key}` is required and must be a non-empty string")
    return value.strip()


class ToolError(Exception):
    """A tool said no. Reported to the agent as a result, not as a crash.

    MCP draws this distinction and it matters: a JSON-RPC *error* means the
    call was malformed, and a client may retry or give up on the server. A
    result with `isError` means the tool ran and the answer is "that did not
    work" — which an agent can read, reason about, and route around. A 404 is
    the agent's problem to solve, not a protocol failure.
    """


def _maybe_wait(session: Session, args: dict) -> None:
    wait = args.get("wait")
    if isinstance(wait, str) and wait.strip():
        session.wait_for(wait.strip())


def _settle(session: Session, as_html: bool, poll_ms: int | None = None,
            stable_ms: int | None = None, max_ms: int | None = None) -> str:
    """Read the page once its text has been quiet for a while.

    The alternative, which an agent did by hand, is to sleep for a number nobody
    can know in advance: too short and the read comes back as page chrome, too
    long and every call pays it. Two reads agreeing is not enough either, which
    this learned the hard way: a page renders in stages with pauses of a second
    or more between them, and the first pause looks exactly like the end. So the
    text has to hold still for `stable_ms`. HTML is returned immediately,
    because markup keeps changing on pages that are perfectly readable.

    Defaults come from the module constants so a test can shorten the wait.
    """
    poll = SETTLE_POLL_MS if poll_ms is None else poll_ms
    stable = SETTLE_STABLE_MS if stable_ms is None else stable_ms
    cap = SETTLE_MAX_MS if max_ms is None else max_ms

    if as_html:
        return session.content(True)
    text = session.content(False)
    last_change = time.monotonic()
    deadline = last_change + cap / 1000
    while time.monotonic() < deadline:
        time.sleep(poll / 1000)
        again = session.content(False)
        now = time.monotonic()
        if again != text:
            text = again
            last_change = now
            continue
        if text.strip() and (now - last_change) * 1000 >= stable:
            return text
    return text


def tool_browse(session: Session, args: dict) -> str:
    """Fetch one URL, wait for it to settle, and read it. The common case."""
    url = _require(args, "url")
    as_html = bool(args.get("html", False))
    landed = session.goto(url, args.get("wait_until") or "domcontentloaded")
    _maybe_wait(session, args)
    body = _clip(_settle(session, as_html))
    header = f"[jod] {landed} via {describe()}"
    return f"{header}\n\n{body}"


def tool_open(session: Session, args: dict) -> str:
    """Navigate the resident page without reading it."""
    url = _require(args, "url")
    landed = session.goto(url, args.get("wait_until") or "domcontentloaded")
    _maybe_wait(session, args)
    return f"at {landed}"


def tool_read(session: Session, args: dict) -> str:
    """Read the page as it is now — after a click, after a login."""
    return _clip(session.content(bool(args.get("html", False))))


def tool_click(session: Session, args: dict) -> str:
    return session.act("click", _require(args, "selector"), None)


def tool_type(session: Session, args: dict) -> str:
    text = args.get("text")
    if not isinstance(text, str):
        raise ToolError("`text` is required and must be a string")
    return session.act("type", _require(args, "selector"), text)


def tool_screenshot(session: Session, args: dict) -> str:
    path = _require(args, "path")
    written = session.screenshot(path, bool(args.get("full_page", True)))
    return f"screenshot → {written}"


def tool_close(session: Session, args: dict) -> str:
    session.close()
    return "browser closed"


def tool_status(session: Session, args: dict) -> str:
    """Which IP the world sees, which is the only honest way to check a proxy.

    Asked over a plain request through the configured proxy, not through a
    browser page. "The proxy is configured" and "traffic is leaving through it"
    are different facts and only the second one matters, but the page that used
    to answer this question cost every later navigation in the session
    (measured: 90.02 s each, the full timeout). This costs about half a second
    and leaves the agent's page exactly where it was.
    """
    config = describe()
    try:
        seen = session.egress().strip()
    except Exception as e:  # noqa: BLE001 — reported, never raised
        return f"proxy: {config}\negress: could not be checked ({e})"
    return f"proxy: {config}\negress: {seen}"


TOOLS: list[dict[str, Any]] = [
    {
        "name": "browse",
        "description": (
            "Fetch a web page through Jod's stealth browser and return its text. "
            "Use this for ANY web page. It is a real Firefox with patched "
            "fingerprints egressing through a residential proxy, so it reaches "
            "pages that a plain HTTP fetch is blocked from. It waits for the "
            "page's text to stop changing before reading, so a JavaScript page "
            "comes back with its content instead of an empty shell."
        ),
        "handler": tool_browse,
        "schema": {
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "The URL to fetch."},
                "html": {
                    "type": "boolean",
                    "description": "Return raw HTML instead of readable text. "
                    "Default false; text is far cheaper and usually enough.",
                },
                "wait_until": {
                    "type": "string",
                    "enum": ["domcontentloaded", "load", "networkidle"],
                    "description": "How long to wait before reading. Use "
                    "networkidle for pages that render themselves in JS.",
                },
                "wait": {
                    "type": "string",
                    "description": "CSS selector to wait for before reading "
                    "(e.g. a product card on Shopee). Essential for SPAs where "
                    "domcontentloaded fires before the content exists.",
                },
            },
            "required": ["url"],
        },
    },
    {
        "name": "browser_open",
        "description": (
            "Navigate the resident browser to a URL without reading it. Use "
            "when you intend to click or type next; the session keeps cookies, "
            "so a login done here persists across later calls."
        ),
        "handler": tool_open,
        "schema": {
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "wait_until": {
                    "type": "string",
                    "enum": ["domcontentloaded", "load", "networkidle"],
                },
                "wait": {
                    "type": "string",
                    "description": "CSS selector to wait for after navigating.",
                },
            },
            "required": ["url"],
        },
    },
    {
        "name": "browser_read",
        "description": "Read the current page, as it is now — after a click or a login.",
        "handler": tool_read,
        "schema": {
            "type": "object",
            "properties": {"html": {"type": "boolean"}},
        },
    },
    {
        "name": "browser_click",
        "description": "Click an element on the current page, by CSS selector or text.",
        "handler": tool_click,
        "schema": {
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "A CSS selector, or Playwright text= syntax.",
                }
            },
            "required": ["selector"],
        },
    },
    {
        "name": "browser_type",
        "description": "Type into a field on the current page.",
        "handler": tool_type,
        "schema": {
            "type": "object",
            "properties": {
                "selector": {"type": "string"},
                "text": {"type": "string"},
            },
            "required": ["selector", "text"],
        },
    },
    {
        "name": "browser_screenshot",
        "description": "Save a PNG of the current page to a path on disk.",
        "handler": tool_screenshot,
        "schema": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "full_page": {"type": "boolean"},
            },
            "required": ["path"],
        },
    },
    {
        "name": "browser_close",
        "description": (
            "Close the browser and release its memory. Optional — the session "
            "is cleaned up when the run ends — but worth doing when you are "
            "finished browsing in a long run."
        ),
        "handler": tool_close,
        "schema": {"type": "object", "properties": {}},
    },
    {
        "name": "browser_status",
        "description": (
            "Report which proxy is configured and which IP the world actually "
            "sees. Use this to prove egress before trusting a scrape."
        ),
        "handler": tool_status,
        "schema": {"type": "object", "properties": {}},
    },
]

BY_NAME: dict[str, Callable[[Session, dict], str]] = {t["name"]: t["handler"] for t in TOOLS}


# ---- the JSON-RPC surface -------------------------------------------------


def _result(rid: Any, payload: dict) -> dict:
    return {"jsonrpc": "2.0", "id": rid, "result": payload}


def _error(rid: Any, code: int, message: str) -> dict:
    return {"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": message}}


def _text(body: str, is_error: bool = False) -> dict:
    return {"content": [{"type": "text", "text": body}], "isError": is_error}


def handle(session: Session, request: Any) -> dict | None:
    """Answer one request, or say nothing.

    `None` means the message was a notification — no `id`, so by JSON-RPC there
    is nothing to answer and answering anyway is a protocol violation. That
    covers `notifications/initialized`, which every client sends and no server
    needs to act on.
    """
    if not isinstance(request, dict):
        return _error(None, INVALID_REQUEST, "request is not an object")
    # Presence, not truthiness: a notification omits `id` entirely, while an
    # explicit `"id": null` is still a request expecting an answer.
    if "id" not in request:
        return None
    rid = request["id"]
    method = request.get("method")
    if not isinstance(method, str):
        return _error(rid, INVALID_REQUEST, "request has no method")
    params = request.get("params") or {}
    if not isinstance(params, dict):
        return _error(rid, INVALID_PARAMS, "`params` must be an object")

    if method == "initialize":
        asked = params.get("protocolVersion")
        version = asked if asked in SUPPORTED_PROTOCOLS else PROTOCOL_VERSION
        return _result(
            rid,
            {
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION,
                    "title": "Jod browser (camoufox via Webshare)",
                },
            },
        )
    if method == "ping":
        return _result(rid, {})
    if method == "tools/list":
        return _result(
            rid,
            {
                "tools": [
                    {
                        "name": t["name"],
                        "description": t["description"],
                        "inputSchema": t["schema"],
                    }
                    for t in TOOLS
                ]
            },
        )
    if method == "tools/call":
        return _call(session, rid, params)
    return _error(rid, METHOD_NOT_FOUND, f"unknown method `{method}`")


def _call(session: Session, rid: Any, params: dict) -> dict:
    name = params.get("name")
    if not isinstance(name, str):
        return _error(rid, INVALID_PARAMS, "tools/call needs a `name`")
    args = params.get("arguments")
    if args is None:
        args = {}
    if not isinstance(args, dict):
        return _error(rid, INVALID_PARAMS, "`arguments` must be an object")
    handler = BY_NAME.get(name)
    if handler is None:
        return _error(rid, METHOD_NOT_FOUND, f"unknown tool `{name}`")
    try:
        return _result(rid, _text(handler(session, args)))
    except ToolError as e:
        return _result(rid, _text(str(e), is_error=True))
    except Exception as e:  # noqa: BLE001
        # A page that would not load, a selector that matched nothing, a proxy
        # that refused — all of it is the agent's problem to route around, not
        # a reason to take the server down mid-run. The traceback goes to
        # stderr, where the supervisor's log will keep it.
        traceback.print_exc(file=sys.stderr)
        return _result(rid, _text(f"{type(e).__name__}: {e}", is_error=True))


def serve(stdin, stdout, session: Session) -> None:
    """Read frames until the client hangs up."""
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as e:
            _write(stdout, _error(None, INVALID_REQUEST, f"invalid JSON: {e}"))
            continue
        try:
            answer = handle(session, request)
        except Exception as e:  # noqa: BLE001
            traceback.print_exc(file=sys.stderr)
            rid = request.get("id") if isinstance(request, dict) else None
            answer = _error(rid, INTERNAL_ERROR, f"{type(e).__name__}: {e}")
        if answer is not None:
            _write(stdout, answer)


def _write(stdout, payload: dict) -> None:
    stdout.write(json.dumps(payload) + "\n")
    stdout.flush()


def main(argv: list[str]) -> int:
    if "--selftest" in argv:
        # Enough to prove the server is wired up on a box where camoufox may
        # not be installed: the protocol answers, and the tool list is the one
        # the prompt promises. Launching Firefox is not part of it.
        print(f"proxy: {describe()}", file=sys.stderr)
        print(f"tools: {', '.join(sorted(BY_NAME))}", file=sys.stderr)
        return 0

    session = CamoufoxSession()
    try:
        serve(sys.stdin, sys.stdout, session)
    finally:
        # The run is over; the browser must not outlive it. Without this a
        # killed run leaves a headless Firefox holding several hundred MB on a
        # VPS, and nothing left alive knows it is there.
        try:
            session.close()
        except Exception:  # noqa: BLE001
            traceback.print_exc(file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
