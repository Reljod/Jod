import { describe, expect, it } from "vitest";

import { parseSseFrame } from "../src/transport/http";

/**
 * The desktop shell authenticates with a bearer token, and `EventSource` cannot
 * carry a header — so that path reads the event stream off `fetch` and parses
 * the framing itself. This is that parser. Everything here is a rule the
 * browser would have applied for us on the cookie path.
 */
describe("parseSseFrame", () => {
  it("reads the event name and its data", () => {
    expect(parseSseFrame('event: agent\ndata: {"kind":"started"}')).toEqual({
      event: "agent",
      data: '{"kind":"started"}',
    });
  });

  it("defaults to `message` when the frame names no event", () => {
    // `/v1/events` tags frames `agent`, but an untagged frame is still valid
    // SSE and the transport treats it as an event rather than dropping it.
    expect(parseSseFrame("data: {}")).toEqual({ event: "message", data: "{}" });
  });

  it("joins repeated data lines with newlines", () => {
    // How a server splits a long JSON payload. Joining with anything else —
    // or taking only the last line — silently corrupts the event.
    expect(parseSseFrame('data: {"a":1,\ndata: "b":2}')?.data).toBe('{"a":1,\n"b":2}');
  });

  it("strips exactly one space after the colon, not any more", () => {
    // The single space is framing; a second one is payload. Trimming would
    // corrupt any value that legitimately begins with whitespace.
    expect(parseSseFrame("data:  x")?.data).toBe(" x");
    expect(parseSseFrame("data:x")?.data).toBe("x");
  });

  it("ignores comment lines", () => {
    // Servers send `:` lines as keep-alives to hold an idle connection open.
    // Treating one as data would push a malformed frame into the HUD.
    expect(parseSseFrame(": keep-alive\ndata: {}")).toEqual({
      event: "message",
      data: "{}",
    });
  });

  it("returns null for a frame carrying no data", () => {
    expect(parseSseFrame(": keep-alive")).toBeNull();
    expect(parseSseFrame("event: agent")).toBeNull();
    expect(parseSseFrame("")).toBeNull();
  });

  it("treats a field with no colon as an empty value", () => {
    // Per spec a bare `data` line contributes an empty string — which is data,
    // so the frame is dispatched rather than dropped.
    expect(parseSseFrame("data")).toEqual({ event: "message", data: "" });
  });

  it("keeps a lagged frame distinguishable, since it triggers a backfill", () => {
    // Misreading this one is the expensive mistake: `lagged` means the HUD has
    // a hole, and mistaking it for an agent event would leave it showing stale
    // state confidently.
    expect(parseSseFrame('event: lagged\ndata: {"missed":12}')).toEqual({
      event: "lagged",
      data: '{"missed":12}',
    });
  });
});
