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

---

## Severity summary

| ID | Severity | Area | One line |
|---|---|---|---|
| [BUG-14](#bug-14) | **Critical** | delegation | A delegated agent wrote into `$HOME`, outside every root, and the run was recorded `✓ done` |
| [BUG-1](#bug-1) | **Critical** | rendering | A fresh session hides *all* notice-only output — most slash commands render nothing |
| [BUG-2](#bug-2) | **High** | delegation | `Ctrl-B` delegates with almost no confirmation; it looks like nothing happened |
| [BUG-3](#bug-3) | **High** | directory clarity | The directory picker's header is truncated, so you cannot tell which tree you are in |
| [BUG-4](#bug-4) | **High** | directory clarity | The working directory appears nowhere in the chat UI |
| [BUG-5](#bug-5) | **High** | projects | A project cannot be created or cited from the TUI at all |
| [BUG-6](#bug-6) | **High** | discoverability | `Ctrl-G d` (projects) is a silent no-op unless an undiscoverable panel is already open — **survived #75 and got worse** |
| [BUG-7](#bug-7) | Medium | discoverability | `Shift-Tab` — the only way to reach projects/sessions/context — is undocumented |
| [BUG-8](#bug-8) | ~~Medium~~ | rendering | ~~Keymap overlay: key label collides with its description~~ — **FIXED by #75** |
| [BUG-9](#bug-9) | ~~Medium~~ | honesty | ~~The splash claims "Alt-K opens every screen"~~ — **largely FIXED by #75** |
| [BUG-10](#bug-10) | Low | commands | `/main` is listed twice, with two different meanings |
| [BUG-11](#bug-11) | Low | commands | Command descriptions are cut mid-word with no ellipsis |
| [BUG-12](#bug-12) | Low | input | The input box is fixed at ~70 columns and single-line |
| [BUG-13](#bug-13) | Medium | tooling | `jod --version` cannot distinguish two different builds |

---

<a name="bug-14"></a>
## BUG-14 — A delegated run wrote into `$HOME`, outside every root, and reported success · **Critical** · OPEN

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

**Why it happens.** Two documented facts combine badly:

- *"Roots are a convention, not a sandbox — passing one grants; withholding one
  does not deny"* (`docs/try-it.md:203`). Nothing **stops** a write outside a
  root.
- Roots are added **read-only**, and write access is supposed to come from the
  agent calling `claim_worktree`. `tetris/` was an empty non-git directory, so
  there was no worktree to claim.

So the agent had a read-only root it could not write to, an ambiguous phrase
("the tetris directory"), and no visible statement of its own working
directory — and resolved the ambiguity against `$HOME`. Compounding it,
**BUG-4** means the human could not have noticed the mismatch either: the TUI
never shows the cwd a delegated agent is given.

**Why this is Critical rather than High.** The failure is silent in both
directions. The agent believes it succeeded, Jod's records agree (`✓ done`,
$1.18 spent), the fleet shows a green check — and the directory the user
actually pointed at is untouched. Nothing in the TUI would ever tell you. A
user who trusts the green check has a repo that never received the work and an
unrelated tree in `$HOME` that silently did.

**Suggested fixes**, in order of value:

1. **Pass and display the delegated cwd.** The delegate confirmation should
   name the working directory the run was launched with, and the run detail
   pane should show it. Today neither does.
2. **Resolve bare directory names against the roots** before falling back to
   anything else, and if a name matches no root, raise a blocking card instead
   of guessing. Guessing `$HOME` is the worst available default.
3. **Warn when a run's file writes all land outside every declared root.** The
   supervisor already sees the events; a run that declares "done" having
   touched nothing inside any root is worth a card, not a green check.
4. Consider whether a root that cannot be written to, with no worktree to
   claim, should be accepted silently at all.

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
## BUG-2 — delegate gives almost no confirmation · **High** · OPEN

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

**Root cause:** the delegation confirmation is an `Entry::Notice`, hidden by
BUG-1. Fixing BUG-1 likely fixes most of this. It is filed separately because
delegation deserves a *loud* confirmation regardless — it is the single most
consequential key in the program, it spends money, and it is fire-and-forget.

**Suggested fix.** On delegate, push a non-notice transcript entry naming the
agent id, the prompt, **and the working directory** it was given (see BUG-4 and
BUG-14). That last field is not cosmetic: it is the one piece of information
that would have exposed BUG-14 the moment it happened.

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
## BUG-6 — projects toggle is a silent no-op unless an undiscoverable panel is open · **High** · OPEN

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

**Why the test suite missed it.** `cli/src/tui/mod.rs:6396`
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

---

<a name="bug-7"></a>
## BUG-7 — `Shift-Tab` is undocumented, and it is the only way in · Medium · OPEN

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

## The Tetris itself — delivered, and independently verified

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
  ~43-row list is not cut in half — a deliberate touch (`ui.rs:1192`) that
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

## How this was driven

For anyone reproducing: the TUI was driven through an isolated tmux server
(`tmux -L jodtest`, socket deliberately not the default, so the user's own
session and iTerm are untouchable), at 200×50 and 260×50, sending real
keypresses and capturing the rendered pane. `send-keys` + `capture-pane` is
enough to hand-drive this program end to end, and is worth wiring into CI as a
smoke test — **every single finding above is invisible to the unit suite**, and
several are actively *masked* by it (BUG-3 and BUG-6 each have a green test
sitting on top of the broken behaviour).
