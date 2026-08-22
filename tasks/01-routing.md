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
Status: **in flight** · Severity: high

`orchestrator_preamble` (`core/src/orchestrator.rs:353`) opens with:

> **You do not do the work.** You decide who does, hand it over, and come
> straight back. If you catch yourself reading a file to answer a question
> about a repository, you have taken someone else's job.

There is no branch anywhere in it for answering directly, and the CLI help
repeats the same rule ("It never does the work itself", `jod main --help`). So
every instruction — including "what does A2A stand for" — costs a spawned
agent, a conversation row and a round trip.

The shipped `docs/spec-ceo-and-managers.md` spec already decides the shape of
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
the orchestrator already had the answer available to it in `docs/spec-a2a.md`. This
is the whole of R1 in one exchange: not a slow answer, no answer, for the price
of a spawned agent.

### It is a coin flip, not a consistent failure — and that is worse

A second run of very nearly the same question went the other way. O4 in
[`10-orchestration.md`](10-orchestration.md) asked "What does A2A stand for, in
one word or phrase?" and got back "Agent-to-Agent — Google's open protocol for
interoperability between AI agents." directly, with **zero** agents spawned.

Both results are real. The difference is probably that my wording said "in this
project", which reads as a repository question, while the other did not. But
nothing in Jod decides this. The preamble has no branch that says "answer
directly", so whether it happens is the model's judgement on the day.

That makes the fix more valuable rather than less. An orchestrator that always
delegated would at least be predictable; one that sometimes answers and
sometimes spawns an agent and then loses the reply is one you cannot learn to
use. State R1's fix as a rule the model cannot talk its way past, and make R2's
table assert both directions — a question that must be answered, and an
instruction that must be handed over.

---

## R2. Nothing tests that a routing decision is correct
Status: **closed — satisfied by R1's disposition suite** · Severity was: medium

`parse_decision` and `router_prompt` (`core/src/orchestrator.rs:229`, `:300`)
are tested for *shape* — that a decision parses — and nothing anywhere asserts
that a given instruction reaches the right verb. So R1 can be fixed and
silently regress on the next preamble edit.

**Now settled — see R7.** `parse_decision` and `router_prompt` are not merely
"probably superseded"; they have no production caller at all. The live path is
the tool-using orchestrator, so this test belongs against which MCP tool an
instruction causes, never against the JSON router.

**Status update: R2 is satisfied by R1's work**, which ships a
`tests/e2e/main-chat/dispositions.sh` table asserting both directions — three
rows that must be answered with nothing handed over, five that must be routed.
Do not open this as a separate task; a second agent would write a table that
already exists.

Fix: a fixture table of instruction → expected disposition (`answer`,
`continue_agent`, `open_work`/`ask_manager`, `delegate`, `schedule_create`,
`goal_create`), run against the real router. It needs a model, so it belongs
behind the same marker as the other harness-touching tests rather than in the
unit suite.

---

## R3b. The return leg only ran inside a daemon nobody was running
Status: **fix open on `worktree-tui-delivers-main-mail`** · Owner: Reljod ·
Severity: high

Observed live, from the console, on 22 Aug 2026. Reljod asked the main chat for
the weather in Manila. The chat delegated the lookup to a one-shot run, the run
fetched it from `wttr.in`, sent the answer to `main`, and said in its own panel
"Reported back to `main`." Nothing appeared in the chat. The screenshots are the
report; the database says the same thing.

R3's fix (#134) is not the problem — every part of it worked. The message was on
the bus, correctly addressed, with `main` on the run's roster pointing at the
pinned conversation:

```
team_messages id=4  team=01965778…  sender=manila-weather  recipient=main
                    state=queued  delivered=0
```

**The only thing that moves that row is `Ticker::tick_mail`, and the only caller
of `tick_mail` is `core/src/daemon.rs:121`.** No `jod daemon` was running — just
`jod tui`. So the mail sat queued, and nothing on screen said so. Two older
reports were queued the same way, from 20 and 21 Aug, along with ten card
answers (see R3c).

Two things had to be true at once for this to be invisible for days. The run
truthfully reported success, because `send_message` genuinely succeeded; and the
console cannot show a turn it did not take, because it draws the chat from
entries held in memory and never reads them back. So even with a daemon running,
the daemon's own delivery would have resumed the chat's session in another
process, written the turn to the database, and left the screen blank. **Whoever
holds the chat has to take the turn** — that is the part R3 did not cover.

Fix: `Store::collect_main_chat_mail` lifts the main-chat drain out of the tick,
and `jod tui` runs it on its own tick and hands the result to the orchestrator
as a turn of its own, echoed as a routing line rather than as something Reljod
typed. The daemon keeps its copy for headless boxes; both settle in one
transaction each, so whichever arrives first takes the message.

Evidence: run against a copy of the live database with the chat made resumable,
the drain moves all four stranded messages and the injection leads with the
weather report Reljod never saw.

Check: `cargo test -p jod-core --lib team::` and `cargo test -p jod-cli --bin
jod tui::tests::a_report` — the console delivers when idle and looking at the
chat, holds under a turn in flight, holds while watching another agent, and
never draws a delivered report as Reljod's own line.

---

## R3c. Card answers reach nobody without a daemon either
Status: open · Owner: — · Severity: high

Found while confirming R3b, and not fixed by it. `pending_deliveries` on the
live database holds **ten** answered cards sitting at `queued`, the oldest from
13 Aug — every one of them an answer Reljod typed into the rail that the agent
waiting on it never received. `core/src/delivery.rs` says this path is "the
thing Reljod asked for most directly", and its own module note says the queue
"now has a caller". It does: `Ticker::tick_deliveries`, in the daemon.

R3b deliberately does not cover these. They are addressed to *other agents'*
conversations, and delivering one means resuming that agent's session in a
process of its own — which is exactly what a daemon is for and what a console
should not become.

So the question is not where the code goes but what a console owes the person
using it. Today `jod tui` says nothing while ten answers rot. It already warns
"nothing is watching these sessions for stalls — start `jod daemon`" for
heartbeats; an answered card nobody will ever be told about deserves at least as
much.

Fix, smallest first: say it on screen — the rail knows a card is answered and
the queue knows it was never delivered, so a card stuck at `queued` for more
than a tick or two is a sentence the console can write. Whether the console
should instead start a daemon itself is a larger question and a separate one.

Check: answer a card with no daemon running and assert the console says the
answer is waiting rather than showing it as delivered.

---

## R3. A delegated run has no route back to the orchestrator
Status: **fixed — merged as #134**, then found incomplete: see R3b ·
Severity: medium

Reljod's ask includes the return leg: the sub-agent communicates back to the
orchestrator with answers, or to say it has finished. The bus exists —
`send_message`, `reply`, `ask`, `roster`, `read_messages` in `core/src/mcp.rs`,
designed in `docs/spec-a2a.md` — but it is addressed by *roster name*, and it is
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
Status: open · Owner: — · Severity: high — **now small, see below**

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
Status: **fixed — merged as #127** · Severity was: high

(Raised from medium. The orchestration sweep found this on **every one** of its
roughly ten live runs, and my own run too. It is universal, not occasional —
every main-chat turn pays one or two `ToolSearch` calls before it touches a
single Jod tool.)

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
Status: **fixed — merged as #125** · Severity was: medium

(Ranked low at first because the fix is one word. That was the wrong axis: the
consequence is that somebody compacts a conversation with five sixths of its
room left, acting on information the screen presented as fact.)

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

---

## Scenarios run

These are the routing scenarios run from this file's own testing. The wider
sweep — continuation versus fresh spawn, several agents running, ambiguous and
malformed instructions, schedule-shaped and goal-shaped instructions — is in
[`10-orchestration.md`](10-orchestration.md), and this table does not repeat it.

| # | Scenario | Expected | Actual | |
|---|---|---|---|---|
| 1 | A one-line factual question needing no repository | answered directly | spawned an agent, returned no answer, $0.39 | **fail — R1** |
| 2 | The same turn's tool calls | hand over and return | `sleep 45` and a shell poll loop | **fail — R4** |
| 3 | The same turn's tool set | Jod's verbs only | also called `ToolSearch select:Monitor` | **fail — R5** |
| 4 | Compaction warning on a fresh chat | quiet | `⚠ compact` after one exchange | **fail — R6** |
| 5 | `jod main` with no instruction | shows the chat | "the main chat is empty — …" | pass |
| 6 | A delegated run appears in `list_agents` | visible with status | visible, `running`, with cost and session id | pass |
| 7 | The delegated run belongs to no work | no work row | `works` empty, correct for `delegate` | pass |
| 8 | The instruction is recorded on the main chat | one user turn | recorded, with the delegation row beside it | pass |
| 9 | Cost and token accounting | reported | `1957 out · $0.3864 · 42s` | pass |
| 10 | A child session can reach the orchestrator | a return path exists | not established — see R3 | **needs confirming** |

---

## R7. The old JSON router is dead code with a passing test suite around it
Status: **fixed — merged as #162**, by deleting it · Severity was: medium

`parse_decision` (`core/src/orchestrator.rs:229`) and `router_prompt` (`:300`)
have **no production caller anywhere**. Verified with `git grep`: zero
references outside `core/src/orchestrator.rs`, and every reference inside it is
in the `#[cfg(test)]` module from line 1515 down. The live path is the
tool-using orchestrator, which routes by calling MCP tools.

Why this is worth a task rather than a shrug: the tests pass, so the code looks
maintained. Anyone searching for "how does routing work" finds these first —
they are the only things in the file that *look* like a router, they have a
clean JSON contract, and they have a thorough test suite vouching for them.

It nearly caused a wrong fix. An agent briefed to add the "answer directly"
disposition was told to extend this plumbing if it was missing. It would have
carefully extended code that never runs, with green tests proving it worked. It
checked callers first, found `Decision::Reply` already *is* that disposition,
backed out its edits, and shipped a change touching only the preamble.

A test suite around dead code is worse than dead weight. Dead weight is
ignored; this actively vouches for a trap.

Fix: delete them, or if they are being kept deliberately, say so in a comment
at the top of each — that they are the earlier design, that nothing calls them,
and where the live path is. The spec's "Out of scope" section says to leave
them alone, which is a reason not to delete them *as part of another task*, not
a reason to leave them unlabelled.

Check: `git grep` for either name returns no non-test caller, and whatever
remains says plainly that it is not live.
