# Working as a team

Agent teams are enabled in `.claude/settings.json`. Teammates are separate
Claude sessions that **share this one checkout** — they are not isolated in
worktrees — so ownership is the whole safety mechanism.

Why teams instead of worktree-isolated subagents:
[`decisions.md`](decisions.md#parallelism-here-is-agent-teams-and-ownership-is-what-makes-it-safe).

## The rules

- **One owner per path.** Each teammate owns a disjoint set of files and edits
  nothing outside it. `.agents/skills/<name>/` is one unit; `install.sh` +
  `bin/` + `tests/install.test.sh` is another. Two teammates in one directory
  means one of them loses work.
- **The lead owns `AGENTS.md` and `README.md`.** Teammates report the charter
  note they think is warranted; the lead writes it. Those are the files every
  teammate would otherwise touch.
- **A task closes green, or blocked in writing — never quietly.** The
  `TaskCompleted` hook (`.claude/hooks/task-completed-tests.sh`) runs every
  `*.test.sh` suite and refuses the completion if any fail, because a teammate
  can go green on its own work while having broken a peer's. It accepts a
  `BLOCKED.md` covering every failing suite as the alternative exit.

## Roles

Reusable teammate roles live in `.claude/agents/`:

| Role | Access | Scope |
|---|---|---|
| `skill-author` | write | exactly one skill directory |
| `toolkit-engineer` | write | the installer + CLI + their suites |
| `reviewer` | read-only | exactly one review lens |
| `investigator` | read-only | exactly one hypothesis about a bug |

Spawn 3–5. Scale by whether the work genuinely splits into disjoint paths, not
by how big it feels.
