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

**Binary under test:** freshly built `target/release/jod` at the tip of `main`
(`79f9fdf`). The build matters — see BUG-13.

---

## Severity summary

| ID | Severity | Area | One line |
|---|---|---|---|
| [BUG-1](#bug-1) | **Critical** | rendering | A fresh session hides *all* notice-only output — most slash commands render nothing |
| [BUG-2](#bug-2) | **High** | delegation | `Alt-B` delegates with almost no confirmation; it looks like nothing happened |
| [BUG-3](#bug-3) | **High** | directory clarity | The directory picker's header is truncated, so you cannot tell which tree you are in |
| [BUG-4](#bug-4) | **High** | directory clarity | The working directory appears nowhere in the chat UI |
| [BUG-5](#bug-5) | **High** | projects | A project cannot be created or cited from the TUI at all |
| [BUG-6](#bug-6) | Medium | discoverability | `Alt-D` is a silent no-op unless a panel you cannot discover is already open |
| [BUG-7](#bug-7) | Medium | discoverability | `Shift-Tab` — the only way to reach projects/sessions/context — is undocumented |
| [BUG-8](#bug-8) | Medium | rendering | Keymap overlay: key label collides with its description |
| [BUG-9](#bug-9) | Medium | honesty | The splash claims "Alt-K opens every screen". It does not. |
| [BUG-10](#bug-10) | Low | commands | `/main` is listed twice, with two different meanings |
| [BUG-11](#bug-11) | Low | commands | Command descriptions are cut mid-word with no ellipsis |
| [BUG-12](#bug-12) | Low | input | The input box is fixed at ~70 columns and single-line |
| [BUG-13](#bug-13) | Medium | tooling | `jod --version` cannot distinguish two different builds |

---

<a name="bug-1"></a>
## BUG-1 — A fresh session silently swallows every notice-only command · **Critical** · OPEN

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
## BUG-2 — `Alt-B` delegate gives almost no confirmation · **High** · OPEN

You suspected "delegate task does not spawn". **It does spawn.** The bug is
that the UI barely admits it, which is indistinguishable from failure.

**Repro:** type a prompt, press `Alt-B`.

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

and `Alt-A` (fleet) shows `● 87e84b92 running 20s cc Build a working Tetris game`.

**Root cause:** the delegation confirmation is an `Entry::Notice`, hidden by
BUG-1. Fixing BUG-1 likely fixes most of this. It is filed separately because
delegation deserves a *loud* confirmation regardless — it is the single most
consequential key in the program, it spends money, and it is fire-and-forget.

**Suggested fix.** On delegate, push a non-notice transcript entry naming the
agent id, the prompt, **and the working directory** it was given (see BUG-4).

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

**Root cause.** `cli/src/tui/ui.rs:2179`:

```rust
let width = (screen.width.saturating_sub(8)).min(96).max(40);
```

The picker is capped at **96 columns no matter how wide the terminal is**, and
the header at `ui.rs:2185` is a plain `Line` with no truncation strategy:

```rust
format!("  in {}", p.base.display()),
```

so ratatui clips it at the panel edge.

**Why the test suite missed it.** `ui.rs:8964`
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

**Suggested fix.** Put the root (elided from the left) in the status bar, or in
the input box's border title.

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
  matches**; it is absent from the `/` list entirely.
- It is not in the `Alt-K` menu.
- The panel's own empty state, `nothing set`, names no remedy.

The capability exists everywhere *except* the TUI: `core/src/projects.rs:436`
has `add_project`, and the CLI has a full `jod project add|ls|archive|restore`
(`cli/src/main.rs:313`). You must quit the TUI, or open a second terminal, and
run `jod project add` to make the panel non-empty.

Compare the roots empty state, which does this correctly
(`cli/src/tui/mod.rs:1293`):

```
no roots — /add-dir picks one (Alt-P), and `@` says so until there is
```

That is exactly the affordance the projects panel lacks.

The cost is stated in Jod's own CLI help (`cli/src/main.rs:309`): *"until a
repository is listed, saying 'let's fix this' has nothing to resolve to and
every instruction about it has to spell the path out."* So an empty catalog
degrades every instruction — and the TUI gives no way to fill it.

**Suggested fix.** Add `/project add|ls`, and make the empty state say how to
fix itself.

---

<a name="bug-6"></a>
## BUG-6 — `Alt-D` is a silent no-op unless an undiscoverable panel is open · Medium · OPEN

**Repro:** from a cold `jod tui`, press `Alt-D`. The `?` overlay advertises it
as **"show or hide the projects"**.

**Actual:** nothing. No panel, no message, no change of any kind.

**Root cause.** `cli/src/tui/mod.rs:2492` toggles `app.projects_open`:

```rust
KeyCode::Char('d') if alt => {
    app.projects_open = !app.projects_open;
    handled(None)
}
```

but the projects catalog only renders inside the side panel, which is gated on
`app.panel` — opened by `Shift-Tab` (BUG-7), and `false` at startup. So the key
flips a flag that draws nothing, and says nothing about why.

**Why the test suite missed it.** `cli/src/tui/mod.rs:6371`
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

**Suggested fix.** Either have `Alt-D` open the panel when it is closed, or
have it push a notice explaining that `Shift-Tab` opens the panel. (The notice
route requires BUG-1 to be fixed first, or it will be invisible.)

---

<a name="bug-7"></a>
## BUG-7 — `Shift-Tab` is undocumented, and it is the only way in · Medium · OPEN

`Shift-Tab` opens the side panel holding **projects, sessions, mode, harness,
spend and context usage** — a large fraction of the program's state.

It appears in **neither** the `?` keymap (which claims to be the whole keymap —
23 bindings, no `Shift-Tab`) **nor** the `Alt-K` menu. The only place it is
written down is the bottom border of the panel itself:

```
└ Shift-Tab closes ──────────────┘
```

— which you can only read *after* you have already discovered the key. I found
it by reading source, not by using the program.

**Suggested fix.** Add `Shift-Tab` to `GLOBAL` in `cli/src/tui/keys.rs`. Note
the overlay is described as budget-tight at 100×30 (`keys.rs:164`), so this may
need a row freed — but a key that opens six panels earns its row more than most
of the ones present.

---

<a name="bug-8"></a>
## BUG-8 — Keymap overlay: the key label collides with its description · Medium · OPEN

**Repro:** press `?`. Read the fifth row from the bottom:

```
  Ctrl-A/E/Home/Endstart / end of the line
```

`End` and `start` are glued together. Every other row is aligned; this one
reads as a typo in the program.

**Root cause.** `cli/src/tui/ui.rs:2409` pads to a *minimum* of 12 and never
truncates:

```rust
Span::styled(format!("  {:<12}", binding.key), fg(WARN)),
```

The label at `cli/src/tui/keys.rs:179` is `"Ctrl-A/E/Home/End"` — 17 chars, so
it overruns the column and eats the gap. `keys.rs:678` states the design
constraint explicitly ("the overlay has twelve columns for a key"); this label
is the one that breaks it.

**Suggested fix.** Either widen the column to fit the longest label
(computed, not hardcoded), or split the row. Note `press_of` splits on `/` and
the drift test replays printed labels as real keypresses, so **the label text
is load-bearing** — shortening it to `Ctrl-A/E` would silently stop
advertising `Ctrl-Home`/`Ctrl-End`. Widen the column; do not trim the label.

---

<a name="bug-9"></a>
## BUG-9 — The splash claims "Alt-K opens every screen"; it does not · Medium · OPEN

The splash caption reads:

```
jod · an orchestrator, not a chat window · Alt-K opens every screen
```

`Alt-K` lists: chat, fleet, memory, schedules, goals, hooks, tasks, activity,
team, new…, editor, keys.

Reachable **only** by chord, and absent from that menu: **projects** (`Alt-D`),
**the rail** (`Alt-R`), **background shells** (`Alt-J`), **transcript search**
(`Alt-S`), **delegate** (`Alt-B`), and the **side panel** (`Shift-Tab`).

A user who believes the caption will never find them. Either soften the caption
or add the missing entries.

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
## BUG-13 — `jod --version` cannot distinguish two different builds · Medium · OPEN

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

## NEEDS-REPRO — observed once, could not reproduce

Recorded for honesty; **do not act on these without reproducing first.**

**N-1 — momentary blank screen.** Once, after `tmux resize-window` to 260×50
followed immediately by `/add-dir tetris` + `⏎`, the pane rendered 50 blank
lines. The process was alive (`pane_dead 0`) and still accepted input; typing
text made it repaint correctly.

I tried to pin it on resize handling — and there is a real smell there: there
is **no `Event::Resize` arm anywhere in the TUI** (`grep -rn "Event::Resize"
cli/src/tui/` → zero hits), only a catch-all at `cli/src/tui/mod.rs:616`
commented *"A resize just needs a redraw, which the next loop does."*

But I could not reproduce it: resize alone repaints fine (11 non-blank lines
before and after), and the exact type-then-Enter sequence came back clean
**5/5**. So the catch-all appears to be adequate and I am **not** filing this
as a bug. If someone sees a blank TUI in the wild, start here.

---

## Verified working

Worth recording, since the point of a hand-drive is to separate the two:

- `jod tui` starts clean and fast; the splash renders correctly at 200×50.
- `?` and `Alt-K` overlays open, render and dismiss correctly.
- `/` completion filters live and anchors the input to the bottom so the
  ~43-row list is not cut in half — a deliberate touch (`ui.rs:1192`) that
  works.
- `/add-dir <path>` **correctly honours its argument** and stores the root
  (verified against `jod root ls`), despite BUG-3 making it look otherwise.
- `Alt-A` fleet is genuinely good: live status, age, harness, per-run detail,
  spend, and the last message body in a side pane.
- **Delegation works.** `Alt-B` spawned a real Claude Code run against the
  right prompt; `jod ls` and the fleet both showed it running.
- Roots are read-only by design (`jod root add` → "Add a directory,
  read-only"); write access comes from the agent's own `claim_worktree`. Not a
  bug — behaves as `docs/try-it.md:199` documents.
