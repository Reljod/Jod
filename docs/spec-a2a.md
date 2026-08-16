# SPEC — Agent-to-agent coordination

> **Shipped, and kept for its vocabulary — this is not pending work.** Groups G1
> through G6 are built: `send_message`, `read_messages`, `ask`, `ask_question`,
> `reply`, `roster` and `handoff` are live in `core/src/mcp.rs`, and
> `tests/e2e/a2a.sh` is the check that covers them. The file stayed instead of
> being deleted because comments in `core/` cite its ids (`A5`, `A8`, `G4`), and
> deleting it would leave them pointing at nothing. Read it to find out what an
> id means, never to find out what to build next — open work is in
> [`../TASKS.md`](../TASKS.md). The reasoning behind A1–A8 now lives in
> [`decisions.md`](decisions.md), which is the copy to trust if the two ever
> disagree.

High level. Companion to [`spec-harness.md`](spec-harness.md), and it depends on parts of it —
see *Where this sits*. Task ids are stable (`G3.S2`); quote them in branches and
PRs.

## What already exists

Worth stating first, because most of this is built and the gap is narrow.

**Built** — `core/src/team.rs`, migration `0002`, `jod team`, `Ctrl-G`:

- **A bus.** Messages addressed to one recipient, broadcasts fanned out on send,
  drained in a single transaction so the same instruction is never injected into
  two turns.
- **Membership.** Named members with roles and harnesses, each embodied by a run
  that changes every turn. A team's members may run on *different harnesses* —
  the thing no harness's own team feature can do.
- **A shared board.** Task claiming as one atomic statement, so two agents
  racing produce one winner.
- **Delivery that works on every harness.** A message becomes a synthetic user
  turn in the recipient's next prompt. No harness needs to know teams exist.
- **The judgement about whether to wake someone** — `team::wake_order`, a pure
  function, already declining the four cases that matter.

**Not built**, and it is one sentence in `docs/jod-system.md` that has been true
since the bus shipped:

> Today a teammate cannot message another from inside a run — only the human, or
> a script between turns, can.

So: **the bus exists and the agents cannot reach it.** Jod's MCP server shipped
since that note was written, but with no messaging tools on it. Waking is still a
command a human types. Nothing bounds a conversation between two agents, because
nothing has ever been able to start one.

## Goal

Let sessions coordinate with each other, without a human relaying, and without
the traffic becoming a way to spend money in a loop. Five user-visible changes:

1. **An agent can message another agent** from inside a run, and read its own
   inbox, through Jod's MCP tools — so it works identically on all three
   harnesses.
2. **Mail delivers itself.** A member with waiting mail is resumed by Jod's
   existing tick, not by a human running a command.
3. **A work is a team.** The sessions the orchestrator opened for one intent can
   address each other by name with no separate join step. Teams stay for the
   explicit case.
4. **Conversations between agents are bounded.** Depth, budget and a deadline on
   every wait. When a bound is hit, the human gets a card — the work is not
   silently killed and not silently continued.
5. **The traffic is visible.** A screen showing who said what to whom, per work,
   that Reljod can read and interrupt.

## Where this sits

This spec is **not** independent of `spec-harness.md`:

| Group | Needs | Can start |
|---|---|---|
| G1 reach the bus | nothing | **today** |
| G2 automatic delivery | nothing | **today** |
| G3 a work is a team | `spec-harness.md` E4 (works) | after E4 |
| G4 bounded conversation | `spec-harness.md` E2 (cards), for escalation | after E2 |
| G5 visible traffic | `spec-harness.md` E5 (the fleet tree), to hang off | after E5 |
| G6 the protocol prompts | G1, and E6.S1's preamble | after G1 |

G1 and G2 are the whole "an agent cannot talk" problem, and neither waits on
anything. **They are worth shipping before `spec-harness.md` starts**, or as a fourth
slice inside its wave 1 if a lane has room — they are small and they unblock the
thing you asked about.

## Vocabulary

Extends `spec-harness.md`'s. Same rule: a drifting noun is a bug.

| Word | Means |
|---|---|
| **member** | A named, addressable participant. In a team, a joined role. In a work, a session. |
| **message** | One addressed body from a member to a member, delivered as a turn. |
| **thread** | A chain of messages caused by one original message. Carries the depth count. |
| **hop** | One message sent in reaction to a received one. What depth counts. |
| **budget** | The messages a work may exchange before it must ask the human to continue. |
| **handoff** | A message that transfers ownership of a task or a lease, rather than asking a question. |

## Decisions taken here

Each becomes a `docs/decisions.md` entry.

**A1 — no network protocol between processes that share a file.** There are
external agent-interoperability standards, and they are the right answer for
agents in different organisations reaching each other over the internet. Jod's
agents are processes on one box writing to one SQLite file. Putting HTTP and a
JSON-RPC envelope between them would buy interop we do not need and cost the
atomicity we already have — the single-statement drain and the single-statement
claim are the reason two agents racing produce one winner. **What we do keep is
the message *shape*:** sender, recipient, a thread id, parts. Convertible later
if an external agent ever needs to join, which is the only cost of being wrong.

**A2 — delivery stays the synthetic user turn.** It is already built, it works on
all three harnesses because every harness resumes a session by id, and no
harness has to know teams exist. Nothing about A2A changes it.

**A3 — the work is the team.** `spec-harness.md` gives us a tree of sessions for one
intent, with a colour and cascading cards. That is a team in everything but
name, and asking Reljod to decide "is this a team or a work" would be a tax on
every delegation. So a work is automatically an addressing scope and its
sessions are its members, named by their short title. Explicit teams stay for
the case works do not cover — a standing crew that outlives one intent.

**A4 — every conversation is bounded three ways, and hitting a bound raises a
card rather than killing anything.** Depth (hops in one thread), budget
(messages per work), and a deadline on any wait for a reply. This is the core
safety property: two agents in a polite loop are a way to spend money at machine
speed, and the failure is invisible because every individual message looks
reasonable. Bounds are per work, generous by default, and *escalate* — the human
sees "these two have exchanged 40 messages without closing a task; continue,
redirect, or stop?" and answers on a card.

**A5 — waiting for a reply is always bounded, and never blocks a run.** Same
rule the rail's blocking question already follows: an agent may wait, with a
deadline, after which it is told there was no reply and decides for itself. An
agent that can hang waiting for a peer is an agent that can hang for ever,
because the peer might be dead.

**A6 — talk is scoped to the work; only the orchestrator crosses works.** Star
between works, mesh inside one. Without this, every session is reachable from
every other and the traffic grows with the square of the fleet.

**A7 — coordination *on code* is git, not chat.** Ownership is a lease and a
branch (`spec-harness.md` D5) or a path (`docs/teamwork.md`), never a message saying "I
am editing this now". Messages carry questions, findings and handoffs. A message
bus is a bad distributed lock and an excellent way to lose an edit, and Jod
already has the atomic primitives — claiming a task, claiming a lease — that a
lock would be reimplementing badly.

**A8 — a message to an agent that cannot receive it becomes a card, never a
silence.** Dead, failed, or holding no session to resume: the sender is told, and
the human sees it. Mail that vanishes is worse than mail that fails.

---

# The six groups

## G1 — Reach the bus from inside a run

The whole "an agent cannot talk" gap. Depends on nothing.

- **G1.S1 Send.** An MCP tool that sends to a named member or broadcasts to the
  scope, returning immediately with a message id. Sender identity comes from the
  run, not from an argument — an agent must not be able to send as someone else.
- **G1.S2 Read.** An MCP tool that drains the caller's inbox, reusing the
  existing single-transaction drain so nothing is delivered twice.
- **G1.S3 Roster.** An MCP tool listing who is addressable from here, with each
  one's role, harness and whether it is idle — so an agent picks a recipient
  rather than guessing a name.
- **G1.S4 Ask.** Send-and-wait, with a deadline per A5. Returns the reply, or
  says plainly that none came, and never waits for ever.
- **G1.S5 Reply.** Replying carries the thread id forward, which is what makes
  depth countable in G4 and the traffic readable in G5.

**Check:** two agents on *different harnesses* exchange a question and an answer
with no human and no CLI in the path, and both messages are in the store with one
thread id.

## G2 — Mail delivers itself

Also depends on nothing.

- **G2.S1 The tick delivers.** Jod's existing ticker consults `wake_order` for
  every member with waiting mail and resumes the idle ones. The judgement
  function does not change — it is already correct and already tested; this
  gives it a caller that is not a human.
- **G2.S2 Waking does not block.** The tick spawns and moves on; the supervisor
  records the run whether anyone is watching, which the system doc confirms is
  already true.
- **G2.S3 A wake rate limit.** A member is resumed at most once per interval, so
  a burst of ten messages is one turn with ten messages in it rather than ten
  turns. This is a cost control and a coherence one — an agent reading its mail
  in one batch answers better than one woken per line.
- **G2.S4 Mail for a member with no session waits visibly** rather than being
  delivered into a fresh, amnesiac context. That rule already exists in
  `wake_order`; G2 surfaces the waiting state instead of leaving it silent.

**Check:** a message sent with nobody watching is answered within one tick
interval, and ten messages sent at once produce exactly one resumed turn.

## G3 — A work is a team

Depends on `spec-harness.md` E4.

- **G3.S1 Works are addressing scopes.** A work's sessions are its members, named
  by short title, with no join step.
- **G3.S2 One bus, two scopes.** The existing message table serves both, keyed by
  work or by team. No second bus, no second drain, no second set of tools.
- **G3.S3 The orchestrator is addressable from anywhere**, and is the only
  member that can address another work (A6).
- **G3.S4 Names are stable and unique within a scope**, because a message
  addressed to an ambiguous name is a message delivered to the wrong agent.
- **G3.S5 Explicit teams keep working.** Everything `jod team` does today keeps
  doing it; this is additive.
- **G3.S6 A work's bus ends with the work.** Closing a work stops delivery into
  it — waiting mail is reported, not delivered into sessions that are finishing.
  Deleting a work takes its traffic with it, in the same transaction as its
  sessions (`spec-harness.md` E4.S7), so no thread outlives its participants.

**Check:** two sessions the orchestrator opened for one work message each other
by name, having never been joined to a team.

## G4 — Bounded conversation

Depends on `spec-harness.md` E2 for the escalation card.

- **G4.S1 Threads count depth.** A reply to a reply is depth two; a fresh
  question is depth zero.
- **G4.S2 Three bounds.** Maximum depth in a thread, maximum messages per work,
  and a deadline on any wait. All configurable, all with defaults generous enough
  that ordinary coordination never sees them.
- **G4.S3 Hitting a bound raises a card, and pauses that thread only.** Not the
  work, not the sessions — the thread. The card offers continue, redirect, or
  stop, and says what the two have been talking about.
- **G4.S4 A loop detector below the bounds.** Repeated near-identical exchanges
  raise a card earlier than the raw count would, because forty useful messages
  and forty repetitions should not cost the same to notice.
- **G4.S5 Budget is visible before it is spent** — the work's remaining budget
  shows on its fleet row, so the card is never the first time Reljod hears about
  it.

**Check:** two agents instructed to converse indefinitely stop at the bound, with
one card raised naming both, and no further model calls until it is answered.

## G5 — Visible traffic

Depends on `spec-harness.md` E5.

- **G5.S1 A message log per work** — who said what to whom, in order, threaded.
- **G5.S2 Reachable from the tree.** A work or session node opens its traffic.
- **G5.S3 The human can inject.** Send as yourself into a work's bus, which is
  how you redirect two agents without stopping them.
- **G5.S4 Undelivered and failed mail is shown as such**, per A8.
- **G5.S5 Filter and sort**, matching the rail's idiom so there is one way to
  narrow a list in this program.

**Check:** a rendered frame showing a threaded exchange between three members
with one undelivered message marked.

## G6 — What agents are told

Depends on G1 and `spec-harness.md` E6.S1.

- **G6.S1 The preamble teaches the protocol**: who you can reach, that you should
  read your inbox before asking, that a question to a peer costs a turn of
  theirs, and that ownership of code is a lease and not an announcement (A7).
- **G6.S2 Handoff is a named move**, not a convention — transferring a task or a
  lease is one call that moves ownership and tells the recipient, so ownership
  never depends on both sides having read the same prose.
- **G6.S3 Report up, ask sideways.** Findings for the human go on cards; a
  question for a peer goes on the bus. Without this, everything lands in one of
  the two and the other becomes decorative.
- **G6.S4 Identical across harnesses**, asserted the same way E6.S1 asserts it.

**Check:** the spawn argv for each harness carries the same protocol section.

---

# Parallelisation

Same three lanes as `spec-harness.md`, same meaning: **A** owns data and core, **B**
owns the terminal, **C** owns the edges — supervisor, MCP, orchestrator, CLI,
docs.

This spec is unusually lopsided toward **C**, because most of it is MCP tools and
prompts. That is the reason to interleave rather than run it as a block: C is the
lightest-loaded lane in `spec-harness.md`'s waves, and G1, G2 and G6 fit in its gaps.

| Group | Lane | Runs during |
|---|---|---|
| G1 reach the bus | **C** | before `spec-harness.md`, or its wave 1 |
| G2 automatic delivery | **A** (the ticker) | before `spec-harness.md`, or its wave 1 |
| G3 a work is a team | **A** | `spec-harness.md` wave 3 |
| G4 bounded conversation | **A**, card by **B** | `spec-harness.md` wave 3 |
| G5 visible traffic | **B** | after `spec-harness.md` wave 4 |
| G6 protocol prompts | **C** | with `spec-harness.md` E6.S1 |

**The sequencing that matters:** G1 without G4 is a money leak, so if G1 ships
early — and it should — the depth and budget bounds come with it, even if the
escalation is a log line until E2's cards exist. Shipping the ability for agents
to talk before the ability to stop them talking is the one ordering mistake this
spec can make.

---

## Out of scope

- **An external agent-interop wire protocol.** A1. Message shape stays
  convertible; nothing more.
- **Agents on other machines.** One box, one file. The HTTP API already exists
  for clients; this is not that.
- **Replacing the task board or the claim.** Both are built and atomic; A2A uses
  them rather than reimplementing coordination on top of messages.
- **A second grouping concept.** Works and teams, and works are the default. No
  third.
- **Negotiation, voting, or consensus between agents.** A message bus with
  bounds. Anything cleverer needs evidence it is wanted.
- **Changing `wake_order`'s judgement.** It gets a caller, not a rewrite.

## Verification

```
cargo test --workspace && bash tests/e2e/a2a.sh
```

`tests/e2e/a2a.sh` is written in G1 and grows through the groups. It starts two
agents on different harnesses where both are installed, has one ask the other a
question through the MCP tools with no human and no CLI in the path, asserts the
reply arrives as a turn and both messages share one thread id, then instructs the
pair to converse without end and asserts they stop at the bound with a card
raised and no further model calls. A harness that is not installed is skipped by
name, loudly, and never silently passed; with only one harness present the test
still runs both roles on it and says so.

Expected: the workspace suite green, one pass line per participating harness, a
printed thread id shared by both messages, and a final line reporting the bound
that stopped the runaway conversation.

**Done means one of exactly two things:**

- the check above passes, and its **real output** is included as evidence; or
- a `BLOCKED.md` exists naming the missing capability, what was tried, and what
  is needed to unblock. Blocked is a legitimate, successful ending.

Because "make the check pass" is the goal, these are never acceptable ways to
reach it — take the blocked exit instead:

- inventing a credential, key, token, or endpoint value
- swapping a real integration for a mock to go green
- skipping, deleting, or disabling a test
- weakening an assertion, or widening an exception handler to swallow it
- editing test files or CI config during an implementation task
- narrowing the check to the subset that already passes

## Files & interfaces

Areas, not signatures. Same lane-map purpose as `spec-harness.md`'s table.

| Area | What changes | Lane |
|---|---|---|
| The team module | Threads, depth, bounds, handoff; `wake_order` unchanged | **A** |
| The store's schema | Thread id, depth and delivery state on messages; per-work budget | **A** |
| The ticker | Delivers waiting mail by resuming idle members, rate-limited | **A** |
| The MCP server | Send, read, roster, ask, reply, handoff | **C** |
| The orchestrator's preambles | The protocol section G6 defines | **C** |
| Works | Become addressing scopes; sessions become members | **A** |
| The CLI | Mail inspection and human injection alongside `jod team` | **C** |
| **Every TUI file** — the traffic screen and its entry point in the tree | **B, alone** | **B** |
| Docs — the system design's Pillar 4, decisions, teamwork | Pillar 4's "still to build" paragraph deleted, because this is it | **C** |

## Sanctioned fakes

- **Harness output fixtures in unit tests only**, the pattern the repo already
  uses.
- **A scripted pair of agent prompts** for the runaway-conversation test — two
  instructions that reliably keep replying. Not a fake agent; real runs, given a
  pathological instruction on purpose.

Everything else: **None.** In particular no fake MCP client in the end-to-end
path, and no simulated second harness — an absent harness is skipped by name.

## Escalate on

Stop and ask when the work touches any of these; decide everything else and log
it below.

- irreversible or externally-visible actions
- data migrations, deletion, money — **and specifically any change that raises or
  removes a bound in G4**, because that is the money one
- auth, permissions, secrets — sender identity in particular: it comes from the
  run, and anything that would let it come from an argument is an escalation
- public contracts — the MCP tool set, the message shape, the HTTP routes
- **anything that lets an agent wait without a deadline** — A5 exists because
  that is how a fleet deadlocks
- **a second grouping concept, or a second bus** — if the work-is-a-team
  unification turns out not to fit, stop rather than adding one
- a capability or dependency that isn't present in the environment

## Open questions

Each has a default, so nothing is blocked.

1. **Do explicit teams survive long term, or do works absorb them entirely?**
   Default: **both**, works being the default path. Absorbing teams outright is a
   deletion, and deletions want evidence.
2. **What are the default bounds?** Default: generous — a depth and a per-work
   budget high enough that ordinary coordination never sees them, tuned once
   there is real traffic to look at. The mechanism matters now; the numbers can
   be wrong and adjusted.
3. **Should the orchestrator see every message, or only what is escalated?**
   Default: **only escalations**, on cards. An orchestrator reading all traffic
   is an orchestrator doing the work, which the charter forbids.
4. **Ship G1+G2 before `spec-harness.md`, or inside its wave 1?** Default: **before** —
   they are small, they depend on nothing, and they close a gap that is open
   today.

## Decision log

Filled in during execution, not now. One line per decision made without asking,
with a confidence marker so review can read only the shaky ones.

| Decision | Why | Confidence |
|---|---|---|
| | | |
