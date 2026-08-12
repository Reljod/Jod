"""The browser MCP server's protocol and tool surface, without a browser.

Everything camoufox does is behind `Session`, so a fake with four methods
exercises the entire dispatch path: negotiation, notifications, malformed
frames, argument validation, truncation, and the error/result distinction that
decides whether a bad page takes the server down or is handed to the agent to
route around.

What this deliberately does NOT cover is whether Firefox launches and whether
Webshare passes traffic. That needs a 150MB browser download and a paid proxy,
so it is a setup step (`browser/setup.sh`) and a live check
(`jod_browser_mcp.py --selftest`, then the `browser_status` tool), not a unit
test that would either be skipped everywhere or lie.

    python3 -m pytest browser/test_jod_browser_mcp.py
    python3 browser/test_jod_browser_mcp.py       # same, no pytest needed
"""

from __future__ import annotations

import io
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import jod_browser_mcp as srv  # noqa: E402


class FakeSession:
    """A browser that records what it was told to do and never opens one."""

    def __init__(self, text: str = "hello", fail: Exception | None = None) -> None:
        self.text = text
        self.fail = fail
        self.calls: list[tuple] = []
        self.closed = False
        self.url = "about:blank"

    def goto(self, url: str, wait_until: str) -> str:
        self.calls.append(("goto", url, wait_until))
        if self.fail:
            raise self.fail
        self.url = url
        return url

    def content(self, as_html: bool) -> str:
        self.calls.append(("content", as_html))
        if self.fail:
            raise self.fail
        return f"<html>{self.text}</html>" if as_html else self.text

    def act(self, action: str, selector: str, text: str | None) -> str:
        self.calls.append(("act", action, selector, text))
        if self.fail:
            raise self.fail
        return f"{action}ed {selector}"

    def screenshot(self, path: str, full_page: bool) -> str:
        self.calls.append(("screenshot", path, full_page))
        if self.fail:
            raise self.fail
        return path

    def close(self) -> None:
        self.calls.append(("close",))
        self.closed = True


def req(rid, method, params=None):
    frame = {"jsonrpc": "2.0", "id": rid, "method": method}
    if params is not None:
        frame["params"] = params
    return frame


def call(session, name, args=None):
    return srv.handle(session, req(1, "tools/call", {"name": name, "arguments": args or {}}))


def text_of(answer):
    return answer["result"]["content"][0]["text"]


def errored(answer):
    return answer["result"]["isError"]


# ---- protocol -------------------------------------------------------------


def test_initialize_answers_with_a_protocol_and_a_tools_capability():
    answer = srv.handle(FakeSession(), req(1, "initialize", {"protocolVersion": srv.PROTOCOL_VERSION}))
    assert answer["jsonrpc"] == "2.0"
    assert answer["id"] == 1
    assert answer["result"]["protocolVersion"] == srv.PROTOCOL_VERSION
    assert answer["result"]["serverInfo"]["name"] == "browser"
    assert isinstance(answer["result"]["capabilities"]["tools"], dict)


def test_initialize_agrees_to_an_older_protocol_the_client_asked_for():
    answer = srv.handle(FakeSession(), req(1, "initialize", {"protocolVersion": "2024-11-05"}))
    assert answer["result"]["protocolVersion"] == "2024-11-05"


def test_initialize_offers_its_own_protocol_for_one_it_does_not_know():
    answer = srv.handle(FakeSession(), req(1, "initialize", {"protocolVersion": "1999-01-01"}))
    assert answer["result"]["protocolVersion"] == srv.PROTOCOL_VERSION


def test_a_notification_is_ignored_rather_than_answered():
    """No `id` means no answer. Replying anyway is a protocol violation."""
    assert srv.handle(FakeSession(), {"jsonrpc": "2.0", "method": "notifications/initialized"}) is None


def test_an_explicit_null_id_is_still_a_request():
    """Presence, not truthiness — `"id": null` expects an answer."""
    answer = srv.handle(FakeSession(), {"jsonrpc": "2.0", "id": None, "method": "ping"})
    assert answer is not None
    assert answer["result"] == {}


def test_an_unknown_method_is_a_protocol_error():
    answer = srv.handle(FakeSession(), req(1, "no/such/method"))
    assert answer["error"]["code"] == srv.METHOD_NOT_FOUND


def test_a_request_that_is_not_an_object_is_refused():
    assert srv.handle(FakeSession(), ["not", "an", "object"])["error"]["code"] == srv.INVALID_REQUEST


def test_tools_list_describes_every_tool_with_a_schema():
    answer = srv.handle(FakeSession(), req(1, "tools/list"))
    tools = answer["result"]["tools"]
    assert {t["name"] for t in tools} == set(srv.BY_NAME)
    for t in tools:
        assert t["description"].strip(), f"{t['name']} has no description"
        assert t["inputSchema"]["type"] == "object"


def test_browse_is_described_as_the_way_to_reach_any_page():
    """The prompt tells agents to route all browsing here; the tool's own
    description has to agree, because that is what a harness actually shows."""
    browse = next(t for t in srv.TOOLS if t["name"] == "browse")
    assert "ANY web page" in browse["description"]


# ---- tool dispatch --------------------------------------------------------


def test_browse_returns_page_text_and_names_the_egress():
    s = FakeSession(text="the page body")
    answer = call(s, "browse", {"url": "https://example.com"})
    body = text_of(answer)
    assert not errored(answer)
    assert "the page body" in body
    assert "https://example.com" in body
    assert ("goto", "https://example.com", "domcontentloaded") in s.calls


def test_browse_can_return_html_when_asked():
    s = FakeSession(text="x")
    assert "<html>" in text_of(call(s, "browse", {"url": "https://e.com", "html": True}))


def test_browse_honours_an_explicit_wait_until():
    s = FakeSession()
    call(s, "browse", {"url": "https://e.com", "wait_until": "networkidle"})
    assert ("goto", "https://e.com", "networkidle") in s.calls


def test_a_missing_url_is_a_tool_error_the_agent_can_read():
    """Not a JSON-RPC error: the call was well-formed, the argument was not,
    and an agent can fix that itself if it is told."""
    answer = call(FakeSession(), "browse", {})
    assert errored(answer)
    assert "url" in text_of(answer)
    assert "error" not in answer, "a bad argument must not read as a protocol failure"


def test_a_blank_url_is_refused_like_a_missing_one():
    assert errored(call(FakeSession(), "browse", {"url": "   "}))


def test_an_unknown_tool_is_a_protocol_error():
    answer = srv.handle(FakeSession(), req(1, "tools/call", {"name": "nope", "arguments": {}}))
    assert answer["error"]["code"] == srv.METHOD_NOT_FOUND


def test_tools_call_without_a_name_is_refused():
    answer = srv.handle(FakeSession(), req(1, "tools/call", {"arguments": {}}))
    assert answer["error"]["code"] == srv.INVALID_PARAMS


def test_missing_arguments_are_treated_as_an_empty_object():
    """Clients omit `arguments` for a no-argument tool; that is not an error."""
    answer = srv.handle(FakeSession(), req(1, "tools/call", {"name": "browser_close"}))
    assert not errored(answer)


def test_non_object_arguments_are_refused():
    answer = srv.handle(FakeSession(), req(1, "tools/call", {"name": "browse", "arguments": "url"}))
    assert answer["error"]["code"] == srv.INVALID_PARAMS


def test_open_navigates_without_reading_the_page():
    """The point of `browser_open` is that it does not pay for the content."""
    s = FakeSession()
    call(s, "browser_open", {"url": "https://e.com"})
    assert not any(c[0] == "content" for c in s.calls)


def test_read_returns_the_page_as_it_is_now():
    s = FakeSession(text="after the click")
    assert "after the click" in text_of(call(s, "browser_read"))


def test_click_and_type_reach_the_session():
    s = FakeSession()
    call(s, "browser_click", {"selector": "#go"})
    call(s, "browser_type", {"selector": "#q", "text": "hello"})
    assert ("act", "click", "#go", None) in s.calls
    assert ("act", "type", "#q", "hello") in s.calls


def test_typing_requires_text_but_accepts_the_empty_string():
    """Clearing a field is a real thing to want, so "" must not be refused the
    way a missing argument is."""
    assert errored(call(FakeSession(), "browser_type", {"selector": "#q"}))
    assert not errored(call(FakeSession(), "browser_type", {"selector": "#q", "text": ""}))


def test_screenshot_defaults_to_the_full_page():
    s = FakeSession()
    call(s, "browser_screenshot", {"path": "/tmp/x.png"})
    assert ("screenshot", "/tmp/x.png", True) in s.calls


def test_close_releases_the_browser():
    s = FakeSession()
    assert not errored(call(s, "browser_close"))
    assert s.closed


def test_status_reports_the_proxy_and_the_ip_the_world_sees():
    s = FakeSession(text='{"ip":"1.2.3.4"}')
    body = text_of(call(s, "browser_status"))
    assert "proxy:" in body
    assert "1.2.3.4" in body


def test_status_says_so_when_egress_cannot_be_checked():
    """A proxy that is configured and a proxy that works are different facts.
    An unreachable check must not read as a working one."""
    s = FakeSession(fail=RuntimeError("connection refused"))
    body = text_of(call(s, "browser_status"))
    assert "could not be checked" in body
    assert "connection refused" in body


# ---- failure handling -----------------------------------------------------


def test_a_page_that_fails_to_load_is_an_agent_problem_not_a_server_crash():
    """The distinction the whole `ToolError` split exists for: a 404 is
    something an agent routes around, not a reason to take the server down
    mid-run and lose every later call in the same session."""
    s = FakeSession(fail=RuntimeError("net::ERR_NAME_NOT_RESOLVED"))
    answer = call(s, "browse", {"url": "https://nope.invalid"})
    assert errored(answer)
    assert "ERR_NAME_NOT_RESOLVED" in text_of(answer)
    assert "error" not in answer


def test_the_server_keeps_serving_after_a_tool_blows_up():
    s = FakeSession(fail=RuntimeError("boom"))
    assert errored(call(s, "browse", {"url": "https://e.com"}))
    s.fail = None
    assert not errored(call(s, "browse", {"url": "https://e.com"}))


# ---- truncation -----------------------------------------------------------


def test_a_long_page_is_bounded_and_says_that_it_was():
    """Silent truncation makes an agent conclude the content it needed is
    absent, which is a worse failure than a page it knows is clipped."""
    s = FakeSession(text="x" * (srv.MAX_TEXT_CHARS + 5_000))
    body = text_of(call(s, "browser_read"))
    assert "truncated" in body
    assert len(body) < srv.MAX_TEXT_CHARS + 500


def test_a_page_at_the_limit_is_not_touched():
    s = FakeSession(text="y" * srv.MAX_TEXT_CHARS)
    assert "truncated" not in text_of(call(s, "browser_read"))


# ---- the stdio loop -------------------------------------------------------


def test_the_loop_answers_requests_and_stays_silent_on_notifications():
    frames = [
        json.dumps(req(1, "initialize", {"protocolVersion": srv.PROTOCOL_VERSION})),
        json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json.dumps(req(2, "tools/list")),
    ]
    out = io.StringIO()
    srv.serve(io.StringIO("\n".join(frames) + "\n"), out, FakeSession())
    answers = [json.loads(line) for line in out.getvalue().splitlines()]
    assert [a["id"] for a in answers] == [1, 2], "the notification was answered"


def test_a_malformed_frame_is_reported_without_ending_the_session():
    """One bad line must not cost every later call — the client would see a
    server that died for no visible reason."""
    frames = ["{not json", json.dumps(req(7, "ping"))]
    out = io.StringIO()
    srv.serve(io.StringIO("\n".join(frames) + "\n"), out, FakeSession())
    answers = [json.loads(line) for line in out.getvalue().splitlines()]
    assert answers[0]["error"]["code"] == srv.INVALID_REQUEST
    assert answers[1]["id"] == 7


def test_blank_lines_are_skipped():
    out = io.StringIO()
    srv.serve(io.StringIO("\n\n" + json.dumps(req(1, "ping")) + "\n"), out, FakeSession())
    assert len(out.getvalue().splitlines()) == 1


def test_every_frame_is_one_line_of_json():
    """The transport is line-delimited; an embedded newline would split one
    answer into two frames and desynchronise the client for the rest of the run."""
    out = io.StringIO()
    srv.serve(io.StringIO(json.dumps(req(1, "tools/list")) + "\n"), out, FakeSession())
    assert len(out.getvalue().strip().splitlines()) == 1


if __name__ == "__main__":
    failures = 0
    for name, fn in sorted(globals().items()):
        if not name.startswith("test_") or not callable(fn):
            continue
        try:
            fn()
            print(f"ok   {name}")
        except AssertionError as e:
            failures += 1
            print(f"FAIL {name}: {e}")
        except Exception as e:  # noqa: BLE001
            failures += 1
            print(f"ERROR {name}: {type(e).__name__}: {e}")
    print(f"\n{failures} failure(s)")
    sys.exit(1 if failures else 0)
