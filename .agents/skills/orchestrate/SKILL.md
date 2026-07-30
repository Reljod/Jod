---
name: orchestrate
description: >
  Use when the user wants to delegate work to more than one Claude session
  at once — run a task across several repos, spin up a session per
  workstream, check on or reply to sessions that are already running, or
  act as an orchestrator over an "agent team". Triggers on "delegate this
  to N agents", "run this in every project", "orchestrate", "message that
  session", "what are my sessions doing", "@Project do X". Uses ordinary
  `claude --bg` sessions, so everything it starts is visible and
  controllable from `claude agents` / the agent view.
---

# orchestrate

The `Agent` tool spawns **subagents**: they live inside one conversation,
report back once, and vanish. That is the wrong shape for "run this across
five repos" or "check on the thing you started an hour ago" — a subagent
has no address, no inbox, and no life after its report.

A **session** is the durable unit. It has an id, a working directory, a
transcript that survives, and a slot in the agent view. `jod orc` treats
sessions as the team members.

## The two facts that make this work

1. **`claude --bg "<task>"` starts a session and returns immediately**,
   printing a short id. It is a normal background session — `claude agents`
   lists it, the agent view shows it, `claude attach <id>` opens it.

2. **`claude --bg -r <sessionId> "<message>"` continues an existing session
   with its transcript intact.** This is the inbox. A session that replied
   `PONG` then `PONG-2` answers a later "list every word you have replied"
   with `PONG, PONG-2` — the context is genuinely still there.

Everything below is those two commands plus bookkeeping.

## Running it

The CLI is a single Node script. Invoke it directly — this always works,
including from a plugin install where the `jod` shim is absent:

```sh
node "${CLAUDE_SKILL_DIR}/scripts/orc.mjs" <subcommand> [args]
```

With the toolkit installed (`install.sh`), `jod orc <subcommand>` is the same
thing, shorter. The examples below use `jod orc`; substitute the `node …` form
if `jod` is not on your PATH.

## Commands

```sh
jod orc projects                       # what @names resolve to
jod orc spawn @Socially "audit auth"   # start one, prints its id
jod orc ls                             # id / state / name / cwd
jod orc send 4f21a9 "also check CSRF"  # continue it, in context
jod orc result 4f21a9                  # its final answer
jod orc wait 4f21a9 --timeout=600      # block until done or blocked
jod orc logs 4f21a9                    # raw terminal output
jod orc stop 4f21a9
```

Fan one task across projects — the whole point of an agent team:

```sh
jod orc fanout @Jod @Jod-Apps @Socially -- "list every TODO older than 6 months"
```

Or give each member a different brief:

```sh
jod orc fanout --spec - <<'JSON'
[
  { "project": "@Jod-Apps", "task": "rebase claude/structured-logging onto main" },
  { "project": "@Socially", "task": "summarize open PRs" }
]
JSON
```

## Projects are `@name`

`@Socially` resolves against the directories Claude already knows about
(the `projects` map in `~/.claude.json`), so it works from any cwd with no
workspace config. A plain path works too. `jod orc projects` lists them and
flags the untrusted ones with `!`.

## The one real failure mode

**A session spawned in an untrusted directory hangs forever.** It starts,
appears in the agent view, and stops on an interactive trust/MCP prompt that
no one is watching — so it never reaches the model, and it looks identical
to a session that is thinking hard.

`orc` refuses to spawn there rather than leaving you a zombie. Clear it once:

```sh
jod orc trust @NewProject      # writes ~/.claude.json, keeps a .orc-backup
```

Opening Claude in that directory by hand once does the same thing.

## Orchestrating well

- **Fan out on independent work only.** Sessions share a filesystem. Two
  sessions editing one repo will clobber each other — give each its own
  repo, or its own worktree (`git worktree add`), or run them in sequence.
- **Put the acceptance check in the task.** A session ends when it decides
  it is done. "Refactor X" drifts; "refactor X until `pnpm test` passes"
  terminates. → the charter's *every task needs one runnable check*.
- **`blocked` means it is waiting on a human**, not that it failed. Read
  `state.detail` / `needs`, answer with `jod orc send`, and it resumes.
- **Don't `send` to a `working` session.** That runs a second process
  against one transcript. Wait for `done` or `blocked` first — `orc` warns
  but does not stop you.
- **Harvest, don't re-read.** `jod orc result <id>` returns the session's
  final answer. Reading its whole transcript costs far more and says less.

## What this is not

It is not a scheduler, a queue, or a supervisor that restarts failures.
Sessions are independent processes; if one dies, its work is gone and you
start another. Keep individual briefs small enough that losing one is cheap.
