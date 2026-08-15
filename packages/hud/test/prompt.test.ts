import { describe, expect, it, vi } from "vitest";
import { openingPrompt } from "../src/state/prompt";
import type { Transport } from "../src/transport";
import type { ConversationSummary, Message } from "../src/types";

function conversation(over: Partial<ConversationSummary> = {}): ConversationSummary {
  return {
    id: "c1",
    title: "ship the HUD",
    harness: "claude_code",
    model: "claude-opus-5",
    session_id: "sess-1",
    head_id: null,
    forked_from: null,
    message_count: 3,
    updated_at_ms: 1,
    ...over,
  };
}

function message(over: Partial<Message> = {}): Message {
  return {
    id: 1,
    conversation_id: "c1",
    parent_id: null,
    role: "user",
    text: "ship the HUD",
    tool_name: null,
    tool_input: null,
    run_id: "a1",
    run_seq: 0,
    at_ms: 1,
    active: true,
    ...over,
  };
}

/** Only the two reads the join uses; everything else throws if it is touched. */
function fake(
  conversations: ConversationSummary[],
  threads: Record<string, Message[]>,
): Transport & { calls: { conversations: number; messages: string[] } } {
  const calls = { conversations: 0, messages: [] as string[] };
  const unreachable = () => {
    throw new Error("the prompt join must not reach for this");
  };
  return {
    label: "fake",
    calls,
    start: unreachable,
    stop: unreachable,
    spawn: unreachable,
    kill: unreachable,
    events: unreachable,
    authenticate: unreachable,
    harnesses: unreachable,
    history: unreachable,
    async conversations(limit: number) {
      calls.conversations += 1;
      return conversations.slice(0, limit);
    },
    async messages(id: string) {
      calls.messages.push(id);
      return threads[id] ?? [];
    },
  } as unknown as Transport & { calls: typeof calls };
}

describe("openingPrompt", () => {
  it("finds the run's own user turn through its session", async () => {
    const t = fake([conversation()], { c1: [message()] });

    await expect(openingPrompt(t, { id: "a1", session_id: "sess-1" })).resolves.toBe(
      "ship the HUD",
    );
  });

  /**
   * The guarantee that makes this join safe to show at the top of a transcript.
   * A conversation spans every run that continued it, so a session match alone
   * would eventually attribute one run's prompt to another. `run_id` is exact,
   * and a miss returns nothing rather than the neighbouring run's ask.
   */
  it("refuses a prompt belonging to a different run in the same conversation", async () => {
    const t = fake([conversation()], {
      c1: [message({ run_id: "a0", text: "the previous run's ask" })],
    });

    await expect(openingPrompt(t, { id: "a1", session_id: "sess-1" })).resolves.toBeNull();
  });

  it("ignores a conversation whose session is a different one", async () => {
    const t = fake([conversation({ id: "c9", session_id: "sess-other" })], {
      c9: [message({ conversation_id: "c9" })],
    });

    await expect(openingPrompt(t, { id: "a1", session_id: "sess-1" })).resolves.toBeNull();
    expect(t.calls.messages).toEqual([]); // never fetched a thread it could not use
  });

  it("takes only a user turn, not the assistant's reply", async () => {
    const t = fake([conversation()], {
      c1: [
        message({ id: 2, role: "assistant", text: "done", run_seq: 4 }),
        message({ id: 3, role: "user", text: "the real ask" }),
      ],
    });

    await expect(openingPrompt(t, { id: "a1", session_id: "sess-1" })).resolves.toBe(
      "the real ask",
    );
  });

  /** Before `started` lands there is no key to search by, and no request to make. */
  it("asks for nothing until the harness has reported a session", async () => {
    const t = fake([conversation()], { c1: [message()] });

    await expect(openingPrompt(t, { id: "a1", session_id: null })).resolves.toBeNull();
    expect(t.calls.conversations).toBe(0);
  });

  it("searches every conversation on the session before giving up", async () => {
    const t = fake(
      [
        conversation({ id: "c1" }),
        conversation({ id: "c2" }),
        conversation({ id: "c3" }),
      ],
      { c3: [message({ conversation_id: "c3", text: "found on the third" })] },
    );

    await expect(openingPrompt(t, { id: "a1", session_id: "sess-1" })).resolves.toBe(
      "found on the third",
    );
    expect(t.calls.messages).toEqual(["c1", "c2", "c3"]);
  });

  it("lets a failing transcript store surface rather than swallowing it", async () => {
    const t = fake([], {});
    t.conversations = vi.fn().mockRejectedValue(new Error("no store on this daemon"));

    await expect(openingPrompt(t, { id: "a1", session_id: "sess-1" })).rejects.toThrow(
      "no store",
    );
  });
});
