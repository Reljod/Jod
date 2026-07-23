# AGENTS.md — {{PROJECT_NAME}}

This file is the operating charter for any agent working in this repository —
Claude Code, a Claude Agent SDK process, or any other AGENTS.md-compatible
tool. `CLAUDE.md` is a symlink to this file: one charter, read by every
runtime.

## What this repo is

{{PROJECT_DESC}}

## Operating principles

1. **Act decisively where the call is clearly yours.** Prefer well-reasoned
   action over hedged options; escalate the genuinely ambiguous decisions
   instead of guessing.
2. **System of record over ad hoc storage.** Keep each kind of information
   where it actually belongs (tasks in the tracker, docs in the docs, code
   in the repo) rather than scattering shadow copies.
3. **Reversible by default.** Local, reversible actions (drafting, editing,
   reading) don't need a check-in. Anything hard to reverse or visible to
   others — sending messages, pushing to shared branches, closing tickets —
   gets confirmed first.
4. **Extend by writing it down.** When a recurring behavior proves itself,
   promote it to a skill under `.agents/skills/` rather than re-deriving it.
5. **Keep this file thin.** This charter describes identity and principles.
   Detailed, area-specific procedure belongs in its own doc, linked from
   here.

## How code quality is enforced (the layer model)

Quality comes from layering cheap deterministic checks early with mandatory
ones later, so nothing depends on remembering to be careful:

1. **Local, fast, skippable** — git hooks catch typos, formatting, and
   malformed commit messages in under a second. Bypassable; never the real
   gate. → **`setup-git-hooks`**
2. **Server-side, slower, mandatory** — branch protection + required CI
   checks + required reviewers. This is where "tests must pass" and coverage
   thresholds actually bite.
3. **Continuous, not per-PR** — heavier suites (E2E, fuzzing) on a schedule;
   they open a ticket, they don't block a merge.
4. **Upstream of code** — a short behavior list before non-trivial work, so
   "did we build the right thing" is answered before the code exists.
   → **`tdd-loop`**

## Branching

Feature branches only, never directly on `main`. Names follow
`{{BRANCH_PREFIX}}/<short-description>-<id>` for agent-driven work.

## Commits

```
<type>: <TICKET> <subject>
```

- `type` ∈ feat, fix, bug, chore, docs, refactor, test, perf, ci, build,
  style, revert.
- `TICKET` is the issue key (e.g. `{{TICKET_PREFIX}}-12`), required except
  for housekeeping types (chore/docs/style/ci).
- Keep the subject imperative and ≤ 72 chars.

Match whatever `setup-git-hooks` installs for this repo. → **`setup-git-hooks`**

## PRs

Draft by default; open one after pushing if no open PR exists for the
branch. Build the body with **`create-pr`** — visuals first, and surface
that the deterministic checks are green so review attention goes to
judgment, not to re-verifying the boring correctness.

## Skills

Reusable skills live under `.agents/skills/<skill-name>/SKILL.md`, each with
a thin slash-command wrapper in `.claude/commands/`. Invoke either the
command or the skill directly. Promote a new skill here once a behavior has
proven itself more than once.
