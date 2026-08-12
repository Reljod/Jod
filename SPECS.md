# SPEC — Jod as the coding harness

High level only. What gets built, in what order, and **what can be built at the
same time as what**. No implementation detail — the executing session decides
that. Task ids are stable (`E2.S4`); quote them in branches, commits and PRs so
a half-finished epic is legible to the next session.

## Goal

Make `jod tui` the surface Reljod codes in, instead of `claude` — without Jod
becoming a harness. Six user-visible changes:

1. **A session has working directories** (plural). `@` fuzzy-picks a file or
   folder across all of them; content search goes through ripgrep. With no roots
   set, `@` says so rather than silently searching the process directory.
2. **A left rail carries the agent's decisions and its open questions.** An
   autonomous choice arrives as a small card ("chat DB: chose SQLite — switch?");
   a real blocker arrives with a coloured border and the word `blocked`. Expand a
   card to pick an option or answer in prose. Answered cards leave the stack and
   stay findable — filter by text, sort by importance or age.
3. **Credentials come in through that same rail and never reach the model.** The
   value is stored outside every repo, injected into the agent's environment, and
   scrubbed out of everything the harness prints. The agent is told a *name*,
   never a value — so a missing key blocks one test, not the session.
4. **The main chat is an orchestrator over a tree of sessions.** "Work on
   @some-repo, do X" opens a *work*: a titled group of sessions that read your
   real checkout and, the moment one needs to change something, claim a git
   worktree of their own to write in. The orchestrator never blocks; it delegates
   and comes back.
5. **Fleet becomes that tree.** Arrows walk it, expand and collapse, enter opens.
   Every node shows whether it is running and what it is doing. Cards from every
   descendant cascade up to the orchestrator's rail, colour-coded per work.
6. **The experience is identical on all three harnesses**, because everything
   above is Jod's rather than Claude Code's. Slash commands and skills found in
   the repo are offered in Jod's palette; pull requests opened by a run are
   shown, and auto-PR is a toggle.

## Vocabulary

Fixed here because six epics use these words, and a drifting noun is a bug.

| Word | Means |
|---|---|
| **root** | A directory a conversation may read, marked writable or read-only. A conversation has zero or more. |
| **work** | One intent, spanning several conversations. Titled and summarised by a throwaway model call. Owns a colour. |
| **project-session** | A conversation belonging to a work. Not a new type — an existing conversation with a work attached. |
| **card** | One row in the left rail: a decision, a question, or a secret request. |
| **lease** | A git worktree a session claimed to write in, tracked so siblings can reuse it. |
| **task** | One item on a work's board. A work opens with at least one, so "all done" always means something. |
| **closed** | Every task complete. The work is over; its record and its worktrees remain. |
| **deleted** | The work and its sessions are gone from Jod. Its worktrees and branches are not. |
| **redaction** | The supervisor step that replaces a live secret's value in every line before it is stored. |

## The two seams

Everything else is detail hanging off these.

**Cards are emitted over Jod's own MCP server**, which all three harnesses
already register. That is the single reason the rail is harness-agnostic instead
of a Claude Code feature reimplemented twice. Behind it sits a passive lifter, so
a harness launched without the server still produces cards from what it prints.

**Secrets are injected and redacted by the supervisor**, the only process that
sees both the harness's environment and its output. Collect in the rail, store
outside every repo, inject at spawn, scrub on the way out.

## Files & interfaces

Areas of the repo, not signatures — the executing session designs those. This
table is here because it is also the **lane map**: two lanes that share a row are
two lanes that will conflict.

| Area | What changes | Lane |
|---|---|---|
| The store's schema | New tables for roots, cards, works, leases, pull requests, discovered commands; new columns tying a conversation to a work and a parent | Wave 0 |
| The store's queries | Everything the rail, the tree and the CLI read | **A** |
| New core modules | Roots, ranking, cards, works, leases, the tree model, pull requests — one module each, no shared file | **A** |
| Secrets and the supervisor | Storage, injection, and scrubbing both output streams | **C** |
| The spawn contract | Carries roots and environment pairs through to the harness | Wave 0 |
| The MCP server | The card tools, opening a work, listing roots | **C** |
| The orchestrator | Preambles rewritten; the router learns to open a work | **C** |
| The conversation store | Deleting a conversation, which does not exist today | **A** |
| The delivery handler | Queued card answers and team mail injected at a turn boundary by one component, never mid-turn | **A** |
| The task board and work lifecycle | Tasks per work, closing when the board empties, cascading delete with its confirmation | **A** |
| Command discovery and pull requests | Scanning, caching, detection, reconciliation | **C** / **A** |
| **Every TUI file** — rail, picker, tree, and the shared renderer, keymap, mode switch and app state | The screens themselves, and registering them | **B, alone** |
| The CLI | Root, card, secret, work and session-delete subcommands | **C** |
| Docs — decisions, system design, harness config, README | The seven decisions, the new concepts, the measured support matrix | **C** |

**One lane owns the terminal.** That is the single most important row: it is why
three lanes need no conflict protocol at all. See *Why three, and not four*.

## Decisions taken here

Each becomes a `docs/decisions.md` entry in the epic that implements it.

**D1 — Jod builds fzf's *feel*, and depends on no picker binary.** The target is
the interaction: type a few scattered letters, see ranked matches update on every
keystroke with the matched characters highlighted, move with the arrows, accept
with enter. None of that requires `fzf` itself — and shelling out to it would
actively prevent the good version, because `fzf` owns a whole terminal, so every
`@` would tear down and restore the screen, and an inline popup under the cursor
is not something an external full-screen program can draw at all. So: fuzzy
matching in-process, over a candidate list ripgrep enumerates, with a walker
fallback when ripgrep is absent. No picker binary is required, preferred, or
supported.

The UX bar this sets, which the epic is checked against: results ranked, not
merely filtered; matched characters highlighted in every row; live on every
keystroke with no perceptible lag on a large repo; arrows and enter; escape
leaves what you typed alone.

**D2 — cards go over MCP, with a passive lifter behind it.** Three tools —
record a decision, ask a question, request a secret — are the supported path and
behave identically on all three harnesses. Emission never blocks the agent: a
question returns a card id immediately unless it is explicitly blocking, and even
a blocking one gives up after a bounded wait rather than hanging the run.

**Neither raising a card nor answering one may perturb a running turn.** Both
directions are queued. Raising is a write and a return — the agent does not wait
to see whether anyone is looking. Answering enqueues a *pending delivery* against
the conversation; it does not interrupt the turn in flight, because an answer
spliced into the middle of a turn arrives in a context that was assembled before
it existed, and the agent either ignores it or acts on it twice.

**A single small handler owns the timing** — when a queued answer is injected,
and whether several are batched into one. Its rules are the ones `wake_order`
already encodes for team mail: deliver to an idle session now, hold for a running
one until the turn ends, batch what accumulated in between into one turn rather
than one turn each, and never deliver into a session that has no context to
receive it. Delivery itself is the synthetic user turn the bus already uses, so
card answers and agent mail travel the same road and there is exactly one place
in Jod that decides when an agent is spoken to.

The consequence worth stating plainly: **a card answer is asynchronous, and the
UI must not pretend otherwise.** An answered card shows as *answered, queued*
until the handler delivers it, then as *delivered*. Reljod can answer ten cards
while a run is mid-turn and none of them touch it until it comes up for air.

**D3 — a secret's value is never in the model's context.** Stored outside every
repo at owner-only permissions, injected as an environment variable at spawn,
and scrubbed from the harness's output before anything is parsed or stored. This
is the model GitHub Actions, Doppler, Infisical and `op run` converged on: inject
at exec, mask on output, reference by name. Redaction is the belt to injection's
braces — an agent that echoes the variable still cannot get the value into the
transcript.

**D4 — a work is a group, not a new kind of session.** Nothing in Jod learns a
second session type, and the fleet tree becomes a self-join over what already
exists.

**D5 — a session reads the real checkout and writes only in a worktree it
claims.** It starts pointed at your actual repo, marked read-only. The moment it
needs to change something it claims a lease — a fresh branch and worktree — and
that becomes its only *writable* root. The original stays in its roots as a
read-only one, so it can still read and diff against what you are editing.
Leases are per work-and-repo and reusable: a second session on the same repo in
the same work is offered the existing lease before a new one is cut.

Claiming is an explicit step the agent takes, not something Jod infers, because
"detect the first write" has no harness-agnostic implementation — every harness
spells its pre-write hook differently, and two of the three barely have one. So
the agent is told: this root is read-only, claim a worktree before changing
anything. A watcher on the read-only root is the backstop, and a write that
lands there anyway raises a card rather than being silently kept.

**This is a convention, not a sandbox.** Jod passes a deny rule where a harness
supports one, but nothing here stops a determined agent writing outside its
roots. What the design actually guarantees is narrower and still worth having:
work happens on a branch by default, so your checkout is not where a run's
half-finished state accumulates.

**D6 — the titler is a throwaway conversation that is then deleted.** Cheap
model, one turn, then removed. This is why deleting a conversation is in scope at
all.

**D8 — a work ends when its tasks do, and deleting one never eats uncommitted
code.** *Done* is not a judgement call: a work opens with at least one task and
is **closed** when every task on its board is complete. Closing is automatic and
cheap — the record stays, the tree stays, nothing is destroyed.

**Deleting** is the separate, explicit act, and it takes every session in the
work with it: their transcripts, their unanswered cards, their bus traffic.
Because that is unrecoverable and easy to fire by accident, deleting a work that
holds worktrees **refuses the first time** — it prints exactly what would be
lost and what is dirty — and proceeds only if the *same* command is repeated. A
different work's delete does not inherit that confirmation.

What deletion does **not** do is remove the git worktrees or their branches. Jod's
records are cheap to recreate; a branch with uncommitted work on it is not, and
the moment of deleting a session's history is exactly the moment nobody is left
to remember what was on it. The paths are printed, so nothing is orphaned
silently, and removing them is a separate deliberate flag that still refuses a
dirty tree.

**D7 — repo slash commands are forwarded, not reimplemented.** Jod sends the
command line through to harnesses that expand it themselves, and inlines the
command's text for those that do not. Which harnesses do which is *measured*
before the code is written, not assumed.

---

# The six epics

Each `Sn` is a shippable slice with its own check and its own PR.

## E1 — Roots, mentions and ripgrep

- **E1.S1 Roots exist.** A conversation owns an ordered set of directories, each
  marked writable or read-only — the flag E4 sets when a session claims a
  worktree. Existing conversations keep the directory they already had. Add,
  remove, list, and a containment test the other epics use.
- **E1.S2 Candidates and ranking.** Enumerate files and folders per root through
  ripgrep, falling back to a walker; rank in-process against D1's UX bar. Cached
  briefly, because `@` is typed one character at a time.
- **E1.S3 The mention popup.** Opens on `@`, ranks live under the cursor with
  matched characters highlighted, arrows and enter, escape leaving what you typed
  alone. Inserts a root-qualified path when several roots are set. With zero
  roots it says so and accepts nothing. A folder mention expands to a capped
  listing at send time.
- **E1.S4 Setting roots.** A full-screen directory picker starting at the current
  directory — the same matcher and the same keys as the popup, so there is one
  picker with two sizes rather than two pickers — plus add/remove/list from both
  the palette and the CLI, plus a repeatable launch flag.
- **E1.S5 Ripgrep as the search path.** Grep across every root from the palette,
  and roots reaching the harness through whatever each one's directory flag is —
  measured per harness, with the degradation documented where a harness has none.

**Check:** roots survive a round trip through the CLI; the picker ranks a deep
exact path above a scattered-letters match; and a keystroke over a repo of a
hundred thousand files still re-ranks within one frame. No picker binary is
invoked — asserted, so nobody quietly reintroduces one.

## E2 — The decision rail

- **E2.S1 The card store.** Cards with kind, importance, status, options,
  answers and full-text search. One query builder serving the rail, the CLI and
  the MCP tool, so the three cannot drift.
- **E2.S2 Emission.** The three MCP tools, plus a lifter that turns a harness's
  own question and plan-approval calls into cards, de-duplicated against the MCP
  path.
- **E2.S3 The rail, collapsed.** A narrow left column of two-line cards, a toggle
  key, cycle keys that do not cost the sentence you were typing, border colour by
  kind and importance, auto-open once on the first blocker, and a one-line
  summary instead of the rail on a narrow terminal.
- **E2.S4 The rail, expanded.** Full card with provenance, numbered options
  answerable by digit, a free-text line for prose, dismiss, and answered cards
  toggled back into view.
- **E2.S5 Filter and sort.** Text filter through search, sort by importance,
  created or updated, kind filter, all surviving navigation away and back.
- **E2.S6 CLI parity.** List, show and answer cards from the command line, so a
  headless or phone-side answer is possible.
- **E2.S7 The delivery handler.** Per D2, answering a card enqueues rather than
  interrupts. One handler decides when a queued answer reaches its session —
  immediately if idle, at the end of the current turn if running — batches
  everything that accumulated into a single turn, and marks each answer
  *delivered* only once it is actually in a prompt. It is the same road team mail
  travels, so this slice generalises the existing delivery rather than adding a
  second mechanism. A session that ends before its answers are delivered reports
  them as undelivered instead of dropping them.

**Check:** a rendered frame showing three cards, one bordered `blocked`, the
answered one hidden until toggled. Separately: answering a card against a session
with a turn in flight leaves that turn's prompt untouched and marks the card
*queued*; the answer appears in the next turn, and ten answers queued during one
turn arrive as one turn carrying ten, not ten turns.

## E3 — Secrets the agent cannot read

- **E3.S1 The secret store.** Values outside every repo, owner-only permissions
  verified on read, scoped global / work / conversation, names validated so they
  are always legal environment variables. Names are readable; values are not
  returned to anything but the spawn path.
- **E3.S2 Injection.** The spawn request carries environment pairs; the
  supervisor applies them; nothing about them enters the prompt or the
  transcript.
- **E3.S3 Redaction.** Every line of the harness's output, on both streams,
  passes through a scrubber before parsing. Short values are not redacted — the
  false positives would mangle ordinary output — and the rail says so when one is
  stored.
- **E3.S4 The rail flow.** A secret request opens a card explaining where the
  value will live; answering writes it straight through without it ever sitting
  in the UI's state; the card afterwards shows only a name and a scope. Injection
  applies from the next spawn, and the card says so.
- **E3.S5 Telling the agent.** The worker preamble names the available secrets,
  says they are environment variables, forbids echoing them, and restates that a
  missing key is a *blocked* ending rather than a reason to invent one.

**Check:** a run told to print a secret prints the redaction marker, and the
value appears nowhere in the database.

## E4 — Works, the session tree, worktree leases

- **E4.S1 Works.** A titled, coloured group; conversations gain a work, a parent
  and an origin; the tree and the whole forest are queryable; cycles are refused.
- **E4.S2 The throwaway titler.** One cheap turn produces a title and a summary,
  then the conversation is deleted. Deleting a conversation is new here. It
  refuses the pinned main chat, and it refuses a conversation that belongs to a
  work — deleting the *work* is the only sanctioned way to remove those, so a
  session cannot be quietly cut out of a tree that still points at it. A titler
  outage falls back to the first few words of the instruction rather than
  blocking the work.
- **E4.S3 Claiming a worktree.** A session starts with the real checkout as a
  read-only root. An explicit claim — the agent's own step, per D5 — cuts a
  branch and worktree, records the lease, and adds it as the session's writable
  root while the original stays read-only beside it. Leases are reusable within a
  work: a sibling on the same repo is offered the existing one first. Releasing
  removes the tree only when it is clean and merged, and otherwise keeps it and
  says why. A non-git root cannot be claimed and raises a card, not a crash.
- **E4.S3b The read-only backstop.** A watcher over each read-only root turns an
  unclaimed write into a card naming the file, so the convention failing is
  visible rather than silent. It reports; it does not revert.
- **E4.S4 The orchestrator opens works.** The preamble is rewritten around the
  new vocabulary; a new routing decision opens a work, titles it and spawns the
  first session against the read-only checkout, returning as soon as it is
  spawned — no worktree is cut until the session asks for one. Sessions may spawn
  their own children, which is what makes the tree deeper than two levels.
- **E4.S5 Cascading cards.** Card queries gain subtree scope; the main rail shows
  every descendant's cards, tinted by work; cascade is upward only; every card
  names the session it came from so an answer never lands on the wrong agent.
- **E4.S6 A work has a board, and finishing it closes the work.** Opening a work
  records at least one task — the instruction itself, if nothing finer is known —
  so "all tasks complete" is always a meaningful state. Tasks reuse the existing
  board and its atomic claim rather than a second one. When the last task closes
  the work becomes **closed** on its own: its idle sessions are stopped, its
  running ones are left alone to finish, and a closing card summarises what came
  out of it — the branches, the pull requests, the leases still on disk, and any
  card nobody answered. A work whose tasks are done but whose sessions are still
  running is shown as *finishing*, not closed, because the two are different
  questions and only one of them is safe to act on.
- **E4.S7 Deleting a work.** Removes the work and every session attached to it —
  transcripts, cards, delegations, bus traffic — in one transaction, so a
  half-deleted tree is not a state that exists. Per D8 it **refuses the first
  time if the work holds any worktree**, printing each lease's path, branch,
  whether it is dirty, and whether it is merged, plus the count of transcripts
  and unanswered cards about to go. Repeating the identical command completes it.
  The confirmation is bound to that work and expires, so a stale confirmation
  cannot arm a later delete. A work with no leases deletes on the first command,
  because there is nothing on disk to lose.
- **E4.S8 The worktrees outlive the work.** Deletion prints the paths it left
  behind and leaves the branches alone. Removing them is a separate explicit
  flag, and even then a dirty or unmerged tree is kept and reported rather than
  removed. `jod work leases` remains the way to find and clean them afterwards,
  including leases whose work is gone.

**Check:** one instruction naming a folder produces a titled work and a session
with the folder as a read-only root and **no worktree yet**; the session's first
claim cuts a branch, and after it the original root is still readable and no
longer writable. A printed two-level tree shows both. Completing the work's last
task closes it and raises the closing card. Deleting it then **fails the first
time** naming the lease, succeeds when the command is repeated, removes every
session in the work — asserted by counting conversations before and after — and
leaves the branch and its worktree on disk.

## E5 — Fleet as a tree

- **E5.S1 The tree model.** Works, sessions and runs flattened in one pass, with
  expansion state persisted and selection held by id rather than index, because
  the tree reshapes as runs finish.
- **E5.S2 Navigation.** Up and down through visible rows; right expands or
  descends; left collapses or jumps to the parent; space toggles; enter opens the
  node's session or run; expand-all and collapse-all. The existing fleet verbs
  keep their keys.
- **E5.S3 Rendering.** Tree guides with an ASCII fallback, a declared column drop
  order at narrow widths, spinners on running nodes, a card count per node so the
  tree says where the questions are, work colour on the row, and a filter that
  keeps ancestors of every hit visible.
- **E5.S3b Closed works get out of the way.** A closed work collapses by default
  and sorts below live ones; a toggle hides them entirely. Otherwise the tree
  becomes an archive of everything ever done, which is the state that makes
  people stop reading it.
- **E5.S4 Summaries.** The newest message or tool call as the node's summary — no
  extra model call — refreshed on the existing tick, off the render path.

**Check:** a rendered frame with two works, four sessions, one expanded run, and
a blocked count in the gutter; navigation asserted by test.

## E6 — Parity: prompts, commands, pull requests

- **E6.S1 Preambles.** One worker preamble naming the roots and which of them are
  read-only, the rule that changing anything means claiming a worktree first, the
  available secret names, and the card tools; skills and the charter pointed at
  under every root; the body asserted identical across harnesses except for
  documented per-harness lines.
- **E6.S2 Harness commands in the palette.** Discover commands and skills under
  each root and in the user's own config, cache the discovery, list them in Jod's
  palette marked with their source, and forward them per D7. **The forwarding
  behaviour is probed against each binary first** — if all three expand
  themselves, the inlining branch is deleted rather than kept just in case.
- **E6.S3 Pull requests.** Detected two ways — parsed from the event stream for
  immediacy, reconciled by polling for authority — shown on the work's row and in
  the panel, with an off-by-default auto-PR that opens a *draft* through the
  existing skill and never merges. Absent or unauthenticated tooling degrades
  quietly and says why once.
- **E6.S4 Documentation.** The seven decisions, the rail and works and leases in
  the system doc, a measured per-harness support matrix, and the README's six
  changes.

**Check:** a repo command appears in the palette with its description and
forwards literally; the spec's own completeness checker passes.

## E7 — Parity with Claude Code as a place to code all day

Added after a survey of what `jod tui` already does, because the goal is not
"the six changes above" but "Reljod codes here instead of in `claude`". Most of
the list came back **present**: slash commands with tab-completion, plan mode,
per-turn cost and a context-usage bar, scrollback, session fork, rewind and
cross-harness handoff, and a task board. These are the gaps that make a working
day painful, and nothing here is speculative — each one was measured absent.

- **E7.S1 Interrupt a turn without killing the session.** Today the only stop is
  `Alt-X`, which kills the process group outright; there is no way to say "stop,
  but stay". That is the single most-used key in a coding harness — you see it
  going the wrong way in the first two seconds and you correct it. Escape
  interrupts the run, keeps the conversation and its session id, records the
  partial turn as what it was, and leaves you typing the correction. A second
  Escape with nothing running is the existing back behaviour, unchanged.
- **E7.S2 Per-tool approval, honestly scoped.** Jod runs harnesses in print
  mode, where the permission *mode* is fixed at spawn and there is no interactive
  callback to hang an allow/deny prompt on. So the prompt cannot be
  reimplemented — but the need behind it can be met, and already is: an agent
  that wants permission raises a **blocking card**, which is the rail Reljod is
  already watching. This slice makes that the documented answer, teaches it in
  the preamble, and states the limit plainly in the support matrix rather than
  shipping a dialog that only works on one harness. **Measure first**: if a
  harness does expose a mid-run permission event, lift it into a card.
- **E7.S3 Diffs render as diffs.** An edit currently shows as a one-line tool
  summary, which is unreadable as review. File-editing tool calls render as a
  proper diff — added and removed lines coloured, hunks collapsed past a
  threshold, and the path as a header. This is the difference between watching an
  agent work and trusting it afterwards.
- **E7.S4 The plan and the todo list live in the transcript.** The board exists
  as its own screen, which is the wrong place while a turn is running: what you
  want is the current plan updating in front of you. Todo and plan events from
  the harness stream render inline and in place, one block that updates rather
  than a new block per revision.
- **E7.S5 Search the transcript.** `/` filters every list screen but not the
  conversation, and `messages_fts` has been there since `0006`. Search within
  the open conversation and across all of them, jumping to the hit.
- **E7.S6 Yank.** Copy the selected message, the last agent reply, or a code
  block, without relying on the terminal's own selection — which is unusable once
  a pane has scrollback and wrapping.

**Check:** a run is interrupted with Escape and then continued in the same
session — asserted by the session id being unchanged across the interruption —
and a rendered frame shows a file edit as a coloured diff.

---

# Parallelisation

The epics are **not** a queue. Below is what actually blocks what, and how to
run several sessions at once without them colliding.

**A lane is one agent session, working alone, on a set of files nobody else is
allowed to touch.** Four lanes means four `jod` sessions running at the same
time on four branches. The number is not about speed — it is about how many
non-overlapping piles of files this spec can be cut into. Cut it into more piles
than that and the lanes start editing the same file, and you spend the time you
saved resolving conflicts.

**A wave is a synchronisation point.** Every lane in a wave starts from the same
base and finishes before the next wave starts, because the next wave's lanes
need what this one produced. Waves are where the plan is allowed to be serial.

## What forces order

Only four hard dependencies exist. Everything else is schedule, not logic.

```
        ┌──────────────────────────── W0 ────────────────────────────┐
        │  contracts: table shapes · query names · spawn fields       │
        └───────┬────────────┬────────────┬───────────┬───────────────┘
                │            │            │           │
        ┌───────▼────────┐  ┌────▼───────────┐  ┌──▼──────────────┐
   W1   │ A: roots ·     │  │ B: the rail ·  │  │ C: secrets ·    │
        │    ranking ·   │  │    the mention │  │    injection ·  │
        │    card store  │  │    popup       │  │    the probe    │
        └───────┬────────┘  └────┬───────────┘  └──┬──────────────┘
                │                │                 │
   W2   ┌───────▼────────┐  ┌────▼───────────┐  ┌──▼──────────────┐
        │ A: works ·     │  │ B: the picker  │  │ C: MCP tools ·  │
        │    titler ·    │  │    screen ·    │  │    discovery ·  │
        │    leases ·    │  │    the secret  │  │    CLI parity   │
        │    tree query  │  │    card        │  │                 │
        └───────┬────────┘  └────┬───────────┘  └──┬──────────────┘
                │                │                 │
   W3   ┌───────▼────────┐  ┌────▼───────────┐  ┌──▼──────────────┐
        │ A: pull        │  │ B: the fleet   │  │ C: preambles    │
        │    requests    │  │    tree        │  │                 │
        └────────────────┘  └────────────────┘  └─────────────────┘

   W4    docs · the last wiring   ← last, because they describe the rest
```

The four real edges:

1. **E4 needs E1.S1** — a lease rebinds a session's roots, so roots must exist.
2. **E3.S4 needs E2.S3** — the secret flow is a card in the rail.
3. **E5 needs E4.S1** — the tree renders the forest query.
4. **E6.S3 needs E4.S3** — a PR is discovered per lease.

Everything else is free. In particular **E2 does not need E1**, and **E3's
storage, injection and redaction do not need the rail** — those two facts are
what make three lanes possible on day one.

## Wave 0 — the contracts, and why it is worth a day

One short session, alone, before any lane starts: land the migrations and the
empty query signatures the lanes will call. Nothing implemented, everything
named.

This exists because the alternative is three lanes each inventing a card table.
It is the only genuinely serial work in the spec, and it is small. Skip it and
wave 1 spends its time in merge conflicts instead.

## The three lanes

They are **stable across every wave** — the same three owners from wave 1 to
wave 4 — because a lane's value is the context its owner accumulates, and
reshuffling ownership between waves throws that away.

| Lane | Owns, throughout | Never touches |
|---|---|---|
| **A · data and core** | The store's queries and every new core module: roots, ranking, cards, works, leases, the tree model, pull requests | The terminal, the supervisor |
| **B · the terminal** | Every TUI file, shared ones included — the rail, the pickers, the tree, and the wiring that registers them | Core, the supervisor, the CLI |
| **C · edges** | The supervisor, secrets, the MCP server, the orchestrator's preambles, command discovery, the CLI, the docs | The terminal, the store's queries |

**The dividing rule between A and B: anything testable without a terminal lives
in core and belongs to A.** Fuzzy ranking, the tree's flatten, card filtering and
sorting are logic, not drawing. B writes only what paints and what handles keys.
This is what keeps the one-terminal-lane rule from making B the bottleneck, and
it is a better architecture regardless — none of that logic should need a
terminal to be tested.

## Wave 1

- **A** — roots exist; candidate enumeration and ranking as a core module; the
  card store and its one query builder.
- **B** — the rail, collapsed and expanded, with filter and sort; the mention
  popup over A's ranking.
- **C** — secret storage, injection and redaction, end to end; and the
  command-expansion probe.

C's probe is deliberately first rather than last. It is a *measurement* — does
each binary expand its own slash commands — and its answer deletes or keeps a
branch of E6's design. An hour in wave 1; a redesign in wave 4.

## Wave 2

- **A** — works, the throwaway titler, leases and claiming, and the tree query
  E5 will render.
- **B** — the full-screen picker; the secret card flow; cards cascading by work.
- **C** — the MCP tools over A's queries; command discovery; CLI parity for
  roots, cards, secrets and session delete.

## Wave 3

- **A** — pull requests, detected per lease.
- **B** — the fleet tree.
- **C** — the preambles, once there is something settled to describe.

## Wave 4

Documentation and the last wiring. Small, and genuinely last: written earlier it
would be written twice.

## Why three, and not four

Four lanes forced a coordination protocol. Rail, picker and tree were three
lanes all editing the same four shared terminal files, so the plan needed a
"one wiring task per wave, one owner" rule, and a standing instruction to stop
and reassign if two lanes ever landed in the renderer together.

**At three lanes that whole problem disappears**, because one lane owns the
terminal outright. There is no wiring task, no shared-file protocol, and nothing
to police. The cost is that B is the critical path — which is why A and C are cut
to feed it: A hands it tested logic, C takes every CLI mirror, doc and preamble
off its plate.

Three is not a compromise between two and four here. It is the width at which
the file boundaries and the work boundaries are the same boundary.

## Sequencing rules

- **A lane opens one PR per slice**, not per epic. Six slices in E2 is six PRs.
  A lane that opens one big PR blocks every reviewer and every rebase behind it.
- **A lane rebases before it opens**, not after review.
- **A blocked lane writes it down and stops.** The wave does not wait on it —
  the other lanes carry on, and the blocked slice moves to the next wave.
- **The wave boundary is a real boundary.** Nobody starts wave 2 work in wave 1
  because they finished early; they take a slice from a wave-1 lane instead. The
  boundaries exist because the contracts change across them.

## What would collapse the plan back to serial

Worth naming, so it is recognised early:

- Wave 0 slipping. Every lane calls those names.
- The card table needing a shape change after E2 ships — E3, E4 and E5 all read
  it. This is the highest-value thing to get right in wave 0.
- Discovering in wave 3 that a harness cannot pass extra directories or register
  an MCP server. That is what lane C's probe is for, and it should be widened to
  cover both if that is cheap.
- **Lane B falling behind.** It is the critical path by construction. If it
  slips, A and C should take more logic out of the terminal rather than opening a
  fourth lane, which would re-create the problem three lanes exist to avoid.

---

## Out of scope

Named because each is a tempting neighbour:

- **Rewriting the transcript, compaction, or the memory graph.** Untouched.
- **A second permission system.** Roots are not a sandbox and nothing here may
  imply they are. A harness that ignores a directory flag can still read outside
  its roots — a documented limit, not a bug to fix here.
- **An OS keychain for secrets.** File permissions plus redaction now; a keychain
  later if it earns its way in.
- **Merging pull requests, or changing the merge script.** E6 shows and opens; it
  never merges.
- **Web, desktop, iOS and voice clients.** They read the same tables and can
  follow later.
- **A fourth harness.** Three is the set.

## Verification

One runnable check, because the charter requires one and this is the only place
in the spec that names a command:

```
cargo test --workspace && bash tests/e2e/harness_parity.sh
```

The parity script is written in E6. For each harness present on the box it
drives one run that sets two roots, mentions a file in the second, records a
decision, asks a blocking question answered from the CLI, requests a secret, and
prints it — then asserts the cards exist, the answer is stored, and the secret's
value appears nowhere in the database. A harness that is not installed is
skipped by name, loudly, and never silently passed.

Expected: the workspace suite green, one pass line per installed harness, and a
final count of zero leaked secrets.

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

## Sanctioned fakes

- **Harness output fixtures in unit tests only** — canned streams per harness,
  the pattern the repo already uses for probes and tickers.
- **A fixture git repository** built by the test helper for lease tests.
- **A test-generated token** for the redaction check. It is not a credential for
  anything.

Everything else: **None.** In particular no fake GitHub CLI, no fake MCP client
in the end-to-end path, and no simulated harness in the parity script — an absent
harness is skipped by name, never stood in for.

## Escalate on

Stop and ask when the work touches any of these; decide everything else and log
it below.

- irreversible or externally-visible actions — opening a pull request, pushing a
  branch, removing a worktree with uncommitted work in it
- data migrations, deletion, money — deleting a conversation or a work is a hard
  delete. Its refusal list must not be widened, its repeat-confirmation must not
  be weakened into a flag, and deletion must never grow the power to remove a
  worktree that is dirty or unmerged
- auth, permissions, secrets — any change to where a secret is written, what is
  redacted, or what reaches the model
- public contracts — the spawn request, the MCP tool set, the HTTP routes
- **a harness that turns out not to support a seam this spec assumes** — roots,
  MCP, or command expansion. Record the measurement and ask before designing
  around it
- **anything that would make the orchestrator block** — that is the property the
  whole design exists to protect
- a capability or dependency that isn't present in the environment

## Answered, and what each answer changed

Settled with Reljod; recorded because two of them changed the design rather than
just confirming it.

1. **Worktree on delegation, or on first write?** → **On first write.** This is
   the answer that changed the most: a session now starts in your real checkout
   rather than a copy of it, and D5 grew a *claim* step, a read-only root flag
   (E1.S1) and a backstop watcher (E4.S3b). The reason it costs that much is that
   "first write" has no harness-agnostic detector, so the claim has to be
   something the agent does rather than something Jod notices.
2. **Does the original checkout stay visible, read-only?** → **Yes.** It stays in
   the session's roots, readable and mentionable, so a session can diff against
   what you are editing. It pairs with the answer above: read the real thing,
   write on a branch.
3. **Secret scope default.** → **Work**, so a key given for one project is not
   handed to every session on the box.
4. **Rail on the left permanently, or a third column?** → **Default**: a left
   column, toggled, auto-opening once on the first blocker, and replaced by a
   one-line summary on a narrow terminal.

## Settled: three lanes

Three `jod` sessions, stable owners, four waves. Recorded here rather than left
open because it changed the plan's shape: at three lanes one owner holds the
whole terminal, so the shared-file coordination protocol the four-lane version
needed was deleted rather than adapted. See *Why three, and not four*.

Nothing else in the spec is waiting on an answer.

## Decision log

Filled in during execution, not now. One line per decision made without asking,
with a confidence marker so review can read only the shaky ones.

| Decision | Why | Confidence |
|---|---|---|
| | | |
