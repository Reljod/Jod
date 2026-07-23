# Conventions

Portable coding conventions and workflow for any project this agent touches:
branching, commit style, PR habits, and the quality-enforcement layers it sets
up and respects.

This is the reusable toolkit, not personal data. The whole `.agents/` folder is
meant to be **dropped into any repo** — copy it in and you get the skills
([`skills/`](../skills/)), these conventions, and the retro loop
([`retros/`](../retros/)), all self-contained.

## Portability rule

Nothing in the toolkit — the skills, these conventions, or
[`AGENTS.md`](../../AGENTS.md) — may reference or route into `domains/`.
`domains/` holds one person's private operating data (Linear, Notion, finance
notes); the toolkit is project-agnostic and must stay copyable into repos that
have no `domains/` at all. Learnings about how the agent codes and works land
**here**, in a skill, or in the charter — never in a personal domain.

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

## Skills

Skills stay canonical under [`skills/`](../skills/) (one source of truth). This
is the index, each with its slash command.

| Skill | Slash command | Use it when |
|---|---|---|
| [`create-pr`](../skills/create-pr/SKILL.md) | `/create-pr` | Opening a PR — builds a visual-first description a reviewer can approve from the description alone. |
| [`setup-git-hooks`](../skills/setup-git-hooks/SKILL.md) | `/setup-git-hooks` | Standing up local git hooks: commit-message convention, pre-commit lint/format, optional pre-push. |
| [`tdd-loop`](../skills/tdd-loop/SKILL.md) | `/tdd-loop` | Building a feature or fixing a bug test-first, in a tight red-green-refactor loop. |
| [`create-retro`](../skills/create-retro/SKILL.md) | `/retro` | Closing out a session that had real back-and-forth or diverged from the agent's first attempt — captures the WHYs into these conventions. |

Slash commands are thin wrappers in [`.claude/commands/`](../../.claude/commands/)
that read and follow the skill. Invoke either the command or the skill directly.

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

### create-retro — turn corrections into standing preferences

The feedback loop for the agent itself. It gates first: run only when the
session earned it — real back-and-forth with the user, or a shipped result
that diverged from what the agent first produced — and stay quiet on clean,
first-try sessions. When it runs, it mines each correction for the **WHY**
(coding-style preference, design/architecture rationale, workflow correction),
folds the distilled rule into the doc the next session will read (these
conventions, a skill, or the charter), and keeps the full reasoning trail as a
dated entry under [`retros/`](../retros/). Distinguishes a one-off (parked as
not-yet-durable) from a real preference (promoted on the second occurrence),
so the conventions don't fill with over-fitted rules.

## Branching

Feature branches only, never directly on the default branch. For agent-driven
work, names follow `claude/<short-description>-<id>`.

## Commits

Match whatever `setup-git-hooks` installs for the repo you're in. The default
convention this toolkit standardizes on:

```
<type>: <TICKET> <subject>
```

- `type` ∈ feat, fix, bug, chore, docs, refactor, test, perf, ci, build,
  style, revert.
- `TICKET` is the issue tracker key (e.g. `JOD-12`), required except for
  housekeeping types (chore/docs/style/ci).
- Keep the subject imperative and ≤ 72 chars.

## PRs

Draft by default; open one after pushing if no open PR exists for the
branch. Build the body with `create-pr` — visuals first, and surface that
the deterministic checks are green so review attention goes to judgment,
not to re-verifying the boring correctness. Detect and populate any repo PR
template rather than fighting it.
