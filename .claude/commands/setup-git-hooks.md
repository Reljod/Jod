---
description: Install deterministic local git hooks (commit-msg convention, pre-commit lint) for a repo.
argument-hint: "[target repo dir, defaults to current repo] [--force]"
---

Set up local git hooks using the **setup-git-hooks** skill at
`.agents/skills/setup-git-hooks/SKILL.md`. Read that skill and follow it.

Target: $ARGUMENTS (if empty, the current repository).

Steps:
1. Run `.agents/skills/setup-git-hooks/scripts/install-hooks.sh $ARGUMENTS`
   to copy the hooks into `<repo>/.githooks/`, wire up `core.hooksPath`,
   and pre-fill `commit-convention.conf` for the detected ecosystem.
2. Confirm the commit-message convention with the user before locking it
   in — the default enforces `<type>: <TICKET> <subject>` with a
   Linear-style ticket key (e.g. `feat: ENG-123 add retries`). Adjust
   `ALLOWED_TYPES`, `TICKET_REGEX`, and `TICKET_EXEMPT_TYPES` in
   `.githooks/commit-convention.conf` if they want different rules.
3. Verify **both** the pass and fail paths deterministically (the installer
   prints the exact commands), then commit the `.githooks/` directory.
4. Remind them these hooks are the fast *local* courtesy layer — the
   mandatory gate is the same checks in CI + branch protection.
