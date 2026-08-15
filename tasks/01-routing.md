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
