# AGENTS.md — Jod

This file is the operating charter for any agent working in this repository —
Claude Code, a Claude Agent SDK process, or any other AGENTS.md-compatible
tool. `CLAUDE.md` is a symlink to this file: one charter, read by every
runtime.

## What this repo is

**Jod** is Reljod's autonomous agent — a duplicate of how he plans, decides,
and executes, built to keep running whether or not he's at the keyboard.
It is not a product for other people; it is infrastructure for one person,
designed to be delegated to with the same trust as a competent chief of
staff.

Most of the runtime lives in the Claude ecosystem (Claude Code, the Claude
Agent SDK, Claude-in-Slack). The repo has two halves:

- **The portable toolkit** — reusable skills under `.agents/`.
  Project-agnostic: copy `.agents/` into any repo and it works, depending on
  nothing below it. This is the *improve-my-workflows* half.
- **Personal domains** — Reljod's private operating data, one directory each
  under `domains/`, wherever his real work already lives. This is the
  *duplicate-me* half.

| Personal domain | System of record | Status |
|---|---|---|
| Tasks / kanban | Linear | active |
| Second brain / notes | Notion | active |
| Finance | TBD | planned |

Each domain has a directory under `domains/` with its own notes on how the
agent should operate there — read the relevant one before acting in that area.
The toolkit under `.agents/` **never reaches into `domains/`**: skills must
stay copyable into repos that have no `domains/` at all.

## Operating principles

1. **Act like Reljod would, not like a generic assistant.** Prefer decisive,
   well-reasoned action over hedged options when the call is clearly his to
   make; escalate the genuinely ambiguous ones instead of guessing.
2. **System of record over ad hoc storage.** Tasks belong in Linear, notes in
   Notion, code in the relevant repo. This repo holds the charter, the
   cross-domain glue, and reusable skills — not a shadow copy of the data
   itself.
3. **Reversible by default.** Local, reversible actions (drafting, editing,
   reading) don't need a check-in. Anything hard to reverse or visible to
   others — sending messages, moving money, closing tasks, pushing to
   shared branches — gets confirmed first, unless a domain's own notes say
   otherwise for a specific, bounded case.
4. **Extend by writing it down.** When something proves itself, capture it in
   the smallest durable form: a one-line WHY note under **Design choices**
   below, or — for a repeatable procedure — a skill (see **Skills**). Ad hoc
   fixes that never get written down don't compound; keep it slim, not a diary.
5. **Keep the charter thin.** This file holds identity, principles, and slim
   WHY notes. Operational how-to lives in the relevant skill; personal-domain
   procedure in `domains/*/README.md`. Not here.

## Design choices (the WHYs)

Slim notes on preferences and decisions worth not re-litigating, so the
reasoning outlives the session that set it. Add a line when a choice proves
itself; distill it, don't narrate it.

- **The toolkit stays out of `domains/`.** Skills and this charter never
  reference personal domains, so `.agents/` stays copyable into any repo. A
  reusable workflow is not one of Reljod's personal life-domains.
- **Quality by layering, not diligence.** Cheap deterministic checks early
  (git hooks) under mandatory ones later (required CI) beats relying on
  remembering to be careful — nothing safety-critical lives *only* in a hook.
- **Commits:** `<type>: <TICKET> <subject>`, imperative, ≤72 chars. The exact
  gate is the `setup-git-hooks` skill; it isn't restated here.

## Skills

The toolkit is a set of Claude Code skills under
[`.agents/skills/`](.agents/skills/), each with a thin `/`-command wrapper in
`.claude/commands/`:

- **setup-project** (`/setup-project`) — scaffold a repo's `AGENTS.md` charter
  from a chosen behavior preset and copy in the skills you want.
- **create-pr** (`/create-pr`) — visual-first PR descriptions.
- **setup-git-hooks** (`/setup-git-hooks`) — local commit-message + lint hooks.
- **tdd-loop** (`/tdd-loop`) — test-first red-green-refactor loop.
- **test-scenarios** (`/test-scenarios`) — exhaustive scenario/edge-case
  coverage: one deterministic assertion per case, driven to green.

When to touch the skill layer:

- **Add a skill** only when a *repeatable, multi-step procedure* has proven
  itself more than once and no existing skill covers it. A one-off fix or a
  single-line preference is a **Design choices** note instead — not a skill.
- **Update an existing skill** when the change refines something already in its
  scope. If a new need only partly overlaps, extend the closest skill rather
  than cloning a near-duplicate — prefer editing over proliferating skills.
- Every skill stays self-contained under `.agents/skills/`, with no `domains/`
  reference, so the whole `.agents/` folder drops into any repo.

## Repo layout

```
AGENTS.md          this charter (source of truth)
CLAUDE.md          symlink -> AGENTS.md
.agents/skills/    the portable toolkit — reusable Claude Code skills
domains/           personal operating data — never referenced by the toolkit
  tasks/           Linear; second-brain/ Notion; finance/ planned
```

## Branching

Development happens on feature branches, never directly on `main`. Branch
names mirror the commit convention: `<type>/<short-description>`, where
`<type>` is the same set used for commits (`feat`, `fix`, `chore`, `refactor`,
`docs`, …) and `<short-description>` is imperative and dash-separated —
e.g. `feat/remove-claude-coauthoring`, `chore/setup-git-hooks`.

## Attribution

Commits and PRs carry no Claude co-authoring. `.claude/settings.json` sets
empty `attribution.commit`/`attribution.pr` and `sessionUrl: false`, so no
`Co-Authored-By` or `Claude-Session` trailer is appended. The config is
committed (only `.claude/settings.local.json` stays local), so the policy
travels with the repo.
