<!-- blurb: the full Jod charter — layered quality gates, draft-PR habits -->
# AGENTS.md — {{PROJECT_NAME}}

Operating charter for any agent working in this repo — Claude Code, a Claude
Agent SDK process, or any AGENTS.md-compatible tool. `CLAUDE.md` symlinks here.

**Guidelines only.** Every rule is a bullet. Put the reasoning in
`docs/decisions.md` and the procedure in the skill that owns it — if a line here
needs a paragraph, it belongs behind a link.

## What this repo is

{{PROJECT_DESC}}

## Principles

1. **Act decisively where the call is clearly yours;** escalate the genuinely
   ambiguous. Declare escalation triggers before a long unattended run.
2. **System of record over ad hoc storage.** Tasks in the tracker, docs in the
   docs, code in the repo. No shadow copies.
3. **Reversible by default.** Reading, drafting, editing need no check-in.
   Hard-to-reverse or externally-visible actions get confirmed first.
4. **Extend by writing it down.** A line in `docs/decisions.md`, or a skill for
   a repeatable procedure. Undocumented fixes don't compound.
5. **Keep this file thin.** Guidelines here; detail behind a link.

## Never work around a blocked check

A check that can't pass makes faking it the cheapest path, so honesty is a real
exit here.

- **Never**, in service of a check: invent a credential value · swap a real
  integration for a mock to go green · skip, delete or `xfail` a test · weaken
  an assertion · widen an `except`/`catch` to swallow a failure · edit test or
  CI files during an implementation task · narrow a check to the part that
  already passes.
- **Instead** write `BLOCKED.md` — `Missing:` / `Tried:` / `Needs:` + the
  failing check — and stop. Blocked is a successful ending.

## How work runs

- **Non-trivial work starts with a spec, not a plan.** Resolve the ambiguity
  first → `SPEC.md` → execute in a *fresh* session. → **`write-spec`**
- **Every task needs one runnable check.** Without one, "looks done" is the only
  stop signal.
- **Unattended runs need their whole dependency set present.** Missing key,
  service, or fixture → prepare it first or run attended.
- **PRs carry evidence, not claims** — the check's real output, visuals first.
  → **`create-pr`**

## Quality gates (layered, so nothing rests on being careful)

1. **Local, fast, skippable** — git hooks; never the real gate.
   → **`setup-git-hooks`**
2. **Server-side, mandatory** — branch protection + required CI checks. Where
   "tests must pass" actually bites.
3. **Continuous, not per-PR** — heavier suites on a schedule; they open a
   ticket, they don't block a merge.
4. **Upstream of code** — the spec, before any of the above.

## Conventions

- **Branches:** `{{BRANCH_PREFIX}}/<short-description>-<id>`, never `main`.
- **Commits:** `<type>: <subject>` — imperative, ≤72 chars, `type` ∈ feat, fix,
  chore, docs, refactor, test, perf, ci, build, style, revert.
  → **`setup-git-hooks`**
- {{TICKET_RULE}}
- **PRs:** draft by default; open one after pushing if none is open.

## Skills

Under `.agents/skills/<name>/SKILL.md`, each with a `/`-command wrapper in
`.claude/commands/`. Add one only for a repeatable procedure proven more than
once; otherwise it's a `docs/decisions.md` line.
