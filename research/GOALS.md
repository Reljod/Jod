# Session goals — set by Reljod, 2026-08-10

These are the standing bar for this work. Every deliverable is graded against
them, not against "it compiles".

## G1 — Hermes parity on the core four

Complete Hermes Agent's core feature set in Jod: **full memories, Telegram
support, chat, scheduling**. When the work looks finished, go back to Hermes,
diff the feature list again, and implement what is still missing. "Finished" is
a claim that must be re-tested against the reference, not a feeling.

## G2 — Nothing ships ungrounded

Every design decision is produced by a **hypothesis → experiment → iterate ×10 →
grade** loop. Not one pass. Ten. Each iteration must change something real and
be scored on a fixed rubric, and the final artifact is a *ranking* over the ten,
with the winner justified by its score rather than by being last.

Applies to *all* research tracks, not only the UI: memory schema, graph engine,
scheduler semantics, transports, and the TUI.

## G3 — The UI has to actually feel good

Managing **tasks, agent tasks, scheduled tasks, saved memories, and the graph of
memories** must feel intuitive. Grounded in research into what real, well-loved
TUIs do — not in personal taste. The bar is "obvious without the manual".

## G4 — Harness parity: OpenCode, Claude Code, Antigravity

The Jod TUI must be **feature complete against all three harnesses it drives**.
Audit each, implement everything, then re-audit and implement what was missed.

Called out by name as required:

- **Compaction when switching harnesses.** Moving a conversation from Claude
  Code to OpenCode mid-thread must carry the thread across, compacted.
- **A list of conversations.**
- **Recover, revert, fork, branch out** — a conversation is a tree with undo,
  not a line.

This one has an architectural consequence worth stating up front:
`docs/jod-system.md` currently asserts *"Jod needs no memory of the transcript:
the harness owns it."* Fork, revert and cross-harness handoff all require the
opposite — Jod must own a **portable transcript** that no single harness owns.
The event stream in `jod.db` is already most of it. That assumption is now
under review, and if it changes, `docs/decisions.md` records why.

---

## How a track is graded

A track is not done until it has:

1. Falsifiable hypotheses written **before** the evidence was gathered.
2. Ten graded iterations against a rubric fixed in advance.
3. A ranking table over those iterations with scores.
4. A single recommendation, justified by the numbers.
5. Sources, or measured output, for every load-bearing claim.
