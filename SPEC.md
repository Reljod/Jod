# SPEC — main answers again, and anything running can be interrupted

Main used to answer a simple question itself. #229 took that away, because
main's turn is what holds the input box shut and a chat you cannot type into is
the one failure this chat cannot have. The fix was a new layer: main hands every
instruction to an assistant and answers nothing.

Reljod's verdict on that, and the instruction this spec executes:

> I made a mistake. Jod or main should still be able to answer simple questions.
> It's okay for it to run for quite a while but what's important is we can
> interrupt it by having an assistant read queue messages and determining when
> it will interrupt the main and tell the new message. Oh and also, we should be
> able to click "Esc" to interrupt main and every other sessions.

So the block stops being avoided and starts being broken. Main answers and
routes again. What you type while it is busy goes into a queue that an assistant
reads, and that assistant decides whether the message can wait for the turn to
end or has to stop the turn now. Esc keeps stopping the turn in front of you,
and Shift-Esc stops everything running anywhere.

## Why the layer is going, in one paragraph

The assistant was never really a routing layer. It was a workaround for a
blocked input box, and `docs/decisions.md:3985` says so in as many words. No
mainstream harness solves that problem with a model: in Claude Code, Codex CLI
and OpenCode the agent you type at both answers and delegates, and the blocking
is solved in the terminal — queue while busy, steer at a tool boundary, Esc to
interrupt. Anthropic's own guidance on multi-agent systems puts a number on what
the extra hop costs, three to ten times the tokens of doing it directly, and
says work should only be split when context can genuinely be isolated. Routing
an instruction isolates nothing.

The assistant is not deleted, though. It is re-pointed at a job main
structurally cannot do, because main is the thing that is busy: reading the
queue while a turn is in flight and deciding whether it can wait. That job needs
its own conversation by definition, it runs only when Reljod types into a busy
chat, and it is the whole of what this spec leaves the assistant doing.

## What is already true — do not rebuild it

All of this was checked by reading the code in this worktree. Most of the
machinery this spec needs already exists, and the largest risk in executing it
is building a second copy of something below.

- **The delivery queue is already the right queue.** `core/src/delivery.rs`
  holds `pending_deliveries`, a per-conversation queue with a `Kind::Human`
  variant whose documented meaning is "Reljod, typing into a running session".
  `Store::enqueue_delivery` writes to it, `Store::pending_for` reads it,
  `Store::conversation_is_busy` answers whether a turn is in flight, and
  `Store::plan_injection` decides what to say and when. `render_injection`
  already batches everything queued into one turn.
- **The queue is already drained on a timer.** `Ticker::tick_deliveries`
  (`core/src/ticker.rs:2109`) walks `conversations_awaiting_delivery` on every
  tick, asks `plan_injection`, and injects what is ready. It already reads
  `store.pinned_conversation()` so it knows which conversation is the main chat.
- **Interrupting one run already works.** `cli/src/tui/mod.rs:6058` handles Esc
  on a busy watched run: it records `interrupted after <time>` in the
  transcript, frees the input box on the keypress, sets `App::interrupting` so
  the status bar can say a stop is under way, and returns `Action::Interrupt`,
  which calls `Service::kill_agent`. Killing a run ends one process; the
  conversation and the harness-side session survive, so the next thing typed
  carries on. `escape_interrupts_the_turn_without_losing_the_session` pins that.
- **Stopping any run from anywhere already works.** `Service::kill_agent`
  (`core/src/service.rs:1431`) is what both Esc and the `stop_agent` MCP tool
  go through, and `Service::agents` lists what is running.
- **The manager tier exists and is not changing.** `ask_manager`,
  `manager_preamble`, `plan_work` and the engineer rules all shipped in #228 and
  #229 and this spec does not touch them. What changes is only who calls
  `ask_manager` — main, instead of the assistant.
- **Preambles are tested against the tools their runs can actually reach.**
  `every_tool_the_preamble_tells_an_agent_to_call_is_one_that_exists` in
  `core/src/orchestrator.rs` sweeps harness, access level, assignment and
  placement. Changing what a preamble names without changing the access level
  its runs are spawned with will fail this, correctly.

## Decisions made here

**D1. Main is the primary agent again.** It answers what needs no repository and
no work outlasting the turn, and it hands everything else to a manager. This is
the shape `docs/spec-ceo-and-managers.md` originally settled on — "it routes and
it answers" — and it is the shape every other harness has.

**D2. There is one answer-or-delegate branch, and it is main's.** The branch
existing in two preambles is the thing the preamble test suite was written to
prevent. `assistant_preamble` loses it entirely.

**D3. The assistant becomes the doorman.** A fresh, tool-poor session started by
Jod itself — never by main — that runs only when something is queued against a
busy conversation. It reads what was typed, reads what main is doing, and
answers one question: does this wait, or does it stop the turn now.

**D4. An interrupt is a kill and a resume, not a splice.** `core/src/delivery.rs`
states the invariant the whole module rests on: a prompt is assembled once at
spawn, so nothing can be spliced into a turn in flight. Interrupting therefore
means ending the run process and letting the existing queue deliver the message
as the next turn. True mid-turn steering, the way Codex does it, would need the
supervisor to hold the harness's stdin open — and `core/src/proc.rs:38` closes
it deliberately, only Claude Code supports the input format, and the other two
harnesses would fall back to this anyway. Rejected for now; if it is ever
wanted, it is a separate spec.

**D5. One queue, not two.** `App::queued`, the `Vec<String>` in
`cli/src/tui/app.rs:579`, is a second queue that only the TUI can see. It is
deleted. Typing into a busy chat enqueues a `Kind::Human` delivery in the store,
which means the doorman can read it, the CLI and the Telegram bridge feed the
same queue, and a message survives the TUI being closed.

**D6. Esc keeps its meaning; the fleet gets its own key.** Esc stops the turn in
front of you and nothing else, because a key that sometimes stops one thing and
sometimes stops forty is a key nobody can press confidently. Shift-Esc stops
every run everywhere.

**D7. A fleet stop kills turns and keeps sessions.** Every process dies; every
conversation survives and can be carried on by saying what to do instead. It is
the Esc gesture applied to everything at once, not a shutdown.

**D8. Shift-Esc needs the terminal's permission, so `/stop` is its twin.**
Crossterm only reports a modified Esc where the keyboard-enhancement protocol is
on, and `cli/src/tui/mod.rs:604` never pushes those flags. E3 pushes them and
degrades quietly where they are refused. Because that means Shift-Esc genuinely
will not arrive on some terminals, `/stop` does exactly the same thing, is named
on the keybar beside it, and is not a lesser path.

---

# E1 — Main answers and routes again

## E1.S1 Rewrite `orchestrator_preamble`

In `core/src/orchestrator.rs`. Keep everything the current one says about not
doing the work itself, about the vocabulary of works, sessions, roots, cards and
projects, and about Taglish dictation. Replace the intake-only framing with the
branch, in this order:

1. **Answer directly** when the instruction needs no repository, no work that
   outlasts the turn, and nothing to go away and research. One trivial call
   finished inside the turn is still answering. Touching a repository never is.
2. **`ask_manager`** for anything that touches a repository.
3. **`delegate`** for a one-shot needing a tool but no repository, and
   **`continue_agent`** where a finished scratch session was on the same
   subject.
4. `schedule_create` and `goal_create` keep their current sentences unchanged.

Two paragraphs from `assistant_preamble` move up rather than being rewritten,
because they are already right and were written against real failures: the one
beginning "Naming the project does not by itself make it repository work", and
the "Never wait for a busy one" paragraph. The "Look once" rule stays on main
too — `list_agents` once per turn, second call refused.

Delete the paragraph saying `open_work`, `ask_manager` and `delegate` are not
main's to call. `open_work` is still not main's, so say that alone and name
`ask_manager` as what to use instead.

## E1.S2 Rewrite `assistant_preamble` as the doorman brief

Same function, entirely new content. It is handed the queued message, a summary
of what main is currently doing, and nothing else. It has exactly two things it
can do — call `interrupt_main`, or say nothing and let the message wait — and
the brief has to make the judgement concrete rather than leaving it to taste:

- **Interrupt** when the message changes what main should be doing: a stop, a
  correction, a contradiction of the instruction in flight, a new priority that
  makes the current turn wasted work.
- **Hold** when the message adds to what main is doing, asks about something
  else, or is a new instruction that reads fine as the next turn.
- When it cannot tell, **hold**. A wrong hold costs a wait; a wrong interrupt
  throws away work Reljod asked for and cannot be undone.

It answers in one short sentence either way, and that sentence is what the chat
shows Reljod.

## E1.S3 Lift the refusals at the tool boundary

In `core/src/mcp.rs`, the refusal helper at `:2899` rejects routing calls from
main's run and names `ask_assistant`. `ask_manager`, `delegate` and the
project-checkout `delegate` are no longer refused from main. `open_work` stays
refused, and its refusal now names `ask_manager`.

The three tests that pin the old behaviour —
`open_work_from_the_main_chat_is_refused_and_names_ask_assistant`,
`ask_manager_from_mains_run_is_refused_and_names_ask_assistant` and
`delegate_from_mains_run_is_refused_and_names_ask_assistant` — are inverted, not
deleted: the first keeps refusing and asserts it names `ask_manager`, the other
two assert the call now goes through.

## E1.S4 Retire `ask_assistant` as a tool

Main no longer calls it, and the doorman is started by Jod rather than asked
for. Remove the tool from the MCP tool list (`core/src/mcp.rs:206`) and its
dispatch arm (`:1315`). Keep `Server::ask_assistant`'s body as the private
function E2 calls to spawn a doorman — it already builds the fresh
never-resumed conversation with `ASSISTANT_ORIGIN`, which is exactly what is
wanted; only its caller changes.

## E1.S5 Nothing to do about access, and here is why

Checked, so nobody goes looking: main's chat spawns already grant
`ToolAccess::Orchestrate` (`cli/src/tui/mod.rs:6727`), which is above the
`Delegate` that `ask_manager` and `delegate` need. The refusal being lifted in
E1.S3 is keyed on *identity* — "is this caller main" — and never on access
level, and `refuse_routing_from_main`'s own doc comment explains why it had to
be: `ToolAccess` is a ladder, so lowering main to take `delegate` away would
have taken `schedule_create` and `goal_create` with it.

So E1.S3 is the whole of the enforcement change. Re-run
`every_tool_the_preamble_tells_an_agent_to_call_is_one_that_exists` anyway,
because it sweeps every access level a preamble renders under and the new
wording has to survive all of them.

**Check:** `cargo test -p jod-core orchestrator` and `cargo test -p jod-core mcp`.

---

# E2 — One queue, and a doorman that reads it

## E2.S1 Typing into a busy chat writes to the store

In `cli/src/tui/mod.rs:5993`, replace `app.queue(prompt)` with a new
`Action::Enqueue { conversation_id, text }` whose handler calls
`Store::enqueue_delivery` with `Kind::Human`. Delete `App::queued`,
`App::queue`, `App::next_queued` and the drain at turn end — `tick_deliveries`
already does that job, and doing it twice would deliver each message twice.

The notice stays, and stops promising turn-end delivery, because that is no
longer the only thing that can happen: `queued — an assistant is reading it
({n} waiting)`. `App::status` reads the count from the store rather than from a
local Vec, and `queued_prompts_come_back_in_the_order_they_were_typed` is
replaced by a store-level test of the same property.

## E2.S2 `plan_injection` gains a third answer

Today it returns `Some(Injection)` or `None`, and busy always means `None`. Give
it an enum:

```rust
pub enum Plan {
    /// Nothing to say, or nothing that may be said yet.
    Hold,
    /// Say all of this now.
    Speak(Injection),
    /// A turn is in flight and something is queued behind it. Nobody has
    /// judged it yet.
    Judge { conversation_id: String, items: Vec<Pending> },
}
```

`Judge` is returned only when busy *and* something is queued *and* no doorman is
already reviewing that queue. It stays a pure value for the reason the module
gives about `wake_order`: all the judgement is in *when* to speak, and it has to
be testable without a harness binary.

## E2.S3 A queue under review is a state, so only one doorman runs

Add `State::Reviewing` to `core/src/delivery.rs`'s `State`, with a migration
taking the next free number — **read every worktree before choosing it**, not
just `git log`; `docs/decisions.md` has the entry on why. `Judge` marks its rows
`reviewing`, so the next tick sees no unjudged queue and starts no second
doorman. A verdict of hold puts them back to `queued` and stamps a
`reviewed_at_ms`, so the same message is never judged twice.

## E2.S4 The ticker starts a doorman

In `Ticker::tick_deliveries`, the `Judge` arm spawns the doorman through the
function kept in E1.S4. The prompt it is given contains, and contains nothing
else:

- what Reljod typed, verbatim,
- what main is doing — the in-flight run's own description and its most recent
  event summary, read from the store,
- the id of the run it may stop.

Count doormen started in `TickReport` so the tick's own accounting stays honest.

## E2.S5 `interrupt_main`, the doorman's one verb

A new MCP tool, reachable only from a run whose conversation origin is
`ASSISTANT_ORIGIN`, and refused from anywhere else. It takes the run id and a
one-sentence reason. It calls `Service::kill_agent` on that run, writes the
reason into the target conversation's transcript so Reljod can see why his turn
stopped, and moves the queue's rows back to `queued`.

Nothing else is needed: the run is now not busy, so the next
`tick_deliveries` finds `Speak` and delivers the queue as the next turn through
the path that already exists.

A doorman that ends its turn without calling it is a hold. Its closing sentence
is written into the main chat either way, so Reljod sees `held — this reads like
a follow-up, it will go in when the turn ends` rather than silence.

**Check:** a test at store level that a queued human message against a busy
conversation plans `Judge` once and `Hold` on the next tick; a test that
`interrupt_main` from a non-assistant origin is refused; and an end-to-end test
in `core/tests/` that a message queued against a busy run is delivered as the
next turn after the run is killed.

---

# E3 — Esc here, Shift-Esc everywhere

## E3.S1 Esc already reaches the main chat — leave it alone

The arm is `KeyCode::Esc if app.busy && app.watching.is_some()`, and the worry
was that `App::watching` might only be set for a run entered from the fleet.
It is not: sending from the chat box ends in `App::begin_turn`
(`cli/src/tui/app.rs:2176`), which sets `watching` to the run it just started.
So Esc on a busy main chat already interrupts main and keeps the conversation.

Nothing to change here. The step exists so that the first thing done under E3 is
not a rewrite of something that works.

## E3.S2 Ask the terminal for modified keys

Push crossterm's `PushKeyboardEnhancementFlags` next to `enable_raw_mode` at
`cli/src/tui/mod.rs:604`, popping them on the way out beside
`LeaveAlternateScreen`. Terminals that refuse are left exactly as they are —
this must not be allowed to fail startup, and a terminal that will not report
Shift-Esc keeps working through `/stop`.

## E3.S3 Shift-Esc, and `/stop`

A new arm, above the single-run Esc so it wins, and a `/stop` command in
`cli/src/tui/command.rs` that produces the same action. Both:

- ask `Service::agents` for everything running,
- call `Service::kill_agent` on each,
- leave every conversation alone,
- and report one line naming how many stopped, or `nothing was running`.

The keybar gains `Shift-Esc stop everything`, and `keys.rs` gains the entry —
the doc comment at `cli/src/tui/keys.rs:19` explains why a key that is not on
that list may as well not exist.

No confirmation prompt. Stopping is reversible here by D7, and a stop you have
to confirm is not a stop you can reach for in the two seconds you have.

**Check:** a handler test that Shift-Esc yields the fleet-stop action while a
plain Esc on the same state yields the single interrupt, and a test that the
fleet stop leaves conversations intact.

---

# E4 — Write down what changed

- **`docs/decisions.md`**: an entry reversing "Main hands every instruction to
  an assistant and answers nothing itself" at `:3985`. Do not edit that entry —
  it is the record of what was believed then. The new one says what was learned:
  the block was a transport problem, a model was the wrong tool for it, and the
  assistant found a job only it can do.
- **`AGENTS.md`**: nothing to change unless the tool lists there name
  `ask_assistant`. Check.
- **The retired spec**: `docs/spec-unblock-main-and-roles.md` describes the
  layer this removes. Add a line at its top saying which parts of it are no
  longer true and which — the roles panel, the scratch lane — still are.

# The whole check

```
cargo test -p jod-core
cargo test -p jod-cli
```

Then run it, because green code that cannot run is this repo's most common
fault: start `jod tui`, ask main a question that needs no repository and get an
answer from main itself, type a second line while a long turn is running and
watch a doorman appear in the fleet, and press Shift-Esc with several agents up.
