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

## Configuration

Environment-only, read from `~/.jod/browser.env` — see `jodbrowser.py`, whose
loader this reuses rather than duplicating:

    JOD_PROXY_SERVER    http://p.webshare.io:80   (omit to browse direct)
    JOD_PROXY_USERNAME
    JOD_PROXY_PASSWORD
    JOD_PROXY_GEOIP     1 by default; 0 disables

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
import traceback
from typing import Any, Callable, Protocol

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from jodbrowser import browser_options, describe  # noqa: E402

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
# Generous, because the whole point of the proxy is that traffic takes a detour
# through somebody else's ISP, and a residential exit is slower than the VPS's
# own link by a wide margin.
NAV_TIMEOUT_MS = 90_000


class Session(Protocol):
    """The browser, as the four things this server actually needs from one.

    Kept this small on purpose: every method here is a method the fake in the
    tests has to implement, and every method beyond it is protocol surface that
    cannot be tested without a real Firefox.
    """

    def goto(self, url: str, wait_until: str) -> str:
        """Navigate, and return the landed URL (which redirects may change)."""

    def content(self, as_html: bool) -> str:
        """The current page, as innerText or as markup."""

    def act(self, action: str, selector: str, text: str | None) -> str:
        """Click or type. Returns a one-line description of what happened."""

    def screenshot(self, path: str, full_page: bool) -> str:
        """Save a PNG and return the path it was written to."""

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
        self._camoufox = None

    def _ensure(self):
        if self._page is not None:
            return self._page
        # Imported here rather than at module scope so that `--selftest`, and
        # the tests, work on a box where camoufox was never installed. An
        # import error at the top would make the whole server unloadable.
        from camoufox.sync_api import Camoufox

        self._camoufox = Camoufox(**browser_options())
        self._browser = self._camoufox.__enter__()
        self._page = self._browser.new_page()
        return self._page

    def goto(self, url: str, wait_until: str) -> str:
        page = self._ensure()
        page.goto(url, timeout=NAV_TIMEOUT_MS, wait_until=wait_until)
        return page.url

    def content(self, as_html: bool) -> str:
        page = self._ensure()
        if as_html:
            return page.content()
        # innerText rather than the DOM: an agent reading this pays for every
        # token, and the markup is almost never the thing it needs.
        return page.evaluate("document.body.innerText")

    def act(self, action: str, selector: str, text: str | None) -> str:
        page = self._ensure()
        if action == "click":
            page.click(selector, timeout=NAV_TIMEOUT_MS)
            return f"clicked {selector}"
        page.fill(selector, text or "", timeout=NAV_TIMEOUT_MS)
        return f"typed into {selector}"

    def screenshot(self, path: str, full_page: bool) -> str:
        page = self._ensure()
        page.screenshot(path=path, full_page=full_page)
        return path

    def close(self) -> None:
        if self._camoufox is None:
            return
        try:
            self._camoufox.__exit__(None, None, None)
        finally:
            self._browser = None
            self._page = None
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


def tool_browse(session: Session, args: dict) -> str:
    """Fetch one URL and read it. The overwhelmingly common case."""
    url = _require(args, "url")
    as_html = bool(args.get("html", False))
    landed = session.goto(url, args.get("wait_until") or "domcontentloaded")
    body = _clip(session.content(as_html))
    header = f"[jod] {landed} via {describe()}"
    return f"{header}\n\n{body}"


def tool_open(session: Session, args: dict) -> str:
    """Navigate the resident page without reading it."""
    url = _require(args, "url")
    landed = session.goto(url, args.get("wait_until") or "domcontentloaded")
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

    Reads it through the browser rather than asking the proxy what it claims,
    because "the proxy is configured" and "traffic is leaving through it" are
    different facts and only the second one matters.
    """
    config = describe()
    try:
        session.goto("https://api.ipify.org?format=json", "domcontentloaded")
        seen = session.content(False).strip()
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
            "pages that a plain HTTP fetch is blocked from."
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
