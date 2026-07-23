---
name: setup-git-hooks
description: >
  Use when the user wants to set up, add, or standardize local git hooks
  for a repo — pre-commit lint/format checks, a commit-message convention
  (e.g. enforce a "feat/chore/bug: LINEAR-123" prefix), or a pre-push
  gate. Triggers on "add a pre-commit hook", "enforce commit message
  format", "set up git hooks", "block commits without a Linear ticket".
  Installs deterministic, version-controlled hooks via core.hooksPath.
---

# setup-git-hooks

Local git hooks are the **first, fastest, cheapest** layer of the quality
stack — deterministic checks that run on the developer's machine before a
commit or push even exists. They are a *courtesy layer*, not the real gate:
`git commit --no-verify` bypasses them, and every serious org assumes it
can be bypassed. So nothing safety-critical lives *only* here — the same
checks must also run server-side in CI as required status checks.
What hooks buy you is a sub-second feedback loop that catches typos,
formatting drift, and malformed commit messages before they ever reach a
PR.

Two properties matter for everything this skill installs:

1. **Deterministic.** Same input → same pass/fail, every time. A regex on
   the commit message either matches or it doesn't. No network, no clock,
   no LLM judgment in the hot path. That is what makes a hook safe to gate
   on locally.
2. **Version-controlled.** Hooks go in a committed `.githooks/` directory
   wired up with `git config core.hooksPath .githooks`, not the
   un-tracked `.git/hooks/`. That way the whole team (and every fresh
   clone) gets the same hooks, and they evolve through normal PRs.

## Worked example: the commit-message gate

The canonical ask is "every commit must start with `feat/chore/bug:
LINEAR-123`". A note on git mechanics first: **message validation belongs
in the `commit-msg` hook, not `pre-commit`** — `pre-commit` runs *before*
the message is written, so it can't see it. This skill puts the format
check where it actually works. The check is a pure regex, so it is fully
deterministic:

```
<type>(optional-scope)!: <TICKET> <subject>
   │        │          │      │        └─ imperative description
   │        │          │      └────────── Linear-style key, e.g. ENG-123
   │        │          └───────────────── optional breaking-change marker
   │        └──────────────────────────── optional scope, e.g. (api)
   └───────────────────────────────────── feat | fix | bug | chore | ...
```

`feat: ENG-123 add retry to the sync worker` passes.
`update stuff` fails. `feat: add retry` fails (no ticket).

## How to run it

1. **Install into the target repo.** From inside the repo that should get
   the hooks (or pass its path):

   ```
   .agents/skills/setup-git-hooks/scripts/install-hooks.sh [target-repo-dir]
   ```

   This copies the hook templates into `<repo>/.githooks/`, sets
   `core.hooksPath`, makes them executable, and writes a
   `.githooks/commit-convention.conf` pre-filled with lint/format commands
   detected for the repo's ecosystem. It will **not** clobber an existing
   `commit-convention.conf` unless you pass `--force`, so local tuning
   survives re-runs.

2. **Tune the convention.** Open `.githooks/commit-convention.conf` and
   adjust `ALLOWED_TYPES`, `TICKET_REGEX` (default matches Linear keys like
   `ENG-123`), `TICKET_EXEMPT_TYPES`, and the `PRECOMMIT_CMDS` /
   `PREPUSH_CMDS` lists. Everything the hooks enforce is driven from this
   one file — no logic changes needed for the common cases.

3. **Verify it's live and deterministic.** Prove both the pass and fail
   paths before declaring done:

   ```
   echo "update stuff"              | .githooks/commit-msg /dev/stdin   # should FAIL
   echo "feat: ENG-123 add retries" | .githooks/commit-msg /dev/stdin   # should PASS
   ```

   (The installer prints the exact commands for the repo.) A real
   `git commit -m "bad message"` should now be rejected.

4. **Commit the hooks.** `.githooks/` is tracked — commit it so teammates
   and fresh clones inherit the same gate. Note that `core.hooksPath` is
   *local* git config, so each clone still runs the installer (or a
   one-line `git config core.hooksPath .githooks`) once; the installer
   output says this.

## What each hook does

- **`commit-msg`** — the deterministic message gate described above. Reads
  its rules from `commit-convention.conf`. Skips merge/revert/fixup/squash
  auto-messages so it never fights git's own plumbing.
- **`pre-commit`** — runs fast, staged-file-scoped checks (lint, format
  `--check`, import sort). Delegates to an existing hook manager if the
  repo already uses one (`lefthook`, the `pre-commit` framework); otherwise
  runs the `PRECOMMIT_CMDS` from the conf. Keep these **fast** (< a few
  seconds) — anything slow belongs in CI.
- **`pre-push`** — optional heavier gate (e.g. the fast unit-test subset).
  Off by default (`PREPUSH_CMDS` empty) because slow pre-push checks train
  people to reach for `--no-verify`. Enable it only for a genuinely quick
  suite.

## Choosing a hook manager

The native `core.hooksPath` approach this skill installs is the
zero-dependency default and works for any language. If the repo is already
committed to an ecosystem tool, prefer integrating with it instead of
fighting it — the installer detects and delegates. See
`references/hook-managers.md` for when to pick Husky (JS-only),
Lefthook (polyglot, single binary, parallel), the `pre-commit` framework
(Python-native, huge hook registry), or staying native.

## Boundaries

- Do not put secrets, network calls, or anything non-deterministic in a
  hook — it breaks the "same input → same result" contract and makes
  failures impossible to reason about.
- Do not treat hooks as the enforcement layer. They are the courtesy
  layer. The mandatory gate is server-side CI + branch protection; a hook
  that isn't *also* a required CI check is advisory only.
- Keep `pre-commit` fast. If a check takes more than a couple of seconds,
  move it to `pre-push` or CI.
