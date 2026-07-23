---
name: create-retro
description: >
  Use at the end of a working session to capture durable learnings about how
  the agent should behave — but only when the session earned it: there was
  real back-and-forth with the user, or the shipped result diverged from what
  the agent first produced. Triggers on "/retro", "run a retro", "do a
  retrospective", "capture what we learned", "what should you do differently
  next time". Distills the WHYs — coding-style preferences, design and
  architecture rationale, workflow corrections — into the repo's reference
  docs so the next session starts smarter.
---

# create-retro

A retro turns one session's corrections into a standing preference. The unit
of value is a **WHY**: not "the user changed X" but "the user changed X
*because* Y" — because that reason is what generalizes to the next decision.
A retro that only records what changed is a changelog; a retro that records
why is a preference the agent can act on before being corrected again.

This is the loop behind the charter's fourth principle — *extend by writing
it down*. Ad hoc fixes that never get written down don't compound. This skill
is how they get written down.

## 1. Decide whether a retro is even warranted

Read the **whole** session context first, then gate. Run the retro only if at
least one is true:

- **Back-and-forth** — the user pushed back, redirected, corrected, or
  explained a preference more than once. A single clarifying question is not
  back-and-forth; a real exchange that reshaped the work is.
- **Divergence** — what shipped is meaningfully different from what the agent
  first proposed or wrote: an approach the agent chose got replaced, code it
  wrote got rewritten by the user, a design it defaulted to got overruled.

If **neither** holds — the session was a clean, first-try execution with no
correction — say so in one line and **stop**. Do not invent learnings to
justify running. A retro with nothing durable in it is noise that buries the
retros that matter.

When in doubt, look for the moment the user said "no", "actually", "instead",
"I prefer", "next time", or gave a reason for a change. Those are the seams a
retro mines.

## 2. Extract the durable signal, not the task trivia

For each correction or divergence, capture three things:

1. **What the agent did first** — the default it reached for.
2. **What it became** — the shipped choice.
3. **The WHY** — the reason the user gave, or the reason implied by the
   change. If there is no discernible why, note it as unexplained rather than
   guessing one.

Keep only what will **still be true next session**. Promote:

- **Coding-style preferences** — naming, structure, error handling, test
  layout, dependency choices, comment density — anything the user corrected
  toward a consistent taste.
- **Design & architecture decisions** — why one approach was chosen over
  another, a boundary that was drawn, a pattern to prefer or avoid, a
  trade-off the user weighted a particular way.
- **Workflow & process corrections** — how the user wants the agent to
  operate: when to ask vs. act, how much to explain, what to check first.

Drop the one-offs: task-specific facts, a value that only applied to this
ticket, anything that won't recur. And never record secrets, tokens, or
private data pulled from the session — a preference is a rule, not a payload.

## 3. Route each learning to its durable home

A learning only compounds if it lands where the next session will actually
read it. This skill is part of the portable toolkit, so it routes **only**
into the toolkit — never into `domains/` (see the portability rule in
[`.agents/conventions/README.md`](../../conventions/README.md)). Route by
scope:

| Learning is about… | Write it to |
|---|---|
| Coding style, PR habits, testing, review, general workflow | [`.agents/conventions/README.md`](../../conventions/README.md) |
| A repeatable multi-step behavior | a skill under [`.agents/skills/`](../../skills/) (new or existing) |
| Cross-cutting agent identity/principle | [`AGENTS.md`](../../../AGENTS.md) — sparingly; keep the charter thin |

A preference about a personal domain (how to operate in Linear, Notion, etc.)
is out of scope here — it's private operating data, not a portable convention.
Leave it for that domain's own notes; don't route it from this skill.

Fold the distilled **rule** into the target doc — phrased as guidance the next
agent can follow, in that doc's existing voice, not as a diary entry. If a
rule contradicts something already written there, update the existing line
rather than appending a competing one.

The charter edit is the one to be careful with: prefer the conventions doc, and
only touch `AGENTS.md` for something that is genuinely cross-cutting identity.
Editing local docs is reversible, so apply the edits — but surface each one
plainly in your summary. Confirm before anything irreversible or shared
(pushing to a shared branch, opening a PR) per the charter's reversibility
principle.

## 4. Keep the reasoning trail in the retro log

The target docs get the terse rule; the **retro log** keeps the full context
and the WHY, so a future reader can see how a preference came to be. Append
one dated entry per retro under:

```
.agents/retros/YYYY-MM-DD-<short-slug>.md
```

Use this shape:

```markdown
# Retro — <date> — <short title>

**Session:** <one line: what the work was>
**Trigger:** back-and-forth | divergence from first attempt

## What changed vs. the first attempt
- <agent's initial approach> → <what shipped> — because <the WHY>

## Preferences captured
- <durable rule, as guidance> → routed to <file>

## Not yet durable
- <a signal seen once that needs another occurrence before it's a rule>
```

The "Not yet durable" section is deliberate: a single occurrence is an
anecdote, not a preference. Park it here, and promote it to a real rule the
next time the same correction recurs — that's the evidence threshold that
keeps the reference docs from filling with over-fitted rules.

## 5. Report back

Close with a short summary: whether a retro was warranted (and why), the
durable preferences captured, which doc each was routed to, and anything
parked as not-yet-durable. Keep it to the decisions — the log entry holds the
detail.
