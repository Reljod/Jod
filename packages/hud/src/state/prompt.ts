import type { Transport } from "../transport";
import type { AgentSummary } from "../types";

/**
 * How far back through the conversation list to look for a run's thread.
 *
 * The list is newest-first and a run's own conversation is touched when the run
 * is, so the thread being looked for is near the front for anything recent. A
 * deeper scan would cost a bigger response on every selection to find prompts
 * for runs nobody is looking at.
 */
export const CONVERSATION_SCAN = 60;

/**
 * Recover the turn that opened a run.
 *
 * The event stream does not carry it. A prompt is appended to the transcript as
 * a `user` message keyed by `run_id`, and the harness never echoes it back, so
 * a trajectory built from events alone can show every answer and not the
 * question. This is the join that fixes that, over routes that already exist.
 *
 * Two steps, and the second is the one that matters: `session_id` *finds*
 * candidate conversations, `run_id` *confirms* the message. The session is a
 * weak key — a thread that moves between harnesses changes it, and a
 * conversation spans every run that continued it — so matching on it alone
 * would eventually attribute one run's prompt to another. `run_id` is exact.
 * Anything short of an exact match returns null, because a wrong prompt at the
 * top of a transcript is worse than no prompt at all.
 */
export async function openingPrompt(
  transport: Transport,
  agent: Pick<AgentSummary, "id" | "session_id">,
): Promise<string | null> {
  // Before the harness reports a session there is nothing to search by. The
  // caller retries once `started` lands.
  if (!agent.session_id) return null;

  const conversations = await transport.conversations(CONVERSATION_SCAN);
  const candidates = conversations.filter((c) => c.session_id === agent.session_id);

  for (const candidate of candidates) {
    const thread = await transport.messages(candidate.id);
    const opening = thread.find((m) => m.run_id === agent.id && m.role === "user");
    if (opening) return opening.text;
  }
  return null;
}
