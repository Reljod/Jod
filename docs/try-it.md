# Using Jod to code

What works, how to reach it, and — kept deliberately explicit — what has been
driven for real versus what has only been tested.

Every claim below is marked:

- **verified** — driven end to end against a real harness, with output in
  `tests/e2e/jod/out/`
- **tested** — unit or render tests only; the wiring is asserted but nobody has
  sat in front of it
- **unverified** — believed to work, never exercised

That distinction earned its place. Five features in this build passed every
test they had while being connected to nothing, and two more were wired to the
wrong thing. A green suite is not evidence that a feature exists; see
[decisions](decisions.md#a-unit-test-proves-a-function-only-an-entry-point-test-proves-a-feature).

## Before anything

```
cargo build --release
```

`jod` and `jod-run` must both be on `PATH` — the supervisor is not optional, and
a `jod` that cannot find `jod-run` refuses to spawn rather than pretending.

Everything below assumes the harnesses you want are installed. `jod harnesses`
says which are usable.

## The two automated proofs

Run these first; they are the fastest way to see the system work.

```
bash tests/e2e/a2a.sh              # agents coordinating, ~25 minutes
bash tests/e2e/harness_parity.sh   # roots, cards, secrets, per harness
```

**`a2a.sh` — verified. 23 checks, 0 failures.** Two agents on two *different*
harnesses — an asker on Claude Code, an answerer on OpenCode — exchange a
question and a reply with no human and no CLI in the path, sharing one thread
id, the reply one hop deeper than what it answers. Then a deliberately
pathological pair converses until Jod stops them: thirteen hops, then
`thread paused: 13 hops is past the bound of 12`. At the production default,
not a lowered one.

It is slow because nearly all of it is that runaway climbing to the bound with
real model turns. `A2A_MAX_TICKS` and `A2A_WAKE_WAIT` are the knobs.

**`harness_parity.sh` — verified for Claude Code (12/12).** One run that sets
two roots, reads a file in the second, records a decision, asks a blocking
question answered from the CLI mid-turn, requests a secret by name, and prints
it. The store then contains `TOKEN=[redacted]` and the value appears nowhere in
`jod.db`, its write-ahead log, or its shared-memory file.

AGY is skipped **by name, with its reason** — its MCP config derives from
`$HOME`, and redirecting `$HOME` would take its credentials with it. A harness
that is not installed is never silently passed.

## Working directories and `@`

**Tested.** A conversation owns an ordered set of roots, each writable or not.

```
jod root add <path>          # or Alt-P in the TUI for a fuzzy picker
jod root ls
```

In the chat box, `@` opens a picker under the cursor: type scattered letters,
matches rank live with the matched characters highlighted, arrows move, enter
accepts, escape leaves what you typed alone. With no roots set it says so and
accepts nothing — it will not silently search the process's directory.

There is no `fzf` dependency and there is a test asserting no picker binary is
invoked. Ranking prefers consecutive runs, word and path-segment boundaries, and
matches in the filename over matches in directories.

## The decision rail

**Tested** in the terminal; **verified** that cards are raised from a real run.

An agent records a decision, asks a question, or requests a credential through
Jod's own MCP tools, so it behaves identically on every harness. Cards appear in
a left rail. Expand one to answer by digit or in prose.

**Answering never interrupts a running turn.** The answer is queued, and a
handler injects it at the next safe boundary, batching everything that
accumulated into one turn rather than one turn each. So a card shows as
*answered, queued* until it has actually been delivered, and then *delivered* —
the rail does not pretend an answer landed before it did.

Answer ten cards while a run is mid-turn and none of them touch it until it
comes up for air.

## Credentials

**Verified.** Requested through the rail, stored outside every repository at
owner-only permissions, injected into the harness's environment at spawn, and
scrubbed out of everything it prints before Jod parses it.

The guarantee, stated precisely, because a looser version of this sentence was
wrong for most of the build: **the value never reaches the record.** Not the
database, not the transcript, not the launch plan. Whether the *model* sees it
is decided by the preamble telling it not to go looking — the supervisor sits
between the harness and Jod's store, not between the harness and the model, so
a harness's own tool loop returns command output to the model before Jod ever
sees the line. → [decisions](decisions.md)

A missing credential blocks one test, not a session. The agent is told to treat
an absent key as a *blocked* ending rather than a reason to invent one.

## Works, sessions and worktrees

**Tested**, and the path was broken for most of this build — see the caveat.

"Work on @some-repo, do X" opens a *work*: a titled group of sessions. A session
starts pointed at your real checkout **read-only** and claims a git worktree of
its own the moment it needs to write, through an explicit `claim_worktree` call.
The original stays beside it, readable, so it can diff against what you are
editing.

Claiming is the agent's step rather than something Jod infers, because "detect
the first write" has no harness-agnostic implementation. **Roots are a
convention, not a sandbox** — passing one grants; withholding one does not deny.

A work closes itself when the last item on its board is done, and raises a
closing card naming the branches, pull requests and leases it left behind.
Deleting a work refuses the first time if it holds a worktree, prints what would
be lost, and proceeds only if the same command is repeated. **Deleting never
removes a worktree or its branch** — records are cheap to recreate and a branch
with uncommitted work on it is not.

> **Caveat, and it is recent.** Until `f019467` a work's session could complete
> exactly one turn: the harness's session id was recorded only by the process
> that launched the run, and for a session opened through a work that process
> exits when the turn ends. It is fixed, and the fix is not yet verified by a
> hand-driven second turn. Do that before trusting a long-lived work.

## Agents coordinating

**Verified** — this is what `a2a.sh` proves.

Sessions in a work address each other by name with no join step. `send_message`,
`read_messages`, `roster`, `ask`, `reply` and `handoff` are MCP tools, so they
work the same on every harness.

**A sender's identity comes from the run and cannot be argued with.** The MCP
server resolves its own process group against the run that owns it; an agent
passing `from`, `sender` or `as` has all three ignored. If a claimed identity
disagrees with the process group, the call is refused and both answers are named
rather than one being quietly chosen.

Every conversation is bounded three ways — depth in a thread, messages per work,
and a deadline on any wait. Hitting a bound pauses **that thread only**, not the
work and not the sessions.

Mail delivers itself: a member holding waiting mail is resumed by Jod's tick,
rate-limited so ten messages become one turn carrying ten rather than ten turns.

## Coding in the terminal

`jod tui`. **Tested**, none of it hand-driven.

| | |
|---|---|
| `Esc` | interrupt the turn, keep the session — the conversation survives |
| `@` | mention a file across every root |
| `Alt-P` | pick a directory to add as a root |
| `Alt-S` | search every transcript, including compacted turns |
| `Alt-Y` | copy the last reply, or its code block without the fences |
| `Alt-K` | which-key |

File edits render as diffs with the path as a header and counts. A todo list
renders as one block revised in place rather than a new block per revision.
Plans, cost, context usage, fork, rewind and cross-harness handoff were already
there.

**Per-tool allow/deny prompts do not exist and are not coming.** Jod runs
harnesses in print mode, where the permission mode is fixed at spawn with no
interactive callback to hang a prompt on. The blocking card is the substitute:
an agent that wants permission raises one, in the rail you are already watching.

## What is not done

- **Discovered commands do not reach the palette.** `jod commands ls` finds a
  repository's slash commands and skills; the TUI's palette is still a separate
  hardcoded list.
- **The traffic view.** Agent-to-agent messages are stored and bounded but there
  is no screen showing who said what to whom.
- **A2A across works.** Only the orchestrator crosses works, by design, but the
  human is not addressable from inside one — an agent replying to you is told
  you are not a member.
- **OpenCode's extra roots.** Measured: `--dir` takes exactly one directory and
  repeating it kills the process before any model call. Its other roots reach
  the agent as prose and Jod does not claim to have granted them.
- **Several dead wrappers** — an older function left beside the newer one that
  replaced it. Harmless, and the sort of thing somebody later calls by mistake.
