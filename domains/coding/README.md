# Coding

Primary tool: **Claude Code**.

How this agent should behave across the repos it's given access to:
branching, commit style, PR habits, and the quality-enforcement layers it
sets up and respects. Domain-specific procedure lives here; identity and
principles stay in [`AGENTS.md`](../../AGENTS.md).

## How code quality is enforced (the layer model)

Quality doesn't come from discipline — it comes from layering cheap
deterministic checks early with mandatory ones later, so nothing depends on
remembering to be careful. Two families of control:

- **Deterministic** — same input → same pass/fail: linting, type checks,
  the commit-message convention, coverage thresholds, required CI status
  checks. Cheap and fast, but only catch what you thought to specify.
- **Non-deterministic** — exploratory/generative: property-based testing,
  mutation testing, fuzzing, AI review. Catch unknown-unknowns, but must be
  wrapped in a deterministic gate ("job must pass") to actually block.

They stack the same way everywhere that does this well:

1. **Local, fast, skippable** — git hooks catch typos, formatting, and
   malformed commit messages in under a second. Bypassable (`--no-verify`);
   never the real gate. → **`setup-git-hooks`**
2. **Server-side, slower, mandatory** — branch protection + required CI
   checks + required reviewers. This is where coverage thresholds and
   "tests must pass" actually bite. Anything safety-critical lives here,
   not only in a hook.
3. **Continuous, not per-PR** — full E2E, fuzzing, mutation testing on a
   schedule; they open a ticket, they don't block a merge.
4. **Upstream of code** — a short spec / behavior list before non-trivial
   work, so "did we build the right thing" is answered before the code
   exists. → **`tdd-loop`** (behavior-list-first)

The skills below automate layer 1 and the test discipline feeding layers 1
and 2. The PR skill makes the output of all of it legible to a reviewer.

## Skills for this domain

Skills stay canonical under [`.agents/skills/`](../../.agents/skills/) (one
source of truth — the charter's rule, not duplicated per domain). This is
the index of the ones that apply to coding, each with its slash command.

| Skill | Slash command | Use it when |
|---|---|---|
| [`create-pr`](../../.agents/skills/create-pr/SKILL.md) | `/create-pr` | Opening a PR — builds a visual-first description a reviewer can approve from the description alone. |
| [`setup-git-hooks`](../../.agents/skills/setup-git-hooks/SKILL.md) | `/setup-git-hooks` | Standing up local git hooks: commit-message convention, pre-commit lint/format, optional pre-push. |
| [`tdd-loop`](../../.agents/skills/tdd-loop/SKILL.md) | `/tdd-loop` | Building a feature or fixing a bug test-first, in a tight red-green-refactor loop. |

Slash commands are thin wrappers in
[`.claude/commands/`](../../.claude/commands/) that read and follow the
skill. Invoke either the command or the skill directly.

### create-pr — digestible pull requests

Show, don't tell. Classifies the diff (UI / API / architecture / tooling /
infra / docs / logic) and reaches for the right artifact per category —
screenshots and GIFs for UI, mermaid diagrams for API/architecture/infra,
terminal output for CLI — instead of a wall of prose. `pr_body_skeleton.sh`
seeds a visuals-first body (Summary → Visuals → What changed → Test plan →
Checks) from the categories the diff actually touches.

### setup-git-hooks — the fast local layer

Installs deterministic, version-controlled hooks via `core.hooksPath`. The
headline check is the commit-message gate: a pure regex enforcing

```
<type>: <TICKET> <subject>      e.g.  feat: JOD-12 add retry to sync worker
```

with `type` in feat/fix/bug/chore/… and `TICKET` a Linear-style key. Also
wires pre-commit lint/format (delegating to Lefthook or the `pre-commit`
framework if the repo already uses one). Remember these are courtesy
checks — the same rules must also be required in CI to actually enforce
them.

### tdd-loop — Loop Engineering

TDD run as an explicit loop: name the next behavior, write one failing test
(RED), minimal code to pass (GREEN), clean up (REFACTOR), repeat.
`tdd-loop.sh` detects the project's test runner and runs a focused test
one-shot (read the RED/GREEN result each turn) or in `--watch` mode for a
human at the keyboard. The one-shot form is also what a pre-push hook or CI
calls.

## Branching

Feature branches only, never directly on `main`. Names follow
`claude/<short-description>-<id>` for agent-driven work (per the charter).

## Commits

Match whatever `setup-git-hooks` installs for the repo you're in. The
default convention this agent standardizes on:

```
<type>: <TICKET> <subject>
```

- `type` ∈ feat, fix, bug, chore, docs, refactor, test, perf, ci, build,
  style, revert.
- `TICKET` is the Linear issue key (e.g. `JOD-12`), required except for
  housekeeping types (chore/docs/style/ci).
- Keep the subject imperative and ≤ 72 chars.

## PRs

Draft by default; open one after pushing if no open PR exists for the
branch. Build the body with `create-pr` — visuals first, and surface that
the deterministic checks are green so review attention goes to judgment,
not to re-verifying the boring correctness. Detect and populate any repo PR
template rather than fighting it.
