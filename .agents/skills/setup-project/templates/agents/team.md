<!-- blurb: Conventional Commits, PR/review norms — OSS & multi-contributor -->
# AGENTS.md — {{PROJECT_NAME}}

Operating charter for any agent — and any contributor — working in this repo.
Read by Claude Code, the Claude Agent SDK, and any AGENTS.md-compatible tool.
`CLAUDE.md` symlinks here. Reasoning goes in `docs/decisions.md`, contributor
detail in `CONTRIBUTING.md`, procedure in the skill that owns it.

## What this repo is

{{PROJECT_DESC}}

## Principles

1. **Optimize for the reviewer, not the author.** Small focused changes, a clear
   description, green checks before you ask for eyes.
2. **Reversible by default.** Reading, drafting, editing are free. Confirm
   anything hard to reverse or visible to others first.
3. **Follow existing conventions** over introducing new ones. When in doubt,
   match the surrounding code and `CONTRIBUTING.md`.

## Never work around a blocked check

A check that can't pass makes faking it the cheapest path — and here a
green-but-hollow check is trusted by people who can't see how it got that way.

- **Never**, in service of a check: invent a credential value · swap a real
  integration for a mock to go green · skip, delete or `xfail` a test · weaken
  an assertion · widen an `except`/`catch` to swallow a failure · edit test or
  CI config during an implementation change.
- **Instead** say so in the PR and open `BLOCKED.md` — `Missing:` / `Tried:` /
  `Needs:` + the failing check. A documented blockage is a valid outcome.

## Commits — Conventional Commits

```
<type>[optional scope]: <description>
```

- `type` ∈ feat, fix, docs, style, refactor, perf, test, build, ci, chore,
  revert. `feat`/`fix` map to minor/patch releases.
- Breaking change: `!` after the type/scope (`feat!:`) plus a
  `BREAKING CHANGE:` footer.
- No issue key required — this convention suits open contribution where not
  everyone shares a tracker. → **`setup-git-hooks`** (ships `TICKET_REGEX`
  empty, so this is the default it enforces).

## Branching & PRs

- Never commit to `main`. Branch per change:
  `{{BRANCH_PREFIX}}/<short-description>` for agent work, or a contributor's own
  convention. Keep branches short-lived.
- Open as a draft until CI is green, then mark ready.
- Fill in the repo's PR template; build the body with **`create-pr`** — visuals
  first, then the check's real output rather than a ticked box.
- Every PR needs review + green required checks. Squash unless the repo says
  otherwise.

## Quality gates

- Local hooks are a courtesy layer (**`setup-git-hooks`**); the mandatory gate
  is required CI checks + branch protection on `main`.
- Behavior changes arrive with tests. → **`tdd-loop`**
- Non-trivial work gets its ambiguity resolved in writing first, so review has a
  stated intent to check the diff against. → **`write-spec`**
