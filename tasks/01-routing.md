# Routing — answer it yourself, or hand it over

What Reljod asked for, in his words: main is the orchestrator and every chat
session is one, but it should **decide depending on the task** whether it
answers by itself or delegates. Long-running work gets delegated, and the
sub-agent reports back to the orchestrator when it has an answer or is
finished. Work that turns out to be a project goes to a project manager —
a suitable existing one, or a new one. A2A carries the messages between them.
A quick question it should just answer, and today it does not.

---

## R1. The orchestrator is forbidden from ever answering anything
Status: open · Owner: — · Severity: high

`orchestrator_preamble` (`core/src/orchestrator.rs:353`) opens with:

> **You do not do the work.** You decide who does, hand it over, and come
> straight back. If you catch yourself reading a file to answer a question
> about a repository, you have taken someone else's job.

There is no branch anywhere in it for answering directly, and the CLI help
repeats the same rule ("It never does the work itself", `jod main --help`). So
every instruction — including "what does A2A stand for" — costs a spawned
agent, a conversation row and a round trip.

The unmerged `docs/spec-ceo-and-managers` branch already decides the shape of
the fix: *"Main may route and may run repo-less one-shots. Nothing else."* and
*"It routes and it answers."* Read that spec's Change 3d before rewriting the
preamble; it lists main's exact tool set.

Fix: give the preamble an explicit first branch — answer directly when the
instruction needs no repository, no tool beyond memory, and nothing that
outlasts this turn. Otherwise route exactly as it does today. This adds a case
rather than changing one.

Check: see R2 — without it this cannot be shown to work or to stay working.

### Observed, not argued

A console was opened in a scratch repository on a fresh `JOD_HOME` and asked
one trivial question:

> what does the acronym A2A stand for in this project? answer in one line

What it did: spawned a sub-agent called `a2a-acronym-lookup`, then polled
`list_agents` in a loop waiting for it. What Reljod got back, after 42 seconds
and **$0.39**, was:

> Still working — the lookup agent is mid-search. I'll report as soon as it
> lands.

He never got the answer. The question was one line, needed no repository, and
the orchestrator already had the answer available to it in `SPECS-a2a.md`. This
is the whole of R1 in one exchange: not a slow answer, no answer, for the price
of a spawned agent.

---

## R2. Nothing tests that a routing decision is correct
Status: open · Owner: — · Severity: medium

`parse_decision` and `router_prompt` (`core/src/orchestrator.rs:229`, `:300`)
are tested for *shape* — that a decision parses — and nothing anywhere asserts
that a given instruction reaches the right verb. So R1 can be fixed and
silently regress on the next preamble edit.

Note before starting: the spec's "Out of scope" section says `Decision`,
`parse_decision` and `router_prompt` are the **earlier** design, superseded by
the tool-using orchestrator, and should be left alone. So this test probably
belongs against the tool-using path — which instruction causes which MCP tool
call — not against the JSON router. Confirm which one is live before writing
it.

Fix: a fixture table of instruction → expected disposition (`answer`,
`continue_agent`, `open_work`/`ask_manager`, `delegate`, `schedule_create`,
`goal_create`), run against the real router. It needs a model, so it belongs
behind the same marker as the other harness-touching tests rather than in the
unit suite.

---

## R3. A delegated run has no route back to the orchestrator
Status: open · Owner: — · Severity: medium · needs confirming

Reljod's ask includes the return leg: the sub-agent communicates back to the
orchestrator with answers, or to say it has finished. The bus exists —
`send_message`, `reply`, `ask`, `roster`, `read_messages` in `core/src/mcp.rs`,
designed in `SPECS-a2a.md` — but it is addressed by *roster name*, and it is
not yet confirmed that a run started by `delegate` or `open_work` can address
the main chat, or that anything makes the orchestrator take a turn when mail
arrives for it.

This one is **not yet observed** — it is read off the code and needs a live
test before anyone builds against it. Whoever picks it up should first run a
delegated agent and try to message main from inside it.

Fix: unknown until confirmed. If the gap is real, the likely shape is that the
main chat appears in the roster under a fixed name and that mail for it starts
a turn, the way `core/src/delivery.rs` already starts one for a teammate.

---

## R4. The orchestrator blocks itself busy-waiting on the run it just delegated
Status: open · Owner: — · Severity: high

Observed in the same exchange as R1. Having spawned the sub-agent, the
orchestrator did this, in its own turn:

```
Bash · until [ "$(jod agents --json | grep -c a2e4f620)" = "0" ]; do sleep 5; done
Bash · sleep 45; echo waited
mcp__jod__list_agents · {"limit":2}      (repeatedly)
```

It sat in a shell loop waiting for its own child. That is the exact thing the
design says it must never do — `core/src/orchestrator.rs:25` is a section
headed "Non-blocking, which is the whole point", explaining that a main chat
which blocks on a task is a chat you cannot use while anything is happening.

The behaviour is worse than slow. The turn burned 42 seconds and $0.39 mostly
on sleeping, and it still ended without the answer, because the poll loop
outlived the model's willingness to keep waiting rather than the run finishing.

Cause: the preamble tells the orchestrator to hand work over and come straight
back, but nothing tells it what to do when it *wants* the result. Given a
question whose answer it is supposed to relay, and no mechanism for the child
to report back (see R3), a shell loop is the only tool it has left. R3 and R4
are the same hole seen from two sides: with no return path, the orchestrator
invents a blocking one.

Fix: give the child a way to report back, then tell the orchestrator plainly
that it never waits — it hands over and returns, and the answer arrives as its
own event later. Consider refusing `sleep`-shaped Bash calls from a run holding
`ToolAccess::Orchestrate`; a rule the model can talk its way past is not a rule,
and this one is measurable.

Check: delegate something from the main chat and assert the turn returns
without a `sleep` or a poll loop in its tool calls.

## R5. The orchestrator reaches for tools outside Jod's set
Status: open · Owner: — · Severity: medium

In the same turn it called `ToolSearch · select:Monitor`, looking for a
generic monitoring tool rather than using Jod's own verbs. `ToolAccess::Orchestrate`
is meant to be the boundary of what the main chat can do, and the harness's own
tool-discovery mechanism reaches straight past it.

Worth confirming how far this goes: if the orchestrator can load arbitrary
harness tools, then `ToolAccess` bounds Jod's verbs but not the session, and
the confinement described in `core/src/orchestrator.rs:14` is narrower than it
reads.

Fix: unknown until scoped. At minimum the preamble should say the Jod tools are
the whole toolbox. Whether the harness can be told to withhold the rest is a
question for `docs/harness-support.md`.

Check: assert a main-chat turn's tool calls are all `mcp__jod__*` plus reading.

## R6. The compaction warning measures against a window the model does not have
Status: open · Owner: — · Severity: low — **diagnosed**

The status bar showed `⚠ compact` after exactly one question and one answer, on
a database created seconds earlier.

It is not the character thresholds in `core/src/orchestrator.rs:65`; those are
what `jod main` uses, and `live_window` (`core/src/conversation.rs:988`) counts
only active transcript messages, so one exchange is nowhere near the 24,000
character mark.

The TUI uses a different measure entirely. `App::should_compact`
(`cli/src/tui/app.rs:1245`) is `context_tokens / CONTEXT_WINDOW >= 0.75`, and
`CONTEXT_WINDOW` is a fixed `200_000` (`cli/src/tui/app.rs:676`). The run in
question was on `claude-opus-5[1m]`, a model with a one-million-token window.
So the bar filled to three quarters of a window five times smaller than the one
actually in use, and warned at roughly 15% of real capacity.

The constant's own doc comment already anticipates this and defends it: Jod
cannot know the real limit, the harness does not report it, and a per-model
table would be wrong the week a model ships. That reasoning is sound, and the
comment adds the condition that makes it honest — "as long as the screen calls
it an estimate". The screen does not. It says `⚠ compact`, which reads as a
fact about this conversation.

Fix, smallest first: say "estimate" on the badge, which is what the constant's
own comment already promises. Better, if the harness reports the model's window
anywhere, use it and keep 200,000 as the fallback. Do not build a per-model
table — the comment is right about that.

Check: a session on a 1M-token model must not show a compaction warning after
one short exchange.
