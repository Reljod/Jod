// @vitest-environment jsdom
/**
 * The two things the phone does to an app that a desktop browser never does:
 * it slides a keyboard over the bottom of the screen, and it suspends the whole
 * process when you put it in your pocket.
 *
 * Both are wired as effects in `App`, and both are invisible to the rest of the
 * suite because jsdom fires neither on its own. They are exactly the code that
 * only misbehaves on a real device, which is why they are worth pinning here.
 */

import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { App } from "../src/App";
import { JodClient } from "../src/client";
import { Conversation } from "../src/conversation";
import { EventSourceSpy, FakeFetch } from "./fakes";

let http: FakeFetch;
let spy: EventSourceSpy;

function build(): Conversation {
  return new Conversation({
    client: new JodClient({
      fetch: http.fetch,
      eventSource: spy.factory,
      newKey: () => "key-1",
    }),
    scopeMemory: { read: () => "write", write: () => {} },
  });
}

async function mounted(): Promise<Conversation> {
  http.on("GET /v1/harnesses", {
    body: [{ id: "claude_code", label: "Claude Code", available: true, path: "/bin/claude" }],
  });
  http.on("GET /v1/agents", { body: [] });

  const conversation = build();
  render(<App conversation={conversation} />);
  await waitFor(() => expect(conversation.getSnapshot().link.phase).toBe("live"));
  return conversation;
}

/** Install a `visualViewport` jsdom does not provide, and drive it. */
function fakeViewport(height: number, offsetTop = 0) {
  const listeners: Record<string, Array<() => void>> = { resize: [], scroll: [] };
  const vv = {
    height,
    offsetTop,
    addEventListener: (kind: string, fn: () => void) => listeners[kind]?.push(fn),
    removeEventListener: (kind: string, fn: () => void) => {
      listeners[kind] = (listeners[kind] ?? []).filter((f) => f !== fn);
    },
  };
  Object.defineProperty(window, "visualViewport", { value: vv, configurable: true, writable: true });
  return {
    vv,
    fire(kind: "resize" | "scroll") {
      act(() => listeners[kind].forEach((f) => f()));
    },
    listenerCount: () => listeners.resize.length + listeners.scroll.length,
  };
}

beforeEach(() => {
  http = new FakeFetch();
  spy = new EventSourceSpy();
  document.documentElement.style.removeProperty("--keyboard");
});

afterEach(() => {
  cleanup();
  Reflect.deleteProperty(window, "visualViewport");
});

describe("the on-screen keyboard", () => {
  it("publishes the space the keyboard takes as a CSS variable", async () => {
    window.innerHeight = 800;
    const viewport = fakeViewport(800);
    await mounted();

    // Nothing on screen yet: the inset is zero, not absent.
    expect(document.documentElement.style.getPropertyValue("--keyboard")).toBe("0px");

    viewport.vv.height = 500;
    viewport.fire("resize");

    expect(document.documentElement.style.getPropertyValue("--keyboard")).toBe("300px");
  });

  it("accounts for a viewport that has been scrolled up as well as shrunk", async () => {
    window.innerHeight = 800;
    const viewport = fakeViewport(800);
    await mounted();

    viewport.vv.height = 500;
    viewport.vv.offsetTop = 50;
    viewport.fire("scroll");

    expect(document.documentElement.style.getPropertyValue("--keyboard")).toBe("250px");
  });

  /** A taller viewport than the window would otherwise give a negative inset. */
  it("never reports a negative inset", async () => {
    window.innerHeight = 500;
    const viewport = fakeViewport(800);
    await mounted();

    viewport.fire("resize");

    expect(document.documentElement.style.getPropertyValue("--keyboard")).toBe("0px");
  });

  it("stops listening and drops the variable when the app goes away", async () => {
    window.innerHeight = 800;
    const viewport = fakeViewport(800);
    await mounted();
    expect(viewport.listenerCount()).toBe(2);

    cleanup();

    expect(viewport.listenerCount()).toBe(0);
    expect(document.documentElement.style.getPropertyValue("--keyboard")).toBe("");
  });

  /** Desktop Safari and jsdom both lack it; the app must still mount. */
  it("does nothing at all where visualViewport is unavailable", async () => {
    Reflect.deleteProperty(window, "visualViewport");
    await mounted();
    expect(document.documentElement.style.getPropertyValue("--keyboard")).toBe("");
  });
});

describe("coming back from the background", () => {
  function setVisibility(state: "visible" | "hidden") {
    Object.defineProperty(document, "visibilityState", {
      value: state,
      configurable: true,
    });
    act(() => document.dispatchEvent(new Event("visibilitychange")));
  }

  /**
   * iOS suspends a backgrounded app and its open sockets go with it, so a run
   * that finished while the phone was in a pocket would never appear.
   */
  it("catches up when the phone comes back", async () => {
    const conversation = await mounted();
    const resume = vi
      .spyOn(conversation, "resumeAfterBackground")
      .mockResolvedValue(undefined);

    setVisibility("visible");

    expect(resume).toHaveBeenCalledTimes(1);
  });

  it("does not catch up merely because the app was hidden", async () => {
    const conversation = await mounted();
    const resume = vi
      .spyOn(conversation, "resumeAfterBackground")
      .mockResolvedValue(undefined);

    setVisibility("hidden");

    expect(resume).not.toHaveBeenCalled();
  });

  it("stops listening once the app goes away", async () => {
    const conversation = await mounted();
    const resume = vi
      .spyOn(conversation, "resumeAfterBackground")
      .mockResolvedValue(undefined);

    cleanup();
    setVisibility("visible");

    expect(resume).not.toHaveBeenCalled();
  });
});
