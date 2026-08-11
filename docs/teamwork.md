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

### Check the call site, not the definition

Three times in one session, across two agents, someone verified a claim about
behaviour by reading the function it named — and was wrong each time, because
the claim was about a *use*:

- A key was wired to `Request::Restore(String)` on the strength of its name.
  The `String` is a conversation needle, not a message id, so a typed `57`
  could prefix-match a uuid and move the head of an entirely different thread.
- A ruling on `sweep_recoverable`'s channel filter was checked against
  `mark_failed`'s arithmetic and accepted the caller's premise that
  `mark_attempting` never ran. It does — `redeliver_owed` calls it — so the
  consequence was three restarts to destruction, not a slow drift to staleness.
- The comment recording that finding then named `STALE_AFTER_MS` as the
  mechanism, written by someone with the measured output in front of them
  showing `MAX_ATTEMPTS`.

Every one was a check of a definition standing in for a check of a use, and
each felt exactly like verifying. So:

- **A claim about what happens is verified at the call site.** Find who calls
  it and under what conditions; the body is where you go second, to confirm
  what you found. A signature tells you what is *possible*, never what is done.
- **Run it if you can.** The third failure above was corrected by a throwaway
  harness that printed the state after each simulated restart. Ten minutes of
  scaffolding beat three careful readings by two people.
- **Name the mechanism, not just the outcome, in comments** — "failed after
  three restarts via `MAX_ATTEMPTS`" rather than "settles eventually". An
  outcome with the wrong mechanism attached reads as verified and sends the
  next person to the wrong file.

The general rule: reading the function you were handed feels identical to
verifying and is not. The difference is whether you went looking for the caller
before you agreed.

There is a fourth instance, and it is the one that explains why the shape keeps
recurring between careful people. The comment naming the wrong mechanism was
written by a teammate, reviewed by the lead, and committed — with the measured
output already in the same thread. It went in because **the lead recognised his
own reasoning rather than checking it**: he had told the teammate the staleness
story an hour earlier, so the sentence read as confirmed the moment it arrived.

That is worth separating from the other three. Those were checks aimed at the
wrong place; this one was a check that never happened, because agreement felt
like evidence. A claim that matches what you already told someone is the claim
you are least equipped to review — you are reading your own argument back and
grading it. Treat your own prior reasoning arriving in someone else's words as
unverified, not as corroboration: two people believing something is one belief
when one of them got it from the other.
