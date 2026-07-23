# AGENTS.md — {{PROJECT_NAME}}

Operating charter for any agent (and any contributor) working in this
repository. Read by Claude Code, the Claude Agent SDK, and any
AGENTS.md-compatible tool. `CLAUDE.md` is a symlink to this file.

## What this repo is

{{PROJECT_DESC}}

## Principles

1. **Optimize for the reviewer, not the author.** Small, focused changes;
   a clear description; green checks before you ask for eyes.
2. **Reversible by default.** Reading, drafting, and editing are free.
   Confirm anything hard to reverse or visible to others first.
3. **Follow existing conventions** over introducing new ones. When in doubt,
   match the surrounding code and the norms in `CONTRIBUTING.md`.

## Branching

Never commit directly to `main`. Branch per change:
`{{BRANCH_PREFIX}}/<short-description>` (agent-driven work) or a contributor's
own convention. Keep branches short-lived.

## Commits — Conventional Commits

```
<type>[optional scope]: <description>
```

- `type` ∈ feat, fix, docs, style, refactor, perf, test, build, ci, chore,
  revert. `feat` and `fix` map to minor/patch releases.
- Breaking changes: add `!` after the type/scope (`feat!:`) and a
  `BREAKING CHANGE:` footer.
- No ticket key is required — this convention is designed for open
  contribution where not everyone shares an issue tracker. → **`setup-git-hooks`**
  (set `TICKET_REGEX` empty to enforce Conventional Commits without a ticket).

## Pull requests

- Open against `main` as a draft until CI is green, then mark ready.
- Fill in the repo's PR template. Build the body with **`create-pr`** —
  lead with visuals and the passing-checks summary so reviewers spend their
  time on judgment, not re-verifying correctness.
- Every PR needs review + green required checks before merge; keep the
  history clean (squash unless the repo says otherwise).

## Quality gates

- Local hooks are a courtesy layer (**`setup-git-hooks`**); the mandatory
  gate is required CI checks + branch protection on `main`.
- Add tests with behavior changes; **`tdd-loop`** for test-first work.

## Skills

Reusable skills live under `.agents/skills/<skill-name>/SKILL.md` with a
thin slash-command wrapper in `.claude/commands/`.
