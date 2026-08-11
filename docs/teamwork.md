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

## A shared checkout is not a shared source of truth

Teammates own disjoint *paths*, but they run `cargo test` against the *whole
tree*. That gap produced two wrong claims in one session, from two different
agents, in opposite directions:

- One agent ran the suite inside the window where a peer had added something,
  watched it fail, and reverted. It read the failure and the peer's twelve-entry
  table, and reported the change to me as landed. `git status` on that file was
  clean the entire time.
- I did the reverse. I read a peer's `Request::Restore(String)`, assumed the
  `String` could name a branch, wired a key to it and shipped. It is a
  *conversation* needle. Conversation ids are uuids, uuids are hex, so a typed
  message number like `57` can genuinely prefix-match a real conversation — and
  the key would have silently moved the head of an entirely different thread.

Both are the same mistake: treating something read out of a shared working tree
as a fact about the project. So:

- **A red test in a file you do not own is evidence about a moment, not about
  the tree.** Run `git status` and `git diff --stat` on that path before
  reporting anything about it. Uncommitted and reverted look identical to
  `cargo test` and completely different to `git`.
- **A teammate's function is not an API until you have read its signature.**
  Not its name, not its call sites — the signature and its doc comment. "I
  wired it assuming it can" belongs in a message *before* the code, and it is
  the sentence that caught the second failure here.
- **Ask about the assumption, not the outcome.** "Does `Restore` take a branch
  id?" gets answered in a minute. "Here is a key that restores branches" gets
  reviewed as a feature, and the wrong premise inside it travels.

The general rule: in a shared checkout, `git` is the source of truth about what
exists and the author is the source of truth about what it means. The test
runner is neither.

### Stage paths, not the tree

The same rule cuts the other way, and the lead is the one who gets it wrong.
`git add -A` in a shared checkout stages whatever every *other* agent has in
flight and commits it under your subject line. It happened here three times:
`cli/src/tui/sessions.rs` — 932 lines, the whole conversation-graph feature —
landed inside a commit titled *"carry a conversation across a harness"*, which
is a different feature by a different agent. Nothing was lost, and
`git log -- cli/src/tui/sessions.rs` now tells a reader something untrue about
where that code came from.

- **Stage the paths you changed.** `git add cli/src/tui/mod.rs core/src/x.rs`,
  not `git add -A`. If that feels tedious, it is measuring the right thing: a
  commit spanning files you did not touch is a commit whose message cannot be
  accurate.
- **`git status` before every commit, and read it.** An untracked file you do
  not recognise is a teammate mid-task, not a stray.
- **Do not fix it by rewriting.** Once a branch is pushed, the repair costs a
  force-push, which this repo forbids outright. The cost is bounded anyway:
  branches land squashed, so the mis-attribution never reaches `main`. Say it
  in the PR body and move on — a wrong history is cheaper than a rewritten one.

The asymmetry is worth naming. A teammate reading the tree wrongly reports
something false and is corrected in a message. A lead committing the tree
wrongly writes something false into history, permanently, and the author finds
out afterwards.
