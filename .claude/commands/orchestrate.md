---
description: Delegate work to several Claude sessions at once, or message ones already running.
argument-hint: "[what to delegate, e.g. \"@Jod @Socially audit the README\"]"
---

Act as an orchestrator over background Claude sessions using the
**orchestrate** skill at `.agents/skills/orchestrate/SKILL.md`. Read that
skill and follow it.

Wherever that skill writes `${CLAUDE_SKILL_DIR}`, read it as
`.agents/skills/orchestrate` — the skill's own directory in this repo.

The CLI is `node .agents/skills/orchestrate/scripts/orc.mjs` (`… help`).
Where the steps below write `orc`, expand it to that.

Request: $ARGUMENTS

Sessions started this way are ordinary `claude --bg` sessions — they appear
in `claude agents` and the agent view, and outlive this conversation. That
is the difference from the `Agent` tool, whose subagents report once and
vanish.

Steps:
1. **Take stock before starting anything.** `orc ls` — a session that is
   `blocked` is waiting on a human answer, and replying to it with
   `orc send` is usually cheaper than starting fresh work.
2. **Resolve the targets.** `@Name` comes from `orc projects`. If a name
   is ambiguous or untrusted, resolve it with the user rather than guessing —
   spawning into an untrusted directory produces a session that hangs
   silently in the agent view.
3. **Split the work so members do not collide.** Sessions share one
   filesystem: one repo (or one worktree) per session, or run them in
   sequence. Independent work only.
4. **Give every brief its own acceptance check** — "until `pnpm test`
   passes", not "make it better". A session stops when it thinks it is done.
5. **Dispatch.** `orc fanout @a @b -- "<task>"` for one shared task, or
   `orc fanout --spec` when each member gets a different brief. Use
   `orc spawn` for a single session.
6. **Harvest, don't re-read.** `orc wait <ids>`, then `orc result
   <id>` per session. Read a transcript only when a result is unclear.
7. **Report back** the session ids, each one's state, and the synthesized
   result — plus anything left `blocked` and what it is waiting for.

Do not start sessions the user did not ask for, and say what each one will
cost in scope before fanning out widely.
