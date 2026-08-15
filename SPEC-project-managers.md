# Project managers: the spec already exists, and it is ready to execute

**This is not a spec.** It is a review of one that already exists, on the
unmerged branch `docs/spec-ceo-and-managers`. My conclusion is that the honest
next step is to land that branch rather than to write a second design, and this
document is here to say what I checked, the four small corrections it needs, and
the seven questions it does not answer.

If you agree, fold the corrections below into `docs/spec-ceo-and-managers` and
merge it, and delete this file. Nothing here needs to survive on its own.

## What already exists

The branch `docs/spec-ceo-and-managers` has one commit,
`5d03d99 docs: spec the CEO/manager orchestrator and visible stalls`, branched
from `ff56e96` on 15 August. It holds a 350-line `SPEC.md`. No pull request is
open on it, and `origin/main` has moved only three commits since it was written.

It designs the whole thing:

- A project manager is one conversation per project, resumed for each
  instruction rather than left resident.
- A `manager_conversation_id` column on `projects`, and a `project_id` column on
  `works`.
- A new `ask_manager` MCP tool, at `ToolAccess::Delegate`.
- `open_work` refused from the main conversation at the tool boundary, not by
  prompt wording.
- Two preambles instead of one — main's, and a manager's.
- A project level and a manager level in the fleet tree.
- Three migrations, a table of the eleven files it touches, seventeen numbered
  checks, and an explicit out-of-scope list.

It also bundles a second, separable change: arming a heartbeat on every spawn so
a wedged session is visible instead of silently occupying the fleet. That change
is real and worth doing. It is independent of managers — managers work without
it, and stall visibility is worth having whether or not managers ship — so it
could reasonably be split into its own pull request. That is a preference, not a
defect.

## Is it executable as written? Yes.

I checked every claim it makes about the codebase against the tree at
`origin/main` today. All of them hold. This is the evidence:

| What the spec claims | Holds on main today |
|---|---|
| The TUI already opens in main; nothing to build | Yes — `cli/src/tui/mod.rs:612` calls `enter_main`, and the test `the_launch_position_is_the_main_chat` is at `cli/src/tui/mod.rs:9521` |
| Stall detection already exists | Yes — `core/src/heartbeat.rs`, and `Verdict::terminates()` at line 251 is true for `Stalled` and `Expired`, exactly as described |
| The sweep is `Ticker::tick_heartbeats` | Yes — `core/src/ticker.rs:761`, the line number is still exact |
| Only three places arm a heartbeat | Yes — `cli/src/main.rs:1741`, `cli/src/tui/mod.rs:1305`, `core/src/ticker.rs:1809`. Nothing the orchestrator spawns arms one |
| `watch_run` is an upsert, so the existing arming sites keep working | Yes — `INSERT INTO heartbeats … ON CONFLICT(run_id) DO UPDATE` at `core/src/store.rs:1796` |
| Projects, the sticky pointer and the resolution audit all exist | Yes — `core/src/projects.rs`, `projects` at `core/src/store.rs:1228`, `conversations.current_project_id` at 1277, `project_resolutions` at 1286 |
| `works` has no `project_id` | Correct. `core/src/store.rs:1032`, and there is no `ALTER TABLE works` anywhere that adds one |
| `AgentView` carries no project, work or health | Correct — `core/src/mcp.rs:2787`, ten fields, `cwd` the only project hint |
| The MCP server knows which run is calling, so a refusal is enforceable | Yes — `mcp::identify` at `core/src/mcp.rs:2522` reads `getpgrp` and looks it up in `runs.pgid`, and `Server::raiser` at `core/src/mcp.rs:1500` already turns that into a conversation id |
| `spawn_agent` is the one place every spawn passes through | Yes — `core/src/service.rs:877` |
| `main_conversation` is get-or-create, and is the shape to copy | Yes — `core/src/orchestrator.rs:1292` |
| `orchestrator_preamble()` is the thing to split in two | Yes — `core/src/orchestrator.rs:353` |
| `core/src/cards.rs` owns the rail | Yes |

Two things it does not claim, which I found while checking and which strengthen
it rather than undermining it:

**Routing is already deterministic, and it already runs before the model sees
anything.** `hand_to_orchestrator` (`core/src/orchestrator.rs:846`) is the single
door every human instruction comes through. At line 875 it calls
`Store::settle_project` on the raw instruction *before* the turn starts, using
`projects::resolve` (`core/src/projects.rs:290`) — plain word-boundary string
matching over names, aliases and path basenames, longest form first. Its own
comment says why: "naming a project is not a judgement call, and paying a
round-trip to be told what the words already said would put a model in the way of
every dictated sentence."

This matters for the question "how does the orchestrator find the suitable
manager". The answer is that it does not have to look. By the time main's model
reads an instruction, the project is already settled and written to
`conversations.current_project_id`, and the suitable manager is that project's
`manager_conversation_id`. `ask_manager` is wiring, not judgement. The spec is
right; it just undersells how much of the hard part is already done.

**`open_work` never touches the catalog.** `orchestrator::open_work`
(`core/src/orchestrator.rs:1179`) takes a `checkout` path and never calls
`project_for_path`, never sets `current_project_id` on the new conversation, and
never writes a resolution row. So the spec's fourth gap is worse than it says: it
is not only that `works` lacks a column, it is that the child session does not
inherit the project either. Adding `works.project_id` fixes the query; setting
`current_project_id` on the opened conversation is what makes everything below
inherit the right project without guessing. Worth adding one line to the spec.

## Four corrections it needs before execution

These are small. None of them changes the design.

### 1. A manager conversation must not use `pinned = 1`

This is the one real bug, and it would break the main chat.

Section 3a says a manager "mirrors `Store::main_conversation`
(`core/src/orchestrator.rs:1261`), which is get-or-create on `pinned = 1`."
Copying the shape is right. Copying the mechanism is not.
`Store::pinned_conversation` (`core/src/orchestrator.rs:1312`) is:

```rust
conn.query_row("SELECT id FROM conversations WHERE pinned = 1", [], |r| r.get(0))
```

No `LIMIT`, no `ORDER BY`, and `query_row` returns the first row rather than
erroring on a second. So a manager row with `pinned = 1` does not fail loudly —
it makes which conversation is "main" depend on SQLite's row order, and
`hand_to_orchestrator` would start appending Reljod's instructions to a project
manager's transcript.

The fix is already in the spec's own design and just needs saying: a manager is
found through `projects.manager_conversation_id`, and its `pinned` stays `0`.
Add a check for it — create a manager, then assert `pinned_conversation()` still
returns the main chat.

### 2. Number the migrations, and do not derive the number by counting

Section "Migrations" lists three `ALTER TABLE` statements in order but gives them
no migration id. The next free id is **`0018`**. The last one in
`core/src/store.rs` is `"0017_approvals"` at line 1310, and note that `0013` is
already used twice — `0013_heartbeats` at line 784 and `0013_roots_and_cards` at
line 847 — so counting entries gives the wrong answer.

### 3. Three line numbers have drifted

The symbol names are all correct; two of the line numbers are not. Whoever
executes should search by name.

- `AgentView` is at `core/src/mcp.rs:2787`, not 961.
- `Store::main_conversation` is at `core/src/orchestrator.rs:1292`, not 1261.
- The heartbeat arming sites are `cli/src/main.rs:1741` and
  `cli/src/tui/mod.rs:1305`, not 1679 and 1301. The third, `core/src/ticker.rs:1809`,
  is still exact.

### 4. Say that `open_work` should also set the conversation's project

Per the finding above. One line in section 2, next to `works.project_id`: the
conversation `open_work` creates gets `current_project_id` set from the work's
project, so the first session and everything it spawns inherit it.

## Open questions for Reljod

I have left out anything the existing spec already decides. It already settles
that a manager is a resumed conversation rather than a resident process, that a
stalled session is marked and never killed, that main may not call `open_work`,
that a project with no manager gets one created on demand, and that domains do
not get managers. Those are recorded there as your choices and I did not reopen
them.

These seven are genuinely unanswered.

### 1. How does a manager's answer get back to you?

The spec's biggest gap. `ask_manager` returns as soon as it hands over, which is
right — main must not block. But the manager's one-or-two-sentence report then
lands in the manager's own transcript while you are looking at the main chat. You
would have to know to go and look.

Options: the manager raises a card, which already cascades up to main's rail
(`core/src/orchestrator.rs:388` promises exactly that); or it messages main on the
bus, which costs main a turn to read; or you enter the manager row in the tree
yourself.

**Recommendation: the manager raises a card when it finishes routing.** It reuses
a path that already works, it puts the answer where the rest of the fleet's
questions already surface, and it keeps `ask_manager` non-blocking. The cost is
one more card per instruction, and whether that is more rail traffic than you want
is the part I cannot judge for you.

### 2. What stops main from doing repository work through `delegate`?

The spec keeps `delegate` on main for repo-less one-shots, which is right — a
lookup needs no work and no board. But `delegate` takes a `cwd`
(`core/src/mcp.rs:189`). A model that has just been refused `open_work` and still
wants to help will reach for `delegate` with the checkout as `cwd`, and it will
feel reasonable at the time. That is the rule routed around, silently, and the
spec does not close it.

Options: rely on the preamble; refuse a `cwd` that `Store::project_for_path`
matches to a known project when the caller is main; or take `delegate` off main
entirely.

**Recommendation: refuse a `cwd` inside a known project.** It is the same
enforcement you already accepted for `open_work`, it costs one call to a function
that exists, and genuinely repo-less one-shots keep working. Removing `delegate`
would leave main unable to answer "what's the weather in Manila" without opening
a work.

This one changes the build, so it wants an answer before someone starts.

### 3. When main cannot tell which project an instruction is about, what does it do?

`settle_project` returns `Match::Ambiguous` when two projects were named, and
`Match::None` with no sticky pointer when the conversation is about nothing yet.
Under this design main can no longer call `open_work`, so it cannot start the work
and sort it out afterwards. The spec does not say what it does instead.

Options: ask; fall back to the most recently touched project, which is what
`projects.last_touched_ms` exists for; or auto-create a project from a path in the
instruction.

**Recommendation: ask, in both cases.** The `project_switch` tool description
already tells the model that "a switch he did not intend is one he can correct",
and `project_resolutions` exists because a wrong quiet resolution is the thing
worth being able to audit. Falling back to recency turns a visible mistake into an
invisible one. Auto-creating is worse: the catalog's value is that it holds the
names you actually say, and a row named after a directory is a name you never said.

### 4. Do goals and schedules stay with main?

The spec leaves `goal_create` and `schedule_create` on main and does not discuss
it. But "keep the Jod test suite green" is a goal about a repository, and under
this design that is a manager's business.

Options: leave both on main; give managers `goal_create` scoped to their project;
or leave them on main but record which project each goal and schedule belongs to.

**Recommendation: leave them on main for now, and add the project column later.**
Arming a schedule spends money at 2am with nobody watching. The comment at
`core/src/mcp.rs:588` argues that the power to create unattended runs is the one to
hold closest, and handing it to a per-project manager multiplies the number of
things that can arm one by the number of repositories you own.

### 5. Is a manager one per project, or one per project per harness?

`Store::resume_for(conversation_id, harness)` (`core/src/conversation.rs:668`)
exists because a session id issued by one harness means nothing to another, and
the comment at `core/src/orchestrator.rs:934` describes the main chat hitting
exactly this after a `/harness` switch.

**Recommendation: one manager per project, resumed through `resume_for` the way
main is.** A manager's value is that it remembers the project, and splitting it by
harness splits that memory for a reason that has nothing to do with the project.

### 6. Does a manager ever get retired, and by whom?

The spec covers creation and says nothing about the end. A manager created on the
first instruction about a project would otherwise live for ever and grow for ever.

Options: never retire it, and let archiving the project make it unreachable;
compact it the way main is compacted; or retire it after a period of silence.

**Recommendation: never retire it, and do compact it.** Archiving a project is
already the deliberate act that says you are done, and the comment on
`projects.state` says archived rows stay so the catalog can still answer "what was
that repo called" months later. `should_compact` (`core/src/orchestrator.rs:105`)
already exists and would apply unchanged. Retiring on silence throws away the
context that is the manager's whole reason to exist, on a project you simply had
not touched in a while.

### 7. Should the heartbeat work ship as its own pull request?

The spec is one document covering two changes: managers, and making a stalled
session visible. They share no code beyond `list_agents` gaining fields. Splitting
them means each lands on its own evidence and a problem in one does not hold up
the other.

**Recommendation: split them.** The stall work is the smaller and the more
urgent — it fixes a fleet filling with hung agents nobody notices — and it does not
need any of the manager design to be right.
