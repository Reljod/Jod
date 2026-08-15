# Orchestration — routing, worktrees and the transcript that forgets

How this was tested: the built binary at `target/debug/jod` (harness `claude`,
model `claude-opus-5`) against a fresh, isolated `JOD_HOME` —
`/home/reljod/.claude/jobs/cd76af0f/tmp/jodhome-orch` — never `~/.jod`. Most
scenarios below are **real runs**, not guesses: `jod main "…"` or
`jod main --wait "…"` against a scratch git repo at
`/home/reljod/.claude/jobs/cd76af0f/tmp/scratch-repo`, with the conversation
and run state read back afterwards straight out of `jodhome-orch/jod.db` with
`python3 -c "import sqlite3…"`, and cross-checked against `jod watch <run>`,
which replays a run's own event log independently of the conversation
transcript. A few of the harder edge cases were read off the code instead,
and are marked `needs confirming`.

Read first, so as not to refile them: `git show
docs/spec-ceo-and-managers:SPEC.md`, `tasks/00-launch-and-roots.md`,
`tasks/01-routing.md` (including its live addenda R4–R6). Nothing below
repeats L1–L6 or R1–R6; several findings corroborate them instead, and say so.

---

## O1. A work session can never actually write anything — the read-only-root-then-claim-a-worktree design is unreachable
Status: **open — needs Reljod's decision, deliberately unclaimed** · Severity: critical
**Still reproduces on main `730e63b`** — a third independent reproduction,
found incidentally while running L4's check rather than by looking for it. The
work opened, the session claimed a worktree
(`worktrees/add-contributing-md-with-prs-wel-1a6da747/`), and the worktree
contains only `README.md` with its git log still reading `init`. Nothing was
written and nothing committed, after every fix merged today.
Verified independently by two further readers. Nobody needs to check it a
third time — spend the effort on the decision instead.

- The `--add-dir` push is at `core/src/harness/claude.rs:75`, inside the same
  function that assembles the rest of the command line, so it is fixed at
  process launch.
- `claim_worktree` (`core/src/mcp.rs:1952`) returns a `String` and nothing else.
- A grep of all of `core/` for any other path touching `add_dir` found only
  comments and tests. **There is no later channel**, so this is exhaustive
  rather than a sample.

**This needs Reljod's decision, not an implementation.** Two shapes were named,
and they are materially different rather than variants of one fix: cut the
worktree before launch and grant it up front, which changes when a worktree is
created for every work; or restart the session with the wider grant after a
claim, which makes a claim a process boundary and breaks the assumption that a
work session is one continuous transcript. Do not hand this to an agent to just
implement.

**Observed, twice, end to end.** `jod main --wait "In the scratch project,
add a one-line CONTRIBUTING.md that says 'PRs welcome.' and commit it."`
correctly routed to `open_work` (run `e85eea7b`). Its own log
(`jod watch e85eea7b`):

```
I'll need a writable worktree first, since the checkout is read-only.
⚙ mcp__jod__claim_worktree
⚙ Write · .../jodhome-orch/worktrees/in-the-scratch-project-add-a-one-dfbecdfa/scratch-repo/CONTRIBUTING.md
✗ Write
⚙ Bash · ls -a .../worktrees/.../scratch-repo ...
✗ Bash
Blocked — [...] This session's sandbox only permits access to
`/home/reljod/.claude/jobs/cd76af0f/tmp/scratch-repo`. Writing the file in
the worktree was denied, and even `ls` there is blocked as outside the
allowed working directories.
```

A follow-up (`continue_agent`, run `5d5d6b57`) hit the identical wall and
correctly reported the block was unchanged — nothing was written, nothing
committed, `main` still at `410dd46 init` with only `README.md`. Verified
directly: the worktree the lease claimed
(`jodhome-orch/worktrees/in-the-scratch-project-add-a-one-dfbecdfa/scratch-repo`)
has no `CONTRIBUTING.md` and its `git log` is still just `init`.

**Cause.** Two design decisions, both individually documented and reasonable,
that contradict each other:

- `prepare_work` (`core/src/orchestrator.rs:1080`) launches the session with
  only the **checkout** in `SpawnRequest.roots`, read-only, per D5 — a
  worktree is deliberately *not* cut until the session asks for one.
- The harness adapters turn `SpawnRequest.roots` into `--add-dir` flags **once,
  at process launch**, and nowhere else: `core/src/harness/claude.rs:59-78`
  and `core/src/harness/agy.rs:62-78` both build the flag list from `req.roots`
  in the same function that assembles the rest of the command line. There is
  no live channel afterwards to widen a running process's sandbox.
- `claim_worktree` (`core/src/mcp.rs:1952`) cuts the branch and returns its
  path as an MCP tool **result** — a string the model reads — and touches
  nothing on the harness side. It cannot, because by the time it runs the
  process is already up with its `--add-dir` list fixed.

So the sequence the design calls for — start read-only, claim a worktree the
moment you need to write — is structurally impossible within one session for
both harnesses tested (Claude Code and AGY both build their sandbox this way;
OpenCode drops roots entirely and is a separate, already-known gap). Every
`open_work` session that needs to write anything, which is the entire reason
`open_work` exists over `delegate`, deadlocks the same way this one did. The
model's response was as good as it could be — it explained the conflict
precisely and asked for one of two ways out — but the conflict is Jod's, not
the model's to solve.

**Fix.** Needs a design decision, not just a patch — two directions get
mentioned by nothing else in the repo, so pick one:
1. Cut the worktree **before** spawning the first session (loses "no worktree
   until it's needed", D5's whole point), or
2. Give `claim_worktree` a way to actually widen the running process's
   sandbox — for Claude Code that likely means the settings-file
   `permissions.additionalDirectories` route (mentioned only in passing in a
   skill description, never used from `core/`), which can plausibly be
   updated and picked up without a restart; needs confirming per-harness.
Either way, the current state — a design that looks complete in prose and
cannot write a single file in practice — is worse than an honest one.

**Check:** `open_work` a session on a fresh git repo with an instruction that
requires a write, run it to completion (real harness, not mocked), and assert
a commit exists on the leased worktree's branch afterwards.

---

## O2. The pinned main chat's own transcript can permanently lose the answer
Status: **verified fixed — check run against main, passes** · Severity was: high

The check was executed in full, not inferred from the merge.

*Uncontended*: one instruction through `jod main --wait`. The run emitted one
`message` event; the conversation holds one assistant message.

*Contended*: a second instruction sent while the first was still running — the
case the task says reproduced independently. Three runs, one `message` event
each; the conversation holds three assistant messages. Nothing lost.

*Tool calls and results*, which the first two runs could not exercise because
they were told to use no tools. Checked against a separate live run that did:

```
run 010711fe  message 1 · tool_call 7 · tool_result 7
run 59b8f982  message 1 · tool_call 7 · tool_result 7
conv 12882652 assistant 1 · tool_call 7 · tool_result 7
conv 96106c1d assistant 1 · tool_call 7 · tool_result 7
```

Every event kind the check names reaches the transcript, in both contention
states. Recorded in this detail because the first pass of this verification was
itself incomplete — instructing the model to use no tools left the
`ToolCall`/`ToolResult` half of the check unexercised while looking like a
pass, which is the same failure this list keeps finding.

### What "fixed" means here, and the trap on the way

**This is a recording bug, not an execution bug.** The run's effects really
happened — a schedule was genuinely armed and `jod schedule ls` proves it — so
any fix that makes the reply reappear by re-running, retrying, replaying or
reconstructing the turn is the wrong shape. In the observed case, re-running
would arm the schedule twice. Fixed means: the event the harness actually
emitted is recorded exactly once.

**The trap is `Finished.text`, and it is subtler than "do not use it".**
`Conversation::from_event` (`core/src/conversation.rs:209`) deliberately
excludes it, and the stated reason is exact:

> `Finished.text` is always a repeat: every harness adapter fills it from the
> last `Message` it already emitted (`Accumulator::note_text`), so appending it
> would double the final assistant turn.

That reasoning is correct for the healthy path. But read it against this bug and
it cuts both ways: because the adapter fills `Finished.text` from the last
`Message` it emitted, **the lost text is very likely still sitting in
`Finished.text` in exactly the failing case.** So a fallback would appear to fix
it — while duplicating every normal reply, and while being a recovery path
rather than a repair.

That makes it a diagnostic rather than a fix. If the adapter still holds the
text, the `Message` event existed at the adapter and was lost after it, which
narrows the drop point to between the adapter and `record_in_conversation`
rather than to the harness. Worth checking first: it is cheap and it discriminates.

Do not change the exclusion as a side effect of fixing this. If the diagnosis
genuinely leads there, argue for it explicitly.

A precise drop point with instrumented evidence is a better outcome than a
guessed fix, and delivering the diagnosis alone is an acceptable result.

**Observed, twice, independently — once under concurrency, once without.**
`jod main` (the CLI command that prints the pinned conversation) reads the
`messages` table. It does not always contain what the run actually said or
did, even when the run finished successfully and its side effects really
happened.

**Case A — no concurrency at all.** `jod main --wait "every day at 9am,
remind me to check the scratch project's open issues"` (run `979f57f7`,
started only after the previous turn had already returned). `jod watch
979f57f7` shows the whole turn, ending:

```
⚙ mcp__jod__schedule_create · Raise a card for Reljod reminding him...
Armed as `scratch-issues-daily`, 9am daily in **Asia/Manila** (inferred...)
✓ done · $0.2507 · 730 out
```

And `jod schedule ls` confirms it really was armed:
`● scratch-issues-daily   0 9 * * *   Aug 16 01:00`. But the `messages` table
for that run stops at message id 40 — the *first* tool call
(`ToolSearch select:mcp__jod__schedule_create`) — and never gets the
`schedule_create` call, its result, or the final "Armed as…" text. `jod main`
permanently shows the turn as if it stalled after a tool lookup, though the
schedule really was created and the model really did report back.

**Case B — two runs resuming the same session concurrently.** A second
instruction (`"quick: what is 2+2?"`, run `439d4bf9`) was sent while an
earlier one (`In this repo, add a...`, run `f6501278`) was still running.
Both runs share `session_id = fc24c70d-f651-4917-96a9-bb935f00fbd8` — expected,
since both resume the same pinned conversation, but it means **two harness
processes were told to resume the identical session concurrently**. `jod
watch 439d4bf9` shows a clean one-word answer, `"4"`, for $0.0145. Nothing
with `run_id = 439d4bf9` appears in `messages` except the user's own prompt
row — the reply is gone from the transcript Reljod reads, though it was said
and it cost money.

**What I ruled out reading the code**, so whoever picks this up does not
re-walk the same path: the per-agent `conversation_id` map is populated
*before* the process launches (`core/src/service.rs:991`, comment: "Register
before launching, so no event can arrive before its agent"), and the
dedupe key on `messages` is `(run_id, run_seq)` — scoped per run, not per
conversation (`core/src/conversation.rs:742-785`) — so a second run's events
cannot be silently treated as duplicates of the first's. Both of the obvious
races are closed. The loss has to be upstream of `record_in_conversation`
(`core/src/service.rs:387`) — most likely in how a harness's stdout becomes
`AgentEnvelope`s and reaches the single `events_rx.recv()` consumer
(`core/src/service.rs:534`) that everything else in this list is fed from.
Also worth noting: `NewMessage::from_event` deliberately never falls back to
`Finished.text` (`core/src/conversation.rs:209-211`, "always a repeat") — so
if the live `Message` event for the final answer is ever missed, there is no
second chance to recover it from the run's own completion event.

**Fix:** not yet isolated. Whoever picks this up should instrument
`events_rx.recv()` in `service.rs:534` to log every envelope's
`(agent_id, seq, event kind)` for a run that reproduces this, and diff it
against what `jod watch` replays for the same run — that will say whether the
event never arrived, arrived and was misrouted, or arrived and the write
failed silently (all three log through `eprintln!`, so check stderr on the
daemon/CLI process too, not just the DB).

**Check:** run an instruction through `jod main --wait` to completion, then
assert every `Message`/`ToolCall`/`ToolResult` event `jod watch` shows for
that run also appears in `messages` for the conversation. Do this once
uncontended and once with a second instruction sent while the first is still
running, since both reproduced independently.

---

## O3. `continue_agent` never checks whether the run it is continuing is dead
Status: open · Owner: — · Severity: medium · needs confirming

Read off the code, not fully observed: `continue_agent`
(`core/src/mcp.rs:1110-1179`) refuses only on a missing `session_id` or a
permission-ceiling mismatch. It never checks `agent.status` — a `killed` or
`failed` run with a recorded session id passes both checks and would be
resumed exactly as if it had ended cleanly.

I killed a running agent (`jod kill 25d9c05a`, confirmed `status = killed`,
`session_id` still recorded) and asked main to continue it. It refused —
but for an unrelated reason (`jod run`'s default permission sat above this
server's ceiling), so the status question was never actually exercised. The
main chat's own reply happened to mention "it's also in `killed` state" as an
aside, which means *the model* read that off `list_agents`' output and chose
to flag it — not that the tool enforces it. A model that does not think to
check, or a killed run whose permission happens to fit under the ceiling,
would sail through.

Fix: refuse `continue_agent` on a `killed` or `failed` run at the tool
boundary, the same way the permission ceiling is enforced now, and name
`delegate`/`open_work` as the way to start fresh beside it — this is close to
what the unmerged CEO/manager spec already prescribes for *stalled* runs
(`docs/spec-ceo-and-managers:SPEC.md`, Decisions section: "A stalled session
is... never continued"); a killed one deserves the same treatment and today
gets none.

Check: `continue_agent` on a run with `status = killed` and a valid session id
under the permission ceiling is refused, and the refusal names the run's
status.

---

## Scenarios that passed, with evidence

## O4. A quick factual question gets answered directly, despite the preamble forbidding it
Status: informational — corroborates R1, adds a data point in the other direction

`jod main --wait "What does A2A stand for, in one word or phrase?"` returned
`"Agent-to-Agent — Google's open protocol for interoperability between AI
agents."` directly, with **zero** delegated agents spawned (`jod ls` showed
only the orchestrate run itself). This is the opposite of R1's own reproduction
in `tasks/01-routing.md` ("what does A2A stand for in this project?" spawned a
sub-agent and never answered). Both are real: the preamble
(`core/src/orchestrator.rs:353`) still has no branch that says "answer
directly," so whether that happens is up to the model's own judgment on the
day, not to anything Jod enforces. R1's fix — give the preamble an explicit
first branch — is what would turn this from a coin flip into a rule.

## O5. "btw, no project named" resolves against the current project, and correctly recognizes it's still the same blocked work
Status: pass

With the CONTRIBUTING.md work already open and stuck on O1's worktree
deadlock, `jod main --wait "btw, let's also add a .gitignore for
node_modules"` did not spawn anything new. It correctly reasoned this was the
same repo, the same work, and reported: *"I'm holding it rather than sending
it: the cyan session is still stuck... One answer unblocks both files."*
Exactly the behavior the preamble asks for around `project_current` and
exactly what Reljod described wanting for an instruction that names no
project.

## O6. `continue_agent` is chosen correctly for a same-task follow-up
Status: pass

While work session `e85eea7b` was mid-flight, `jod main --wait "Also tweak
the CONTRIBUTING.md wording... same task as before."` correctly called
`continue_agent` on `e85eea7b` rather than opening a second work or a fresh
delegate. It worked mechanically too: the follow-up run (`5d5d6b57`) resumed
the same session and picked up exactly where the first left off (including
correctly noting nothing had actually been written yet — see O1).

## O7. "every day at 9am" routes to `schedule_create`, not a one-shot
Status: pass

Covered fully under O2/Case A. Routing chose `schedule_create` correctly,
inferred a timezone and flagged it as inferred rather than guessing silently,
and the schedule really was armed (`jod schedule ls` confirms it). The only
problem with this exchange is O2 — the transcript lost the reply, not the
routing decision.

## O8. Empty and whitespace-only instructions do not send anything
Status: pass

`jod main ""` and `jod main "   "` both fell back to showing the chat rather
than sending a blank instruction — `cli/src/main.rs:2335`,
`instruction.trim().is_empty()`. Verified against the DB: the message count
was identical (17) before and after both calls. Clean, deliberate guard.

## O9. No roots means main correctly declines rather than guessing or crashing
Status: pass · related to L1/L4, not a new instance of either

Before any root or project existed, `jod main --wait "In this repo, add a
one-line CONTRIBUTING.md..."` never called `open_work` at all — it reasoned
ahead of L4's refusal, using `project_current`/`project_list`/`list_agents`,
and answered: *"There's no repo here to act on... Which repo do you mean?"*
This is a good outcome layered on top of L1 — the underlying cause (no roots
on a fresh main chat) is exactly what L1 describes, but the model's handling
of hitting it here was correct rather than a crash or a guess.

## O10. Every real run opened with 1–2 `ToolSearch` calls before touching a single Jod tool
Status: informational — corroborates R5, adds that it is universal, not occasional

Every one of the ~10 real orchestrator turns run for this file opened with
`ToolSearch select:mcp__jod__…` before its first actual Jod tool call — for
tools the orchestrator's own preamble already lists as its toolbox. R5 in
`tasks/01-routing.md` flags this once; on this evidence it is not occasional,
it happened on every single turn observed, adding a fixed round-trip of
latency and a small but real cost tax to literally everything the main chat
does. Worth reconsidering R5's severity as `high` rather than `medium` on
that basis.

---

## Scenarios run

| # | Scenario | Expected | Observed | Result |
|---|---|---|---|---|
| 1 | Quick factual question | Answered directly | Answered directly, no agent spawned | pass (O4, coin-flip per R1) |
| 2 | Repo work, no roots/project | Clean refusal, no crash | Explained it needed a repo, asked which one | pass (O9) |
| 3 | Repo work, root + project set | Routes to `open_work` | Correctly called `open_work`, opened a work and a session | pass |
| 4 | Same task, follow-up while running | `continue_agent`, not fresh spawn | Correctly called `continue_agent` on the live run | pass (O6) |
| 5 | Several agents running at once | Routing still coherent | Multiple concurrent `main`/work runs; routing stayed correct | pass, see O2 for a side effect |
| 6 | No agent running (fresh state) | Starts fresh, no confusion | Fresh `open_work` chosen correctly with nothing running | pass |
| 7 | No project named ("btw...") | Resolves via `project_current` | Correctly resolved to the current project and held rather than duplicating work | pass (O5) |
| 8 | Ambiguous instruction ("btw, let's also fix this") | Reasonable resolution | Same as #7 — resolved sensibly, not flagged as unclear | pass |
| 9 | Empty instruction | No-op / shows chat | Showed chat, sent nothing, DB unchanged | pass (O8) |
| 10 | Whitespace-only instruction | Same as empty | Same as empty | pass (O8) |
| 11 | Very long instruction | Not tested live (cost) | Code has no length cap anywhere in the prompt path (`orchestrator.rs`, `cli/src/main.rs`) | needs confirming |
| 12 | "every day at 9am" → schedule | `schedule_create` | Correctly armed a schedule with inferred timezone | pass (O7), but see O2 for the lost reply |
| 13 | Standing objective ("keep checking X until Y") → goal | `goal_create` | Not run — no cheap way to test without a long-lived loop | needs confirming |
| 14 | Concurrent instructions to main | Both handled, or a clear queue | Both ran concurrently on the same underlying session; second one's reply vanished from the transcript | **fail (O2)** |
| 15 | Main asked something while a run is mid-flight | Routes correctly, no blocking | Correctly used `continue_agent`/held the work rather than duplicating it | pass (O5/O6) |
| 16 | A run that dies (killed) | Continuing it is refused, or clearly flagged | Refused, but for an unrelated permission-ceiling reason — status itself is unchecked in the tool | needs confirming (O3) |
| 17 | A refusal from a tool (sandbox denies a `Write`) | Reported honestly, not hallucinated as success | Reported precisely, offered two concrete ways to unblock | pass, though the underlying cause is O1 |
| 18 | A work session actually finishing a write + commit | File exists, commit exists | Never happened — every attempt deadlocked on the worktree sandbox | **fail (O1)** |

Not run at all, and not claimed: a malformed/garbled instruction beyond what
dictated Taglish already covers (R1's own reproduction is closer to this than
anything I ran), and a true concurrent-instruction race with three or more
overlapping runs — two was enough to reproduce O2, a third would only add
noise to the same finding.
