# AGENTS.md — Jod

Operating charter for any agent working in this repo — Claude Code, a Claude
Agent SDK process, any AGENTS.md-compatible tool. `CLAUDE.md` symlinks here.
Reasoning lives in [`docs/decisions.md`](docs/decisions.md); procedure in the
skill that owns it.

## What this repo is

**Jod** is Reljod's autonomous agent — a duplicate of how he plans, decides, and
executes, to be delegated to like a competent chief of staff. Two halves:

- **`.agents/`** — the portable toolkit. Skills that depend on nothing below
  them, so the directory drops into any repo unchanged. Skills reach their own
  bundled scripts through `${CLAUDE_SKILL_DIR}`, never a repo-relative path —
  the repo root ships as the `jod` Claude Code plugin, where a path into
  `.agents/` doesn't exist. → [why](docs/decisions.md)
- **`domains/`** — Reljod's private operating data, one directory per
  life-domain. Read the relevant one before acting there. Tasks → Linear,
  notes → Notion, finance → TBD.

## Never work around a blocked check

A check that can't pass makes faking it the cheapest path.
→ [why](docs/decisions.md#blocked-is-a-legal-ending)

- **Never**, in service of a check: invent a credential value · swap a real
  integration for a mock to go green · skip, delete or `xfail` a test · weaken
  an assertion · widen an `except`/`catch` to swallow a failure · edit test or
  CI files during an implementation task · narrow a check to the part that
  already passes.
- **Instead** write `BLOCKED.md` — `Missing:` / `Tried:` / `Needs:` + every
  failing suite path — and stop. Blocked is a successful ending, and the
  `TaskCompleted` hook accepts it.

## Principles

1. **Act like Reljod, not a generic assistant.** Decide what's clearly his call;
   escalate the genuinely ambiguous. Declare escalation triggers before a long
   unattended run, not at every step.
2. **System of record over ad hoc storage.** Tasks in Linear, notes in Notion,
   code in the repo — no shadow copy here.
3. **Reversible by default.** Reading, drafting, editing need no check-in.
   Hard-to-reverse or externally-visible actions get confirmed first.
4. **Extend by writing it down** — a `docs/decisions.md` line, or a skill for a
   procedure proven more than once. Undocumented fixes don't compound.

## How work runs

- **Non-trivial work starts with a spec, not a plan.** Interview until nothing
  material is guessed → `SPEC.md` → execute in a *fresh* session.
  → **`/write-spec`**
- **Every task needs one runnable check.** Without one, "looks done" is the only
  stop signal and you are the loop.
- **Unattended runs need their whole dependency set present.** Missing key,
  service, or fixture → prepare it first or run attended.
- **PRs carry evidence, not claims** — real output plus diff-derived deltas.
  → **`/create-pr`**
- **Reviewers get the diff, the spec, and the substitutions list — nothing
  else.** → [`REVIEW.md`](REVIEW.md),
  [`.claude/agents/reviewer.md`](.claude/agents/reviewer.md)
- **Teammates share one checkout, so one owner per path,** and a task closes
  green or blocked in writing. → [`docs/teamwork.md`](docs/teamwork.md)

## Conventions

- **Branches:** `<type>/<short-description>`, never `main`.
- **Commits:** `<type>: <subject>`, imperative, ≤72 chars. No issue key.
  → **`/setup-git-hooks`**
- **PRs:** draft by default.
- **Attribution:** commits and PRs are Reljod's, no Claude branding.
  → [`docs/attribution.md`](docs/attribution.md)
