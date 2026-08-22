# The TUI itself

How this was tested: the binary built from this branch, running inside tmux on
its own socket, under a throwaway `JOD_HOME`, driven by sending real keystrokes
and capturing the pane. Terminal sizes 200x50, 110x42 and 40x20.

The headline is that the console is in good shape. Ten scenarios were run and
most passed cleanly, including the ones most likely to break. The findings below
are small; none of them is the bug Reljod reported. That bug is in
[`00-launch-and-roots.md`](00-launch-and-roots.md) and
[`01-routing.md`](01-routing.md).

---

## T1. Text is lost when a double-width character sits near the wrap column
Status: **fixed — merged as #158** · Severity was: low

> **My hypothesis in this task was wrong, and the fix's measurement says how.**
> I wrote that it "looks like a miscount only when the wide character's cells
> straddle the boundary", making it an off-by-one rather than a missing
> wide-character case — and I reasoned that from the emoji twin passing.
>
> Measured: **every row containing a wide character overflows**, not only a
> straddling one. The twin that "passed" passed because the single clipped
> column happened to hold a space, so nothing visible was lost. I had taken an
> invisible failure as a passing case and built a theory on the difference
> between them.
>
> The lesson is narrower than "reasoning is unreliable" and worth stating
> exactly: **a passing case that differs from a failing one by a space is not a
> contrast, it is the same failure with nothing in the gap.** Where a test's
> pass depends on which character landed in a clipped column, the character has
> to be part of what is asserted.

At 40 columns, a line containing Japanese characters lost three characters at
the wrap:

```
typed:  AAAA BBBB CCCC 日本語 DDDD EEEE FFFF
shown:  › AAAA BBBB CCCC 日本語 DDDD EEEE
          F
```

`FFFF` became `F`. An earlier run lost five characters the same way, from
`tanong: anong ginagawa? 日本語 🚀 café` — the rocket and the `ca` of `café`
both disappeared, at the same terminal width.

What stops this being a confirmed finding is that the obvious twin passed. The
same length of text with one emoji instead of CJK wrapped correctly and lost
nothing:

```
typed:  AAAA BBBB CCCC 🚀 DDDD EEEE FFFF GGGG
shown:  › AAAA BBBB CCCC 🚀 DDDD EEEE FFFF
          GGGG
```

So it is not simply "wide characters break". It looks like a miscount only when
the wide character's cells straddle the boundary, which would make it an
off-by-one in the width accounting rather than a missing wide-character case.
Plain ASCII wraps correctly at the same width, and tmux renders the identical
string correctly on its own, so the terminal is not the cause.

Before fixing: print what the composer thinks the string's display width is at
the moment it wraps. `cli/src/tui/text.rs` is where the wrapping lives.

Check: a property test over the composer's wrapper asserting that the wrapped
lines rejoin to exactly the input, for a mix of ASCII, CJK and emoji at every
width from 20 to 120.

---

## What this pass fixed

Thirty-six changes, each with a test that fails without it. Driven by running the
console against real harnesses under a throwaway `JOD_HOME`, on four projects,
and then re-run end to end on an empty install.

**Three found by running the fleet rather than reading it, and the worst of the pass.**

- **You could not stop an agent a manager started.** `Jod::agents` reads an
  in-process map; `rehydrate` filled it once at launch and was never called
  again. Every engineer a manager hires is spawned by an MCP server in another
  process, so those runs were in the tree — built from SQL — and absent from the
  agent list, which is not. `App::selected_agent` resolves a session row through
  that list, so it answered `None` for a row visibly spinning and **every** run
  verb refused: `s`, `r`, `a`, `d` and the thread keys. In the manager design
  that is nearly the whole fleet, and a runaway agent could not be stopped from
  the screen built to stop it. Proved with the store as referee: same row, same
  running run, `s` before → "nothing running on it", `s` after → "killed after
  6s", and the run gone from the running list.
- **Every agent lost its Jod tools after an upgrade.** `std::env::current_exe`
  reads `/proc/self/exe`, and once that file is replaced Linux returns the old
  path with ` (deleted)` appended. It went straight into each agent's MCP config
  as the command to run: `command: ".../jod (deleted)"`, which nothing can
  execute. `/update` and `/upgrade` replace the binary while the console keeps
  running, so this is the ordinary upgrade path — and it hits anyone running
  from a checkout they rebuild, which is how it was found. Filed as T5 is the
  half that remains: the console says nothing when it happens.
- **`/main <instruction>` moved the chat and promised not to.** Its help said
  "send it one instruction and stay where you are"; `orchestrate` re-asserts the
  binding to main so the reply can be watched here. Harmless from main, and a
  silent move when typed inside a manager. The promise was corrected rather than
  the behaviour, because watching the answer is the point of the command.

**One fault, six times over: when a conversation is superseded, what points at it is left behind.**

Main compacts itself when its context fills. `continue_as_new` opens a fresh
conversation and moves the pin onto it — correctly; the alternative strands the
summary. Five other things named the old conversation and none of them moved.

| What pointed at it | What broke |
|---|---|
| `conversations.pinned` | already handled |
| `team_members.conversation_id` | mail to `main` stranded for ever |
| `projects.manager_conversation_id` | a manager's harness switch silently undone |
| `conversations.parent_conversation_id` | **Reljod's rail emptied** |
| `pending_deliveries.conversation_id` | an answer he had given was owed to a dead thread |
| `cards.conversation_id` | **a question put to him dropped off the rail** |

The last one is the worst, and it **undid a fix made earlier in this same
pass**. Hanging a manager under main is what makes its answers reach the rail;
that was fixed, verified live — 0 cards to 2 — and then compaction reached
around it. Observed an hour later as a fleet reading `alpha [3 cards]` and
`gamma [8 cards]` beside a rail reading "nothing waiting — no agent has asked
anything". After the fix and its backfill, the same store read `rail · 14 open`.

All six now move in the one transaction that moves the pin, and migrations
`0025` to `0028` repair the stores that have already compacted — which is every
store that has been up long enough.

**The line the fix holds is worth stating, because it decides each case.**
Anything still *owed* follows the chat: mail not yet delivered, an answer not
yet injected, a question not yet answered. Anything that already *happened*
stays where it happened: a delivered message, a settled card, the resolution
log. Move the second kind and the record starts lying about which conversation
did what, which is the one thing these tables exist to be honest about.

The last two were found late and separately — the deliveries by auditing every
foreign key into `conversations` after the fourth instance, and the cards by
sitting on the rail pressing `t` and `f` and noticing the answered stack was
empty when it should not have been. Neither would have turned up from reading
the compaction code, because neither is *in* it.

**The lesson is narrower than "test your fixes" and worth stating exactly.** Fix
2 was verified, live, and was genuinely correct when verified. It was still
wrong within the hour, because a *scheduled background operation* reached around
it. Verifying a change at the moment you make it does not tell you it survives
the system's own housekeeping — and the housekeeping here runs on a timer
nobody presses.

**Two more the daemon found, which nothing in the console showed.**

- **Compacting the main chat stranded every bus pointed at it, permanently.**
  `is_main_chat_member` compares a member row against the *currently pinned*
  conversation, and `continue_as_new` — what runs when main's context fills —
  opens a fresh conversation and moves the pin onto it. Every team joined before
  that kept naming the old one, so `main` matched nothing: mail was never handed
  to the chat, fell through to a wake that cannot happen, and waited for ever.
  The daemon said so once a second, indefinitely: *"1 message(s) waiting:
  `main` has no session to resume"*. Main compacts itself, so every console that
  stays up long enough gets there. `carry_forward` moves the rows now, and
  migration `0025` repairs the consoles that have already compacted — verified
  on the live store, where four stranded items were then delivered in one tick.
- **The `(deleted)` binary.** See above; found while chasing the same log.

**A decision that was made and never enforced.**

- **`continue_agent` accepted a stalled run.** Reljod's decision was that a
  stalled session is marked and surfaced, never killed, and that the router
  treats it as **not-continuable** — say so, start a fresh session beside it,
  and leave the wedged one for him to stop. Both preambles say it. Nothing
  checked. Found by wedging alpha's real engineer — live process group,
  heartbeat silent for forty minutes, the daemon maintaining the mark — and
  giving alpha another instruction: the manager called `continue_agent` on the
  stalled run and the tool allowed it. It does not resume the stuck process; it
  starts a *second* one on the same session, leaving the wedged one running and
  unnoticed, which is the state the mark exists to end. Refused at the boundary
  now, on the precedent set in the same file for `open_work` from main:
  *"prompt wording is not enforcement"*.

**The chain of command.**

- **A manager's card never reached Reljod.** `ask_manager` promises "It will
  raise a card on your rail" and the manager preamble calls a card "the only way
  your answer reaches him". Cards cascade on `parent_conversation_id`; an
  engineer is hung under its manager, and a manager was hung under nothing. The
  rail read `0 open` beside a finished piece of work. `Store::manager_conversation`
  sets the parent now, and migration `0024` backfills the managers that exist.
- **The roster offered main and managers as engineers.** `is_free` was
  `completed && session_id.is_some()`, which every router satisfies, so
  `list_agents` answered a manager looking for a worker with *"`run-manager` is
  free … Prefer it for any instruction here"*. `Store::router_run_ids` excludes
  them. One live session declined that advice repeatedly by reasoning past its
  own tool, which is not a safety mechanism.
- **A manager was asked where its own repository was.** `open_work` defaulted
  the checkout to the caller's first root, and a manager has none — so its first
  call was refused and it spent a model turn supplying a path the store held.

**The fleet.**

- **The cursor sat one row above the row every key acted on.** `tree_rows` drops
  the sentinel pinned row once the forest carries its own `Main` node; the pane
  drew it anyway, so ids and rows disagreed by one. `x` on what looked like
  Jod's row untracked the project under it.
- **A closed work was drawn under an unrelated repository.** `fleet(All)`
  appends a second forest, so closed rows landed after the last live row. The
  parent was right and the *position* was not, and everything the renderer does
  — guides, indent, what a collapsed row hides — is positional.
- **A collapsed project spun while its only agent was wedged.** `stalled_for_ms`
  is deliberately null on group rows, so the badge existed and you had to
  already suspect something to expand far enough to see it. The count rolls up
  now, like open cards.
- **Jod's own errands crowded out the agents.** Titlers and compactions write
  into no conversation, so they fell into the pane for runs belonging to no
  work; five of six rows were housekeeping.
- **Nothing said where an agent was writing.** A work session reads the checkout
  and writes to a worktree it claimed, so it reports a file changed while the
  checkout on screen is untouched. The session row names its branch and
  worktree.
- **Nothing said when no daemon was watching for stalls.** Said once into the
  transcript, and never on the screen where every wedged agent draws as healthy.
- **The caption contradicted the screen**, reading "nothing delegated yet" under
  a tree of projects and works whose agents had all finished.

**Projects.**

- **Two checkouts sharing a directory name were unreachable.** Both answer to
  one name, `projects_by_name` refuses two matches, and the catalog printed
  `web, web`. The router correctly asked which was meant and then no answer
  worked — including two attempts that named the full path. A path is unique by
  construction and is now accepted, by `ask_manager`, `project_switch` and the
  CLI; the catalog panel qualifies a shared name with its parent directory.
- **Untracking offered an undo that refused to run** — `jod project restore web`
  against two `web`s, with "Name the one you mean exactly" as the advice.

**The catalog, and the smaller things a person meets at a terminal.**

- **A checkout that had been deleted looked exactly like a healthy one.** The
  panel is where a project is chosen, and choosing one whose directory is gone
  routes an instruction into a directory that cannot be entered — surfacing as
  the supervisor reporting `could not start ".../claude": No such file or
  directory`, which reads as the harness missing from the machine.
  `Project::path_trouble` had said so since it was written and `jod project ls`
  prints the whole sentence; this screen printed the name. It says
  `· missing` now.
- **A popup merged into the transcript's border.** Both the `@` picker and the
  `/` palette were anchored so their bottom border landed on the transcript's,
  and `Clear` covers only the popup's width, so the two joined into
  `└───└──popup──┘───┘`.
- **An empty state clipped into a different sentence** — "nothing remembered
  yet — /remember writes on" at a hundred columns. Wrong, and grammatical, so
  nothing about it looks truncated.
- **`/root` printed `ro`**, two letters nothing on screen explained, and the one
  fact on the line worth reading. `jod root ls` already spells it out.
- **A missing argument was answered with a synopsis**, `usage: /heartbeat <id>
  [off]` — the only telegraphic refusal in a console where every other one is a
  sentence.

**The console.**

- **Nothing said whether you were typing into main or into a manager.** Same
  banner, same composer, same status bar, and the transcript titled after the
  watched *run* — so a manager, which has no run of its own, was titled plainly
  `jod`. An instruction meant for main, typed into beta's manager, is not
  refused; it is carried out, in beta.
- **An active filter was invisible on the fleet.** `filter_line` was drawn only
  by the flat list, and the fleet always has a tree, so rows vanished with
  nothing saying a filter was on.
- **Escape could not put the command palette away.** It is derived from the
  input rather than stored, so there was nothing to close and it fell through to
  `back()`.
- **`/resume <typo>` reported success** on an empty fleet, where it is most
  likely to be a typo and least likely to be a match.
- **The workspace menu's digit hints did nothing**, on the one menu you can only
  read from the state where they were false, and its `new…` row listed four of
  five kinds.
- **The keymap promised a confirmation on a key that does not confirm.** The
  fleet's `x` untracks immediately and reversibly, deliberately; the shared
  spine row said "delete — confirms first". The promise moved to the four
  screens that keep it.
- **A short terminal dropped ten menu entries in silence**, while the `?`
  overlay in the same situation says how many it is hiding.
- **`Tab` and `@` were documented nowhere**, though the side panel says "Tab
  cycles" and `@` is the only route to the file picker.

Three notes on method, because each changed an outcome.

- **Every fix here was checked by reverting it and re-running its test.** Two
  tests passed with their fix removed and were checking nothing: one had a
  precedence mistake that made the condition always false, and one built its
  fixture by hand without the serialised summary the service reads back, so the
  runs were invisible to the call under test. Neither would have been caught by
  a green suite.
- **Finding T1's lesson repeated itself.** The closed-work bug reproduced only
  once the assertion moved from `parent` — which was correct — to *position*,
  which is what the renderer actually reads. A test asserting the plausible
  field passed while the screen was visibly wrong.
- **A fix was written and reverted** rather than weakening the test it broke.
  See T2.
- **Three times a hand-built fixture said something false**, and the repo's own
  index already warns about this — "an unfaithful fixture produced a false
  failure". Twice a row inserted straight into `runs` was invisible to the code
  under test because its `summary` was not what `AgentSummary` serialises to
  (`"harness": "ClaudeCode"` where the real writer emits `"claude_code"`), and
  once a test drove `Store::compact`, which compacts in place, while the console
  actually runs `continue_as_new`, which does not. Each looked like a finding
  for a few minutes. **Build the fixture with the writer the product uses**, or
  the test is about the fixture.

---

## T6. The composer breaks words in half rather than wrapping at spaces
Status: **open — deliberately not attempted** · Severity: low · Owner: —

At any width, a prompt longer than one line splits mid-word:

```
› please refactor the authentication middleware so that it validates tokens before t
  ouching the database
```

`wrap_composer` (`cli/src/tui/ui.rs`) counts columns and breaks at the field
edge. Word wrapping means backing up to the last space before that edge, which
is a dozen lines of change.

**Left alone on purpose, and the reason is the point.** This is the same
function as [T1](#t1-text-is-lost-when-a-double-width-character-sits-near-the-wrap-column)
— the one place in this repo that has already lost characters at a wrap column,
where the fix's own note records that a passing case differed from a failing one
only by which character landed in the clipped column. The caret position is
computed in the same pass and feeds the `@` picker's anchor, so a word-wrap
change moves both. The payoff is that a sentence being typed looks tidier.

That is a bad trade to make in passing. It is a fine trade to make deliberately,
and whoever does has a good spec waiting: the property test
`the_wrapped_rows_rejoin_to_what_was_typed_at_every_width` already asserts the
rows rejoin to exactly what was typed, for ASCII, CJK and emoji at every width
from 20 to 120. Keep the space at the end of the line it broke after, hard-break
any token longer than the field, and that test stays honest.

Check: the existing property test, plus one asserting no row ends mid-token when
a space was available.

---

## T5. A session with no Jod tools looks exactly like one that has them
Status: **open — a feature, not a fix** · Severity: medium · Owner: —

Found while chasing the `(deleted)` bug below. When the `jod` MCP server fails
to start, the agent keeps its model and loses every Jod tool — `ask_manager`,
`open_work`, `list_agents`, the lot. It can still hold a conversation, so it
answers normally and sounds fine. Main spent two turns explaining, correctly and
entirely on its own initiative, that it could not route anything:

> Separately, it's moot right now: the Jod MCP server dropped a couple of turns
> ago, so `open_work`, `ask_manager` and the rest are all unavailable until it
> reconnects.

**Nothing on any screen said so.** The status bar, the fleet and the rail all
looked ordinary. The only reason anybody knew is that the model volunteered it,
and a less forthcoming one would have left the console silently unable to
delegate.

The cause behind *that* instance is fixed — see the note below — but the
invisibility is the general problem, and any MCP failure reproduces it.

What it needs: the supervisor sees the harness's stderr, which is where a
failed MCP server says so. A run whose `jod` server did not start should raise a
card and mark its row, the way a stall does. Worth doing at the same time as
whatever answers "how do we know an agent is healthy" generally.

Check: start a run with a deliberately broken `jod` command in its `mcp.json`
and assert a card is raised naming the run.

---

## T2. An overlay paints over the keybar and the status line
Status: **open — needs a decision from Reljod** · Severity: low · Owner: —

At 80x24 the `?` keymap draws through the bottom two rows, leaving the status
line reading `● auto · Claud` with a box border painted across the middle of it.
At 60x20 and 100x10 both chrome rows disappear entirely while the overlay is up.
Those two lines are how you know which permission mode you are in and which keys
are live, and a half-covered line reads as a rendering fault rather than as an
overlay.

**A fix was written and then reverted, which is why this is a decision and not a
task.** Reserving the two chrome rows inside `centred` (`cli/src/tui/ui.rs`)
makes every overlay two rows shorter, and that breaks a guarantee this repo
documents and tests: `keys.rs` promises the keymap is *complete* at 100x30, held
by `the_keymap_overlay_is_complete_at_the_design_size`. Two rows of chrome cost
two bindings, so the two cannot both hold at that size.

The same question decides two more symptoms, which is why these are one finding
rather than three. A notice toast is drawn over the bottom of the workspace, so
the pane it covers is left with no bottom border —

```
┌ loose · 3 ──────────────┐
│   ✓ 2e576bb0 completed  │
┌ notice ─────────────────┐
```

— and reserving rows for it instead would make the fleet re-window every time a
key guard fires, which is worse than a transient open box. And: on an empty console the splash's caption line shows either side
of the overlay, cut mid-word —

```
jod · a│  Ctrl-N       the cards — and away again          │y screen
```

Structured content behind a modal reads fine; a sentence sliced in half reads as
a rendering fault. Whatever answers "what may an overlay cover" answers both.

Three ways out, none of them obviously right:

1. Keep the overlap and accept it, on the grounds that the keymap closes on any
   key. The corrupted-looking line stays.
2. Reserve the chrome and drop the 100x30 completeness guarantee to, say,
   100x32. Costs nothing at the sizes people use and weakens a documented
   promise.
3. Make an overlay that would otherwise overlap the chrome full width, so the
   status line is covered cleanly rather than half covered. Fixes the
   appearance without losing a row, and is a visual change across every overlay.

Reljod picks. Weakening the existing test to make a fix pass is the one thing
that must not happen here.

Check: a render test asserting the last two lines of the frame carry no box
characters at 80x24, 60x20 and 110x42 — together with the existing completeness
test, which must stay green.

---

## T3. A follow-up while an engineer is running forks a second branch
Status: **open — needs a decision from Reljod** · Severity: medium · Owner: —

Observed by running it. `gamma` was given "add three new files a.txt b.txt and
c.txt", and while that engineer was still working a second instruction —
"actually also add d.txt" — was queued and then delivered. The manager saw its
only engineer was busy, and busy is not continuable, so it opened **new work**.
`gamma` ended with two worktrees on two branches, `create-a-txt-b-txt-and-c-txt`
and `add-a-txt-d-txt-to-gamma-repo`, both containing `a.txt`, `b.txt` and
`c.txt`, one of them also containing `d.txt`. Neither is merged. One intent
became two branches a person now has to reconcile.

Nothing here is a bug in the sense of code doing what it was not told to. The
manager followed its preamble, and the alternative it was refusing is worse:
`continue_agent` accepts a `Running` run and would have spawned a *second*
process resuming the same session id, two agents editing one worktree.

The real question is what a follow-up to a busy engineer should do, and it is
Reljod's: hold the instruction until the engineer's turn ends and then continue
it, or fork as it does now and say plainly on the fleet that two branches are
open on one repository.

Check: whichever answer is chosen, a test that drives two instructions at one
project with the first still running and asserts the number of held leases.

---

## Scenarios run

| # | Scenario | Expected | Actual | |
|---|---|---|---|---|
| 1 | Opening screen, 110x42 | banner, directory, composer, status bar | all four, and the directory is named | pass |
| 2 | Opening screen, 200x50 | same, centred | same | pass |
| 3 | `/` opens the command menu | a scrollable list of commands | 40-odd commands, each with one line of help | pass |
| 4 | `/root` | lists the console's roots | listed `tui-repo`, marked `ro` for read-only | pass |
| 5 | Ctrl-F, empty fleet | says nothing is running | status bar reads "nothing delegated yet" | pass |
| 6 | Ctrl-G | the menu of every screen | full menu, with "Esc cancels · any other key is ignored" | pass |
| 7 | Escape from a screen | back to the chat | back to the chat | pass |
| 8 | A 160-word instruction in the composer | wraps, does not truncate | wrapped across six lines, all present | pass |
| 9 | Unicode and emoji at 110 columns | renders intact | `tanong: anong ginagawa? 日本語 🚀 café` intact | pass |
| 10 | The same at 40 columns | renders intact | lost `🚀 ca` | **fail — T1** |
| 11 | Resize 110 → 40 → 110 | reflows, keeps the text | reflowed; text restored intact at 110 | pass |
| 12 | ASCII wrapping at 40 columns | wraps losslessly | losslessly | pass |
| 13 | Emoji near the wrap column | wraps losslessly | losslessly | pass |
| 14 | CJK near the wrap column | wraps losslessly | lost three characters | **fail — T1** |
| 15 | A pane of nothing but emoji | wraps losslessly | 16 of 20 shown, rest wrapped out of view | inconclusive |

## T4. `←` out of a manager moves the screen but keeps the composer
Status: **open — needs a decision from Reljod** · Severity: low · Owner: —

`←` on an empty line in a manager calls `leave_manager`
(`cli/src/tui/mod.rs`), whose own comment says "a manager is something to
leave … the way back has to be the one key that already means *out*". What it
does is reveal the manager's row on the fleet and go to the fleet screen. It
does not unbind the conversation — so pressing `Esc` from there lands you back
in the manager's chat, and the next thing you type goes to that project.

Both readings are defensible, which is why this is a decision:

- **`←` means "show me where I am in the fleet".** Then the behaviour is right
  and the word *leave* in the comment is what is wrong.
- **`←` means "stop being in this manager".** Then the arm should return
  `Action::EnterMain` as well, so backing out of the fleet lands in main —
  which is what the same comment calls "home".

**This was invisible until this pass and is now merely wrong.** The composer's
box is titled `you → beta · manager`, so the state is on screen either way; the
trap of typing a main instruction into a project without knowing is gone. That
is why it is filed rather than fixed — the danger is handled, and the remaining
question is what the key should mean.

Check: press `←` then `Esc` in a manager and assert what `app.conversation`
holds, whichever answer is chosen.

---

## What an independent sweep of the fixed build found

A second agent, given the twelve claims and told to be adversarial, drove the
console on its own socket and store against a seeded fleet — five projects, six
works, sixteen runs, two same-named `web` repositories — at six terminal sizes
with live resizing between them. **All twelve did what they claimed.** Nothing
panicked or froze.

It also found one regression, and it was mine, in the most fitting place
available: the collapsed projects line added in this pass read `— Ctr` at every
width, dropping the keystroke the sentence exists to name — in the same pass
that added "ellipsise rather than clip" for the empty states two panes over.
`cut` alone did not save it; the sentence was thirty-five characters in a
thirty-two column panel, so it ellipsised the keystroke away regardless. The box
title already says "none set", so the line now carries the two facts the title
cannot: `▸ 5 catalogued · Ctrl-P`.

Two pre-existing faults it verified against `main` are fixed here as well.

- **The `/` palette painted over the right-hand rail** at every width wide
  enough for the rail to exist and too narrow for the chat to cover the palette
  — about ninety-four to a hundred and fifty columns. Its width was measured off
  the whole frame minus the box's left edge, which is not "the room to the right
  of the input box" when something sits beside the chat.
- **The fleet filter's count contradicted the screen**, reporting `0 match`
  beside four visibly matching rows. It counted `row_ids(Fleet)` — the flat
  *agent* list plus a sentinel — while the pane drew tree nodes.

The fleet not carrying the projects panel's same-name disambiguation is fixed
too — both now say `web in one` and `web in two`, because two screens naming one
thing differently is worse than either name.

One it verified is left open, and it belongs to T2's family rather than to
itself: the `Ctrl-P` panel takes a fixed thirty-two columns with no
minimum-width guard, so below about seventy it paints over the chat's border and
at forty leaves the chat two columns wide. The rail proper already handles this
— `rail_beside` moves it below the chat under `RAIL_BESIDE` — so the fix is to
give the panel the same treatment. It is filed with T2 because "what may cover
what, and what yields first, at a size where not everything fits" wants one
answer for the whole screen, and three different answers arrived at separately
is how a layout stops making sense.

**The fix that nearly went in wrong is worth the paragraph.** Bounding the
palette by the *composer* stops it covering the rail and re-cuts the longest
description at two hundred columns — the exact fault a previous author removed a
fixed 72-column cap to fix, held by a test named for it. It failed on the first
run. The honest bound was neither the frame nor the composer but the chat
column: the whole width when nothing is beside it, the chat's share when
something is.

## The compaction fix, verified by a compaction nobody asked for

The six-pointer fix above was unit-tested and then held through the real thing.
While a routing instruction was being sent, main's context reached 76%, it
compacted itself, and the pin moved to a fresh conversation — the exact event
that had emptied the rail an hour earlier. Afterwards:

- the rail read **`15 open`**, where the same event previously left
  `0 open · nothing waiting`;
- all four managers had followed the pin into the new conversation, in the
  transaction that moved it.

Worth recording because it is the only kind of evidence that settles this one.
A test proves the transaction does what it says; a compaction firing on its own
timer, on a store with four projects and fifteen open cards, proves nothing else
in the system undoes it.

## What a daemon left running found

Nothing, which is the point of recording it. Before the compaction fixes the
daemon emitted a stranded-message line **every tick, indefinitely**. After them
a fifteen-minute run produced three lines in total: the run count it reloaded,
a note that this `JOD_HOME` is not the installation harnesses should point at,
and one `[jod/prs]` line saying the scratch repositories have no git remote.
All three are correct and none repeats.

Two things were checked against the running daemon rather than against the
store alone:

- **The stall sweep marks and does not kill.** A run was given a real, live
  process group and a heartbeat silent for forty-five minutes. The sweep marked
  it within ten seconds and **left `runs.status` as `running`** — which is
  Change 1b's whole point, and had not been observed end to end before.
- **Rehydrating on every fleet refresh is affordable.** The console re-reads the
  store now, because a manager's engineer is started in another process. On a
  store seeded to 1500 runs it costs **2.0% CPU**, the same as on a store of 51:
  `rehydrate` is bounded at 200 rows and skips what it already holds, so the
  cost does not grow with the machine's history.

## Scenarios run — second pass

Four projects, real harnesses, a throwaway `JOD_HOME`, then the whole thing
again on an empty install. Passes are listed because a clean pass is what stops
the same ground being covered twice.

| # | Scenario | Expected | Actual | |
|---|---|---|---|---|
| 16 | Catalog three projects from the composer | each confirmed and listed | confirmed, and the rail names how to say each one | pass |
| 17 | An instruction naming a project | routed to that project's manager | manager started, and main said which and why | pass |
| 18 | A second instruction on the same project | the same manager, resumed | "Back to alpha's existing manager (resumed, not a fresh one)" | pass |
| 19 | A follow-up continuing earlier work | the same engineer, continued | one session, one lease, both edits in one worktree | pass |
| 20 | A new intent on a project with no free engineer | a new engineer | new work opened beside the finished one | pass |
| 21 | An instruction naming no project | the project carried over | "Routed to gamma (carried over — nothing in that named a project)" | pass |
| 21b | An instruction naming a project by alias | the alias wins over the ambiguous bare name | "in one web" reached `repos/one/web`, where `web` alone is two projects | pass |
| 22 | An instruction matching two projects | asked, never guessed | blocking question on the rail, with both paths and numbered options | pass |
| 23 | Answering that question | routed to the chosen one | refused — no answer could name either. **fixed** | fail |
| 24 | Enter on a project in the catalog | that project's manager | entered it — but nothing said which. **fixed** | fail |
| 25 | Typing while a turn is in flight | queued, sent after | "you · 1 queued", drained and routed correctly | pass |
| 26 | `x` on a project row | untracked, reversibly | untracked; the undo it offered refused to run. **fixed** | fail |
| 27 | Run verbs on a project and on the pinned row | an explanatory notice, no action | a flash naming what the row is; nothing acted on | pass |
| 28 | A stalled engineer, collapsed fleet | says stalled | the project spun instead. **fixed** | fail |
| 29 | The fleet with no daemon running | says nothing is watching | said it once in the transcript only. **fixed** | fail |
| 30 | Where a finished agent's change lives | named on screen | nowhere at all. **fixed** | fail |
| 31 | Whole chain on an empty install | catalogue, route, fix, card | all four, card on the rail, fix on its branch | pass |
| 32 | 200x50, 110x42, 80x24, 60x20, 40x20, 100x10 | degrades, never corrupts | degrades; overlays overlap the chrome — **T2** | mixed |
| 33 | Mixed CJK, emoji and accents at every width | no text lost | intact at all six widths — T1 stays fixed | pass |
| 34 | All nine workspaces by letter and by digit | open, draw, return | all open and return with both Esc and q | pass |

One thing worth naming that is not a finding. On scenario 31 the engineer
reported "Fixed. `widget.py:2` — `return a - b` → `return a + b`", and it was
telling the truth: the fix is real, on its branch, in its worktree. The checkout
the person is looking at still says `a - b`. That gap is the whole reason a row
now names its branch, and no test would have found it — only sitting in front of
the thing and asking where the change went.

Two notes on things that looked like bugs and were not:

- `/root` appears to print nothing at 110 columns. It does print; the output is
  above the visible region of the captured pane. The narrow-terminal run is
  what showed it, which is a reminder that an empty-looking capture is not an
  empty transcript.
- The fleet's two panes are empty boxes when nothing is running, with the empty
  state only in the status bar. That is a defensible choice rather than a bug,
  but a line inside the pane would read better than a blank box.
