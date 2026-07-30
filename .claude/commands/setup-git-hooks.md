---
description: Install deterministic local git hooks (commit-msg convention, pre-commit lint) for a repo.
argument-hint: "[target repo dir, defaults to current repo] [--force]"
---

Set up local git hooks using the **setup-git-hooks** skill at
`.agents/skills/setup-git-hooks/SKILL.md`. Read that skill and follow it.

Wherever that skill writes `${CLAUDE_SKILL_DIR}`, read it as
`.agents/skills/setup-git-hooks` — the skill's own directory in this repo.

Target: $ARGUMENTS (if empty, the current repository).

Steps:
1. Run `.agents/skills/setup-git-hooks/scripts/install-hooks.sh $ARGUMENTS`
   to copy the hooks into `<repo>/.githooks/`, wire up `core.hooksPath`,
   and pre-fill `commit-convention.conf` for the detected ecosystem.
2. Confirm the commit-message convention with the user before locking it
   in — the default enforces `<type>: <subject>` (e.g. `feat: add retries`)
   with **no** issue key required. Adjust `ALLOWED_TYPES` /
   `MAX_SUBJECT_LENGTH` in `.githooks/commit-convention.conf` if they want
   different rules, and only set `TICKET_REGEX` (plus
   `TICKET_EXEMPT_TYPES`) if they explicitly ask to require a ticket key
   like `ENG-123` — that one is opt-in, never assume it.
3. Verify **both** the pass and fail paths deterministically (the installer
   prints the exact commands), then commit the `.githooks/` directory.
4. Remind them these hooks are the fast *local* courtesy layer — the
   mandatory gate is the same checks in CI + branch protection.
