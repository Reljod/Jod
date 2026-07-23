---
description: Capture durable learnings from this session (the WHYs) into the reference docs — only if the session earned it.
argument-hint: "[optional focus or scope for the retro]"
---

Run a retrospective on this session using the **create-retro** skill at
`.agents/skills/create-retro/SKILL.md`. Read that skill and follow it.

Optional focus: $ARGUMENTS

Steps:
1. **Gate first** — read the whole session context and decide if a retro is
   warranted: real back-and-forth with the user, or the shipped result
   diverged from what you first produced. If neither, say so in one line and
   stop. Don't manufacture learnings.
2. **Extract the WHYs** — for each correction or divergence, capture what you
   did first, what it became, and the reason behind the change. Keep only what
   stays true next session (coding-style preferences, design/architecture
   rationale, workflow corrections); drop task trivia and secrets.
3. **Route each learning** to its durable home — coding prefs to
   `domains/coding/README.md`, cross-domain identity to `AGENTS.md`
   (sparingly), a repeatable behavior to a skill, domain prefs to that
   domain's README. Fold in the distilled rule, in that doc's voice.
4. **Log the reasoning trail** — append a dated entry under
   `domains/coding/retros/YYYY-MM-DD-<slug>.md` with the full context and the
   WHYs, including anything parked as not-yet-durable.
5. **Report** which preferences you captured and where each was routed.

Apply local doc edits (they're reversible) but surface each one; confirm
before anything shared or irreversible (pushing, opening a PR).
