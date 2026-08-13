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
| [BUG-14](#bug-14) | **Critical** | delegation | **The TUI runs every agent in `$HOME`** — work lands outside every root and the run is recorded `✓ done` |
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
| [BUG-17](#bug-17) | Medium | interrupt | Interrupt is unacknowledged for 4–6s, then reported as both `✓ done` and `✗ failed` |
| [BUG-18](#bug-18) | Medium | interrupt | Every interrupt falsely warns the run "may still be writing", worded as a *start* failure |
| [BUG-15](#bug-15) | **High** | mentions | `@` in a non-git directory is ~95% `node_modules` noise; source is invisible |
| [BUG-16](#bug-16) | Medium | mentions | `@` clips paths from the right, so six different files render identically |

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

I confirmed this records correctly:
`9ad3d21f|/…/worktrees/tui-dogfood-tetris/tetris|reply with the word ok`.

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

The comment at `cli/src/tui/mod.rs:4134` states the intent exactly, and it is
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
## BUG-18 — Every interrupt prints a false "would not stop" warning, worded as a start failure · Medium · OPEN

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
