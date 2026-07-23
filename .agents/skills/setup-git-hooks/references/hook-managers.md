# Choosing a hook manager

The skill installs **native `core.hooksPath` hooks** by default: no
dependency, works in any language, hooks are plain scripts in a committed
`.githooks/` directory. That is the right choice for a polyglot or
small/solo repo, and it is what `install-hooks.sh` sets up.

Reach for a dedicated manager only when the repo is already committed to
one, or when you specifically want what it offers. The `pre-commit` hook
this skill installs **delegates** to Lefthook or the `pre-commit`
framework automatically if it detects their config, so the two approaches
coexist — the native `commit-msg` gate keeps enforcing the message
convention while the manager handles lint/format.

| Manager | Best when | Cost |
|---|---|---|
| **Native `core.hooksPath`** (this skill) | Polyglot repo, solo/small team, want zero install and full control | Hooks are hand-written shell; no built-in parallelism |
| **Lefthook** | Polyglot repo wanting parallel hooks, glob filtering, a single static binary | One binary to install; YAML config |
| **`pre-commit` framework** | Python-heavy repo, want the large community hook registry and pinned hook versions | Python + virtualenvs per hook; first run is slow |
| **Husky + lint-staged** | JS/TS-only repo already in the npm toolchain | Node-only; adds npm devDependencies |

## Notes

- **Whatever you pick, the message-format check stays deterministic.** It's
  a regex; keep it a regex. Don't route it through a tool that adds network
  or version drift.
- **`core.hooksPath` is local config**, not committed state. Every clone
  runs the installer (or `git config core.hooksPath .githooks`) once. Husky
  and the `pre-commit` framework paper over this with a `prepare`/`install`
  step; the trade-off is an extra dependency.
- **Never gate anything safety-critical on a hook alone.** `--no-verify`
  exists and people use it. The mandatory layer is server-side CI +
  branch protection. A hook is the fast local echo of a CI check, never a
  replacement for it.

## Migrating an existing setup

If the repo already has `.git/hooks/*` scripts (un-tracked, per-clone),
move their logic into `.githooks/` so it becomes version-controlled, then
run the installer to switch `core.hooksPath`. If it already uses Lefthook
or `pre-commit`, keep that for lint/format and just add the native
`commit-msg` gate — the installed `pre-commit` hook will detect and defer
to the existing manager.
