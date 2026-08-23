# SPEC — the assistant guards the door, and the CEO decides again

Today the layer that decides has no memory, and the layer that remembers is not
allowed to decide. Main takes an instruction and hands it straight on without
reading it. An assistant, created one second earlier and thrown away at the end
of the turn, works out which repository it is about and who should do it. Every
routing decision Jod has ever made was made by something that had never seen the
one before it.

That was a deliberate trade and it bought something real: main's turn stopped
blocking the console. This spec keeps what it bought and puts the decision back
where the context is. The assistant moves above the CEO and becomes the door;
the CEO becomes the standing conversation that decides where work goes; and the
queue that used to live in one client's memory moves into the database, where
every way in can see it.

## Goal

An instruction from Reljod reaches a cheap assistant first. If the CEO is idle,
the assistant is skipped entirely and the instruction goes straight through. If
the CEO is mid-turn, the assistant reads what the CEO is doing and makes one
judgement: interrupt it, or wait behind what is already waiting. Interrupting
stops the CEO's run and leaves a marker in the transcript saying so, so the next
turn can tell being cut off from having finished.

Done when the checks under **Verification** pass and the four passages named
under **Where the behaviour actually comes from** no longer say what they say
today.

## The problem, in Reljod's words

He drew it rather than wrote it. The diagram puts the assistant between himself
and the CEO, with a queue in front of it and a diamond reading "checks if needs
to interrupt" — yes goes to the CEO, no goes to "make it less priority (below
stack)" and loops back to the queue. Under the CEO sit two boxes he had already
built: project managers with engineers under them, and a dotted box of
"ephemeral/short term agents" for anything that is not a project.

Three notes hang off the CEO in his handwriting: "should be no to minimum
blocking (only delegation)", "use smaller models for delegation", and "add
config or slash command for this for each harness".

## Decisions settled by interview

Six questions were asked and answered before any of this was written down. They
are recorded here because each one closes off a design that would otherwise look
equally reasonable to whoever executes this.

1. **The CEO is standing and interruptible.** It keeps one long-lived thread,
   remembers across instructions, and can be mid-turn when the next one arrives.
   This is the whole point of the change, and it is why a queue has to exist in
   front of it.
2. **Interrupting stops the CEO's run mid-turn.** Not a next-turn injection, not
   a card. This is far safer here than it would be anywhere else in Jod, for the
   reason recorded under **What is already true**.
3. **The triage is a model call, not a rules engine.** A small model on the
   `assistant` role reads the instruction. Rules cannot handle dictated Taglish
   with the noun left out, which is most of how Reljod actually talks to it.
4. **The interrupted instruction is not put back on a queue.** Reljod asked
   whether it simply stays in the conversation history as context, and it does —
   see D6, which also names the one thing missing from that record today.
5. **Three things make an instruction worth interrupting for**: it corrects or
   cancels what the CEO is doing now; Reljod said it is urgent; or the assistant
   judges it important to the CEO's current task rather than irrelevant to it.
6. **There is no bypass.** Every instruction goes through the door. The thick
   arrow on the diagram from "me" straight to the CEO is the diagram showing who
   ultimately receives everything, not a second path to build.

## Two things this assumes, stated so they can be corrected

Neither was asked about, and both would be cheap to change if they are wrong.

- **A deferred instruction always eventually runs.** Nothing expires, nothing is
  dropped, and "make it less priority" means it stays in the queue rather than
  that it might never come out.
- **Deferred instructions drain oldest first.** The assistant sorts nothing. Its
  judgement is a single yes-or-no about interrupting, and everything it does not
  interrupt for joins the back of one line.

## What is already true — do not rebuild it

More than half of the diagram is built. Each of these is working today and none
of it should be touched.

- **The roles table and the roles panel.** Migration
  `0030_a_role_says_what_to_spawn_it_on` gives every one of the six roles its own
  harness, model, thinking level and permission ceiling, and the panel in the TUI
  edits them. That is "use smaller models for delegation" and "add config for
  each harness for this", both finished. The only thing this spec asks of it is
  that the `assistant` row be set to something cheap, which is a setting rather
  than a change.
- **A manager per project, with engineers under it.** `ask_manager` resumes one
  standing conversation per repository that remembers every instruction about it
  and opens engineers under a work.
- **The ephemeral lane.** `delegate` opens a `Role::Scratch` session that belongs
  to no work, is not a node in the tree, and is swept away afterwards. That is
  the dotted box.
- **Main stops alone, and this is what makes D6 safe.** `Jod::kill_agent`
  deliberately does not cascade for the pinned conversation
  (`core/src/service.rs:1484`): main hands work out rather than owning it, so
  stopping it says nothing about whether the work below should continue.
  Interrupting the CEO therefore leaves every manager and engineer it already
  started running, and the blast radius is confined to whatever that one turn had
  not yet handed out.
- **The transcript is Jod's, and it is written as it happens.** The supervisor
  appends each line to `events` as it arrives rather than at the end of a run, and
  `hand_to_orchestrator` writes the instruction into the conversation as a user
  turn at spawn time (`core/src/orchestrator.rs:1798`). A killed turn loses
  neither.
- **Cards cascade up to main's rail**, which is how anything below reports.
- **An unpaired tool call already degrades to text** rather than failing a
  request — but only on the harness-switch renderer. See the caveat in D6.

## Where the behaviour actually comes from

Four passages say the current shape plainly, and they will keep saying it until
they are changed. A fifth is the queue in the wrong process.

1. **`core/src/orchestrator.rs:203`, `orchestrator_preamble`** — "You take what
   he says and you hand it straight on. You do not route it, you do not pick a
   repository, and you do not answer it yourself." Every sentence of this is
   about to be false.
2. **`core/src/orchestrator.rs:443`, `assistant_preamble`** — holds the entire
   routing branch: answer, `ask_manager`, `delegate`, `continue_agent`. This is
   what moves up.
3. **`core/src/mcp.rs:3024`, `refuse_routing_from_main`** — refuses
   `ask_manager`, `delegate` and `open_work` from main and names `ask_assistant`
   as the answer. It has to invert.
4. **`core/src/mcp.rs:226`, the `ask_assistant` tool** — main's near-only verb.
   After this it is not main's at all.
5. **`cli/src/tui/app.rs:586`, `queued: Vec<String>`** — the queue, in memory, in
   one process. `jod main` and the Telegram bridge do not consult it, and all
   three enter through `hand_to_orchestrator`, which has no busy check. A message
   sent from a phone while the console's turn is in flight already spawns a second
   run resuming the same harness session. Moving the queue into the database
   closes that hole as a side effect.

## D1 — the CEO decides again

`orchestrator_preamble` takes the routing branch that
`assistant_preamble` holds today: answer it, `ask_manager` for anything touching
a repository, `delegate` for a one-shot needing a tool but no repository,
`continue_agent` when a recent scratch session was on the same subject.

Three things stay exactly as they are. `schedule_create` and `goal_create` never
left main and do not move now — arming one spends money at 2am with nobody
watching, and that argument is untouched by any of this. The vocabulary
paragraph — work, session, root, card, project — stays. And the paragraph about
dictated Taglish stays, but its instruction reverses: main no longer hands the
words on unread, it is now the thing that has to work out the missing noun.

One sentence is added, and it is the one that makes the whole design safe:

> A turn of yours can be stopped in the middle. If your transcript shows an
> instruction of Reljod's followed by a note that the turn was interrupted, that
> instruction was never finished and it is still yours to carry out. Deal with
> what interrupted you first, then go back to it.

## D2 — the assistant becomes the door

`assistant_preamble` is replaced rather than edited. Nothing of the routing
branch survives in it, because routing is not its job any more. What it gets
instead is one question with two answers.

The brief tells it: an instruction from Reljod is in front of you, the CEO is
mid-turn, and here is what the CEO is working on. Decide whether this needs to
stop that. Interrupt when the instruction corrects or cancels what the CEO is
doing, when Reljod has said it is urgent, or when it bears on the CEO's current
task closely enough that letting the turn finish would produce the wrong thing.
Otherwise it waits, and waiting is the normal answer.

It gets exactly two tools, `interrupt_ceo` and `queue_for_ceo`, and no others.
Not `ask_manager`, not `delegate`, not `open_work` — a door that can do the work
is not a door. This is `ToolAccess::ReadOnly` plus those two verbs, which is
below `Delegate` and therefore a new access level or an explicit allowance; the
executor should pick whichever fits `ToolAccess` more honestly and say which in
the pull request.

**What the CEO is doing arrives in the framing, not from a tool call.** The
spawner assembles it, exactly as `project_context` is assembled today and for the
same reason recorded at `core/src/orchestrator.rs:1687`: paying a round-trip to
be told what the caller already knew puts a model in the way of every sentence.
The framing carries the CEO's current instruction, how long it has been running,
and the instructions already waiting behind it.

## D3 — the door is skipped when there is nothing to guard

If the CEO is idle and the queue is empty, no assistant is spawned at all. The
instruction goes straight to the CEO.

This is not a rules engine sneaking back in through the side door. It is
declining to ask a question that has only one answer: there is nothing to
interrupt, so there is no judgement to make. It is also the first node on
Reljod's diagram, the diamond reading "queue?" that sits *before* the assistant
rather than after it.

The saving is not small. Most instructions arrive when nothing is running, so
most instructions will cost no triage at all.

## D4 — the queue is a table

Migration `0032_an_instruction_waits_where_every_client_can_see_it`.

```sql
CREATE TABLE intake (
  id            TEXT PRIMARY KEY,
  conversation  TEXT NOT NULL,   -- the main chat this is destined for
  instruction   TEXT NOT NULL,   -- Reljod's words, unmodified
  source        TEXT NOT NULL,   -- console | cli | telegram
  state         TEXT NOT NULL,   -- waiting | running | done
  queued_at_ms  INTEGER NOT NULL,
  started_at_ms INTEGER,
  run_id        TEXT             -- the CEO run that took it, once one has
);
CREATE INDEX ix_intake_waiting ON intake(conversation, queued_at_ms)
  WHERE state = 'waiting';
```

Taking the next instruction is a single guarded `UPDATE` on `state = 'waiting'`,
never a read followed by a write, for the reason the rest of the store already
documents: zero rows changed means you lost the race. Two processes draining the
same queue must produce one winner.

`source` is recorded because a queue you cannot attribute is one you cannot
debug. It is not read by any logic.

## D5 — one front door, and it is not `hand_to_orchestrator` any more

A new `hand_to_intake` in `core/src/orchestrator.rs` becomes what the TUI, `jod
main` and the Telegram bridge call. It writes the row, then either spawns the
CEO directly (D3), spawns an assistant to triage (D2), or does nothing but
return, because the instruction is now waiting and something else will drain it.

`hand_to_orchestrator` keeps its name, its signature and every one of its four
bug-fixes, and stops being public intake. It is what the drain calls.

**The drain runs on the ticker.** When a CEO run reaches a terminal event, the
oldest waiting row for that conversation is claimed and handed over. No second
triage happens at drain time — there is nothing left to interrupt, so there is
nothing to judge.

## D6 — an interrupted turn says that it was interrupted

`interrupt_ceo` does three things in order, and the order is the whole of it.

1. **Stop the CEO's run** through `Jod::kill_agent`, which stops it alone.
2. **Append a system message to the main conversation** naming what interrupted
   it and what it was part-way through. This is the piece that does not exist
   today and is the reason decision 4 is safe. The instruction and the partial
   work are already in the record; what is missing is anything saying the turn
   ended early. Without it the CEO reads an instruction it never finished and
   has no way to tell that from one it did.
3. **Hand the interrupting instruction over** as the CEO's next turn.

**The caveat that has to be checked rather than assumed.** The next turn resumes
the harness's own session by id (`store.resume_for`), not Jod's transcript, and a
turn killed between a tool call and its result can leave an unpaired `tool_use`
in that session. Jod already degrades an unpaired call to text, but on the
harness-switch renderer rather than on ordinary resume. Whether Claude Code,
OpenCode and AGY each survive a resume after a mid-stream kill is an empirical
question about three binaries, and the answer is not in this repository. Check 9
is that experiment. If a harness cannot survive it, the fallback is to carry the
transcript across as a harness switch already does — machinery that exists and
shipped in #238.

## D7 — the console shows the queue it no longer owns

`queued: Vec<String>` and `next_queued` come out of `cli/src/tui/app.rs`. The
status line's `{n} queued` reads the `intake` table instead, so what the console
shows is what is actually waiting rather than what this process happens to
remember — including instructions sent from a phone.

A queued instruction becomes cancellable, which it never was: `Ctrl-A` over a
waiting row, or `/queue` to list and `/unqueue <id>` to drop one. This is new
behaviour rather than a port, and the executor should build it only after
everything above is green.

## Migration

The `intake` table starts empty and every instruction sent before it existed has
already run, so there is nothing to backfill.

The two preambles change together or not at all. A build where main routes and
the assistant also routes would send every instruction down two paths, and a
build where neither does would drop them silently. If this ships in more than one
commit, the preambles belong in the same one.

## Files & interfaces

| File | What changes |
|---|---|
| `core/src/store.rs` | Migration 0032, the `intake` table, and the guarded claim |
| `core/src/orchestrator.rs` | `orchestrator_preamble` gains the branch; `assistant_preamble` is replaced; `hand_to_intake` is new; `hand_to_assistant` carries the CEO's state |
| `core/src/mcp.rs` | `refuse_routing_from_main` inverts; `ask_assistant` goes; `interrupt_ceo` and `queue_for_ceo` are new |
| `core/src/ticker.rs` | The drain, on a terminal CEO event |
| `core/src/service.rs` | Nothing, if `kill_agent` is reused as-is. Confirm rather than assume |
| `cli/src/tui/app.rs` | The in-memory queue comes out; the status line reads the table |
| `core/src/telegram.rs` | Calls `hand_to_intake` instead of `hand_to_orchestrator` |

## Verification

Every check is runnable. Numbers 1 to 8 are unit or integration tests; 9 and 10
need real binaries and are the two that cannot be faked.

1. An instruction arriving when the CEO is idle and the queue is empty spawns no
   assistant. Assert on the runs table, not on timing.
2. An instruction arriving when the CEO is mid-turn spawns an assistant tagged
   `Role::Assistant` and writes an `intake` row in state `waiting`.
3. The assistant's framing contains the CEO's current instruction verbatim.
4. `interrupt_ceo` stops the CEO's run and leaves every manager and engineer it
   started still running.
5. After `interrupt_ceo`, the main conversation contains, in order: the original
   instruction, whatever the turn produced, a message saying the turn was
   interrupted, and the interrupting instruction.
6. `queue_for_ceo` starts nothing. The row goes to `waiting` and no run appears.
7. When a CEO run finishes, the oldest waiting row is claimed and handed over,
   and no assistant is spawned for it.
8. Two processes draining the same queue produce one winner. Assert on the guarded
   `UPDATE` returning zero rows for the loser.
9. **The resume experiment.** Start a real CEO turn on each of the three
   harnesses, kill it mid-tool-call, resume, and read what comes back. This is
   the check that decides whether D6's fallback is needed. Record the result in
   `docs/decisions.md` whichever way it goes.
10. **End to end.** Send an instruction from the console, send a second from
    Telegram while the first is running, and confirm the second is triaged rather
    than spawning a second run on the same session. This is the hole named in
    passage 5, and it is a bug today.

## What this costs, and why it is still right

**Routing serialises.** Today two unrelated instructions about two different
repositories are routed in parallel, by two assistants that know nothing about
each other. After this they queue behind one CEO. That is a genuine regression in
throughput and it should be named rather than discovered.

It is still right because the parallelism was buying less than it looked like.
Each of those assistants was making a routing decision with no memory of the last
one, which is why the same instruction phrased two ways can land in two different
repositories today. One CEO that remembers will be slower and more consistent,
and consistency is what a chief of staff is for.

**The console does not go back to being blocked**, which is the failure the
previous release existed to fix. Reljod can type while the CEO is mid-turn; the
difference is that his second instruction now reaches a judgement instead of a
`Vec`.

## Escalate on

- A harness that cannot be resumed after a mid-turn kill, on check 9. That is a
  finding about someone else's binary, not something to work around.
- Any temptation to give the assistant a third tool. If triage seems to need to
  read a file or look at a repository, the design is wrong and it is worth saying
  so rather than widening the door.

## Out of scope

- Re-ordering the queue by anything other than arrival. The assistant judges one
  thing.
- Expiring or dropping a waiting instruction.
- A bypass that skips triage. Decided against, in interview.
- Anything about managers, engineers, the scratch lane or the roles table, all of
  which are built and none of which this touches.
