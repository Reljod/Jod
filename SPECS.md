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
   @some-repo, do X" opens a *work*: a titled group of sessions, each pointed at a
   git worktree it owns, none able to touch the original checkout. The
   orchestrator never blocks; it delegates and comes back.
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
| **root** | A directory a conversation may read. A conversation has zero or more. |
| **work** | One intent, spanning several conversations. Titled and summarised by a throwaway model call. Owns a colour. |
| **project-session** | A conversation belonging to a work. Not a new type — an existing conversation with a work attached. |
| **card** | One row in the left rail: a decision, a question, or a secret request. |
| **lease** | A git worktree bound to one conversation, tracked so siblings can reuse it. |
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

| Area | What changes | Owned in |
|---|---|---|
| The store's schema and queries | New tables for roots, cards, works, leases, pull requests, discovered commands; new columns tying a conversation to a work and a parent | Wave 0, then each epic's own lane |
| New core modules | Roots, cards, secrets, works, leases, command discovery, pull requests — one module each, no shared file | Their epic's lane |
| The supervisor | Applies the injected environment, scrubs secrets from both output streams | Lane C only |
| The spawn contract | Carries roots and environment pairs through to the harness | Wave 0 |
| The MCP server | Three card tools, plus opening a work and listing roots | Lanes B and E4's lane |
| The orchestrator | Preambles rewritten; the router learns to open a work | E4's lane, then wave 4 |
| The conversation store | Deleting a conversation, which does not exist today | E4's lane |
| **New TUI modules** — rail, picker, tree | One per surface, so three lanes never share a file | Lanes A, B and E5's lane |
| **Shared TUI files** — mode switch, renderer, keymap, app state | Registration only: a key, a workspace entry, a draw call | **One wiring task per wave, one owner** |
| The CLI | Root, card, secret, work and session-delete subcommands | Their epic's lane |
| Docs — decisions, system design, harness config, README | The seven decisions, the new concepts, the measured support matrix | Wave 4 |

The bolded pair is the whole coordination problem; see *The real constraint is
not the epics* below.

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

**D5 — a delegated session works in a worktree it leases, never in the root.**
The original checkout is not among the session's roots, so the mention picker
cannot reach it. Leases are per work-and-repo and reusable: a second session on
the same repo in the same work is offered the existing lease before a new one is
cut.

**D6 — the titler is a throwaway conversation that is then deleted.** Cheap
model, one turn, then removed. This is why deleting a conversation is in scope at
all.

**D7 — repo slash commands are forwarded, not reimplemented.** Jod sends the
command line through to harnesses that expand it themselves, and inlines the
command's text for those that do not. Which harnesses do which is *measured*
before the code is written, not assumed.

---

# The six epics

Each `Sn` is a shippable slice with its own check and its own PR.

## E1 — Roots, mentions and ripgrep

- **E1.S1 Roots exist.** A conversation owns an ordered set of directories.
  Existing conversations keep the directory they already had. Add, remove, list,
  and a containment test the other epics use.
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

**Check:** a rendered frame showing three cards, one bordered `blocked`, the
answered one hidden until toggled.

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
  then the conversation is deleted. Deleting a conversation is new here, and
  refuses the pinned main chat. A titler outage falls back to the first few words
  of the instruction rather than blocking the work.
- **E4.S3 Worktree leases.** On delegation, a branch and worktree are cut and
  recorded; the session's roots become the worktree alone. Leases are reusable
  within a work. Releasing removes the tree only when it is clean and merged,
  and otherwise keeps it and says why. A non-git root is handled by a card, not a
  crash.
- **E4.S4 The orchestrator opens works.** The preamble is rewritten around the
  new vocabulary; a new routing decision opens a work, titles it, leases a tree
  and spawns the first session, returning as soon as it is spawned. Sessions may
  spawn their own children, which is what makes the tree deeper than two levels.
- **E4.S5 Cascading cards.** Card queries gain subtree scope; the main rail shows
  every descendant's cards, tinted by work; cascade is upward only; every card
  names the session it came from so an answer never lands on the wrong agent.

**Check:** one instruction naming a folder produces a titled work, a session, a
lease on a fresh branch, and a printed two-level tree.

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
- **E5.S4 Summaries.** The newest message or tool call as the node's summary — no
  extra model call — refreshed on the existing tick, off the render path.

**Check:** a rendered frame with two works, four sessions, one expanded run, and
a blocked count in the gutter; navigation asserted by test.

## E6 — Parity: prompts, commands, pull requests

- **E6.S1 Preambles.** One worker preamble naming the roots, the secret names and
  the card tools; skills and the charter pointed at under every root; the body
  asserted identical across harnesses except for documented per-harness lines.
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

---

# Parallelisation

The epics are **not** a queue. Below is what actually blocks what, and how to
run five or six sessions without them colliding.

## What forces order

Only four hard dependencies exist. Everything else is schedule, not logic.

```
        ┌──────────────────────────── W0 ────────────────────────────┐
        │  contracts: table shapes · query names · spawn fields       │
        └───────┬────────────┬────────────┬───────────┬───────────────┘
                │            │            │           │
        ┌───────▼──┐  ┌──────▼──────┐  ┌──▼───────┐  ┌▼────────────┐
   W1   │ E1 roots │  │ E2 cards    │  │ E3.S1–3  │  │ E6.S2 probe │
        │ + picker │  │ + rail      │  │ secrets  │  │ + discovery │
        └───────┬──┘  └──┬───────┬──┘  └──┬───────┘  └─────────────┘
                │        │       │        │
                │        │       └────────▼──────┐
                │        │            ┌──────────▼─┐
   W2           │        │            │ E3.S4–5    │   rail flow needs the rail
                ▼        │            └────────────┘
        ┌────────────────▼─┐
        │ E4 works+leases  │   leases rebind roots → needs E1.S1
        └───────┬──────────┘
                │
   W3    ┌──────▼─────┐   ┌──────────────┐
        │ E5 tree     │   │ E6.S3 PRs    │   both need E4's leases/tree
        └─────────────┘   └──────────────┘

   W4    E6.S1 preambles · E6.S4 docs   ← last, because they describe the rest
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

This exists because the alternative is four lanes each inventing a card table.
It is the only genuinely serial work in the spec, and it is small. Skip it and
wave 1 spends its time in merge conflicts instead.

## Wave 1 — four lanes, no overlap

| Lane | Owns | Does not touch |
|---|---|---|
| **A · roots + picker** | E1 | the rail, works, the supervisor |
| **B · cards + rail** | E2 | roots, works, the supervisor |
| **C · secrets** | E3.S1–S3 | any TUI file |
| **D · commands probe** | E6.S2 | everything else |

Lane D is deliberately first rather than last. It is a *measurement* — does each
binary expand its own slash commands — and its answer deletes or keeps a branch
of E6's design. Measuring it in wave 1 costs an hour; discovering it in wave 4
costs a redesign.

Lane C is the other one worth starting early despite shipping late: redaction
touches the supervisor, which nothing else in this spec touches, so it is the
one lane that can run to completion without ever waiting.

## Wave 2 — two lanes

Lane A folds into **E4** (works and leases) as soon as roots land, because the
same person now holds the root model in their head. Lane B finishes the rail and
picks up **E3.S4–S5**, the secret flow, for the same reason.

Lanes C and D are done and their owners move into wave 3 early.

## Wave 3 — two lanes

**E5** (the tree) and **E6.S3** (pull requests) are independent of each other and
both unblocked by E4. This is the widest point in the plan and the moment to add
a session rather than earlier.

## Wave 4 — one lane

**E6.S1** and **E6.S4** — preambles and documentation — last, because they
describe what the other five epics turned out to be. Writing them earlier means
writing them twice.

## The real constraint is not the epics

It is four shared TUI files: the mode switch, the renderer, the keymap and the
app state. Three of wave 1's four lanes want to edit all four.

**One owner per path, per the charter.** So:

- Every new surface — rail, picker, tree — is **its own module**, written by its
  lane and touched by nobody else.
- The shared files get **one wiring task per wave**, owned by a single lane, that
  registers whatever that wave produced: the key, the workspace entry, the draw
  call. It is small, it is mechanical, and it is the only thing that ever
  conflicts.
- Wave 1's wiring belongs to lane B, because the rail is the most invasive of the
  three.

If two lanes are editing the renderer at once, the plan has already failed —
stop and reassign rather than resolving conflicts.

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
  an MCP server. That is what lane D's probe is for, and the probe should be
  widened to cover both if it is cheap.

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
- data migrations, deletion, money — deleting a conversation is a hard delete and
  its refusal list must not be widened without asking
- auth, permissions, secrets — any change to where a secret is written, what is
  redacted, or what reaches the model
- public contracts — the spawn request, the MCP tool set, the HTTP routes
- **a harness that turns out not to support a seam this spec assumes** — roots,
  MCP, or command expansion. Record the measurement and ask before designing
  around it
- **anything that would make the orchestrator block** — that is the property the
  whole design exists to protect
- a capability or dependency that isn't present in the environment

## Open questions

Answers change the work; each has a default, so nothing is blocked on them.

1. **Worktree on delegation, or on first write?** Default: **on delegation** —
   "once it writes" means discovering the boundary at the moment it is crossed.
2. **Does the original checkout stay visible, read-only?** Default: **no**, it
   leaves the session's roots entirely, per your wording. The cost is that a
   session cannot diff against the checkout you are editing.
3. **Secret scope default.** Default: **work**, so a key given for one project is
   not handed to every session on the box.
4. **Rail on the left permanently, or a third column that steals from chat?**
   Default: a left column, toggled, auto-opening once on the first blocker, and
   replaced by a one-line summary on a narrow terminal.
5. **How many lanes do you actually want to run?** The plan is built for four in
   wave 1 and two in waves 2–3. It degrades cleanly to two lanes throughout —
   roughly double the wall clock, none of the coordination.

## Decision log

Filled in during execution, not now. One line per decision made without asking,
with a confidence marker so review can read only the shaky ones.

| Decision | Why | Confidence |
|---|---|---|
| | | |
