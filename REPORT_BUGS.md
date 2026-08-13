# REPORT_BUGS.md — hand-driving the Jod TUI

`docs/try-it.md:241` says of the TUI: **"Tested, none of it hand-driven."** This
is that hand-drive. The task used as the vehicle: *build a working Tetris game
in Node/Vite/HTML, delegated through the TUI.*

**This file is written incrementally and is being appended to while the run is
still going.** Items marked `OPEN` are safe to pick up. Items marked
`NEEDS-REPRO` are honest observations that I could **not** reproduce — do not
"fix" them without reproducing first.

Every finding below was produced by driving the real binary in a real terminal
(tmux, isolated socket `jodtest`, 200×50 and 260×50), not by reading code. Root
causes were then confirmed in source and are cited `file:line`.

**Binary under test:** rebased onto `origin/main` mid-run and rebuilt, so
everything below reflects `f43e7a7` / `abc3e8f` — **including PR #75, which
moved every global chord from `Alt` to `Ctrl`.** Findings were re-verified
against the new binary after the rebase; two are now fixed and are marked so
rather than deleted, since the fix is the useful record. The build matters —
see BUG-13.

> **Read this first.** The single most serious finding is
> [BUG-14](#bug-14): a delegated run wrote its output into **`$HOME`**, outside
> every declared root, and Jod recorded it as `✓ done`.

> **Correction, 2026-08-13 — eleven line numbers in this file were wrong.**
> Rebased onto `f3aaf45` and re-checked every `file:line` citation
> mechanically. Eleven pointed a consistent ~18–22 lines too early, because I
> had read them in the **main checkout** (still parked on the pre-#75 commit)
> rather than in this worktree. The *findings* were unaffected — every one was
> reproduced against the running binary, and the quoted code is verbatim — but
> anyone opening `ui.rs:2489` would have landed on `Block::default()` and
> wondered what I was talking about. All corrected; every citation in this file
> now resolves in the rebased tree. The claims themselves are unchanged.

---

## Where this stands — 19 of 21 findings are closed in `main`

As of `3e39c2c`, **only two PRs remain open**, and between them they carry the
last five findings:

| PR | Findings | State |
|---|---|---|
| [#86](https://github.com/Reljod/Jod/pull/86) | [BUG-3](#bug-3), [BUG-11](#bug-11), [BUG-20](#bug-20), [BUG-21](#bug-21) | draft — the whole of **Pattern B** in one sweep |
| [#89](https://github.com/Reljod/Jod/pull/89) | [BUG-10](#bug-10), [BUG-12](#bug-12) | draft |

Everything else has merged: **#75** (BUG-8, BUG-9), **#80** (BUG-13), **#81**
(BUG-6, BUG-7), **#82** (BUG-1, BUG-2), **#78** (BUG-14, BUG-4), **#85**
(BUG-5), **#87** (BUG-17, BUG-18, BUG-19), **#83** (BUG-15, BUG-16), **#84**
(BUG-14's carding half).

**The headline result: the TUI now builds a working classic Tetris in one
shot, in the directory you launched it in.** That was not true when this
report was opened — the same instruction put the project in `$HOME` and
reported `✓ done`.

The per-bug sections below still describe the **broken** behaviour on purpose:
each one is the regression reference for the fix that closed it. Read the
status line at the top of a section before assuming it is still true.

---

## Severity summary

Status key: **merged** = in `main`, reported by the maintainer, *not* re-driven
by me. **PR open** = CI-green draft awaiting merge. **open** = nobody on it.

| ID | Severity | Status | Area | One line |
|---|---|---|---|---|
| [BUG-14](#bug-14) | **Critical** | ½ **merged** #82 · #84 open | delegation | **The TUI ran every agent in `$HOME`** — work landed outside every root and the run was recorded `✓ done` |
| [BUG-1](#bug-1) | **Critical** | **merged** #82 | rendering | A fresh session hid *all* notice-only output — most slash commands rendered nothing |
| [BUG-2](#bug-2) | **High** | **merged** #82 | delegation | `Ctrl-B` delegated with almost no confirmation; it looked like nothing happened |
| [BUG-3](#bug-3) | **High** | PR open #86 | directory clarity | The directory picker's header is truncated, so you cannot tell which tree you are in |
| [BUG-4](#bug-4) | **High** | **merged** #78 | directory clarity | The working directory appeared nowhere in the chat UI |
| [BUG-5](#bug-5) | **High** | PR open #85 | projects | A project cannot be created or cited from the TUI at all |
| [BUG-6](#bug-6) | **High** | **merged** #81 | discoverability | The projects key was a silent no-op unless an undiscoverable panel was already open |
| [BUG-7](#bug-7) | Medium | **merged** #81 | discoverability | `Shift-Tab` — the only way to reach projects/sessions/context — was undocumented |
| [BUG-8](#bug-8) | ~~Medium~~ | **fixed** #75 | rendering | ~~Keymap overlay: key label collides with its description~~ |
| [BUG-9](#bug-9) | ~~Medium~~ | **largely fixed** #75 | honesty | ~~The splash claims "Alt-K opens every screen"~~ |
| [BUG-10](#bug-10) | Low | **open — unclaimed** | commands | `/main` is listed twice, with two different meanings |
| [BUG-11](#bug-11) | Low | PR open #86 | commands | Command descriptions are cut mid-word with no ellipsis |
| [BUG-12](#bug-12) | Low | **open — unclaimed** | input | The input box is fixed at ~70 columns and single-line |
| [BUG-13](#bug-13) | Medium | **merged** #80 | tooling | `jod --version` could not distinguish two different builds |
| [BUG-17](#bug-17) | Medium | with an agent | interrupt | Interrupt is unacknowledged for 4–6s, then reported as both `✓ done` and `✗ failed` |
| [BUG-18](#bug-18) | Medium | with an agent | interrupt | Every interrupt falsely warns the run "may still be writing", worded as a *start* failure |
| [BUG-19](#bug-19) | Medium | with an agent | fleet | An interrupted run reads `✗ failed` in the TUI but `killed` in `jod ls` and the database |
| [BUG-20](#bug-20) | **High** | PR open #86 | destructive UI | The "cannot be undone" dialog clips its own warning and hides what cancels |
| [BUG-21](#bug-21) | Medium | PR open #86 | diffs | The diff header's untruncated path pushes the promised `+N -M` counts off screen |
| [BUG-15](#bug-15) | **High** | #83 rebased, **needs a human** | mentions | `@` in a non-git directory is ~95% `node_modules` noise; source is invisible |
| [BUG-16](#bug-16) | Medium | #83 rebased, **needs a human** | mentions | `@` clips paths from the right, so six different files render identically |

**Nothing is unclaimed except [BUG-10](#bug-10) and [BUG-12](#bug-12).** Check
this column before starting; five findings have merged since the body text
below was written, and the per-bug sections still describe the *broken*
behaviour so the repro survives as a regression reference.

---

## Verification status — five findings have now merged into `main` (`27a7072`)

**Read the provenance line on every row before trusting it.** This report's
value is that its claims were driven by hand in a real terminal. The merge
statuses below are **not** that — they were reported to me by the maintainer,
and I could not re-drive them: this session is restricted to editing this file,
with no builds. They are recorded as *reported merged*, not as *re-verified*.

An earlier revision of this section said "six draft PRs are open… **none are
merged**" and carried three rows reading "still renders nothing". That was true
when written and is now **out of date** — those rows predate the merges below
and have been removed rather than left to mislead.

### Merged into `main`

| PR | Commit | Findings | What landed | Re-verified by me? |
|---|---|---|---|---|
| #80 | `0ca9f2e` | [BUG-13](#bug-13) | `jod --version` stamps the commit | ✗ reported |
| #81 | `2c0963f` | [BUG-7](#bug-7), [BUG-6](#bug-6) | `Shift-Tab` advertised; the projects key opens the panel from a cold start | ✗ reported |
| #82 | `27a7072` | [BUG-1](#bug-1), [BUG-2](#bug-2), [BUG-14](#bug-14) *(display half)* | notice-only output no longer swallowed; the delegation confirmation names id, full prompt **and cwd**; fleet detail shows the run's cwd | ✗ reported |

### ✅ Now re-driven by hand, against `main` at `a15c016`

The `✗ reported` column above is **closed**. I rebuilt from a clean tree at
`a15c016` (no local patches — I discarded two in-flight fixes of my own once I
found they duplicated #78) and drove the real TUI in a terminal. Results:

| Finding | PR | Hand-driven result |
|---|---|---|
| [BUG-13](#bug-13) | #80 | ✅ **fixed** — `jod --version` → `jod 0.1.0 (a15c016 2026-08-14)`. Commit **and** date, exactly the fix suggested. |
| [BUG-7](#bug-7) | #81 | ✅ **fixed** — the `?` overlay now lists `Shift-Tab  show or hide the side panel`. |
| [BUG-6](#bug-6) | #81 | ✅ **fixed** — `Ctrl-G d` from a **cold start** now shows the projects panel. This was the one with the misleading green test; it is genuinely fixed in the default state. |
| [BUG-1](#bug-1) | #82 | ✅ **fixed** — `/root` typed as the *first* action now prints its answer immediately. Verified the constraint too: the splash still appears on a fresh session. |
| [BUG-2](#bug-2) | #82 | ✅ **fixed** — and better than I asked for (see below). |
| [BUG-4](#bug-4) | #78 | ❌ **still open** — status bar reads `● auto · Claude Code · ready`. No directory. |
| [BUG-14](#bug-14) | #78 | ❌ **STILL BITES** — see below. This is the last blocker. |

**#82's delegate confirmation is the best fix in this report.** It reads:

```
⇢ delegated b2dd9d66 · in the background, Ctrl-F to watch
    in /Users/reljodoreta
    reply with the word here
```

id, **working directory**, and prompt. (I nearly filed a correction claiming it
omitted the cwd — my `grep` had caught only the first line. It does not omit
it; the maintainer's description was right and mine was wrong.)

### ⚠ BUG-14 is not fixed on `main`, and it is the one thing left

`main` still has, at `cli/src/main.rs:1789`:

```rust
cwd: cwd.unwrap_or_else(jod_core::service::default_cwd),
```

Driven just now: launched `jod tui` **from `tetris-oneshot`**, with no `--cwd`,
delegated one prompt. The run recorded:

```
b2dd9d66|/Users/reljodoreta|reply with the word
```

`$HOME`. So on today's `main`, a one-shot Tetris **still lands in the home
directory**. [PR #78](https://github.com/Reljod/Jod/pull/78) is the fix and is
still a draft — **merging it is the highest-value action available.**

### ✅ PR #78 verified by hand — it fixes BUG-14. Merge it.

I did not stop at reading the diff. I checked out
`origin/worktree-tui-launch-dir-root` (`412125e`) into its own worktree, built
it, and drove it exactly as I drove `main` — same machine, same launch
directory, same single action. The two runs sit side by side in the same
table:

```
540db7a2|/…/worktrees/tui-dogfood-tetris/tetris-oneshot|reply with the word pr   <-- PR #78
b2dd9d66|/Users/reljodoreta                             |reply with the word he   <-- main
```

**The only difference is the build.** PR #78 records the launch directory;
`main` records `$HOME`.

It also delivers BUG-4, though not where I suggested. The launch directory is
named on the **splash**, `~`-abbreviated and elided sensibly:

```
▪ ~/Developer/Repositories/Projects/Jod/.claude/worktrees/tui-dogfood-tetris/tetris-oneshot
```

and the delegate confirmation carries the full path on its own continuation
line.

**One residual worth a follow-up, not a block:** the splash is only on screen
while the session is fresh. Once the first turn lands, the splash gives way to
the transcript and the directory is no longer visible anywhere — the status bar
still reads `● auto · Claude Code · ready`. So "which directory am I in?"
is answered at the moment you start and at each delegation, but not
continuously. That is a large improvement over nothing, and the status-bar
field suggested in [BUG-4](#bug-4) would close the gap.

**Recommendation: merge #78.** It is the last thing standing between this
program and a clean one-shot, it is verified working by hand rather than by
assertion, and it is a strictly smaller behaviour change than the two PRs
already merged.

> **Update — #78 is now merged** as `a625bec`, along with **#85** (`8160282`,
> the `/project` command — [BUG-5](#bug-5)) and **#87** (`8ffcdd5`, stopped
> runs no longer reported as failures — [BUG-17](#bug-17), [BUG-19](#bug-19)).
> Verified present in the merged tree: `cli/src/tui/command.rs:267` now matches
> `"project" | "projects" | "repo" | "repos"`, and `cli/src/tui/app.rs:372`
> reasons explicitly about *"the elapsed counter stopped: a frozen clock, which
> reads as a hung"* — the exact failure this report described.
>
> **That is six of the findings closed in `main`:** BUG-1, BUG-2, BUG-5,
> BUG-6, BUG-7, BUG-13, BUG-14 and BUG-4, plus BUG-17/BUG-19 addressed and
> BUG-8/BUG-9 fixed earlier by #75.

---

The consolation is that #82 turned a silent failure into a loud one: the
confirmation now prints `in /Users/reljodoreta` right under the delegation, so
the mistake is visible the moment it happens instead of after a paid run has
written a project into the wrong tree. That is the difference between a
critical bug and an annoying one — but the annoying one still misplaces the
work.

---

That closes the two findings I most wanted closed. [BUG-1](#bug-1) was the one
hiding every other message in the program, and #82 taking [BUG-2](#bug-2) with
it matches what I predicted after testing the delegate confirmation in a
non-fresh session: the message was always good, it was only ever invisible.
#82 also adds the cwd to that confirmation — the single field whose absence let
[BUG-14](#bug-14) run a paid agent into the wrong tree unnoticed.

### Still open — CI-green drafts

| PR | Findings | Note |
|---|---|---|
| #83 | [BUG-15](#bug-15), [BUG-16](#bug-16) | **conflict resolved, rebased, CI green — blocked on human review (size)** |
| #84 | [BUG-14](#bug-14) *(backend half)* | the card for a run that wrote outside every root |
| #85 | [BUG-5](#bug-5) | `/project ls\|add` |
| #86 | [BUG-3](#bug-3), [BUG-11](#bug-11), [BUG-20](#bug-20), [BUG-21](#bug-21) | Pattern B — four sites, one shared helper |

**#86 is stacked on #83's branch, so #83 must merge first.**

#### #83 — conflict fixed; do not retry the merge, it needs a person

The conflict is **resolved and pushed**. It was in `cli/src/tui/ui.rs`, where #81
and #83 had each appended tests to the end of the same test module. Both sides
were kept — they are independent — and all four tests pass:

```
test tui::ui::tests::a_clipped_row_still_bolds_the_characters_it_matched ... ok
test tui::ui::tests::two_long_paths_that_differ_only_at_the_end_render_differently ... ok
test tui::ui::tests::the_projects_key_shows_the_catalog_from_a_cold_start ... ok
test tui::ui::tests::the_keymap_names_the_key_that_opens_the_panel ... ok
```

Full suite after the final rebase: **973 passed, 0 failed.** Both CI checks
(`test`, `triage`) pass.

**It still will not merge, and that is correct.** `merge_pr.sh 83 --ready`
refuses:

```
REFUSED to merge PR #83:
 - triage says human-review — run pr_triage.sh for the reasons
```

and the reason is **size**, not a false positive:

> **size** — 590 code lines changed, over the 400-line limit
> *A human must read this before it merges … an agent must not merge it, and
> clearing them is a review, not a re-run.*

So this is a `human-review` verdict on an honest measurement — six code files,
590 lines. Per the charter this is a successful ending, not a failure: nothing
here is bypassable by an agent, and re-running the gate will not change it.
**Do not rebase-and-retry in the hope it clears.** It needs Reljod, or a split
into smaller PRs.

One caveat for whoever picks this up: `main` moved three times during this work
(#78, then two more), so the branch goes stale quickly. Rebase immediately
before the human merges rather than in advance.

#86 is the shape this report was arguing for: four width bugs taken as one
sweep through one helper, not four agents in one 9,000-line file. See
[Pattern B](#pattern-b--a-width-computed-from-content-ignoring-the-chrome-around-it).

### Open and unclaimed

[BUG-10](#bug-10) and [BUG-12](#bug-12). [BUG-17](#bug-17),
[BUG-18](#bug-18) and [BUG-19](#bug-19) are with an agent.

### What to check first, once the rest land

[BUG-14](#bug-14) has **two halves in two PRs** — #82 (display, merged) and #84
(backend, open). Worth confirming that a run writing outside every root *both*
raises the card **and** shows its cwd, rather than one landing without the
other. A half-fixed BUG-14 is the dangerous state: the cwd is now visible, so
it looks handled, while nothing yet objects when work lands outside every root.

### BUG-14's fix: independently confirmed before it merged

Earlier in this session an uncommitted `cli/src/main.rs` change appeared in
this worktree, replacing `cwd.unwrap_or_else(default_cwd)` with a `tui_cwd`
helper falling back to `std::env::current_dir()`. I built and drove it rather
than assume, launching with **no `--cwd` flag at all** — the exact condition
that originally sent Tetris into `$HOME`:

```
ab8a6b9d|/…/worktrees/tui-dogfood-tetris|reply with the single word verified
87e84b92|/Users/reljodoreta|Build a working Tetris game          <-- the original failure
```

The run landed where the console was launched. That change turned out to be a
stray process's work and duplicates **#78**, which does the same thing via
`console_cwd` — so it has no independent value as a patch, but the behavioural
confirmation stands on its own.

---

## ⚠ Collision notice — resolved, and worth learning from

**Resolved.** The stray processes have been stopped. The cause is now known and
is worth recording, because it is an easy trap: *messaging an agent in this
worktree spawned a second process inside it*, and that process started
**implementing** a BUG-14 fix in place rather than just relaying the message.
Two source trees then compiled into one `CARGO_TARGET_DIR`, and my build
printed `Blocking waiting for file lock on build directory`.

The working tree still carries their edits:

```
 M cli/src/main.rs          <-- stray process's BUG-14 fix; duplicates #78
 M cli/src/tui/ui.rs        <-- stray process's edits
?? tetris/  tetris-oneshot/ <-- the dogfood projects
```

They have deliberately **not** been reverted. Someone else's uncommitted work
is not mine to destroy, and that judgement was upheld. `cli/src/main.rs`
duplicates #78 (which does the same job via `console_cwd`), so it has no
independent value as a patch — but deleting it was still not my call to make
unasked.

Three things to carry forward:

1. **A worktree with uncommitted foreign edits is not a clean room.** A build
   here compiles somebody's in-flight fix, not the base commit. Every finding
   in this report was taken against the clean tree; the single exception is the
   BUG-14 confirmation above, which says so explicitly.
2. **Confirm behaviour, not builds.** With a contended `target/`,
   `target/release/jod` may be built from *either* tree at any moment. I
   confirmed the cwd fix by what it *recorded* in the database, not by trusting
   the binary.
3. **One worktree per agent.** This is the "one owner per path" rule in
   `docs/teamwork.md` being crossed in practice — and the crossing came from
   the dispatch mechanism, not from any agent misbehaving.

---

## For agents picking this up

This file is the work queue. Read this section before editing code.

### Claim protocol

1. Pick a bug that is `OPEN`. Change its status to
   `IN PROGRESS — <your name>` **in this file** and commit that one-line change
   **before** you start editing code. That commit is your claim.
2. When it is done and verified, set it to `FIXED — <your name>` and record the
   commit, plus the verbatim output proving it.
3. If you cannot finish, set it back to `OPEN` with a note. A silently
   abandoned claim is worse than no claim.

### One owner per path — this is where collisions will happen

Most of these bugs live in **the same two files**. Do not take two bugs from
the same row of this table in parallel with someone else:

| File | Bugs living there |
|---|---|
| `cli/src/tui/ui.rs` | BUG-1, BUG-3, BUG-4, BUG-11, BUG-16, BUG-20, BUG-21 |
| `cli/src/tui/mod.rs` | BUG-6, BUG-17 |
| `cli/src/tui/app.rs` | BUG-19 |
| `cli/src/main.rs` + `core/src/service.rs` | BUG-14 |
| `cli/src/tui/command.rs` | BUG-5, BUG-10 |
| `cli/src/tui/keys.rs` | BUG-7 |
| `core/src/rank.rs` | BUG-15 |

`ui.rs` holds seven of them. It is a 9,000-line file — **one agent should take
all seven together**, not seven agents in parallel.

### Suggested order

1. **[BUG-14](#bug-14)** — agents are running in `$HOME`. Nothing else matters
   until work lands where the user is standing. One line.
2. **[BUG-1](#bug-1)** — unblocks BUG-2 for free, and makes every other fix
   observable. Until this is fixed you cannot *see* most of the program's
   output, including your own fixes.
3. **[BUG-4](#bug-4)** — one status-bar field that would have caught BUG-14 in
   seconds.
4. **[BUG-20](#bug-20)**, **[BUG-6](#bug-6)** — small, self-contained, and both
   currently mislead the user about destructive or missing behaviour.
5. The remaining width bugs (BUG-3, 11, 16, 21) — one sweep, one owner, since
   they are one root cause (see Pattern B).

### Expect the TaskCompleted hook to block you — it is not your fix that broke

Marking a task complete runs the suites, and on **macOS** `tests/reclaim-disk.test.sh`
fails with **6 failures** regardless of what you changed:

```
touch: out of range or illegal time specification: YYYY-MM-DDThh:mm:SS[.frac][tz]
…
== 31 passed, 6 failed ==
```

That is BSD `touch` rejecting a GNU-style `-d` argument, so the fixture never
gets the old mtimes its assertions depend on — every failure is an
age/"is it stale" assertion. It is **host-dependent and pre-existing**; the
suite passes in CI. It has nothing to do with the TUI.

**Do not "fix" it by skipping tests, loosening assertions, or editing the
suite to go green** — the charter forbids exactly that, and you would be
deleting a real signal to unblock an unrelated task. Confirm against CI before
chasing it. If it genuinely blocks you, write `BLOCKED.md` as the charter
describes.

### Rules that apply to every fix here

- **Add the regression test, and make sure it fails first.** Four of these bugs
  were green in CI while broken (see Pattern A). A fix without a test that
  would have caught the original is not a fix.
- **Never weaken an existing test to go green.** If an existing test encodes
  the broken behaviour, say so in your report rather than deleting it.
- **Build with the shared target dir** — this worktree has no `target/` and the
  disk is tight:
  `CARGO_TARGET_DIR=/Users/reljodoreta/Developer/Repositories/Projects/Jod/target cargo build --release --bin jod`
- **Re-verify by hand.** Every finding here came from driving the real TUI, not
  from reading code. Drive it and confirm, using the harness described at the
  bottom of this file.

### In flight right now

Superseded by the [verification status](#verification-status--five-findings-have-now-merged-into-main-27a7072)
table above — that is the live one; keep it current rather than this note.
Summary as of `main` = `27a7072`:

- **Merged:** BUG-1, BUG-2, BUG-6, BUG-7, BUG-13, and BUG-14's display half.
- **PR open, CI-green:** #78 (BUG-4), #83 (BUG-15, BUG-16), #84 (BUG-14
  backend), #85 (BUG-5), #86 (BUG-3, BUG-11, BUG-20, BUG-21).
  **#86 is stacked on #83 — merge #83 first.**
- **With an agent:** BUG-17, BUG-18, BUG-19.
- **Unclaimed:** BUG-10, BUG-12.

**Dispatch note that proved out.** #86 took four `ui.rs` width bugs as a single
sweep through one shared helper, rather than four agents in one 9,000-line
file. That is the one-owner-per-path table above doing its job, and it is the
recommended shape for the rest.

### ⚠ Citations were verified against this branch — they are now stale against `main`

All 24 `file:line` citations were machine-checked and resolved correctly
against **this branch's tree**. That was before #80, #81 and #82 merged, and
those PRs changed `cli/src/tui/ui.rs`, `cli/src/main.rs` and
`cli/src/tui/keys.rs` — the files holding most of the citations. **Assume every
line number below is now off against `main` (`27a7072`)**, and re-run the check
before trusting one.

The **repros and root causes are still accurate**; only the line numbers drift.
Grep for the quoted code rather than jumping to the line.

An earlier revision of this file cited eleven line numbers read from a **stale
checkout of `main`** — an easy mistake in a repo with a worktree beside it, and
the reason this check exists. Verify with:

```bash
grep -oE '(cli/src/tui/[a-z_]+\.rs|core/src/[a-z_]+\.rs|cli/src/main\.rs)[:.]*[0-9]+' REPORT_BUGS.md \
  | sed 's/[:.]*\([0-9]*\)$/ \1/' | sort -u \
  | while read -r f l; do printf '%-26s %-6s %s\n' "$f" "$l" "$(sed -n "${l}p" "$f" | cut -c1-70)"; done
```

Line numbers are the fragile part of this document; the repros and root causes
are not.

---

## Two patterns underneath most of this

Twenty findings, but they are not twenty independent mistakes. Fixing the two
shapes below would prevent most of them recurring.

### Pattern A — a fixture supplies a precondition the real entry point never has

Four features are **broken in their default state and green in their tests**,
each because the test hands the code a starting state a user cannot produce:

| Bug | The fixture's lie | What a user actually has |
|---|---|---|
| [BUG-6](#bug-6) | `app.panel = true` | panel closed at startup — the key does nothing |
| [BUG-3](#bug-3) | path `/home/reljod/notes` (18 chars) | real paths overflow the 96-column cap |
| [BUG-20](#bug-20) | asserts only `"y confirms"` (10 chars) | the other 26 chars are clipped away |
| [BUG-1](#bug-1) | *no test at all* for a notice-only command at startup | every such command renders nothing |

`docs/try-it.md:16` already warns that "a green suite is not evidence that a
feature exists". These are that warning, four times, in the module the doc says
was never hand-driven.

**The cheap structural fix:** for any keybinding or overlay, assert *something
observable* starting from the state a fresh `jod tui` actually produces —
`App::default()`, no panel, no `watching`, empty transcript. Every bug above
dies to that one rule.

### Pattern B — a width computed from content, ignoring the chrome around it

Four defects are the same arithmetic error: size a box from its text, then draw
a longer thing into it, and let the terminal clip mid-word with no ellipsis.

| Bug | Sized from | What gets clipped |
|---|---|---|
| [BUG-3](#bug-3) | fixed `.min(96)` | the directory you are browsing |
| [BUG-11](#bug-11) | popup width | half the command descriptions |
| [BUG-16](#bug-16) | popup width | the filename — the only distinguishing part |
| [BUG-20](#bug-20) | `question.len() + 8` | the words "undone" and "else cancels" |
| [BUG-21](#bug-21) | `room` computed, then unused | the filename *and* the `+N -M` counts |

Notably the code **already knows how to do this correctly** — the transcript
wraps notices properly, and the search overlay elides with a real `…`. The
helper exists; these four sites do not use it.

**The fix:** size to `max(content, title, footer)`, and elide **from the left**
for paths, where the tail carries the meaning.

---

<a name="bug-14"></a>
## BUG-14 — A delegated run wrote into `$HOME`, outside every root, and reported success · **Critical** · **HALF MERGED**

> **Two halves, two PRs.** The **display** half merged via #82 (`27a7072`):
> the delegation confirmation and the fleet detail now name the run's cwd. The
> **backend** half — the card raised when a run writes outside every declared
> root — is **still open as #84**. The launch-directory fix itself is #78.
>
> **This is the dangerous in-between state:** the cwd is now visible, so it
> looks handled, while nothing yet objects when work lands outside every root.
> Check both halves together, not either alone.

This is the finding that matters most, and it is the one the whole exercise was
for. It is also **not** a one-off: your own run history shows the same failure.

**What I did**, entirely through the TUI:

1. `/add-dir tetris` → `⏎`, adding
   `…/worktrees/tui-dogfood-tetris/tetris` as a root. Verified stored:
   `jod root ls` → `read-only human /…/tui-dogfood-tetris/tetris`.
2. Typed: *"Build a working Tetris game **in the tetris directory** using
   Node.js, Vite and HTML…"*
3. `Ctrl-B` to delegate.

**What happened.** The run completed and Jod recorded it as success:

```
$ jod ls
87e84b92   done   Claude Code  Build a working Tetris game
```

The fleet shows `✓ done · $1.1813 · 17425 out`. But the root I added is
**empty**:

```
$ ls -la …/tui-dogfood-tetris/tetris/
total 0          # nothing. not one file.
```

The game was written **to the home directory instead**:

```
$ ls /Users/reljodoreta/tetris/
dist  index.html  node_modules  package.json  pnpm-lock.yaml
pnpm-workspace.yaml  src/
```

The agent's own closing report confirms it plainly: *"`pnpm install && pnpm dev`
in `/Users/reljodoreta/tetris` starts it."* It also ran `pnpm install` there,
so a `node_modules/` tree landed in `$HOME` as well.

**This recurs.** The `tetris-rust` run already in your history ends with:
*"Everything is at `/Users/reljodoreta/tetris-rs`; nothing else under your home
directory was touched."* Same instruction shape, same outcome, different
session — the agent resolves a bare directory name against `$HOME` rather than
against the root or the launch directory.

**Root cause — found, and it is a one-line inconsistency between two entry
points.** The agent did not "guess" `$HOME`. **Jod launched it there.** Every
run started from the TUI records `$HOME` as its working directory:

```
$ sqlite3 ~/.jod/jod.db "select substr(id,1,8), cwd, substr(name,1,26) from runs order by created_at_ms desc"
7de30036|/Users/reljodoreta|say hi in one word
87e84b92|/Users/reljodoreta|Build a working Tetris gam     <-- the Tetris run
715fb69c|/Users/reljodoreta|continue
```

— even though `jod tui` was itself launched from the worktree, and a root had
been added. The chain:

`cli/src/main.rs:1786` builds the TUI options with

```rust
cwd: cwd.unwrap_or_else(jod_core::service::default_cwd),
```

and `core/src/service.rs:1038` is:

```rust
pub fn default_cwd() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
}
```

**The TUI defaults an agent's working directory to `$HOME`.** `jod run` does
not — `cli/src/main.rs:3343`, `:3754` and `:3862` all use
`cwd.unwrap_or(std::env::current_dir()?)`. Two entry points into the same
system, opposite defaults, and only the CLI one matches where the user is
standing.

Given cwd `$HOME`, the agent behaved **correctly**: "the tetris directory"
resolved to `~/tetris`. The instruction was fine; the working directory it was
interpreted against was wrong before the agent ever read it.

Three things then combine to make it invisible:

- **BUG-4** — the TUI never displays the cwd, so the mismatch cannot be seen.
- *"Roots are a convention, not a sandbox"* (`docs/try-it.md:203`) — nothing
  **stops** a write outside a root, by design.
- Roots are read-only until `claim_worktree`, and `tetris/` was an empty
  non-git directory, so there was no worktree to claim even if it had tried.

**A second, quieter casualty.** Because the workspace is `$HOME`, Claude Code
refuses to trust it, and says so mid-transcript:

> `Ignoring 3 permissions.allow entries from .claude/settings.json: this
> workspace has not been trusted. … set projects["/Users/reljodoreta"].hasTrustDialogAccepted`

So the project's own permission settings are **silently discarded** on every
TUI run — and the `projects["/Users/reljodoreta"]` in that message is the
same bug showing through from the harness's side.

**Workaround available today** (verified): launch with an explicit directory —

```
jod tui --cwd "$PWD"
```

Verified for **both** paths — a main-chat turn (`⏎`) and a background
delegation (`Ctrl-B`) both inherit it:

```
cbe879eb|/…/worktrees/tui-dogfood-tetris/tetris|echo hello from a delegate   <-- Ctrl-B
9ad3d21f|/…/worktrees/tui-dogfood-tetris/tetris|reply with the word ok       <-- Enter
```

So `--cwd` is a complete workaround until the default is fixed. Worth telling
users now: **launch `jod tui` with `--cwd "$PWD"`, or your agents are working
in your home directory.**

**Why this is Critical rather than High.** The failure is silent in both
directions. The agent believes it succeeded, Jod's records agree (`✓ done`,
$1.18 spent), the fleet shows a green check — and the directory the user
actually pointed at is untouched. Nothing in the TUI would ever tell you. A
user who trusts the green check has a repo that never received the work and an
unrelated tree in `$HOME` that silently did.

**Suggested fixes**, in order of value:

1. **Make the TUI default to `std::env::current_dir()`, as `jod run` already
   does.** This is the actual fix and it is one line at `cli/src/main.rs:1786`.
   `$HOME` is a defensible default for a resident service with no launch
   context (the phone bridge, the daemon) — but `jod tui` is typed into a
   terminal that is standing in a directory, and that directory is the answer.
   If `default_cwd` must stay for the service paths, the TUI should stop
   calling it.
2. **Display the cwd** — in the status bar (BUG-4) and in the delegate
   confirmation (BUG-2). Either one would have exposed this in seconds.
3. **Warn when a run's writes all land outside every declared root.** The
   supervisor sees the events; a run that reports `done` having touched nothing
   inside any root deserves a card, not a green check.
4. **Add a test that the TUI's default cwd is the launch directory.** There is
   no coverage of this today, which is why an entry point could default to
   `$HOME` without anything going red.

---

<a name="bug-1"></a>
## BUG-1 — A fresh session silently swallows every notice-only command · **Critical** · **MERGED (#82)**

> **Fixed in `main` via #82 (`27a7072`)** — reported by the maintainer, not
> re-driven by me. The repro below is kept verbatim as the regression
> reference: `/root` on a cold session must print something.

This is the highest-value finding in this report, and it is the root cause of
BUG-2. It makes the TUI look broken on the *first* thing a new user does.

**Repro** — from a cold `jod tui`:

```
/root      ⏎
```

**Expected:** the roots list, or `no roots — /add-dir picks one (Alt-P)…`.
**Actual:** absolutely nothing. The splash stays up, the input clears, no
notice, no error. The command *did* run.

Proof it ran: `/add-dir tetris` → `⏎` also renders nothing, yet
`jod root ls` from a shell immediately shows the root was stored correctly:

```
read-only  human  /…/tui-dogfood-tetris/tetris
```

**Root cause.** `cli/src/tui/ui.rs:1173`:

```rust
fn fresh(app: &App) -> bool {
    app.watching.is_none()
        && !app.transcript.iter().any(|entry| !matches!(entry, Entry::Notice(_)))
}
```

A session counts as "fresh" while the transcript holds **nothing but
`Entry::Notice`**, and `fresh()` makes `draw_splash` (`ui.rs:112`) take the
column — which paints over the transcript. But `Action::ListRoots`
(`cli/src/tui/mod.rs:1289`) emits **only** `Entry::Notice`:

```rust
for line in lines {
    app.push(Entry::Notice(line));
}
```

So the notice is pushed, `fresh()` is still true, the splash still owns the
screen, and the output is never seen. The command is correct; the renderer
hides it.

**Proof, caught live.** After sending one real prompt — which puts a
non-`Notice` entry in the transcript and makes `fresh()` false — every notice
that had been suppressed appeared *retroactively*, including the `/add-dir`
confirmation from many minutes earlier:

```
• Ctrl-G opens every screen · / for commands · Enter send · Ctrl-B delegate…
• added /Users/reljodoreta/tetris — read-only, as every root is until something claims it
› say hi in one word
```

That second line is the confirmation I was told did not exist. It was in the
transcript the whole time, behind the splash. Nothing was lost — only hidden,
and only during the exact window where a new user is forming their first
impression of whether the program works.

**Blast radius.** Every command whose entire output is notices is invisible
until something non-notice enters the transcript. Confirmed dead on arrival at
startup: `/root`, `/add-dir` (its confirmation), and the delegation notice
(BUG-2). Any `/config`, `/memory`, `/sessions`, error text and card
confirmations that push only notices are in the same class — worth auditing
every `Entry::Notice` producer.

The irony: the doc comment above `fresh()` is careful to explain why "the
transcript is empty" was the *wrong* test — but the replacement condition
swallows real output rather than just startup hints.

**Suggested fix.** `fresh()` should distinguish the *startup hint* notices from
notices produced by a user action — e.g. tag the entry (`Entry::Notice` vs a
new `Entry::Output`), or track "has the user run anything" explicitly. Do not
simply drop the splash on any notice, or the startup hint alone would kill it.

---

<a name="bug-2"></a>
## BUG-2 — delegate gives almost no confirmation · **High** · **MERGED (#82)**

> **Fixed in `main` via #82**, exactly as predicted below: it was never a
> missing message, only a hidden one, so fixing BUG-1 restored it. #82 also
> adds the **cwd** to the confirmation — the field this report argued for,
> and the one that would have exposed BUG-14 in seconds.

You suspected "delegate task does not spawn". **It does spawn.** The bug is
that the UI barely admits it, which is indistinguishable from failure — and
when the run then writes to the wrong place (BUG-14), the silence is what stops
you noticing.

**Repro:** type a prompt, press `Ctrl-B` (`Alt-B` before #75).

**Actual:** the input clears, the splash stays up, the transcript stays empty,
and the *only* feedback anywhere on screen is a suffix appended to the status
bar at the very bottom:

```
● auto · Claude Code · ready · 1 in background
```

No "delegated", no agent id, no target directory, no indication of *what* was
delegated. On a 50-row terminal that is a 17-character change at the bottom
edge, while the centre of the screen still shows the "tell Jod what to do"
splash.

**It really did spawn** — confirmed independently:

```
$ jod ls
87e84b92   running   Claude Code  Build a working Tetris game
```

and the fleet shows `● 87e84b92 running 20s cc Build a working Tetris game`.

**Root cause — confirmed by experiment: this is entirely a symptom of BUG-1.**
I re-ran the same `Ctrl-B` in a session that already had a real turn in its
transcript, and the confirmation is *good*:

```
• delegated cbe879eb — echo hello from a delegated · runs in the background, Ctrl-F to watch
```

Agent id, the prompt, where it went, and which key follows it. Nothing needs
writing — the message already exists and is well judged. It is a
`Entry::Notice`, so at startup the splash eats it (BUG-1), and startup is
exactly when a first-time user presses `Ctrl-B`.

**So: fix BUG-1 and BUG-2 goes away.** Whoever takes BUG-1 should verify this
case as their regression test — it is the highest-consequence instance of it.

**One thing still missing from that message**, independent of BUG-1: it does
not name the **working directory** the run was launched with. Had it said
`in /Users/reljodoreta`, BUG-14 would have been caught in the first thirty
seconds instead of after a $1.18 run wrote a project into the wrong tree.

**Suggested fix.** Fix BUG-1 (which restores the existing message), then add
the working directory to it. Do not rewrite the message otherwise — it is
already the right message.

---

<a name="bug-3"></a>
## BUG-3 — The directory picker cannot tell you which directory it is showing · **High** · OPEN

This is the concrete form of "directory unclear".

**Repro:** `/add-dir tetris` → `⏎`.

**Actual header, at 200 columns *and* at 260 columns — identical:**

```
  in /Users/reljodoreta/Developer/Repositories/Projects/Jod/.claude/worktrees/tui-dogfood-tetr
```

The path is cut mid-word (`tui-dogfood-tetr`) with **no ellipsis**, so nothing
signals that text is missing. The real base was
`…/tui-dogfood-tetris/tetris` — a *different directory* from the one the
truncated string implies.

This actively cost me time during this run: `/add-dir tetris` listed only `.`
while `/add-dir` listed 30+ directories, and I initially read that as the
argument being broken. It was not — the `tetris` dir is simply empty, and the
behaviour was correct. **The truncated header made correct behaviour
indistinguishable from a bug.**

**Root cause.** `cli/src/tui/ui.rs:2300`:

```rust
let width = (screen.width.saturating_sub(8)).min(96).max(40);
```

The picker is capped at **96 columns no matter how wide the terminal is**, and
the header at `ui.rs:2185` is a plain `Line` with no truncation strategy:

```rust
format!("  in {}", p.base.display()),
```

so ratatui clips it at the panel edge.

**Why the test suite missed it.** `ui.rs:8979`
`the_full_screen_picker_names_the_tree_it_is_walking` asserts the header is
present — using the path `/home/reljod/notes` (18 chars) at width 120. Any
realistic path (worktrees, monorepos, nested projects) overflows. The test
proves the function, not the feature.

**Suggested fix.** Elide from the **left** (`…/tui-dogfood-tetris/tetris`) —
the tail is the informative end of a path — and let the picker use the
terminal's real width.

---

<a name="bug-4"></a>
## BUG-4 — The working directory is shown nowhere in the chat UI · **High** · OPEN

There is no indication anywhere on the main screen of which directory the
session operates in. The status bar reads:

```
● auto · Claude Code · ready
```

— mode, harness, state, and nothing about *where*. The splash says nothing.
The input box says nothing. `/root` is the only way to ask, and at startup it
renders nothing (BUG-1), so at the moment a new user most needs the answer it
is unobtainable from inside the TUI.

For a program whose core action is delegating file-modifying agents, "which
directory am I about to change?" should never require a command.

**This is not cosmetic — it is what let BUG-14 through.** The answer the TUI
declines to show was `$HOME`, and it was wrong. A single line of status bar
would have made a critical bug self-evident on the first run instead of after
a $1.18 agent had written a project into the wrong tree.

**Suggested fix.** Put the working directory (elided from the left) in the
status bar beside the harness and mode, where `● auto · Claude Code · ready`
already sits.

---

<a name="bug-5"></a>
## BUG-5 — A project cannot be created or cited from the TUI · **High** · OPEN

This is "project cannot be cited", and it is real.

`Shift-Tab` opens a panel headed:

```
┌ projects · none set ───────────┐
│ ▸ nothing set                  │
└────────────────────────────────┘
```

There is **no way to populate it from the TUI**:

- No `/project` command. `grep -n "project" cli/src/tui/command.rs` → **zero
  matches**; it is absent from the `/` list entirely. Re-checked after #75:
  still zero.
- The `Ctrl-G` menu has a `projects` entry, but it only *toggles visibility* of
  the catalog — it cannot add to it, and from a cold start it does nothing at
  all (BUG-6).
- The panel's own empty state, `nothing set`, names no remedy.

The capability exists everywhere *except* the TUI: `core/src/projects.rs:436`
has `add_project`, and the CLI has a full `jod project add|ls|archive|restore`
(`cli/src/main.rs:313`). You must quit the TUI, or open a second terminal, and
run `jod project add` to make the panel non-empty.

**Every other empty state in the program names its own remedy.** I swept them
all:

```
roots        no roots — /add-dir picks one (Ctrl-P), and `@` says so until there is
memory       nothing remembered yet — /remember writes one
tasks        the board is empty — n adds a task, /todo does too
schedules    nothing scheduled yet — n makes one, /new schedule too
goals        no goals yet — n makes one, /new goal too
webhooks     no webhooks yet — n makes one, /new hook too
activity     nothing has happened yet — cron, hooks and goals write here
team         no team — start one with `jod tui --team <name>`
rail         nothing waiting — no agent has asked anything

projects     nothing set                    <-- the only one that just stops
```

The convention is established and followed nine times out of ten. Projects is
the single outlier, and it is the one where the remedy is *not* guessable,
because it lives outside the TUI entirely (`jod project add`, from a shell).

The cost is stated in Jod's own CLI help (`cli/src/main.rs:309`): *"until a
repository is listed, saying 'let's fix this' has nothing to resolve to and
every instruction about it has to spell the path out."* So an empty catalog
degrades every instruction — and the TUI gives no way to fill it.

**The display side works — only creation is missing.** I registered one from a
shell (`jod project add ~/tetris --name tetris`) and the panel picked it up
immediately, listing `tetris`. So the panel is not broken; it is unreachable
from the only surface the user is sitting in. (I archived the entry afterwards
to leave the catalog as I found it — `jod project restore tetris` brings it
back.)

Two smaller things found while confirming that:

- **The header says `projects · none set` even while listing a project.**
  `cli/src/tui/ui.rs:1451` derives the title from `app.current_project` — which
  project is *active* — not from whether the catalog has entries. Technically
  correct, but "none set" doing double duty for "catalog is empty" and "no
  current project" reads as a contradiction once a project is listed beneath
  it. Worth distinguishing: `· none current` vs `· none catalogued`.
- **Even the CLI's empty state names its remedy** — `no projects yet —
  \`jod project add .\` catalogs the repository you are in` — which makes the
  TUI's bare `nothing set` the only place in the whole program that leaves the
  user without a next step.

**Suggested fix.** Add `/project add|ls`, and make the empty state say how to
fix itself — the sentence the CLI already prints would do.

---

<a name="bug-6"></a>
## BUG-6 — projects toggle is a silent no-op unless an undiscoverable panel is open · **High** · **MERGED (#81)**

> **Fixed in `main` via #81 (`2c0963f`)** — the projects key now opens the
> panel from a cold start. Regression reference: press it from
> `App::default()`, with no panel already open, and something must appear.
> That missing precondition (see [Pattern A](#pattern-a--a-fixture-supplies-a-precondition-the-real-entry-point-never-has))
> is what hid this through an entire refactor.

> **Re-verified after #75 — still broken, and now worse.** The binding moved
> from `Alt-D` to `Ctrl-G d` and was *promoted* into the workspace menu, where
> it is now a described, first-class entry: `d  projects  show or hide the
> catalog`. It still does nothing. Severity raised from Medium: a chord nobody
> can find failing quietly is bad; a menu item the program advertises to every
> new user failing quietly is worse.

**Repro:** from a cold `jod tui`, press `Ctrl-G`, then `d`.

**Actual:** nothing. No panel, no message, no change of any kind. (Pre-#75 the
same was true of `Alt-D`.)

**Root cause.** `cli/src/tui/mod.rs:2940` toggles `app.projects_open`:

```rust
'd' => {
    app.overlay = Overlay::None;
    app.projects_open = !app.projects_open;
    None
}
```

The handler's own comment gives the mistaken assumption away — *"Collapse the
catalog **without closing the whole panel**"* — it is written for the case
where the panel is already open.

but the projects catalog only renders inside the side panel, which is gated on
`app.panel` — opened by `Shift-Tab` (BUG-7), and `false` at startup. So the key
flips a flag that draws nothing, and says nothing about why.

**Why the test suite missed it.** `cli/src/tui/mod.rs:6393`
`the_catalog_is_collapsed_without_closing_the_panel` opens with:

```rust
let mut app = app_on(HarnessKind::ClaudeCode);
app.panel = true;                 // <-- the precondition a real user never has
assert!(app.projects_open);
```

It sets the panel open *first*, so it only ever exercises the state where
`Alt-D` is meaningful. The default state — the one every user starts in — is
never tested. This is precisely the failure mode `docs/try-it.md:16` warns
about.

**Suggested fix.** Have the projects key **open the panel when it is closed**
(`app.panel = true` alongside `projects_open = true`). The notice route —
"press Shift-Tab first" — is worse, and would be invisible anyway until BUG-1
is fixed.

**Regression guard worth adding:** assert the key does something observable
starting from `App::default()`, i.e. without pre-setting `app.panel`. That one
missing precondition is what hid this through an entire refactor.

**Ruled out: this is not "the catalog is empty".** I registered a real project
(`jod project add ~/tetris`) and retried from a cold start — `Ctrl-G d` still
renders nothing, while `Shift-Tab` immediately shows the panel *with* `tetris`
in it. The gate is `app.panel`, confirmed.

---

<a name="bug-7"></a>
## BUG-7 — `Shift-Tab` is undocumented, and it is the only way in · Medium · **MERGED (#81)**

> **Fixed in `main` via #81** — `Shift-Tab` is now advertised rather than
> discoverable only from the border of the panel it opens.

`Shift-Tab` opens the side panel holding **projects, sessions, mode, harness,
spend and context usage** — a large fraction of the program's state.

**Re-verified after #75: unchanged.** It appears in **neither** the `?` keymap
(which the `Ctrl-G` menu bills as "the whole keymap" — no `Shift-Tab` in it)
**nor** the `Ctrl-G` menu, even though that menu gained six other entries in
the same PR. The only place it is written down is the bottom border of the
panel itself:

```
└ Shift-Tab closes ──────────────┘
```

— which you can only read *after* you have already discovered the key. I found
it by reading source, not by using the program.

**Suggested fix.** Add `Shift-Tab` to `GLOBAL` in `cli/src/tui/keys.rs`, and to
the `Ctrl-G` menu. #75 freed several rows from the overlay by moving verbs into
the menu, so the row-budget objection that used to apply has largely gone
away — and a key that opens six panels earns its row more than most of the ones
still present.

---

<a name="bug-8"></a>
## BUG-8 — Keymap overlay: the key label collides with its description · ~~Medium~~ · **FIXED by #75**

> **Re-verified after the rebase: fixed.** The overlay now renders
> `Ctrl-A/E/Home/End start / end of the line` with a proper gap, and the
> hardcoded `{:<12}` pad is gone from `ui.rs`. Kept here because the *class* of
> bug (a fixed-width column that pads but never truncates) still applies to
> BUG-3 and BUG-11, which remain open.

**Original repro (pre-#75):** press `?`. Read the fifth row from the bottom:

```
  Ctrl-A/E/Home/Endstart / end of the line
```

`End` and `start` are glued together. Every other row is aligned; this one
reads as a typo in the program.

**Root cause (in the pre-#75 tree; both sites are gone now).** `ui.rs` padded
to a *minimum* of 12 and never
truncates:

```rust
Span::styled(format!("  {:<12}", binding.key), fg(WARN)),
```

The label — still present, now at `cli/src/tui/keys.rs:224` — is
`"Ctrl-A/E/Home/End"`, 17 chars, so
it overruns the column and eats the gap. `keys.rs:723` states the design
constraint explicitly ("the overlay has twelve columns for a key"); this label
is the one that breaks it.

**Suggested fix.** Either widen the column to fit the longest label
(computed, not hardcoded), or split the row. Note `press_of` splits on `/` and
the drift test replays printed labels as real keypresses, so **the label text
is load-bearing** — shortening it to `Ctrl-A/E` would silently stop
advertising `Ctrl-Home`/`Ctrl-End`. Widen the column; do not trim the label.

---

<a name="bug-9"></a>
## BUG-9 — The splash claims the menu "opens every screen" · ~~Medium~~ · **largely FIXED by #75**

> **Re-verified after the rebase: mostly fixed.** The caption is now `Ctrl-G
> opens every screen`, and `Ctrl-G` genuinely gained the entries it was missing
> — **editor, jobs, unread, clear, projects and search** are all in the menu
> now. This was a real improvement.

**Original finding (pre-#75).** The caption read `Alt-K opens every screen`,
but projects, the rail, background shells, transcript search, delegate and the
side panel were all reachable only by chord and absent from that menu.

**What remains.** Two gaps survive:

- **`Shift-Tab` is still absent** from the menu and the keymap — see BUG-7.
  It is the single biggest one left, because it opens six panels.
- **`projects` is in the menu but does not work** from a cold start — BUG-6.
  Being listed and being reachable are not the same thing, and this entry is
  currently listed without being reachable.

---

<a name="bug-10"></a>
## BUG-10 — `/main` is listed twice with two different meanings · Low · OPEN

In the `/` list:

```
/main   go into the main chat — the pinned one
/main   send it one instruction and stay where you are
```

Two rows, same token, opposite behaviours (navigate vs. send-and-stay), with
nothing shown to distinguish them — presumably bare vs. with-argument, but the
list does not say so. Arrowing onto either gives the same completion.

**Suggested fix.** Render them as `/main` and `/main <instruction>`.

---

<a name="bug-11"></a>
## BUG-11 — Command descriptions are cut mid-word with no ellipsis · Low · OPEN

In the `/` list, at 200 columns, several descriptions simply stop:

```
/model      set the model for this conversation; no argument restore
/add-dir    pick a folder this session can work in and `@` — a path
/heartbeat  reap it if it goes silent — for runs you leave alone for
/update     build and install the newest patch of Jod; 'check' just
```

`restore` → `restores`, and the rest are mid-sentence. With no `…` there is no
signal that text is missing, so each reads as a sentence the author forgot to
finish. Same class as BUG-3: hard clip, no ellipsis.

---

<a name="bug-12"></a>
## BUG-12 — The input box is fixed at ~70 columns and single-line · Low · OPEN

On a 260-column terminal the input box still occupies ~70 columns, wasting
three quarters of the width. It is also single-line with horizontal scrolling:
a 200-character delegation prompt is only ever ~68 characters visible, and you
cannot see the whole thing before committing it with `Alt-B`.

Given that `Alt-B` spends money and runs unattended, not being able to read
your own prompt before sending it is a poor trade. `Alt-F` ($EDITOR) is the
mitigation, but it is a detour for something the box could show.

---

<a name="bug-13"></a>
## BUG-13 — `jod --version` cannot distinguish two different builds · Medium · **MERGED (#80)**

> **Fixed in `main` via #80 (`0ca9f2e`)** — `jod --version` now stamps the
> commit. This retires a whole category of phantom bug report: at the start of
> this session 58 source files were newer than the binary, and nothing in the
> program's own output revealed it.

`~/.local/bin/jod` is a **copy**, not a symlink, so rebuilding the repo does
not update the binary on `PATH`. Both report the same version:

```
$ jod --version                     # ~/.local/bin/jod  (installed copy)
jod 0.1.0
$ ./target/release/jod --version    # freshly built
jod 0.1.0
$ md5 -q ~/.local/bin/jod ./target/release/jod
0590e2c60690581c663f50a4c9323e9e    # different
d85e2a5a219895566c19e48e78b6082f    # binaries
```

This is not academic — it changes what the program *does*. The stale binary's
`jod --help` listed **27** subcommands; the fresh one lists **34**. `card`,
`root`, `secret`, `commands`, `work`, `project` and `voice` simply did not
exist in the installed build. A user running `jod project add` on the installed
binary gets "unrecognised subcommand" for a feature that is documented and
shipped.

At the start of this session, 58 source files were newer than
`target/release/jod`. Any TUI bug reported against an unrebuilt tree is
suspect, and nothing in the program's own output reveals it.

**Suggested fix.** Put the git SHA and build timestamp in `--version`
(`git describe --always --dirty` at build time via `build.rs`). Cheap, and it
retires a whole category of phantom bug reports.

---

<a name="bug-17"></a>
## BUG-17 — Interrupting a turn is unacknowledged for seconds, then reported twice, contradictorily · Medium · OPEN

> **Correction — I got this wrong first.** My initial write-up said "`Esc` does
> not interrupt at all". That was **false**, and I am leaving the correction
> visible because the file is being acted on. `Esc` *does* interrupt. I checked
> four seconds after pressing it, saw `⠹ working`, and concluded it was dead.
> A clean re-run — `Esc` only, never `Ctrl-X`, sampled every two seconds —
> shows it landing between **t+4s and t+6s**. Do not "fix" a dead `Esc`; it
> is not dead. What is below is what is actually wrong.

**Repro:** in the main chat, run `run the shell command: sleep 120; then say
finished`, wait for `⠧ working`, press `Esc` once, and sample the status bar.

**Actual:**

```
before Esc:  … ⠧ working 10s
t+2s      :  … ⠧ working 10s        <-- no acknowledgement
t+4s      :  … ⠧ working 10s        <-- still nothing
t+6s      :  … ready                <-- lands here
```

Two defects, both in the reporting rather than the mechanism:

**1. No acknowledgement for 4–6 seconds.** The status bar keeps saying
`working`, and the elapsed counter **freezes** at the value it had when `Esc`
was pressed (`10s`, unchanging). So the one visual cue that something happened
is a timer that has stopped — which reads as a hung UI, not as an interrupt in
progress. There is no `interrupting…` state. A user who presses `Esc` and sees
nothing for four seconds presses it again, or reaches for `Ctrl-C`.

**2. The turn is then reported twice, and the two disagree:**

```
✓ done · interrupted after 10s
• stopped — the conversation is kept, so just say what to do instead
✗ failed · 0 out · $0.0000
```

A green `✓ done` and a red `✗ failed` for the same turn, adjacent. The
database gets it right — `sqlite3 ~/.jod/jod.db` records the run as `killed` —
so this is purely the transcript rendering both the interrupt entry and a
generic failure entry when the harness process ends.

The comment at `cli/src/tui/mod.rs:4156` states the intent exactly, and it is
the right intent — *"A partial turn silently dropped would leave the transcript
claiming the agent simply stopped talking, and the next reader cannot tell an
interruption from a crash."* The interrupt entry does that job. The trailing
`✗ failed` then undoes it.

**Suggested fix.** Show an `interrupting…` state on the keypress so the input
is acknowledged immediately, and suppress the generic terminal entry when the
turn already has an interrupt entry — a deliberate stop is not a failure.

See [BUG-18](#bug-18) for the false "would not stop" warning that accompanies
every interrupt.

---

<a name="bug-18"></a>
## BUG-18 — Every interrupt prints a false "would not stop" warning, worded as a start failure · ~~Medium~~ · **FIXED by #87** (`8ffcdd5`)

> **Verified fixed in the merged tree**, and fixed exactly as suggested — both
> halves:
>
> - **The self-contradicting wording is gone.** `core/src/error.rs:23` now has
>   a dedicated `Kill(String)` variant, so a failure to *stop* no longer
>   reports itself as a failure to *start*. `core/src/service.rs:885` raises
>   `JodError::Kill(format!("process group {pgid}: {e}"))`.
> - **The false alarm is gone.** `core/src/proc.rs:75` now treats `ESRCH` — the
>   group having already exited — as success, with a test named
>   `signalling_a_group_that_is_already_gone_is_success` (`proc.rs:285`) and
>   another at `proc.rs:330` whose comment reads *"The whole of BUG-18:
>   stopping a run that has already ended must be a success"*.
>
> The `it may still be writing` sentence still exists at
> `cli/src/tui/mod.rs:1184`, which is correct — it is now reachable only when a
> stop genuinely fails, rather than on every interrupt.
>
> The repro below is kept as the regression reference.

**Repro:** interrupt any running turn (`Esc` or `Ctrl-X`). Observed on **2 of
2** interrupts, with different pgids — this is systematic, not a one-off.

**Actual:**

```
• the run would not stop (could not start the agent: could not stop
  process group 37237: Operation not permitted (os error 1))
  — it may still be writing; Ctrl-X kills it outright
```

Two separate defects in one line.

**1. The claim is false, and it is the alarming kind of false.** The run *had*
stopped. `ps -p 7003` showed `<defunct>` — a reaped zombie — and no `sleep`
process survived either interrupt. Nothing leaked. Yet the user is told the
agent "may still be writing", which for a file-modifying agent is the most
worrying sentence the program could produce, and is told to press a key that
will do nothing.

The cause looks like a race with an already-dead process group: by the time
`terminate_group` fires, the group leader is a zombie and the `killpg` fails
rather than reporting "already gone".

**2. The wording contradicts itself twice.** `core/src/service.rs:881`, inside
`kill_agent`, wraps a *stop* failure in the *spawn* error variant:

```rust
proc::terminate_group(pgid, KILL_GRACE).await.map_err(|e| {
    JodError::Spawn(format!("could not stop process group {pgid}: {e}"))
})?;
```

and `core/src/error.rs:14` renders that variant as:

```rust
#[error("could not start the agent: {0}")]
Spawn(String),
```

So a failure to **stop** is reported as a failure to **start**, nested inside a
message that says the run "would not stop". Three contradictory claims in one
sentence, in the one message a worried user reads most carefully.

**Suggested fix.** Treat an already-dead or already-reaped process group as
success — the goal state is "not running", and it is. Add a `JodError::Kill`
variant (or make it `Invalid`) so a stop failure stops claiming to be a spawn
failure. Only warn about "may still be writing" when processes genuinely
survive the grace period, which is a condition worth checking rather than
assuming.

---

<a name="bug-21"></a>
## BUG-21 — The diff header pushes its own line counts off the screen · Medium · OPEN

`docs/try-it.md:252` promises: *"File edits render as diffs with the path as a
header **and counts**."* The counts are not there.

**Repro:** ask the agent to create a file. The diff header renders as:

```
  ± /Users/reljodoreta/Developer/Repositories/Projects/Jod/.claude/worktrees/tui-dogfood-tetri
    +# Tetris
    +
    +- Released in 1984 by Alexey Pajitnov, written on an Electronika 60 at the Soviet Acade…
```

Hard-clipped mid-path, no ellipsis. **Both** informative parts are gone: the
filename (`NOTES.md` — the whole point of a path header) and the `+6 -0` counts
that were supposed to follow it.

**Root cause.** `cli/src/tui/ui.rs:4815`:

```rust
let room = (width as usize).saturating_sub(6);
let mut lines = vec![Line::from(vec![
    Span::styled("  ± ".to_string(), fg(WARN)),
    Span::styled(edit.path.clone(), bold(AGENT)),          // full absolute path, never trimmed
    Span::styled(format!("  +{} -{}", edit.added(), edit.removed()), fg(MUTED)),
])];
```

`room` is computed and then **not applied to the header** — only to the body
lines below. So the path span runs to whatever length it happens to be, and
anything after it, including the counts, is pushed past the right edge and
clipped. Any absolute path longer than the panel loses the counts, which in a
worktree or monorepo is all of them.

The tell is one line above it on screen: the tool's own output line *does*
elide properly with `…`. The helper exists; this header does not use it.

The doc comment directly above explains why the path is a header rather than a
per-line prefix — *"repeated down forty rows it would cost the width the code
needs"* — which is exactly right, and exactly the reasoning that should also
have bounded the header itself.

**Suggested fix.** Elide the path from the **left** to `room`, keeping the
filename, and reserve the width the counts need before laying the path out —
`± …/tui-dogfood-tetris/NOTES.md  +6 -0`. Fifth instance of Pattern B.

**Re-verified after the rebase onto `f3aaf45`**, with a second file and a
second run, at 200 columns:

```
⚙ Write ·
  ± /Users/reljodoreta/Developer/Repositories/Projects/Jod/.claude/worktrees/tui-dogfood-tetri
```

`HELLO.md` is nowhere on screen, and neither is `+1 -0`. Still open.

**One extra detail worth folding into the same fix:** the tool line above the
header renders as `⚙ Write ·` — a trailing separator with nothing after it.
A few lines later the same tool renders as plain `⚙ Write`, with no separator.
So the dangling `·` is not a truncated value, it is a separator emitted for a
field that is empty. Small, but it is on the same two lines a reader looks at
to answer "which file did it just change?", and right now those two lines
answer: a bullet, and a path with the filename cut off.

---

<a name="bug-20"></a>
## BUG-20 — The irreversible-action dialog truncates its own warning and its own instructions · **High** · OPEN

Of every place in this program to clip text, this is the worst one.

**Repro:** `/forget tetris` (or any `Confirm` overlay — hooks, schedules, works
all use it).

**Actual, at 200 columns — the whole dialog:**

```
┌ this cannot be undo┐
│                    │
│  forget tetris?    │
│                    │
└ y confirms · anythi┘
```

The warning reads **"this cannot be undo"**. The instructions read
**"y confirms · anythi"** — the user is not told what cancels, and is left
looking at a half-word on a dialog that destroys data.

**Root cause.** `cli/src/tui/ui.rs:2628` sizes the panel from the question
alone, ignoring its own border titles:

```rust
let panel = centred(f.area(), (question.chars().count() + 8) as u16, 5);
```

For `forget tetris?` that is 14 + 8 = **22 columns**. The titles set six lines
later are:

- `" this cannot be undone "` — **23** chars
- `" y confirms · anything else cancels "` — **36** chars

Both exceed 22, so ratatui clips them at the border. **The severity scales
inversely with the name being destroyed**: the shorter the thing you are
deleting, the more of the warning disappears. `/forget x` gives a 17-column
box, cutting the warning to `this canno`.

There is plenty of room — this is a 200-column terminal. Nothing is competing
for the space.

**Why the test suite missed it — the fourth instance of this pattern, and the
subtlest.** `ui.rs:6953`:

```rust
a.overlay = Overlay::Confirm { verb: "delete".into(), what: "pr-opened".into() };
let screen = rendered(&a, 100, 24);
assert!(screen.contains("cannot be undone"));
assert!(screen.contains("y confirms"));
```

`"delete pr-opened?"` is 17 chars → a 25-column box, wide enough for the
23-char title, so the first assertion passes. The second asserts only
`"y confirms"` — **ten characters**, which survive the clip that eats the
remaining twenty-six. The assertion is a prefix short enough to be true of the
broken rendering. A longer fixture name, or asserting the *whole* footer, would
have failed.

**Suggested fix.** Size the panel to
`max(question, title, footer) + padding`, and assert the **complete** footer
string in the test. The same "content-derived width ignores the chrome" bug
should be swept for elsewhere — this is the fourth width defect in the report
(BUG-3, BUG-11, BUG-16, BUG-20).

---

<a name="bug-19"></a>
## BUG-19 — A run you interrupted is shown as `✗ failed` in the TUI and `killed` everywhere else · Medium · OPEN

The TUI and the database disagree about the same run, in the same moment.

**Repro:** interrupt a turn, then open the fleet (`Ctrl-F`) and run `jod ls`.

**The TUI fleet:**

```
✗ f7eddfb9 failed     4m33s cc  run the shell command: sleep
✗ 5a0a604c failed    10m14s cc  run the shell command: sleep
■ f58fab53 killed    13h37m cc  continue
```

**`jod ls`, same two runs:**

```
5a0a604c   killed    Claude Code  run the shell command: sleep
f7eddfb9   killed    Claude Code  run the shell command: sleep
```

**The store agrees with the CLI, not the TUI:**

```
$ sqlite3 ~/.jod/jod.db "select status, count(*) from runs group by status"
completed|10
killed|5          <-- and zero rows with status 'failed'
```

The menu's dashboard line inherits the error: `15 runs · 0 running · 2 failed`,
counting two failures that do not exist in the record.

**Root cause.** `cli/src/tui/app.rs:1754` counts on the *live* agent list:

```rust
let failed = self.agents.iter().filter(|a| a.status == "failed").count();
```

and the in-memory status of a run terminated by `kill_agent` comes back as
`failed`, while the persisted status is `Killed`. The tell is in the fleet
listing above: the two runs killed *in this session* show `failed`, and the
older killed runs — reloaded from the store — correctly show `killed`. So the
same run changes status when you restart the TUI.

**Why it matters beyond tidiness.** A deliberate stop is not a failure. Red
`✗ failed` invites the user to investigate something they themselves caused,
and "0 running · 2 failed" on the dashboard is a standing false alarm. It is
the same misclassification that produces the contradictory `✗ failed` entry in
BUG-17 — worth fixing in one place.

**Suggested fix.** Map a killed run to `killed` in the live state as well, so
the two views cannot disagree, and count only genuine failures in
`count_for`.

**Confirmed a second way — the same run changes verdict when you restart.**
In the session that killed them, the dashboard read:

```
15 runs · 0 running · 2 failed
```

A freshly started TUI, reading the same runs back from the store, reads:

```
20 runs · 1 running · 0 failed
```

Same two runs, no longer failures. Nothing about them changed except which
source the count came from — which is the bug stated as plainly as it can be:
**the failure count is a property of your session, not of the runs.**

---

<a name="bug-15"></a>
## BUG-15 — `@` in a non-git directory is ~95% `node_modules` noise · **High** · OPEN

**Repro:** `/add-dir ~/tetris` (the project the delegated run just built — a
plain directory, not a git repo), then type `@` in the chat box.

**Actual:** the popup's first eight rows are:

```
▸ tetris/dist
  tetris/dist/assets
  tetris/dist/assets/index-CGus7geV.js
  tetris/dist/assets/index-CWyD2mYX.css
  tetris/dist/index.html
  tetris/index.html
  tetris/node_modules
  tetris/node_modules/.bin
```

Build output and dependencies. **Not one file from `src/`** — the actual source
— is visible without typing a query.

**The numbers.** Jod runs this exact command (`core/src/rank.rs:629`):

```
rg --files --hidden --glob '!.git'
```

In `~/tetris` that returns **249 paths, 236 of them under `node_modules`** —
95% noise. Plain `rg --files` returns **13**.

**Root cause — and it is `--hidden`, specifically.** The doc comment at
`core/src/rank.rs:623` states the design:

> *"Everything else ripgrep leaves out — `target`, `node_modules` — it leaves
> out by reading `.gitignore`, which is the whole reason it is preferred over
> the walker below."*

That reasoning holds **only inside a git repository**. `~/tetris` has no
`.git` and no `.gitignore`, so no ignore rules apply at all. What was
*accidentally* keeping `node_modules` down to 13 files is that pnpm stores the
real files in the **hidden** `node_modules/.pnpm/` directory and exposes
packages as symlinks — and ripgrep skips hidden dirs and does not follow
symlinks. `--hidden` switches exactly that protection off and pulls the whole
store in.

So the flag added to make dotfiles mentionable is the flag that floods the
list, in precisely the case where nothing else is filtering.

**Why this matters more than it looks.** A brand-new project — scaffolded by a
delegated agent, `npm install` run, not yet `git init`-ed — is the *normal*
state of the thing you most want to `@`. It is exactly what the run in BUG-14
produced. In a real dependency tree (thousands of packages, not 236 files),
`@` stops being usable.

**The fix already exists in this codebase, one module over.**
`cli/src/tui/picker.rs:65`:

```rust
const NOISE: [&str; 6] = ["node_modules", "target", ".git", "dist", "build", "venv"];
```

and its comment (`picker.rs:63`) describes this exact failure — *"twenty matches
for `src` are all inside `target/debug/build` is a picker \[nobody wants]"*.
The `/add-dir` picker applies it; the `@` mention path does not. The two
pickers disagree about what noise is, in the same program, on the same tree:
`/add-dir ~/tetris` correctly showed only `.` and `src`, while `@` showed
`dist` and `node_modules`.

**Suggested fix.** Pass the denylist to ripgrep as globs
(`--glob '!node_modules' --glob '!target' --glob '!dist' …`) so the ignore
guarantee no longer depends on the directory happening to be a git repo, and
share one constant between `picker.rs`, `rank.rs::SKIP` and the mention path
instead of three.

---

<a name="bug-16"></a>
## BUG-16 — `@` truncates paths from the right, hiding the filename · Medium · OPEN

**Repro:** with `~/tetris` as a root, type `@engine`.

**Actual:**

```
▸ tetris/src/engine.js
  tetris/node_modules/.pnpm/tinyglobby@0.2.17/node_mod
  tetris/node_modules/.pnpm/tinyglobby@0.2.17/node_mod
  tetris/node_modules/.pnpm/tinyglobby@0.2.17/node_mod
  tetris/node_modules/.pnpm/tinyglobby@0.2.17/node_mod
  tetris/node_modules/.pnpm/tinyglobby@0.2.17/node_mod
  tetris/node_modules/.pnpm/tinyglobby@0.2.17/node_mod
```

Six rows that are **character-for-character identical on screen** and name six
different files. Choosing between them is impossible.

The ranking itself is fine — `src/engine.js` is correctly first, and
`docs/try-it.md:154` accurately describes the ranking rules. The defect is
purely display: paths are clipped from the **right**, which is the end that
distinguishes them. Same class as BUG-3, opposite module.

**Suggested fix.** Elide from the left for paths (`…/tinyglobby@0.2.17/index.js`),
as BUG-3 also needs. Fixing BUG-15 removes most of these rows, but the
truncation is worth fixing independently — deep source paths collide the same
way.

---

## NOT bugs — checked, and ruled out

Recorded so nobody spends time on them.

- **`Ctrl-Y` "copies" but the clipboard does not change.** Reported success
  (`• copied the last reply — 40 lines`) while a sentinel on the macOS
  clipboard survived untouched, twice. **This is my harness, not Jod.**
  `cli/src/tui/yank.rs:10` deliberately uses **OSC 52** — an escape sequence to
  the *terminal's* clipboard, chosen because Jod runs over SSH where a
  clipboard crate would target the wrong machine. My tmux session is
  **detached**, so the sequence has no attached terminal to reach; setting
  `set -g set-clipboard on` changed nothing for the same reason. Untestable
  headlessly. Note OSC 52 has no acknowledgement, so the unconditional
  "copied" notice is the only thing Jod *can* say — the module documents this.
- **`/add-dir tetris` showing only `.`** — correct. The directory was empty.
  Only BUG-3's truncated header made it look wrong.
- **`Esc` not interrupting my first attempt** — that run had genuinely
  finished (all 40 numbers, 7s) before the key landed. The real defect is
  BUG-17, found on a retest with a 90-second run.
- **The `/add-dir` picker hiding `dist` and `node_modules`** — deliberate, via
  `picker.rs:65`'s `NOISE` list, and correct. It is the `@` path that is wrong
  (BUG-15), not this one.
- **The stale-looking `jod --help`** with 27 subcommands — that was my own
  pre-rebuild binary, not a defect. It is what led to BUG-13, which is the real
  issue underneath.

---

## NEEDS-REPRO — observed once, could not reproduce

Recorded for honesty; **do not act on these without reproducing first.**

**N-1 — momentary blank screen.** Once, after `tmux resize-window` to 260×50
followed immediately by `/add-dir tetris` + `⏎`, the pane rendered 50 blank
lines. The process was alive (`pane_dead 0`) and still accepted input; typing
text made it repaint correctly.

I tried to pin it on resize handling — and there is a real smell there: there
is **no `Event::Resize` arm anywhere in the TUI** (`grep -rn "Event::Resize"
cli/src/tui/` → zero hits), only a catch-all at `cli/src/tui/mod.rs:617`
commented *"A resize just needs a redraw, which the next loop does."*

But I could not reproduce it: resize alone repaints fine (11 non-blank lines
before and after), and the exact type-then-Enter sequence came back clean
**5/5**. So the catch-all appears to be adequate and I am **not** filing this
as a bug. If someone sees a blank TUI in the wild, start here.

---

## The one-shot, after the cwd fix — it works

The point of the exercise: **can the TUI build a working classic Tetris in one
shot?** With BUG-14's fix in the tree, yes.

One prompt, one run (`b8d6e5f1`), no follow-ups. And critically, the run
recorded its working directory as the directory the TUI was launched in —

```
b8d6e5f1|completed|/…/worktrees/tui-dogfood-tetris/tetris-oneshot|Build a complete, working clas
bfa2bef4|completed|/…/worktrees/tui-dogfood-tetris/tetris|create a file called HELLO.md
ab8a6b9d|completed|/…/worktrees/tui-dogfood-tetris|reply with the single word
```

— not `$HOME`. Compare the same column before the fix, where every row read
`/Users/reljodoreta`. **BUG-14 is fixed in practice, not just in theory.**

### What the one shot produced, verified three ways

`pnpm build` — clean, 5 modules, 118 ms. `pnpm test` — **40 tests, 40 pass, 0
fail**, covering SRS rotation through four states, I-piece wall kicks, J-piece
floor kicks, rotation refused when every kick is blocked, 7-bag determinism
from a seed, single/double/triple/tetris scoring against level, the stack
dropping after a clear, an unfinished row left alone, level-up every ten lines
with gravity speeding up, and the engine running under plain Node with no DOM
globals stubbed.

Then I **played it in a real browser** — the thing no previous run managed,
because the `browser` MCP was broken (`ModuleNotFoundError: No module named
'camoufox'`). Driving Chrome against `vite dev`:

- renders a 10×20 well, HOLD, NEXT (three deep), CONTROLS and STATS;
- ←/→ move, ↑ rotates, Space hard-drops, and the score advanced 0 → 106 → 222;
- **hold works** — `C` moved the I-piece into the HOLD box;
- the ghost piece tracks the landing position;
- both walls stop the piece instead of letting it escape;
- `P` dims the board and shows `PAUSED · Press P to resume`;
- `R` restarts to a clean board with score 0 and HOLD cleared;
- **zero console errors** throughout.

Line clearing is the one mechanic I could not force by hand in the browser (the
engine is properly module-scoped, not on `window`), so it rests on the unit
tests — which assert it deterministically, including the stack drop and the
score multiplier.

**Location:** `tetris-oneshot/` in this worktree. Run it with
`cd tetris-oneshot && pnpm install && pnpm dev`.

---

## The earlier Tetris — same story, wrong directory

The task did produce a real, working game. It is at **`/Users/reljodoreta/tetris`**
— which is the wrong place (BUG-14), but the code is good.

I did not take the agent's word for it. `pnpm build` succeeds (8 modules, 825 ms),
and I wrote an independent probe against the engine — **14 assertions, 14 pass**:
board is 10×20, the 7-bag yields seven distinct pieces before repeating, left and
right walls stop the piece, stacking hard drops reaches game over in a plausible
number of drops, pause blocks the drop and unpausing resumes, a bottom row fills,
and the score advances as pieces land.

Structure: a headless engine (`src/engine.js`, no DOM), a canvas renderer, and a
keyboard controller, so the rules stay testable independently of the browser.
Ghost piece, next-piece preview, wall kicks, per-level gravity, standard
100/300/500/800 × level scoring.

One caveat the agent reported honestly rather than papering over: it could not
drive a real browser (`browser` MCP failed — `ModuleNotFoundError: No module
named 'camoufox'`), so live key handling and visual output are unverified. That
is a fair "blocked" and worth knowing — the browser MCP is broken on this box.

To run it: `cd ~/tetris && pnpm install && pnpm dev`.

---

## Verified working

Worth recording, since the point of a hand-drive is to separate the two:

- `jod tui` starts clean and fast; the splash renders correctly at 200×50.
- `?` and `Ctrl-G` overlays open, render and dismiss correctly.
- `/` completion filters live and anchors the input to the bottom so the
  ~43-row list is not cut in half — a deliberate touch (`ui.rs:1196`) that
  works.
- `/add-dir <path>` **correctly honours its argument** and stores the root
  (verified against `jod root ls`), despite BUG-3 making it look otherwise.
- The fleet (`Ctrl-F`) is genuinely good: live status, age, harness, per-run
  detail, spend, and the last message body in a side pane. It was the only
  screen that told me what was actually going on.
- **Delegation spawns correctly.** `Ctrl-B` started a real Claude Code run with
  the right prompt; `jod ls` and the fleet both showed it running, then done,
  with accurate cost accounting ($1.18 / 17,425 output tokens).
- Roots are read-only by design (`jod root add` → "Add a directory,
  read-only"); write access comes from the agent's own `claim_worktree`. Not a
  bug in itself — it behaves as `docs/try-it.md:199` documents — but see
  BUG-14 for what happens when there is no worktree to claim.
- **#75 is a real improvement.** Moving the chords off `Alt` fixed BUG-8
  outright, largely fixed BUG-9, and the `Ctrl-G` menu is now a much better
  map of the program. `docs/try-it.md` was updated in step, so the docs did
  **not** drift — I checked specifically.

---

## A supervised run outlives its console — verified by accident

Worth recording as a **positive** result, because it was proven the hard way.

Another agent reading the harness notes below reused the same tmux socket name
(`jodtest`) and started its own TUI on it, which replaced mine mid-run — my
console vanished, and a poller watching the *screen* concluded the run had
ended.

It had not. The run record showed:

```
44461917|running|/…/tui-dogfood-tetris/tetris-final|Build a complete, working
```

The work carried on to completion with no console attached at all. That is
exactly what `docs/jod-system.md` claims — runs are supervised detached
process groups reporting through the database, not children of the UI — and it
is the behaviour that makes `Ctrl-C`'s "press again to leave them running"
promise true.

**Two lessons for anyone driving this program:**

1. **Give each agent its own tmux socket** (`tmux -L <something-unique>`).
   A shared socket name is a shared session, and the second agent silently
   evicts the first.
2. **Poll the run record, not the screen.** `sqlite3 ~/.jod/jod.db "select
   status from runs where id like '<id>%'"` is the truth. The console is a
   view, and a view can be replaced by somebody else.

---

## How this was driven

For anyone reproducing: the TUI was driven through an isolated tmux server
(`tmux -L jodtest`, socket deliberately not the default, so the user's own
session and iTerm are untouchable), at 200×50 and 260×50, sending real
keypresses and capturing the rendered pane. `send-keys` + `capture-pane` is
enough to hand-drive this program end to end, and is worth wiring into CI as a
smoke test — **every single finding above is invisible to the unit suite**, and
several are actively *masked* by it (BUG-3 and BUG-6 each have a green test
sitting on top of the broken behaviour).
