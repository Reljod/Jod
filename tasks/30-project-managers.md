# Project managers — the catalog, and the manager that now exists

> **The manager work in Part 2 has shipped.** What is still live in this file is
> Part 1: findings about the project catalog itself, four of them fixed and one
> waiting on a decision from Reljod.
>
> Part 1 is material pull request #120 did not have — bugs found by running the
> catalog rather than by reading it. Part 2 broke the spec's Change 3 into
> claimable tasks; those tasks are done, and the record of what landed is at the
> end of [`docs/spec-ceo-and-managers.md`](../docs/spec-ceo-and-managers.md).
>
> Two things from #120 that anyone working near managers still needs:
>
> - **A manager must not use `pinned = 1`.** `Store::pinned_conversation` is a
>   `query_row` with no `LIMIT` and no ordering, so a second pinned row makes
>   "which conversation is main" depend on SQLite's row order, and Reljod's
>   instructions would start landing in a manager's transcript. Managers live on
>   `projects.manager_conversation_id`, and
>   `creating_a_manager_does_not_disturb_the_main_chat` holds it.
> - **Routing to a manager is already deterministic.** `settle_project` runs on
>   the raw instruction before the model turn, so `ask_manager` is wiring, not
>   reasoning. Anything treating the choice of manager as a judgement call is
>   overbuilt.

Tested by running the built binary (`target/debug/jod`) against two throwaway
`JOD_HOME`s (`/home/reljod/.claude/jobs/cd76af0f/tmp/jodhome-pm` and
`…/jodhome-pm2`), driving `jod project …` from the CLI, one live `jod main`
round trip against a real harness to observe `settle_project` fire for real,
and reading `~/.jod`'s data model was never touched — only my own throwaway
databases. Scratch repos live under
`/home/reljod/.claude/jobs/cd76af0f/tmp/pm-scratch/`. Everything below is
observed, not guessed, except where marked `needs confirming`.

Written when nothing implemented a manager, which is no longer true — the
findings below are about the **catalog** (`core/src/projects.rs`, table
`projects`, MCP tools `project_add`, `project_list`, `project_switch`,
`project_current`), which was real and mostly working then and still is. That is
what Part 1 is about, and it is unaffected by the manager work.

Already filed, not re-filed here: `jod project current` is missing from the
CLI (`tasks/00-launch-and-roots.md`, L5); `jod team ls` is spelled `list`
(L6); the main chat is a single database-wide row with `pinned = 1` and no
harness/channel key, so it is frozen at its first `cwd` (L1–L3). That last one
turns out to matter for this file too — see the note at the end of Part 1.

---

## Part 1 — bugs in the project catalog that exists today

### P1. Re-cataloguing a path silently wipes its aliases and notes
Status: **fixed — merged as #123** · Severity was: high

Kept here because the fix carries a correction worth reading before anyone
touches a similar upsert.

**Two readers told the fixing agent to copy the `COALESCE` that protects
`remote`. That advice was wrong, and only running it caught why.** `COALESCE`
returns the first non-NULL argument, and `aliases` and `notes` default to an
empty *list* and an empty *string*, never NULL. So a `COALESCE` would never
fire and the wipe would have continued — with a fix in place, tests written,
and everyone satisfied. The agent found it and used `CASE` instead.

The shape of the advice was right and the mechanism was wrong. The general form
of the trap: **`COALESCE` only protects columns whose "absent" value is NULL.**
For anything defaulting to an empty string, empty list, or `0`, it is a no-op
that looks like a guard. See L7 in
[`00-launch-and-roots.md`](00-launch-and-roots.md), which is the same class of
bug on a boolean column and where `COALESCE` would fail the same way.

The symlink route was confirmed rather than argued: adding a project through a
symlink that resolves to an already-catalogued directory produced **two** rows,
not three — the link landed on the real directory's row and emptied it.

The doc comment was corrected too. It claimed re-adding "extends" an alias set;
it now says it replaces when you supply one and keeps when you do not, and
notes that the path is canonicalised so a symlink updates what it resolves to.

`Store::add_project`'s doc comment says re-adding a path "is also how you
rename a project or extend its alias set" — but the SQL does not extend
anything.

Cause: `core/src/projects.rs:463-472`

```sql
ON CONFLICT(path) DO UPDATE SET
  name    = excluded.name,
  remote  = COALESCE(excluded.remote, projects.remote),
  aliases = excluded.aliases,
  notes   = excluded.notes
```

`remote` is protected with `COALESCE` so a second `add` that doesn't mention
it keeps the old value. `aliases` and `notes` get no such protection — they
are overwritten unconditionally, even when the second call left them at their
empty defaults.

Observed:

```
$ jod project add .../alpha --alias "the game" --notes "test project alpha"
alpha · .../alpha · also called: the game · test project alpha

$ jod project add .../alpha --name "Alpha Prime" --alias "the second alias"
Alpha Prime · .../alpha · also called: the second alias
# "the game" alias is gone, and notes are now "" — neither was mentioned
# in the second call, and the doc comment says extend, not replace.
```

Same wipe happens through a completely different path: adding a **symlink**
that resolves to an already-catalogued directory hits the same
`ON CONFLICT(path)` branch (see P-note under the symlink scenario below) and
wipes the same way.

Fix: give `aliases` and `notes` the same `COALESCE`-style treatment as
`remote` — an empty aliases list / empty notes string on the incoming call
should mean "not mentioned," not "clear it." If wiping outright is ever
wanted, that should be an explicit action, not the side effect of renaming.

Check: add a project with an alias and a note, re-add the same path with only
`--name` changed, assert the alias and note both survive.

---

### P2. Cataloguing a file, not a directory, is accepted with no complaint
Status: **verified fixed — merged as #136, check run against main, passes** · Severity was: medium

```
$ jod project add /tmp/jod-plainfile-657189.txt
Error: `/tmp/jod-plainfile-657189.txt` is a file, not a directory. A project is
a checkout a session gets started in, so catalogue `/tmp` instead — or, if that
is not the repository you meant, the checkout that is.
```

Refused rather than flagged, and the message names both the likely intent and
the alternative.

**What fixing it turned up, which was worse than the finding.** A catalogued
path pointing at a *file* is not a politeness problem. `open_work` succeeds and
reports "opened and running"; the failure lands one process later as
`could not start "/home/reljod/.local/bin/claude": Not a directory` — naming the
harness binary and never the project or the path, so it reads as Claude Code
being broken. And `claim_worktree` raised a card saying the path "is not inside
a git repository", which was demonstrably false: the file was inside a real one.

Two lessons outlive the fix. An error that names the wrong subject sends the
reader to the wrong codebase, and a diagnostic that states something false is
worse than one that says nothing. It also found that a *missing* path was never
refused either — which the task had guessed at rather than tested, so the guess
was under-stated rather than wrong.

`add_project` never checks that the path exists or is a directory.

Observed:

```
$ jod project add .../filetarget/afile.txt --name a-file-not-a-dir
a-file-not-a-dir · .../filetarget/afile.txt
  matched by: a-file-not-a-dir, afile.txt
```

This is now a catalogued "repository" whose path is a plain file. Nothing
downstream (`open_work`, `claim_worktree`, a future `ask_manager`) has been
checked here for what happens when it is handed this path as a `cwd` or a
`repo` to branch — but the catalog itself offers zero resistance to creating
it.

Cause: `core/src/projects.rs:436` (`add_project`) — no `metadata()`/`is_dir()`
check anywhere in the function; `normalise` (`core/src/projects.rs:376`)
canonicalizes but does not validate shape.

Fix: refuse (or at least warn loudly) when the path is not a directory.

Check: `jod project add <path-to-a-plain-file>` is rejected, or the returned
row is flagged in a way `project_list` surfaces.

---

### P3. Two projects that share a spoken form are never reported as ambiguous — one is silently picked
Status: **verified fixed — merged as #131, check run against main, passes** · Severity was: high

Two projects seeded with the same basename, then archived by that name:

```
$ jod project archive shared
Error: `shared` is the name of 2 projects — shared (/tmp/jod-b-657189/shared),
shared (/tmp/jod-a-657189/shared). Name the one you mean exactly.
```

Refuses and names both candidates with their paths, rather than picking one.
That is the interactive half of the check. The `settle_project` half — what
happens with no user to ask — is the part this task warned was easy to leave
undone, and is worth confirming separately.

Note for whoever reviews it: there are two halves and only one is easy.
Reporting `Match::Ambiguous` from an interactive command like
`jod project archive` is straightforward — refuse and name the candidates. But
`settle_project` runs before every model turn with **no user to ask**, so it
cannot refuse the same way. A fix that does the interactive half and quietly
leaves the automatic one would look complete and would not be.

Leaving the project unset is probably better than picking wrong, but that is a
judgement the fix has to argue rather than assume.

The catalog explicitly designed for this case — `Match::Ambiguous` exists
because "two genuinely different projects named in one breath is not
something a string matcher may pick between" (the module's own comment,
`core/src/projects.rs:274`). It does not fire when the ambiguity is a
**shared** spoken form rather than **two different** ones — which is exactly
what a basename collision produces.

Observed, explicit lookup (`project_by_name`, used by `jod project
archive/restore` and by the MCP tool `project_switch`):

```
$ jod project add .../collide/proj --name collide-proj
$ jod project add .../other/proj   --name other-proj
$ jod project archive proj
other-proj archived — it can still be named, but will not be inferred
```

Two real, distinct projects both have `proj` as a spoken form (their shared
basename). Naming it archived **exactly one of them**, chosen only by
whichever sorts first in `last_touched_ms DESC, name` order — silently, with
no error and no list of candidates.

Cause, traced (not yet reproduced against `resolve()` directly — see below):
`Store::project_by_name` (`core/src/projects.rs:528`) does
`.find(|p| p.spoken_forms().iter().any(|f| f == &wanted))` — first match
wins, no ambiguity check at all.

The inference path (`resolve()`, `core/src/projects.rs:290`, what
`settle_project` runs on every main-chat instruction) has the *same* failure
for a different reason. It builds a `claimed` list of byte spans already
matched, specifically so a shorter name inside a longer one (`jod` inside
`jod-cloud`) doesn't double-count. But when two *different* projects have the
*identical* spoken form, the second one's span exactly equals the first's
already-`claimed` span:

```rust
if claimed.iter().any(|(s, e)| span.0 >= *s && span.1 <= *e) {
    continue;   // core/src/projects.rs:312
}
```

`span.0 >= s && span.1 <= e` is true when the spans are equal, so the second
project is silently dropped rather than pushed into `hits` — `resolve()`
returns `Match::One`, never `Match::Ambiguous`, and the resolution gets
written with `How::Inferred` and a reason that looks fully confident
(`"the instruction said \"proj\""`) when it was actually a coin flip.

This was traced rather than unit-tested live (no way to call `resolve()`
directly without a rebuild), but the code path is deterministic and the
`project_by_name` half is directly reproduced above by the identical
underlying cause (first-match, no ambiguity check).

Fix: track *which projects*, not just *which byte spans*, have claimed a
form. Two different projects claiming the same span is exactly
`Match::Ambiguous`; only one project claiming an overlapping *shorter* span
inside its own longer form should be suppressed. `project_by_name` needs the
equivalent: report when a name matches more than one project rather than
returning the first.

Check: two projects with the same basename (or the same alias, however that
happens); an utterance naming just the basename resolves to `Ambiguous`, not
`One`; `jod project archive <shared name>` refuses or asks which one, rather
than picking.

---

### P4. `State::Paused` exists but is unreachable
Status: needs Reljod's decision · Owner: — · Severity: low

`projects::State` documents three states — `Active`, `Paused` ("real but
dormant"), `Archived`. `jod project` only exposes `archive` and `restore`,
which move a project to `Archived`/`Active`. Nothing anywhere — not the CLI,
not any MCP tool — ever constructs `State::Paused`.

Confirmed by grep: every other `Paused` in the repo belongs to
`ScheduleState`, `GoalState` or `ThreadState`; `projects::State::Paused` has
no caller of `set_project_state(_, State::Paused)` outside its own tests.

Fix: either add `jod project pause` (and the matching MCP path) or drop the
variant and its "dormant" carve-out from the doc comment — a state nothing
can reach is dead weight in the mental model.

#### What was established

The two options point in opposite directions, so the first question is
whether `Paused` and `Archived` are actually different. They are, and the
difference is in listing rather than in inference.

Both states are kept out of inference. `State::inferrable` returns true only
for `Active`, and `resolve` filters the catalog through it, so an offhand
"let's fix this" can never land on either. Both can still be named outright,
because `Store::projects_by_name` searches the whole table.

They part company in `Store::projects`, which is the default listing. It
filters on `state != 'archived'`, not on `state = 'active'`. So a paused
project stays visible and an archived one disappears, on four surfaces that
all call `projects(false)`:

- `jod project ls` without `--all` (`cli/src/main.rs`)
- the `project_list` MCP tool without `include_archived` (`core/src/mcp.rs`)
- the TUI catalog panel and `/projects` (`cli/src/tui/data.rs`, `mod.rs`)
- the catalog prepended to every main-chat turn (`core/src/orchestrator.rs`)

That matches what the doc comments always claimed. `Archived` says "listed
only when explicitly asked for"; `Paused` says only that it is kept out of
inference, and says nothing about hiding it, because it does not hide it.

So `Paused` is a real third behaviour with no way in, not a second spelling
of `Archived`. Pinned by `pausing_and_archiving_are_not_the_same_thing` in
`core/src/projects.rs`, which asserts both halves: the listing splits, the
inference does not.

Two things worth knowing before deciding:

- The message `jod project archive` prints — "it can still be named, but will
  not be inferred" — describes only half of what archiving does. The other
  half is that the project drops off the default listing. Left alone here so
  as not to widen this task, but it should be said in full.
- `Project::summary_line` prints no state marker, so if a project could be
  paused it would sit in the everyday listing looking exactly like an active
  one, including in the catalog the orchestrator reads. Exposing `pause`
  without a marker would make the catalog say something untrue.

#### What `pause` would have to do

`jod project pause <name>` would set `State::Paused`, and `jod project
restore` already moves any state back to `Active`, so the reverse exists.
Beyond that it needs a matching MCP tool, so the model can pause a project it
is told to stop suggesting, and it needs `summary_line` to mark the state, or
the visible half of the feature is invisible.

The remaining question is not a code question: it is whether Reljod wants a
"still on the list, will not be guessed at" state distinct from "off the
list". That is a product call, so this stops here rather than inventing it.

Check (was): `jod project pause <name>` exists and the project stops being
inferred while still being explicitly nameable, matching what `Archived`
already does but distinct from it. **This check cannot run**, because the
command it names is deliberately not being added.

Check (now): `cargo test -p jod-core --lib projects::` passes, including
`pausing_and_archiving_are_not_the_same_thing`, which is what holds the
finding above true while the decision is open.

**Run against main: passes.** 43 project tests green, including
`pausing_and_archiving_are_not_the_same_thing`.

**This task is not a verification case and should not be counted as one.** There
is no fix to verify — its agent established what distinguishes the two states
and handed the decision back rather than picking a branch. The handling is the
right pattern for that situation and worth copying: mark the original check
unrunnable *with the reason*, and write a replacement that guards the **finding**
rather than one that claims the task is done. The test above would still need to
hold whichever way Reljod decides. A replacement check that only passed under one
branch would be quietly assuming his answer.

---

### P5. A stale catalog entry (moved or deleted directory) is invisible until something tries to use it
Status: **verified fixed — merged as #145, check run against main, passes** · Severity was: low

Catalogued a directory, deleted it, ran `jod project ls`:

```
jod-gone-596806 · /tmp/jod-gone-596806
  cannot be worked in: there is nothing at `/tmp/jod-gone-596806` any more, so
  no session can be started in it. The checkout was deleted or renamed —
  catalogue it at the path it lives at now, or archive this entry if it is gone
  for good.
```

The check asked only that the path be flagged as missing. What landed also says
what to do about it, and names both remedies. Worth noting because it is the
opposite of the failure this list keeps finding: a message that says more than
the reader needs rather than less.

Observed: deleting a catalogued directory, or renaming it, leaves the row
exactly as it was — `jod project ls` shows it identically to a healthy entry,
with no "path no longer exists" marker.

```
$ jod project add .../ephemeral --name ephemeral-proj
$ rm -rf .../ephemeral
$ jod project ls --all --json   # still lists ephemeral-proj at the old path
```

This is plausibly intentional given the module's stated philosophy
(`Archived` over deletion, "a deleted row answers nothing" — the doc comment
on `State`), and P2's missing validation only runs at `add` time regardless.
But paired with P2, there is currently no signal anywhere — not `project_list`
for the model, not `jod project ls` for a human — that a catalogued path has
gone stale. `resolve()` will happily keep matching it and hand a nonexistent
`cwd` onward.

Fix: at minimum, have `project_list`/`jod project ls` note when
`fs::metadata` fails on a project's path, so a stale entry is visible instead
of silently offered as a resolution target.

Check: catalogue a directory, delete it, `jod project ls` (or `project_list`)
flags that path as missing.

---

### Note: project stickiness is one global value, not one per conversation

Not a new root cause — this is `tasks/00-launch-and-roots.md`'s L1–L3 (the
main chat is a single `pinned = 1` row, keyed by nothing) viewed through the
project catalog specifically, and worth recording here because whoever fixes
L3 needs to know it also fixes this.

Traced: `Store::pinned_conversation` (`core/src/orchestrator.rs:1312`) is
`SELECT id FROM conversations WHERE pinned = 1` — no filter by harness kind
despite `main_conversation` taking one as an argument. Every caller of
`hand_to_orchestrator` — `jod main` (CLI), the TUI chat box, the Telegram
bridge, the HTTP API — funnels into this one row. Since `current_project_id`
lives on the conversation row (`core/src/store.rs:1277`), there is exactly
**one** sticky project for the entire installation, shared across every
channel. A bare "let's fix this" typed in the TUI can resolve against a
project that was only ever named in a Telegram message five minutes earlier
from an entirely different train of thought. This is the direct answer to
the "two conversations on different projects at once" scenario below: today
that isn't two conversations, it's one, so there's nothing to keep separate —
they collide.

Once L3 gives main more than one pinned conversation, this resolves itself
for free: `current_project_id` is already a per-conversation-row column.

---

## Part 2 — the manager work, as claimable tasks

> **All shipped. Nothing here is claimable.** T1–T7 below were built together
> with the rest of the spec; the record of what landed, including how the four
> corrections were applied and the seven open questions answered, is at the end
> of [`docs/spec-ceo-and-managers.md`](../docs/spec-ceo-and-managers.md).
>
> Kept because the task descriptions name the files and the traps, and because
> each one says which numbered check it was there to make possible — which is
> still the fastest way to find the test that holds it.
>
> Two things the descriptions below got wrong, worth reading before trusting
> them: the manager lives on `projects.manager_conversation_id` and is keyed by
> **project alone, not by project and harness** (splitting it by harness would
> split the memory that is its whole reason to exist — `resume_for` moves it
> between harnesses the way it does for main); and T3's worry about inheriting
> P3's ambiguity bug did not apply, because `ask_manager` resolves an explicit
> name through `projects_by_name` and refuses when it matches more than one.

Source for everything below:
`docs/spec-ceo-and-managers.md`, **Change 3 — a manager per project**.

### T1. `manager_conversation_id` column + migration
Status: **shipped** — migration `0022_a_project_gets_a_manager`.
Spec section: 3a, "Migrations" (migration 3).
Files: `core/src/store.rs`.
What exists today: nothing. `projects` has no such column; confirmed by
reading its `CREATE TABLE`/migration block.
Proves: check 10 (get-or-create is idempotent per project, distinct across
projects) needs this column to exist before it can even compile against.

### T2. `Store::manager_conversation(project_id, harness)`
Status: **shipped** — `core/src/orchestrator.rs`, keyed by project alone.
Spec section: 3a.
Files: `core/src/store.rs` (or `core/src/orchestrator.rs`, next to
`main_conversation` which it explicitly mirrors — `main_conversation` is at
`core/src/orchestrator.rs:1292`, and depends on T1).
What exists today: nothing; `main_conversation`'s get-or-create pattern is the
template to copy, including — importantly — **not** copying its bug: `main_conversation`'s `pinned_conversation()` lookup ignores the `harness`
argument entirely (see the note above). A manager conversation must be keyed
by `project_id` (and presumably harness), not by a single global flag, or
every project's manager collapses into the same row the way every main chat
already does.
Proves: check 10, check 11 (first `ask_manager` call creates and says so, the
second resumes and reports the same conversation id).

### T3. New MCP tool `ask_manager`
Status: **shipped** — `core/src/mcp.rs`, at `ToolAccess::Delegate`.
Spec section: 3b.
Files: `core/src/mcp.rs` (alongside the other project tools at
`core/src/mcp.rs:459-522`, `ToolAccess::Delegate` like `project_switch` and
`project_add` at `core/src/mcp.rs:3301-3302`).
What exists today: nothing. Resolution should reuse `projects::resolve`
(`core/src/projects.rs:290`) — and whoever builds this should fix P3 first or
inherit its ambiguity bug in a brand-new tool.
Proves: check 11, check 12 (unknown project refuses and names what's known —
this is the same pattern `project_switch` already uses at
`core/src/mcp.rs:1865-1883`, worth copying verbatim rather than re-inventing).

### T4. Refuse `open_work` from main at the tool boundary
Status: **shipped** — and `delegate` at a known checkout with it.
Spec section: 3c.
Files: `core/src/mcp.rs`, near `open_work`'s existing refusal for "no roots to
inherit a checkout from" (`core/src/mcp.rs:2149`, per
`tasks/00-launch-and-roots.md` L4).
What exists today: nothing — `open_work` has no caller-identity check at all
right now, only the roots check. The MCP server already resolves the calling
run's pgid against `runs.pgid` for other purposes per the spec; confirm that
lookup is reachable from `open_work`'s call site before assuming it's a
one-line add.
Proves: check 13 (main's run refused, names `ask_manager`), check 14 (a
manager's run still succeeds — needs T2/T5 to exist first so there's a
manager conversation to call from).

### T5. Two preambles instead of one
Status: **shipped** — `orchestrator_preamble` and `manager_preamble`.
Spec section: 3d.
Files: `core/src/orchestrator.rs`, splitting `orchestrator_preamble()`
(`core/src/orchestrator.rs:353`) into main's version and a new manager
version.
What exists today: one preamble, used for every run regardless of role
(worker, orchestrator, whatever spawned it) — confirmed by grep, there is
exactly one call to `orchestrator_preamble()` and it isn't role-conditional.
This also overlaps `tasks/01-routing.md`'s R1 (the preamble currently
forbids the orchestrator from ever answering directly at all) — that's the
same file, same function, arguably the same PR.
Proves: nothing on the numbered list directly, but every one of Change 3's
checks assumes the tool sets in the spec's table are actually wired to the
right role, and nothing currently enforces that split.

### T6. Project and manager nodes in the fleet tree
Status: **shipped** — `NodeKind::Project` and `NodeKind::Manager`.
Spec section: 3e.
Files: `core/src/tree.rs` (new `NodeKind::Project`, `NodeKind::Manager`),
`cli/src/tui/mod.rs` (entering a manager row — the shape to copy is
`enter_main`, `cli/src/tui/mod.rs:951`, named directly in the spec).
What exists today: `NodeKind` has no project-shaped variant; not checked in
depth here since `tasks/20-fleets.md` (not yet written by whichever teammate
owns fleets) is the more natural owner of tree-shape work — flagging the
dependency rather than duplicating that investigation.
Proves: check 15, check 16.

### T7. `works.project_id` (Change 2, but Change 3 depends on it)
Status: **shipped** — migration `0021_a_work_knows_its_project`.
Spec section: Change 2, "Add `project_id` to the `works` table" — listed
under Change 2 in the spec but load-bearing for T6's "that project's works
under it," so noting it here too.
Files: `core/src/store.rs` (migration 2), `core/src/works.rs`.
What exists today: confirmed above (Part 1) — `works` has no `project_id`
column at all.
Proves: check 9.

---

## Scenarios run

| # | Scenario | Expected | Actual | Pass/fail |
|---|---|---|---|---|
| 1 | Catalogue a project | Row created, matched by name+basename | `alpha` catalogued, `matched by: alpha, the game` | pass |
| 2 | Catalogue the same path twice | Update in place, no duplicate row; per doc comment, aliases/notes *extend* | Updated in place (no dupe) but aliases/notes were **replaced**, losing the first call's alias and note | fail — see P1 |
| 3 | Project named by an alias | Alias resolves like a name | `--alias "the game"` matched `alpha` | pass (unit-tested in code too) |
| 4 | Project named by a path | N/A directly — paths aren't spoken forms, basenames are | see #5 | n/a |
| 5 | Project named by bare directory name | Basename alone resolves without registering it | `"let's work on alpha"` resolved to the project whose dir is `alpha`, live, via `jod main` | pass |
| 6 | Instruction naming no project at all | `settle_project` returns `None`, nothing written, nothing crashes | `jod main "let's fix tetris"` (no such project) — `current_project_id` stayed `NULL`, no `project_resolutions` row | pass |
| 7 | Switching current project mid-conversation | Naming a different project overrides the sticky one | Confirmed live: bare "alpha" instruction set current project; unit-tested for the tetris→jod case in `core/src/projects.rs` | pass |
| 8 | Two conversations on different projects at once | Independent stickiness per conversation | There is only **one** pinned conversation database-wide (`pinned = 1`, no harness/channel key) — this scenario cannot actually occur today; see the note after P5 | fail — root cause is L3, not new |
| 9 | Project whose directory has been deleted or renamed | Some signal that the path is gone | Catalog entry unchanged, no marker, `jod project ls` shows it as healthy | fail — see P5 |
| 10 | Two projects whose directory basenames collide | `Match::Ambiguous`, and explicit lookup refuses/lists candidates | Both silently collapse to one project, chosen by iteration order, no error either way | fail — see P3 |
| 11 | Resolving a project from a worktree path rather than the checkout | `project_for_path` should find the owning project from a cwd/worktree path | `project_for_path` (`core/src/projects.rs:544`) is never called from anywhere outside its own tests — dead code, no path-based resolution exists in the live system at all | fail — this is the concrete shape of spec Gap 3 |
| 12 | Deleting a project that has works or conversations attached | Some defined behaviour | There is no delete path at all — CLI has only `ls/add/archive/restore`, MCP has only `list/current/switch/add`. Deleting is only possible by raw SQL (as the test at `core/src/projects.rs:1105-1116` does), which does correctly leave the conversation/chat intact (`current_project_id` reads back `NULL`, conversation itself is untouched) | pass, but only reachable off the documented surface — "deleting" isn't a real user-facing action today |
| 13 | What `list_agents` tells the router about project | Spec says: nothing | Confirmed — `AgentView` (`core/src/mcp.rs`, struct definition) has no `project` field, and the tool schema has no `project` filter argument | pass (spec accurate) |
| 14 | Edge case: empty name | Refused | `Error: a project needs a name...` | pass |
| 15 | Edge case: unicode name + alias (Japanese + emoji) | Works | Catalogued and listed correctly, `matched by` includes the unicode alias and ASCII basename | pass |
| 16 | Edge case: very long name (200 chars) | Truncated to `MAX_NAME_CHARS` (60) | Truncated to exactly 60 chars | pass |
| 17 | Edge case: relative path | Canonicalized correctly | `./gamma/nested` from `$SCRATCH` resolved to the correct absolute path | pass |
| 18 | Edge case: path that is a file, not a directory | Refused or flagged | Silently accepted as a project | fail — see P2 |
| 19 | Edge case: symlinked path | Canonicalizes to the real path, dedupes against the existing entry for that real path | Correctly deduped onto the existing `alpha` row (good) — but doing so re-triggered P1's alias/notes wipe (bad) | mixed — dedup logic is correct, wipe bug (P1) reproduces through it |

Nothing above needed a real, sustained model conversation beyond the two
short `jod main` round trips used to observe `settle_project` fire live — the
rest is either pure Rust (`resolve`, `project_by_name`, `add_project`) or CLI
output, all directly exercised.

`needs confirming`: whether `open_work`/`claim_worktree` actually break (vs.
just misbehave) when handed a project whose path is a file (P2) or has gone
missing (P5) — not run, since both `open_work` and `claim_worktree` need a
work/session context this task didn't set up, and the MCP tools weren't
driven directly (would need a live agent holding an MCP connection, which is
a bigger live-model cost than the two round trips already spent above).

---

## Part 3 — left open by the manager-plans-engineers-execute work (#228)

### M1. Six subsystems shipped green and unable to run
Status: **fixed in #228** · Severity was: high

Kept because the pattern is the finding, not any one instance, and it will
recur. Each of these was tested, passing, and inert:

| Subsystem | Why nothing happened |
|---|---|
| `Store::stack_for_work` | joined `conversations.task_id`; nothing wrote it |
| `leases::refuse_a_collision_in` | same column, so every share was waved through |
| `prs::auto_pr_instruction` | no production caller — no pull request was ever opened |
| `Store::enqueue_delivery` | no production caller |
| `remember` | `ToolAccess::Orchestrate`, but managers spawn at `Delegate` |
| `orchestrator::roots_lines` | told `ReadOnly` sessions to call `claim_worktree` |

**Every one was found by an engineer reading a colleague's file. None by a
test.** They share a shape: the test builds the state a production writer was
supposed to produce, asserts on it, and passes, while the writer does not
exist. See [`docs/decisions.md`](../docs/decisions.md).

The two habits that fall out are cheap. Test *through* the production writer —
build a board by calling `plan_work`, not by inserting rows. And when a column,
function or tool is added, grep for its **writer** before believing its reader.

### M2. Test helpers that build states production cannot produce
Status: **open** · Severity: low · Owner: unclaimed

The residue of M1. Known instances, none load-bearing:

- `core/src/prs.rs`, the shared-worktree ordering test sets `lease_id` by raw
  `UPDATE`. The branch route covers the real path, so the ordering it checks is
  genuinely covered — but the setup implies a route it does not prove. Reported
  by the engineer who wrote it.
- `core/src/mcp.rs`, the stack-ordering test writes `conversations.task_id`
  directly rather than through `open_work`. There is now a production writer
  (`spawn_onto_first_task`), so this one can simply use it.

Neither is a live fault. Both are the shape that hid six real ones, which is
the reason to close them rather than the severity.

### M3. `additionalProperties` is absent from every tool schema
Status: **open** · Severity: medium · Owner: unclaimed

`obj()` (`core/src/mcp.rs`) emits `{type, properties, required}` and never
`additionalProperties: false`, so an argument a tool does not know is accepted,
ignored, and answered with a success.

This bit during #228: `manager_preamble` described `open_work`'s `placement`
argument before the argument existed, and a manager placing an engineer
read-only was told it worked and got an ordinary engineer. That specific gap is
closed, but the silence is general and applies to every tool in the catalogue.

Not fixed in #228 deliberately. Switching it on is one line and changes the
answer a model gets for every stray argument across every tool, from a success
to a hard error, so it wants its own change and its own thinking about the
blast radius.

### M4. Fifteen git-dependent tests report green without running
Status: **open** · Severity: low · Owner: unclaimed

`let Some(repo) = fixture_repo(..) else { return; }` appears sixteen times in
`core/src/leases.rs` and fifteen of them return silently when git is absent.
Pre-existing convention, followed by the new tests rather than invented by
them.

Not a live fault — git is present here and on CI, so they run. It means "green"
does not distinguish "ran and passed" from "never ran" for that half of the
file, which is worth knowing before trusting the suite on an unfamiliar
machine.

### M5. A delivery to a session that cannot be resumed queues for ever
Status: **open** · Severity: low · Owner: unclaimed

`Ticker::tick_deliveries` holds a delivery when `resume_for` does not return
`Resume::Session`, and nothing sweeps or reports one that stays queued — the
only trace is a per-tick `held` counter nobody reads. In practice a manager has
a session by the time it has spawned an engineer, so this is latent. It has no
floor.
