# TUI UX for Jod, 2026

**Question.** Jod's TUI is a chat window with a fleet panel bolted to `Ctrl-A`. It has
to become a control surface for **chat**, **memory (a graph)**, **schedules
(cron)**, **goals (looping objectives)** and **webhooks (GitHub → agents)**
without losing the thing that makes it good: you type a sentence and an agent
goes and does it.

**Method.** Sixteen falsifiable hypotheses written before the evidence was
gathered, then graded against how best-in-class terminal tools actually behave
and against the usability literature where it exists. Grades are **A** (strong
evidence, multiple independent tools converge), **B** (good evidence, one strong
precedent or a clear argument), **C** (mixed / it depends), **D** (weak, mostly
opinion), **F** (refuted). Where the only evidence is opinion, it says so.

**Deliverables.** The hypothesis table, a pattern catalogue, a concrete design
for Jod (screen inventory, navigation model, a full keymap that does not collide
with the current one), nine ASCII wireframes at exactly 100×30, an anti-pattern
list, and sources.

> Every wireframe in this document was generated and machine-checked to be
> exactly 100 columns × 30 rows. If a line looks ragged in your viewer, your
> viewer is proportional-width, not the wireframe.

---

## 1. What Jod's TUI does today

Read from `cli/src/tui/{mod,app,ui,command}.rs` at `356548a`. **This is the
collision surface** — every proposed binding below is checked against it.

### Global chords (handled before anything else, `mod.rs:372`)

| Key | Action |
|---|---|
| `Ctrl-C` / `Ctrl-D` | quit; first press warns if any agent is running, second leaves |
| `Ctrl-A` | toggle the agents panel |
| `Ctrl-G` | toggle the team panel |
| `Ctrl-T` | show/hide reasoning |
| `Ctrl-O` | show/hide tool output |
| `Ctrl-B` | delegate the typed line to a background agent |
| `Ctrl-X` | stop the run being watched |
| `Ctrl-L` | clear the transcript |
| `Ctrl-U` | clear the input line |
| `Ctrl-W` | delete the previous word |
| `Ctrl-↑` / `Ctrl-↓` | scroll the transcript by one line |
| `Ctrl-Home` | cursor to start of line |
| `Ctrl-E` / `Ctrl-End` | cursor to end of line |

### Chat pane (`mod.rs:529`)

`Enter` send / run a slash command / accept a completion · `Tab` accept
completion · `↑`/`↓` recall sent lines (or move the completion cursor) ·
`PageUp`/`PageDown` scroll one screen · `Esc` jump to the bottom and follow ·
`←`/`→`/`Home`/`End`/`Backspace`/`Delete` line editing · mouse wheel scrolls 3.

### Panel panes — Agents and Team (`mod.rs:581`)

`Esc` / `q` close · `↑`/`k`, `↓`/`j` move · `Home`/`End` first/last · `Enter`
watch (Agents) or mark done (Team) · `s` stop · `a` attach · `r` point the next
turn at that conversation.

### Slash commands (`command.rs:154`)

`/help` `/?` · `/harness` `/agent` · `/model` `/models` · `/thinking`
`/reasoning` · `/details` `/output` · `/new` · `/sessions` · `/resume`
`/continue` · `/agents` · `/team` · `/delegate` `/bg` `/spawn` · `/stop` `/kill` ·
`/watch` `/focus` · `/attach` · `/todo` `/task` · `/done` `/finish` ·
`/clear` · `/exit` `/quit` `/q`

### Which chords are actually free

Taken: `A B C D E G L O T U W X` + `Ctrl-↑↓`, `Ctrl-Home/End`.
Unusable regardless: `Ctrl-H` (= Backspace), `Ctrl-I` (= Tab), `Ctrl-J`/`Ctrl-M`
(= Enter), `Ctrl-S`/`Ctrl-Q` (XON/XOFF flow control on many terminals),
`Ctrl-Z` (users expect job-control suspend).
**Genuinely free: `Ctrl-F`, `Ctrl-K`, `Ctrl-N`, `Ctrl-P`, `Ctrl-R`, `Ctrl-V`,
`Ctrl-Y`.** Seven keys, and five screens to add. That number is the whole reason
the design below is a leader key rather than five more chords.

---

## 2. Hypotheses and grades

| # | Hypothesis | Grade | Evidence |
|---|---|---|---|
| **H1** | Past ~4 destinations, a text-entry palette that reaches every screen beats one dedicated chord per screen. | **A** | k9s makes `:` the *primary* navigation for dozens of resource types (`:pod`, `:ctx`, `:ns`) rather than a chord each. Posting uses `Ctrl+P` for the same job. Helix's Space menu is "a kludge of mappings, mostly pickers". lazygit's `?` opens a searchable keybindings menu. Four independent tools, same conclusion. Jod already has this in `/` plus the completion popup; it needs extending, not inventing. |
| **H2** | A persistent context-sensitive keybar beats a `?` help modal for discoverability. | **F as an either/or; A as a both** | Every tool studied does **both**. lazygit: per-panel keys on screen *and* `?` for the full menu. k9s: "keyboard shortcuts tailored to the currently selected resource are displayed in the top right corner" *and* a help view. Claude Code: `?` on empty input toggles a shortcut panel *and* the status bar shows mode/model. Nielsen #6 ("Minimize the user's memory load by making elements, actions, and options visible") demands the always-on bar; #7 ("Shortcuts — hidden from novice users — may speed up the interaction for the expert user") permits the modal to carry the long tail. Ship both; never make the bar the only place a key is documented. |
| **H3** | Modal editing (vim normal/insert) is the wrong navigation model for a chat-first TUI. | **B** | Helix is explicitly built on "the modal editing paradigm"; it is an *editor*, where the default posture is navigation. Jod's default posture is typing prose into a box. Claude Code ships vim mode but **off by default**, and reaches everything else through chords and `/`. Zellij's "locked mode" exists precisely because a modal TUI wrapping something you type into gets in the way. Argument plus one strong precedent — not measured. |
| **H4** | Panel-focus cycling (Tab / arrows through N panes) scales badly past ~4 panes. | **B** | lazygit's five panels are cycled with left/right arrows and it works — but lazygit also gives each panel a *number* for direct jump, and `0` for "focus main view", i.e. it does not rely on cycling. btop uses number keys to toggle panels. Direct addressing is what both add once cycling would take four presses. |
| **H5** | Drawing a node-link graph in a terminal is not worth building; a focus+neighbours view beats it. | **A** | The strongest evidence comes from tools that *have* a real graph renderer and still do not use it for navigation. Obsidian's own users report "a real ceiling where the graph view stops feeling useful" past a few hundred notes, and that "local graphs feel almost magical by comparison". The plain-text world skipped the picture entirely: `zk` navigates by `--link-to` (backlinks) and `--linked-by`, recursively, through fzf — no drawing at all. ASCII layout engines do exist (Graph::Easy, PHART, ascii-dag with "a pragmatic variation of the Sugiyama Layered Graph Layout algorithm") and the research on terminal-space visualisation is blunt about the constraint: "relatively few marks can be shown simultaneously due to the row and column size of the terminal". Build the local graph; do not build the global one. |
| **H6** | A schedule must show absolute next-run **and** a relative countdown **and** the last outcome — the cron expression alone is not a UI. | **A** | `systemctl list-timers` ships exactly this and nothing else: **NEXT** (absolute), **LEFT** (relative), **LAST** (absolute), **PASSED** (relative), UNIT, ACTIVATES. The stated reason is diagnostic: "if the LAST and PASSED columns show that a timer hasn't run in a long time, it could indicate a problem... the NEXT and LEFT columns confirm that a timer is correctly armed". k9s's CronJob view lacks a next-run column and users filed an issue asking for one. Temporal's Schedules page shows "configured frequency, start and end times, and recent and upcoming runs". |
| **H7** | Run history reads better as a fixed-width outcome strip than as a table of numbers. | **A** | Airflow's Grid view is the canonical form: "each column represents a DAG run... task instances are color-coded according to their status" — a dense grid you scan, then drill into. In a terminal the same idea is a per-row strip of block glyphs, which is the established Unicode sparkline vocabulary `▁▂▃▄▅▆▇█` (U+2581–U+2588). Colour must not be the only channel, so failures get `✗` rather than a red block (see H12). |
| **H8** | Creating a schedule or webhook is best done by `$EDITOR` handoff, not an in-TUI form. | **C — it depends on field count** | Both patterns are load-bearing in real tools and neither wins outright. Claude Code binds `Ctrl+G` *and* the readline-native `Ctrl+X Ctrl+E` to "Open in default text editor" for exactly the long-free-text case; `crontab -e`, `git commit` and `visudo` are the same pattern. Against that, `huh` shows that structured terminal forms with per-field `Validate(func(s string) error)` and multi-page groups are a solved problem, and posting edits HTTP requests entirely in-TUI through tabbed sections with `Ctrl+T`/`Ctrl+L` field jumps. **Conclusion: tier it.** One value → inline prompt. 2–6 structured values → modal form. Free text, or anything you would want to diff → `$EDITOR` on a TOML buffer. |
| **H9** | Destructive actions need friction proportional to reversibility, not a uniform y/n. | **B** | Jod already reasons this way in code: an ambiguous `/stop` prefix is refused because "stopping the wrong agent is not an undoable mistake" (`mod.rs:812`), and quitting warns per running agent. lazygit's answer where it applies is better still — `z` undo / `Z` redo via the reflog. Where undo is impossible (deleting a memory node, deleting a webhook) the confirmation must name the thing. Argument from Jod's own charter ("Reversible by default") plus lazygit; no measurement. |
| **H10** | Toasts alone lose endings; a durable activity screen with an unread count is required. | **A** | This is Jod's own founding observation, stated in `mod.rs:206`: "The whole point of delegating is not to watch, so the ending has to come and find you." A transcript notice satisfies that only while you are looking at the transcript. Once cron, goals and webhooks fire *while nobody is at the terminal*, the notice has to be persisted or it is lost. Terminals give a fourth tier for free: OSC 9 (body only) and OSC 777 (title plus body) raise real desktop notifications — iTerm2, kitty, foot and libvte implement OSC 9; Ghostty, WezTerm and urxvt implement OSC 777 — with the caveat that "tmux strips/blocks OSC sequences by default". |
| **H11** | A fuzzy/regex filter on every list buys more navigability than sorting or pagination. | **A** | k9s gives `/` to every resource view with regex, inverse (`!`) and fuzzy (`-f`) variants. atuin's whole product is filtering history four ways (global / host / session / directory). `zk` "supports an interactive mode powered by fzf to further filter notes manually". lazygit, yazi and harlequin ("searchable data catalog tree") all ship it. It is the single most universal pattern in the sample. |
| **H12** | 8 ANSI colours plus attributes beats truecolor theming for a tool meant to run over SSH on someone else's box. | **A** | The 16 ANSI colours are the ones the user's own theme controls; everything else fights it. Julia Evans: "there's no standard, terminal emulators just choose colours and it's not very consistent", "blue on black is hard to read", and programs using the 256-colour set "may clash with user themes". Truecolor's costs are concrete: "each truecolor program needs its own theme configuration... light/dark switching requires explicit support from program maintainers". And `NO_COLOR` is not optional: "Command-line software which adds ANSI color to its output by default should check for a `NO_COLOR` environment variable". Jod's `ui.rs` already uses only named colours — keep it that way, and add a glyph to every colour-coded state. |
| **H13** | Keeping chat as "home" and making everything else a screen you return *from* preserves the product's character. | **B** | lazygit's model is that "most views are generally visible, always, no matter what operation you are doing (unless you zoom in)" — there is a stable ground state. Jod's ground state is the conversation. Nielsen #4 (consistency and standards) argues for one unambiguous "back": `Esc`, always, ending at chat. Design argument; no measurement. |
| **H14** | Breadcrumbs earn their own row once screens nest two deep. | **C** | k9s does show a navigation path, and general TUI-design guidance recommends "always showing the current navigation path as a breadcrumb... for deep hierarchies where showing all levels simultaneously is impractical". But in Jod's inventory only two chains nest (memory → local graph → re-centred node; webhook → delivery → run). At that depth the *title bar* carries it, and a dedicated breadcrumb row costs 1/30th of the screen. Verdict: put the path in the title bar, not on its own row. |
| **H15** | 80×24 must degrade by dropping columns, not by clipping or scrolling sideways. | **B** | Jod's `ui.rs` already does this and has regression tests for it: the status bar "drops its hints rather than colliding with them" (`ui.rs:1066`), panels clamp to the terminal, and there are tests at `(10,4)` and `(12,5)`. The bug it fixed — `1 queuedCtrl-X stop` — is exactly the anti-pattern. Extend the same discipline: every table below declares which columns die first. |
| **H16** | Mouse support should stay scroll-only. | **B** | Jod enables mouse capture but binds only wheel up/down (`mod.rs:153`). Enabling more breaks terminal text selection, which is how people copy a run id off the screen. lazydocker "supports mouse and keyboard shortcut operations" and is the exception; the keyboard-first tools (k9s, helix, lazygit's core loop, atuin) do not need it. Keep scroll; add OSC 52 clipboard copy on `y` instead of asking for a mouse. |

---

## 3. Pattern catalogue

| Pattern | Who does it | Why it works | Applicability to Jod |
|---|---|---|---|
| **Text palette as primary navigation** | k9s `:pod`/`:ctx`/`:ns`; posting `Ctrl+P`; helix `Space`; lazygit `?` menu | One key reaches N destinations, and the destination list is searchable, so it never runs out of keys | **Adopt — already half-built.** Extend `/` with `/memory`, `/schedules`, `/goals`, `/hooks`, `/activity`. The existing completion popup *is* a palette. |
| **Leader key + which-key popup** | helix `Space`/`g`/`z` minor modes; vim-which-key ("the guide buffer will pop up when there are no further keystrokes within `timeoutlen`") | Turns a chord you must recall into a menu you recognise — Nielsen #6 — without spending a global key per destination | **Adopt.** `Ctrl-K` opens a menu of every screen with a live count beside each. |
| **`?` full keymap overlay** | lazygit `?`; Claude Code `?` on empty input; yazi `F1` cheatsheet | Carries the long tail so the always-on bar can stay short (Nielsen #7 and #8) | **Adopt.** `?` on an empty input, screen-aware — shows *this* screen's keys first. |
| **Context-sensitive keybar** | lazygit per-panel footers; k9s top-right hints; Jod's own panel `title_bottom` | The four keys you need right now are on screen; you never guess | **Already there — make it universal.** Every screen gets the same two bottom rows: keybar, then status. |
| **The same key means different things per panel** | lazygit: "the same key changes meaning per panel" — `<space>` stages a file, checks out a branch, applies a stash | Lets 26 letters cover a large verb space, *provided* the keybar states the current meaning | **Adopt, with a spine.** `⏎ n e x p r S / Esc` mean the same everywhere; everything else is screen-local and printed. |
| **Master / detail split** | harlequin (catalog + editor + results); yazi's Miller columns (parent / current / preview); k9s list → describe | You keep the list's context while reading one item — no round trip | **Adopt for fleet and memory.** 48/52 split at 100 columns; the detail pane collapses below 90. |
| **`/` filter on every list** | k9s (regex, `!` inverse, `-f` fuzzy); atuin; zk + fzf; harlequin | Scales a list to thousands of rows with one key and zero configuration | **Adopt everywhere**, including the fleet panel, which has none today. |
| **Selection window that centres on the cursor** | Jod's own `window_start()` (`ui.rs:561`) | A long list stays navigable without the window jumping about | **Keep** — already correct and already tested. |
| **Direct-jump numbers** | lazygit numbers each panel; btop's number keys toggle panels | Removes cycling once there are more than three destinations | **Adopt.** `1`–`8` jump between workspaces (chat is `1`). Digits stay literal text in the chat input. |
| **NEXT / LEFT / LAST / PASSED** | `systemctl list-timers` | Absolute answers "when", relative answers "soon?", and the LAST+PASSED pair is how you spot a dead timer | **Adopt verbatim** as the schedules table's spine. |
| **Run-history strip** | Airflow Grid view (colour-coded square per run); Unicode sparklines `▁▂▃▄▅▆▇█` | Seven glyphs say "healthy / flaky / dead" faster than seven timestamps | **Adopt.** A 7-cell strip per schedule and per goal, with `✗` for failures so colour is never load-bearing. |
| **Focus + neighbours instead of a drawn graph** | zk `--link-to` / `--linked-by`; Obsidian's local graph beating its global one; backlinks panes generally | Terminal rows are the scarce resource; one node with in-edges above and out-edges below fits and reads | **Adopt as the graph view.** `⏎` re-centres, `Backspace` walks back, no layout algorithm anywhere. |
| **`$EDITOR` handoff for free text** | Claude Code `Ctrl+G` / `Ctrl+X Ctrl+E`; `crontab -e`; `git commit`; `visudo`'s re-edit-on-error | The user already has a configured editor; a TUI textarea will never beat it for a 40-line prompt | **Adopt as tier 3** of the form ladder, on a TOML buffer, re-opened with the error as a comment if it fails to parse. |
| **Structured form with per-field validation** | `huh` (`Validate`, `ValidateLength`, grouped multi-page forms, plus an accessibility mode that switches to plain prompts for screen readers); posting's tabbed request editor | Right for 2–6 known fields where free text would only be a chance to typo | **Adopt as tier 2.** |
| **Inline prompt line** | k9s `:` line; lazygit prompts; atuin's inline mode | One value, no screen change, no context lost | **Adopt as tier 1.** |
| **Reverse-search history** | atuin (`Ctrl-R`, filter modes global/host/session/directory); Claude Code `Ctrl+R` | The prompts you have already sent are your most reusable asset | **Adopt.** `Ctrl-R` over `App::history`, which already exists and is already deduplicated. |
| **Background work, notice on completion** | Claude Code `Ctrl+B` "Backgrounds Bash commands and agents"; Jod's `Ctrl-B` | Identical semantics on an identical key — free muscle memory for the target user | **Keep exactly as is.** |
| **Persistent unread badge + activity log** | Claude Code's PR-status badge that "refreshes every 60 seconds"; k9s's live counts | Endings that arrive while you are away have to survive until you look | **Adopt.** `⚑ n` in the status bar; the Activity screen is the durable record. |
| **OSC 9 / OSC 777 desktop notification** | Ghostty, WezTerm, urxvt (777); iTerm2, kitty, foot, libvte (9) | Gets a goal escalation to you when the terminal is not focused | **Adopt, opt-in**, with tmux passthrough wrapping and a config flag. |
| **Undo backed by a log you already keep** | lazygit's `z`/`Z` via the reflog | Cheaper than confirmation dialogs, and strictly better | **Adopt for memory only** — memory writes are already events in SQLite, so `u` can un-write the last edit. Impossible for a killed process; there, confirm instead. |
| **Locked mode** | zellij `Ctrl+g` | Stops a stray keypress doing something while you read | **Skip.** Jod's chat pane already absorbs stray keys into the input box; a lock would be a mode with no purpose. |

---

## 4. The design

### 4.1 Navigation model

**Chat is home. Everything else is a workspace you return from. There are three
ways in, aimed at three levels of fluency, and they all reach the same place.**

1. **`Ctrl-K` — the which-key menu.** For someone who has memorised nothing. One
   free chord, a popup listing all eight workspaces with live counts, one letter
   each. Recognition, not recall.
2. **`/schedules`, `/memory`, `/goals`, `/hooks`, `/activity` — the palette.**
   For someone who types. This is the *existing* slash system with five new
   verbs; the completion popup already narrows as you type and already shows a
   hint column.
3. **`Ctrl-A` (fleet), `Ctrl-G` (team), `1`–`8` (from any workspace).** For
   someone who is fast. `Ctrl-A` and `Ctrl-G` keep exactly their current
   meanings.

**Not chosen, and why.** A `:` command mode alongside `/` (the k9s spelling)
would give Jod two prefixes doing one job — `/` already parses, completes and
reports unknown commands, and a second prefix is pure recall cost for no new
capability. Full modal editing (the helix spelling) fails H3: the default
posture here is typing prose. One new chord per screen fails on arithmetic —
five screens, seven free chords, and nothing left for the sixth feature.

**One back key.** `Esc` goes back exactly one level and never does anything
else:

```
memory · local graph  ──Esc──▶  memory · list  ──Esc──▶  chat
        ▲                              ▲                    ▲
   ⏎ re-centres,                  / filter active:      Esc here means
   Backspace pops                 Esc clears it first   "follow the tail"
   the visit stack                                      (unchanged today)
```

`q` stays a synonym for `Esc` in workspaces, as it is today.

**Three layers, and the status bar always says which one you are in.**

| Layer | What owns the keyboard | How you can tell |
|---|---|---|
| **Chat** | the input box; letters are text | the input box is bordered and the cursor is in it |
| **Workspace** | the list; letters are commands | the title bar names the workspace; the keybar lists its verbs |
| **Overlay** (which-key, `?`, form, confirm) | the overlay; `Esc` cancels | drawn over everything with a `Clear`, as the existing panels already are |

### 4.2 Screen inventory

| # | Screen | Reached by | Shape | Detail |
|---|---|---|---|---|
| 1 | **Chat** | `Esc` from anywhere, `Ctrl-K c`, `1` | transcript + input + status | — |
| 2 | **Fleet** | `Ctrl-A`, `/agents`, `Ctrl-K f`, `2` | master/detail 48/52 | harness, cwd, pid, spend, last message, tool tail |
| 3 | **Memory · list** | `/memory`, `Ctrl-K m`, `3` | master/detail 48/52 | node body, in-edges, out-edges, provenance |
| 3b | **Memory · local graph** | `g` from the memory list | full width | focus node with in-edges above, out-edges below |
| 4 | **Schedules** | `/schedules`, `Ctrl-K s`, `4` | table over detail | cron + human gloss, prompt, policies, last five runs |
| 5 | **Goals** | `/goals`, `Ctrl-K g`, `5` | table over detail | objective, done-when checklist, stop conditions, budget, iteration log |
| 6 | **Webhooks** | `/hooks`, `Ctrl-K h`, `6` | table over detail | endpoint, secret state, match rule, prompt template, deliveries |
| 7 | **Activity** | `/activity`, `Ctrl-K a`, `Ctrl-N`, `7` | grouped feed | — (`⏎` jumps to the thing itself) |
| 8 | **Team** | `Ctrl-G`, `/team`, `Ctrl-K t`, `8` | members + board | — (unchanged) |
| — | **Which-key** | `Ctrl-K` | overlay | — |
| — | **Keymap** | `?` on empty input | overlay, screen-aware | — |
| — | **Form / confirm** | `n`, `e`, `x` | overlay | — |

The asymmetry is deliberate: **fleet and memory get a side-by-side detail pane
because you scan them; schedules, goals and webhooks get a detail block
underneath because you read one at a time.** Ten rows of table plus twelve rows
of detail beats twenty-two rows of table for objects whose interesting fields
are prose.

### 4.3 The keymap

**Every binding below is checked against §1. Nothing collides, and no existing
binding changes meaning.**

#### New global chords — four, all from the free list

| Key | Action | Why this key |
|---|---|---|
| `Ctrl-K` | which-key menu | free; a leader (helix `Space`, vim-which-key) needs a key nothing else wants |
| `Ctrl-R` | reverse-search sent prompts | free; identical to Claude Code `Ctrl+R` and atuin's `Ctrl-R` |
| `Ctrl-N` | jump to the oldest unread activity item | free; only meaningful once cron and webhooks exist |
| `Ctrl-F` | open the current input (or focused form field) in `$EDITOR` | free. **Deviation, stated plainly:** Claude Code uses `Ctrl+G`, but `Ctrl-G` is Jod's team panel and is documented in `docs/jod-system.md`. `Ctrl-K e` is the discoverable alias. |

`Ctrl-P`, `Ctrl-V` and `Ctrl-Y` stay unbound — deliberate slack, so the next
feature does not have to break something.

#### Chat layer — two additions

| Key | Action |
|---|---|
| `?` **on an empty input** | toggle the keymap overlay. With text in the input, `?` is a literal character. This is exactly Claude Code's rule, including the edge case that backspacing down to a lone `?` must **not** fire it. |
| `1`–`8` | literal text, as today. Direct workspace jump is workspace-layer only. |

#### Which-key suffixes (after `Ctrl-K`)

`c` chat · `f` fleet · `m` memory · `s` schedules · `g` goals · `h` hooks ·
`a` activity · `t` team · `e` `$EDITOR` · `n` → new… (`n s` schedule, `n g`
goal, `n h` hook, `n m` memory, `n t` task) · `?` keymap · `Esc` cancel.
Any other key cancels silently rather than doing something surprising.

#### Workspace layer — the spine (identical on every screen)

| Key | Action |
|---|---|
| `Esc` / `q` | back one level (clears an active filter first) |
| `↑` `k` / `↓` `j` | move the cursor; never wraps — `App::step` is already right about this |
| `PageUp` / `PageDown` | move one screen |
| `Home` / `End` | first / last |
| `Tab` / `Shift-Tab` | move focus master ↔ detail |
| `⏎` | the one obvious thing (per screen, below) |
| `/` | fuzzy filter this list; `Esc` clears it |
| `n` | new item of this screen's kind (enters the form ladder) |
| `e` | edit the selected item |
| `x` | delete / forget the selected item — **typed-name confirmation** |
| `p` | pause / resume |
| `r` | run now / resume |
| `S` | cycle sort |
| `y` | copy the selected item's id or URL to the clipboard (OSC 52) |
| `1`–`8` | jump straight to workspace N |
| `?` | keymap overlay, this screen's keys first |

#### Workspace layer — screen-local verbs (always printed on that screen's keybar)

| Screen | `⏎` does | Local keys |
|---|---|---|
| Fleet | watch the run (closes to chat, as today) | `s` stop · `a` attach · `r` resume the conversation · `d` delegate the same prompt again |
| Memory · list | open the node in the detail pane | `g` local graph · `l` link two nodes · `t` filter by type · `u` undo the last memory write |
| Memory · graph | re-centre on the highlighted neighbour | `Backspace` walk back · `h` toggle 1-hop / 2-hop · `f` filter by edge kind · `g` back to the list |
| Schedules | open the last run's transcript | `r` run now · `p` pause/resume · `t` dry run — compute the next five fire times |
| Goals | open the last iteration | `r` run an iteration now · `p` pause · `a` answer the pending escalation |
| Webhooks | open the run a delivery started | `t` test with a sample payload · `c` copy the endpoint URL · `p` pause |
| Activity | jump to the object the event is about | `m` mark read · `M` mark all read · `u` unread only · `f` cycle source filter |
| Team | mark the task done (as today) | unchanged |

**Collisions checked.** Fleet's `s`/`a`/`r` are exactly what they are today.
`S` (capital) is the new sort key precisely because lowercase `s` is spoken for
in fleet. Goals' `a` (answer) and fleet's `a` (attach) differ — that is the
lazygit rule, "the same key changes meaning per panel", and it is safe **only
because both are on the keybar at all times**. That is the condition, not an
afterthought.

#### New slash commands (parser + `HELP` + completions)

`/memory [query]` · `/schedules` · `/schedule <name>` · `/goals` ·
`/goal <name>` · `/hooks` · `/hook <name>` · `/activity` ·
`/new schedule|goal|hook` · `/pause <name>` · `/unpause <name>` ·
`/run <name>` (fire a schedule or a goal iteration now) · `/remember <text>` ·
`/forget <name>`

Each must satisfy the existing tests `every_documented_command_parses` and
`every_suggested_command_parses`, and argument completion should be wired the
way `/watch` already is — offering live names, so nobody retypes an id.

### 4.4 Forms: a three-tier ladder

| Tier | When | How | Precedent |
|---|---|---|---|
| **1 — inline prompt** | exactly one value (rename, set a budget, pause until) | the keybar row becomes a prompt: `name ▸ nightly-inbox▏` — `⏎` accepts, `Esc` cancels | k9s's `:` line, lazygit prompts |
| **2 — modal form** | 2–6 known fields with closed sets (harness, permission, cadence, repo, event) | centred overlay; `Tab`/`Shift-Tab` between fields, `⏎` next, `Ctrl-S` save, `Esc` → "discard changes? (y/N)"; validation shown under the field, never as a popup | `huh`'s grouped forms with `Validate`; posting's tabbed editor |
| **3 — `$EDITOR` on TOML** | any free text (a prompt), any expression (cron, a jq filter), or anything you would want to diff | write a commented TOML buffer, spawn `$EDITOR` (suspend the TUI and restore on return — the same discipline as `enter`/`restore` in `mod.rs:88`, including the panic hook), parse on save; **if it does not parse, re-open it with the error as a `#` comment at the top** rather than discarding the work | `crontab -e`, `git commit`, `visudo`'s re-edit loop, Claude Code `Ctrl+G` |

`n` on schedules goes straight to tier 3 — a schedule is a cron expression plus
a prompt, and the prompt is the whole point. `n` on a webhook is tier 2 then
tier 3: pick repo and event from closed sets, then edit the prompt template.

### 4.5 Feedback for work you are not watching

Four tiers, each strictly more durable than the last:

1. **Transcript notice** — what exists today (`announce()`), for the session you
   are sitting in.
2. **Status-bar badge** — `⚑ 3` at the right of the status bar, on every screen,
   always. Nielsen #1: "The design should always keep users informed about what
   is going on, through appropriate feedback within a reasonable amount of
   time."
3. **Activity screen** — the durable log. Survives the process, because it is
   read from SQLite the way the team board already is (`refresh_team`,
   `mod.rs:868`), and for the same reason: other processes write it.
4. **OSC 9 / OSC 777** — a real desktop notification, opt-in, for endings that
   need a human: a goal escalation, a schedule's third consecutive failure, a
   webhook whose secret stopped verifying. Wrap for tmux passthrough and expect
   it to be stripped when passthrough is off.

Progress *inside* a run keeps the existing spinner. Nielsen's thresholds justify
the shape: under 1 s no feedback is needed; 1–10 s wants a conspicuous
indicator; beyond 10 s wants a percent-done "combined with a clear way to
cancel". Agent runs are minutes, so they get the elapsed clock — a proxy for
percent-done, since there is no honest denominator — and `Ctrl-X` to cancel.
**Goals are the one place a real percent-done exists**, because the done-when
checklist *is* a denominator. That is why the goals screen shows a progress bar
and the fleet screen does not.

### 4.6 Colour, density, accessibility

- **Eight named ANSI colours plus bold / dim / reverse. Nothing else by
  default.** `ui.rs` already does this; keep it. A truecolor theme may exist
  behind a config flag, but must not be the default: "each truecolor program
  needs its own theme configuration", and Jod runs on other people's boxes over
  SSH.
- **Honour `NO_COLOR`** — present and non-empty means no ANSI colour at all.
- **Never blue on the default background.** Jod uses cyan for the user, which is
  right; blue is the one to avoid.
- **Colour is never the only channel.** Every state carries a glyph: `●`
  running, `✓` done, `✗` failed, `■` killed, `‖` paused, `○` idle, `⚠`
  contradiction. `NO_COLOR` users, 8-colour terminals and colour-blind users all
  get the same information.
- **Glyph width is a correctness issue, not a style one.** Avoid East-Asian
  *Wide* codepoints inside aligned columns. `⏰` (U+23F0) and `⏸` (U+23F8) are
  Wide: each occupies two cells and shears every column to its right. This was
  hit while drawing the wireframes for this report and fixed by switching to
  `◷` and `‖`. Stay inside the vocabulary `ui.rs` already proves safe —
  `✓ ✗ ○ ◐ ▸ ⚙ ⠋ ← ⏎ ↑ ↓ • └` — plus the sparkline blocks `▁▂▃▄▅▆▇█`.
- **Density: 100×30 is the design target; 80×24 is the contract.** Nothing may
  clip and nothing may scroll sideways; columns die in a declared order.

| Screen | Columns dropped first → last at 80 columns |
|---|---|
| Fleet | detail pane → harness → id (name only) |
| Memory | detail pane → degree → confidence |
| Schedules | 7-day strip → the LAST/AGO pair → human gloss (the cron expression stays) |
| Goals | iteration number → cadence → progress bar (the percent stays) |
| Webhooks | 24 h count → repo → event-filter detail |
| Activity | source column (the glyph already carries it) → seconds |

---

## 5. Wireframes — 100 × 30

Nine screens. Every one was generated and machine-checked at exactly 100 columns
by 30 rows, with real-looking data, the keybar and the status bar.

### 5.1 Chat

The ground state, unchanged in shape from today. What is new: a keybar row above
the status bar, the `⚑ 3 unread` badge on the right, and notices arriving from
cron, webhooks and memory alongside the run notices that already exist.

```text
┌─ chat · port the parser · a3f91c22 · running ────────────────────────────────────────────────────┐
│ › rebase the parser branch onto main and run the whole suite                                     │
│                                                                                                  │
│   Thinking · the branch is 6 commits behind; rebase is safer than merge                          │
│   here because history has to stay linear.                                                       │
│                                                                                                  │
│   I'll rebase first, then run the tests before touching anything else.                           │
│ ⚙ Bash · git rebase origin/main                                                                  │
│   └ Successfully rebased and updated refs/heads/feat/parser.                                     │
│ ⚙ Read · core/src/parser.rs                                                                      │
│   └ 412 lines                                                                                    │
│ ⚙ Bash · cargo test -p jod-core                                                                  │
│   └ test result: ok. 214 passed; 0 failed; 3 ignored; finished in 18.44s                         │
│                                                                                                  │
│   Rebased cleanly — no conflicts. 214 tests pass and the three ignored ones                      │
│   were already ignored on main. Pushed with --force-with-lease.                                  │
│                                                                                                  │
│ ✓ done · 4m12s · $0.31                                                                           │
│                                                                                                  │
│ • ✓ audit-the-deps completed after 11m40s — Ctrl-A to open it                                    │
│ • ◷ nightly-inbox ran at 02:00 — 3 items triaged, 1 needs you (Ctrl-K a)                         │
│ • ⚑ gh:ci-failed fired 2m ago → triage-ci is running                                             │
│ • ◆ 3 memories written · 1 contradiction raised (Ctrl-K m)                                       │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌─ you · 1 queued ─────────────────────────────────────────────────────────────────────────────────┐
│ draft the release notes for 0.4 and post them to the team board▏                                 │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 Ctrl-B delegate · Ctrl-A fleet · Ctrl-K menu · / commands · ? keys       Ctrl-X stop · Ctrl-C quit 
 Claude Code · claude-opus-5 · $0.42 · ⠹ 4m12s · 2 running · 1 queued                    ⚑ 3 unread 
```

### 5.2 Which-key overlay (`Ctrl-K`, drawn over chat)

The discoverability spine. Each row carries a **live count**, so the menu is also
a dashboard — you often get your answer without pressing the second key.

```text
┌─ chat · port the parser · a3f91c22 · running ────────────────────────────────────────────────────┐
│   Rebased cleanly — no conflicts. 214 tests pass and the three ignored ones                      │
│   were already ignored on main. Pushed with --force-with-lease.                                  │
│                                                                                                  │
│ ✓ done · 4m12s · $0.31                                                                           │
│                                                                                                  │
│        ┌─ Ctrl-K ───────────────────────────────────────────────────┐                            │
│        │  c  chat            the conversation                       │                            │
│        │  f  fleet           14 runs · 3 running · 2 failed         │                            │
│        │  m  memory          142 nodes · 1 contradiction            │                            │
│        │  s  schedules       8 · next vps-healthcheck in 13s        │                            │
│        │  g  goals           5 · 1 blocked · 1 needs you            │                            │
│        │  h  hooks           6 webhooks · 1 failing                 │                            │
│        │  a  activity        3 unread                               │                            │
│        │  t  team            crew · 4 members · 6 open tasks        │                            │
│        │                                                            │                            │
│        │  n  new…            n s schedule · n g goal · n h hook     │                            │
│        │  ?  keys            the whole keymap                       │                            │
│        └─ Esc cancels · any other key is ignored ───────────────────┘                            │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 Ctrl-K … waiting for a key                                                             Esc cancels 
 Claude Code · claude-opus-5 · $0.42 · ⠹ 4m12s · 2 running                               ⚑ 3 unread 
```

### 5.3 Fleet

Today's `Ctrl-A` panel promoted to a full screen with a detail pane and a filter.
Every key the current panel has keeps its meaning; `/` and the detail pane are
the additions.

```text
┌─ fleet ──────────────────────────────────────┐┌─ run · port-the-parser ──────────────────────────┐
│ ▸ ● a3f91c22 running   4m12s cc  port-the-par││ port-the-parser                                  │
│   ● 77b02e10 running  11m40s agy audit-the-de││ a3f91c22-8e40-4b19-9a71-2c6df0e18aa3             │
│   ● 1d9f0034 running  26m03s cc  triage-ci ⚑ ││                                                  │
│   ✓ 5c18aa93 done      2h05m cc  write-the-do││ harness  Claude Code · claude-opus-5             │
│   ✓ 3b7e6612 done      2h44m oc  bump-version││ cwd      ~/repo/Jod                              │
│   ✗ 0e4471bd failed    3h11m oc  migrate-stor││ started  16:40:12 (4m12s ago)   spend  $0.31     │
│   ✓ c0ffee11 done      5h20m cc  spec-review ││ session  sess-7f3a91c2 · pid 40118 / pgid 40118  │
│   ■ 91ac7752 killed   1d04h  cc  refactor-run││ source   you, 16:40 (chat)                       │
│   ✓ 8ab31d09 done     1d06h  cc  nightly-inbo││                                                  │
│   ✓ 44de1c7a done     2d01h  agy shepherd-prs││ last     Rebased cleanly — no conflicts. 214     │
│   ✓ 2f9c88b1 done     2d09h  cc  write-the-sp││          tests pass and the three ignored ones   │
│   ✗ 6e1a0d55 failed   3d14h  oc  port-the-api││          were already ignored on main.           │
│   ✓ b7c2e340 done     4d02h  cc  deps-audit  ││                                                  │
│   ✓ 19f4aa88 done     5d11h  cc  weekly-revie││ tools    ⚙ Bash  git rebase origin/main    ok    │
│                                              ││          ⚙ Read  core/src/parser.rs        ok    │
│ ─────────────────────────────────────────────││          ⚙ Bash  cargo test -p jod-core    ok    │
│ 14 runs · 3 running · 2 failed · $4.18 today ││          ⚙ Edit  core/src/parser.rs        ok    │
│                                              ││          ⚙ Bash  git push --force-w-lease  ok    │
│ /port         ▸ filter (2 of 14 match)       ││                                                  │
│                                              ││ memory   wrote 2 · read 7                        │
│                                              ││                                                  │
│                                              ││                                                  │
│                                              ││                                                  │
│                                              ││                                                  │
│                                              ││                                                  │
│                                              ││                                                  │
└─ ↑↓ pick · / filter ─────────────────────────┘└─ ⏎ watch · s stop · r resume · w attach ─────────┘
 ⏎ watch · s stop · r resume · d delegate again · w attach · / filter             Esc back · ? keys 
 fleet · 3 running · 2 failed · $4.18 today                                              ⚑ 3 unread 
```

### 5.4 Memory — list

Type is carried by a three-letter tag (`blf` belief, `ent` entity, `epi`
episode, `pro` procedure, `fact`) **and** a glyph, so neither colour nor glyph
alone has to work. `deg` is the node's degree — the cheapest honest answer to
"is this memory load-bearing?". `!` marks a node in an unresolved contradiction.

```text
┌─ memory · list ──────────────────────────────┐┌─ prefers-spec-first ─────────────────────────────┐
│  type    name                  conf  deg  age││ prefers-spec-first                     belief    │
│  ────────────────────────────────────────────││ conf 0.86 · 17 edges · seen 23× · 3d ago         │
│ ▸◆ blf   prefers-spec-first    0.86   17  3d ││ ──────────────────────────────────────────────── │
│  ◆ blf   linear-is-truth       0.94    9  3d ││ Non-trivial work starts with a spec, not a plan. │
│  ◆ blf   reversible-by-default 0.91    6  1w ││ Interview until nothing material is guessed,     │
│  ○ blf   ship-fast-iterate     0.31    2  6w!││ write SPEC.md, execute it in a fresh session.    │
│  ● ent   reljod                1.00   41  1w ││                                                  │
│  ● ent   jod-cloud (vps)       1.00   12  1w ││ ▲ linked from (3)                                │
│  ● ent   Reljod/Jod (repo)     1.00   28  1w ││   supports     ◆ linear-is-truth                 │
│  ▤ epi   2026-08-04 spec-retro 1.00    5  6d ││   supports     ● reljod                          │
│  ▤ epi   2026-07-29 vps-outage 1.00    8  12d││   refines      ▦ how-to-open-a-pr                │
│  ▦ pro   how-to-open-a-pr      1.00   11  3w ││                                                  │
│  ▦ pro   how-to-merge-unread   1.00    7  3w ││ ▼ links to (2)                                   │
│  ◇ fact  tz = Asia/Manila      1.00    3  8w ││   contradicts  ○ ship-fast-iterate          ⚠    │
│  ◇ fact  budget cap $40/day    1.00    4  2w ││   derived-from ▤ 2026-08-04 spec-retro           │
│                                              ││                                                  │
│  ────────────────────────────────────────────││ provenance                                       │
│  142 memories · 61 beliefs · 38 entities     ││   first  run 2f9c88b1 write-the-spec  06-11      │
│  1 contradiction unresolved (! marks it)     ││   last   run c0ffee11 spec-review     08-07      │
│                                              ││   source AGENTS.md §How work runs                │
│  /spec       ▸ filter · 4 of 142 match       ││                                                  │
│                                              ││                                                  │
│                                              ││                                                  │
│                                              ││                                                  │
│                                              ││                                                  │
│                                              ││                                                  │
└─ ↑↓ pick · / filter · t type ────────────────┘└─ g local graph · e edit · x forget ──────────────┘
 g graph · e edit · n new · l link · x forget · / filter · t type · s sort        Esc back · ? keys 
 memory · 142 nodes · 318 edges · 1 contradiction                                        ⚑ 3 unread 
```

### 5.5 Memory — local graph

**This is the answer to "how do you draw a graph in a terminal": you don't.**
You draw one node and its neighbours — incoming above, outgoing below — and make
re-centring a single keypress. No layout algorithm, no edge crossings, no zoom,
and it still reads at 80 columns. `⏎` re-centres on the highlighted neighbour
and pushes the old focus onto a visit stack; `Backspace` pops it. The trail
along the bottom is where you have been. When a node has more edges than the
pane has rows, the header says so ("hop 1 shows 5 of 17 edges") rather than
silently truncating.

```text
┌─ memory · local graph · prefers-spec-first ──────────────────────────────────────────────────────┐
│                                                                                                  │
│                                   ▲  linked from — 3                                             │
│                                                                                                  │
│         ◆ linear-is-truth ──────────── supports ─────────────┐                                   │
│         ● reljod ───────────────────── supports ─────────────┤                                   │
│         ▦ how-to-open-a-pr ─────────── refines ──────────────┤                                   │
│                                                              │                                   │
│                                                              ▼                                   │
│                         ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓                             │
│                         ┃  ◆  prefers-spec-first                   ┃                             │
│                         ┃     belief · conf 0.86 · seen 23×        ┃                             │
│                         ┃     "Non-trivial work starts with a      ┃                             │
│                         ┃      spec, not a plan."                  ┃                             │
│                         ┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛                             │
│                                                              │                                   │
│                                                              ▼                                   │
│         ○ ship-fast-iterate ◀──────── contradicts ⚠ ─────────┤                                   │
│         ▤ 2026-08-04 spec-retro ◀──── derived-from ──────────┘                                   │
│                                                                                                  │
│                                   ▼  links to — 2                                                │
│                                                                                                  │
│   ───────────────────────────────────────────────────────────────────────────────────────────    │
│   hop 1 shows 5 of 17 edges.  ⇧+↑↓ walks the ranked edge list · ⏎ re-centres on it               │
│   ⟨  reljod  ⟩  ⟨ linear-is-truth ⟩  ⟨ prefers-spec-first ⟩         ← where you have been        │
│                                                                                                  │
│                                                                                                  │
└─  ↑↓ neighbour · ⏎ re-centre · Backspace back · h hops 1|2 · l list  ────────────────────────────┘
 ⏎ re-centre · ↑↓ neighbour · Backspace back · h hops · f edge kind · g list      Esc back · ? keys 
 memory · 142 nodes · 318 edges · focus prefers-spec-first (17 edges)                    ⚑ 3 unread 
```

### 5.6 Schedules

The table is `systemctl list-timers` with a run-history strip added: **when**
(human gloss) · **next** (absolute) · **in** (relative) · **last** (absolute) ·
**ago** (relative) · **7d** (outcome strip). The raw cron expression lives in
the detail block, not the table — a column of `0 2 * * *` is a column nobody can
read at a glance.

```text
┌─ schedules · 8 · next: vps-healthcheck in 13s ───────────────────────────────────────────────────┐
│    name             when                next            in       last            ago    7d       │
│    ───────────────────────────────────────────────────────────────────────────────────────────   │
│  ▸ ● nightly-inbox   02:00 every day     Aug 11 02:00    9h14m    Aug 10 02:00    14h    ▇▇▇▇▇▇▇ │
│    ● pr-shepherd     every 30 minutes    Aug 10 17:00      14m    Aug 10 16:30    16m    ▇▇▇▅▇▇▇ │
│    ● weekly-review   Mon 08:00           Aug 17 08:00    6d15h    Aug 10 08:00     8h    ▇▁▇▇▇▇▇ │
│    ● finance-sync    09:00 Mon–Fri       Aug 11 09:00   16h14m    Aug 08 09:00     2d    ▇▇▇✗▇▇▇ │
│    ● vps-healthcheck every 15 minutes    Aug 10 16:59       13s   Aug 10 16:45     1m    ▇▇▇▇▇▇▇ │
│    ‖ deps-audit      Sun 03:00           —  paused        —       Aug 03 03:00     7d    ▇▇▁▁▁▁▁ │
│    ✗ notion-sync     04:00 every day     Aug 11 04:00   11h14m    Aug 10 04:00    12h    ✗✗▇▇▇✗✗ │
│    ● backup-jod-db   23:30 every day     Aug 10 23:30    6h44m    Aug 09 23:30    17h    ▇▇▇▇▇▇▇ │
│                                                                                                  │
│    ────────────────────────────────────────────────────────────────────────────────────────────  │
│    nightly-inbox                                          cron  0 2 * * *   ·  Asia/Manila       │
│                                                                                                  │
│    prompt   Triage the Linear inbox. Close what is done, escalate what is blocked, and leave a   │
│             one-line note on anything you touched.                                               │
│    runs as  Claude Code · claude-opus-5 · ~/repo/Jod · permission: acceptEdits                   │
│    policy   overlap: skip  ·  missed run: run once on wake  ·  timeout 20m  ·  budget $2/run     │
│                                                                                                  │
│    history  Aug 10 02:00  ✓  4m18s  $0.44   3 items triaged, 1 escalated                         │
│             Aug 09 02:00  ✓  3m51s  $0.39   1 item triaged                                       │
│             Aug 08 02:00  ✓  5m02s  $0.51   6 items triaged, 2 escalated                         │
│             Aug 07 02:00  ✓  4m44s  $0.47   2 items triaged                                      │
│             Aug 06 02:00  ✓  2m19s  $0.22   nothing to do                                        │
│                                                                                                  │
│                                                                                                  │
└─  ↑↓ pick · ⏎ open the last run · r run now · p pause · e edit · n new · x delete  ──────────────┘
 ⏎ last run · r run now · p pause/resume · e edit · n new · x delete · / filter   Esc back · ? keys 
 schedules · 6 armed · 1 paused · 1 failing · next in 13s                                ⚑ 3 unread 
```

### 5.7 Goals

A goal is a schedule with a **denominator**. That is the whole design
difference: because "done when" is a checklist, there is a real percent-done, so
goals get a progress bar where nothing else does. The escalation line at the
bottom is the important part — a looping objective that quietly needs you and
never says so is worse than no goal at all.

```text
┌─ goals · 5 · 2 running · 1 blocked · 1 escalation waiting on you ────────────────────────────────┐
│    name                 cadence      progress            last      next     state                │
│    ──────────────────────────────────────────────────────────────────────────────────────────    │
│  ▸ ◎ inbox-to-zero       hourly       ▓▓▓▓▓▓▓░░░  71%     42m ago   in 18m   running  iter 118   │
│    ◎ keep-ci-green       continuous   ▓▓▓▓▓▓▓▓▓▓ 100%      4m ago   in  6m   satisfied iter 903  │
│    ◎ ship-ios-client     daily        ▓▓▓░░░░░░░  31%      6h ago   in 18h   waiting  iter  24   │
│    ◎ reduce-vps-spend    weekly       ▓▓░░░░░░░░  18%      3d ago   in  4d   blocked  iter   6   │
│    ◎ zero-open-prs       every 30m    ▓▓▓▓▓▓▓▓░░  84%     11m ago   in 19m   running  iter 412   │
│                                                                                                  │
│    ───────────────────────────────────────────────────────────────────────────────────────────   │
│    inbox-to-zero                                                          started 2026-06-02     │
│                                                                                                  │
│    objective  Keep the Linear inbox at zero open items older than 48 hours.                      │
│    done when  ☑ no item older than 48h    ☑ every open item has an owner                         │
│               ☐ no item blocked without a written reason   ← 3 items fail this                   │
│    stop if    budget $25/week spent  ·  6 iterations with no progress  ·  you say stop           │
│    budget     $11.40 of $25.00 this week   ▓▓▓▓▓░░░░░                                            │
│                                                                                                  │
│    iterations 118  16:02  +4 items closed, 3 still blocked            5m11s  $0.38  ✓            │
│               117  15:02  +1 item closed                              2m40s  $0.19  ✓            │
│               116  14:02  nothing to do                               0m48s  $0.04  ✓            │
│               115  13:02  +2 closed, escalated ENG-441 to you         6m22s  $0.51  ✓            │
│               114  12:02  harness timed out after 20m                20m00s  $1.02  ✗            │
│                                                                                                  │
│    escalations  ENG-441 needs a decision from you — raised 13:02, still open                     │
│                                                                                                  │
│                                                                                                  │
└─  ↑↓ pick · ⏎ open the last iteration · r run now · p pause · e edit · n new  ───────────────────┘
 ⏎ last iteration · r run now · p pause · e edit · n new · a answer escalation    Esc back · ? keys 
 goals · 2 running · 1 blocked · $11.40 this week                                        ⚑ 3 unread 
```

### 5.8 Webhooks

Three questions the screen must answer without a drill-down: *is it armed*, *is
the secret still verifying*, and *what did the last delivery actually start*.
The delivery list joins straight to the fleet — `⏎` on a delivery opens the run
it created.

```text
┌─ webhooks · 6 · 5 armed · 1 failing · 214 deliveries all time ───────────────────────────────────┐
│    name             repo                event                        runs        24h  last       │
│    ──────────────────────────────────────────────────────────────────────────────────────────    │
│  ▸ ● pr-opened      Reljod/Jod          pull_request.opened          review-pr    18   2m  ✓     │
│    ● ci-failed      Reljod/Jod          workflow_run.completed ✗     triage-ci     3  41m  ✓     │
│    ● issue-labeled  Reljod/Jod          issues.labeled [jod]         plan-issue    6   4h  ✓     │
│    ● review-asked   Reljod/Jod          pull_request.review_req      review-pr    11   1h  ✓     │
│    ○ push-main      Reljod/jod-cloud    push refs/heads/main         deploy-vps    0   —   —     │
│    ✗ release-cut    Reljod/Jod          release.published            announce      1   2d  ✗     │
│                                                                                                  │
│    ──────────────────────────────────────────────────────────────────────────────────────────    │
│    pr-opened                                              created 2026-07-14 · 214 deliveries    │
│                                                                                                  │
│    endpoint   https://jod.reljod.dev/hooks/gh/pr-opened          secret  ✓ verified 2m ago       │
│    match      event = pull_request  ·  action = opened  ·  base = main  ·  draft = false         │
│    runs       review-pr   Claude Code · ~/repo/Jod · permission: plan · budget $1.50             │
│    prompt     Review PR #{{number}} "{{title}}" by {{author}} against REVIEW.md. Veto only.      │
│    policy     dedupe by delivery id · 1 run per PR at a time · queue depth 4 · retry 3×          │
│                                                                                                  │
│    deliveries 16:42  8f2a1c  PR #212 port the parser        ✓ 202  → a3f91c22  running           │
│               15:10  71b93e  PR #211 bump versions          ✓ 202  → 3b7e6612  ✓ clear           │
│               11:58  2c0dd4  PR #210 migrate the store      ✓ 202  → 0e4471bd  ✗ failed          │
│               09:31  a41f77  PR #209 write the docs         ✓ 202  → 5c18aa93  ✓ vetoed          │
│               08:02  55e0b1  PR #208 spec review            ✓ 202  → c0ffee11  ✓ clear           │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
└─  ↑↓ pick · ⏎ open the delivery's run · t test with a sample payload · p pause · e edit  ────────┘
 ⏎ open run · t test payload · p pause · e edit · n new · c copy URL · x delete   Esc back · ? keys 
 webhooks · 5 armed · 1 failing · 28 deliveries today                                    ⚑ 3 unread 
```

### 5.9 Activity

The durable answer to "what happened while I was away". Grouped by day, one
glyph per source, an unread dot in the left gutter, and `⏎` jumps to the object
rather than showing a copy of it.

```text
┌─ activity · 3 unread · 1 needs you ──────────────────────────────────────────────────────────────┐
│    today — Monday 10 August                                                                      │
│    ──────────────────────────────────────────────────────────────────────────────────────────    │
│  ▸ ● 16:44  run      ✓ port-the-parser finished · 4m12s · $0.31 · 214 tests pass                 │
│    ● 16:42  hook     ⚑ pr-opened fired (PR #212) → triage started as a3f91c22                    │
│    ● 16:32  hook     ⚑ ci-failed fired → triage-ci running 26m                                   │
│      16:30  cron     ◷ pr-shepherd ran · 3 PRs swept · 1 merged · 0 vetoed · 1m04s               │
│    ● 16:02  goal     ◎ inbox-to-zero iteration 118 · 71% (+4) · needs you on ENG-441             │
│      15:41  memory   ◆ 3 memories written by audit-the-deps · 1 contradiction raised             │
│      15:10  hook     ⚑ pr-opened (PR #211) → 3b7e6612 · clear                                    │
│      14:55  run      ✗ migrate-store failed · 3h11m · exit 1 · "store is locked"                 │
│      14:02  goal     ◎ inbox-to-zero iteration 116 · nothing to do                               │
│      12:02  goal     ◎ inbox-to-zero iteration 114 · ✗ harness timed out after 20m               │
│      09:00  cron     ◷ finance-sync skipped — previous run still going (overlap: skip)           │
│                                                                                                  │
│    yesterday — Sunday 9 August                                                                   │
│    ──────────────────────────────────────────────────────────────────────────────────────────    │
│      23:30  cron     ◷ backup-jod-db ✓ 41s · 18.2 MB                                             │
│      04:00  cron     ◷ notion-sync ✗ 401 from Notion — token expired  (3rd failure in a row)     │
│      02:00  cron     ◷ nightly-inbox ✓ 3m51s · 1 item triaged                                    │
│                                                                                                  │
│    ──────────────────────────────────────────────────────────────────────────────────────────    │
│    filter  [all]  runs  cron  goals  hooks  memory      only unread: off      f cycles           │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
└─  ↑↓ pick · ⏎ jump to what it is about · m mark read · M mark all read · f filter  ──────────────┘
 ⏎ jump to it · m mark read · M mark all · u unread only · f filter source        Esc back · ? keys 
 activity · 3 unread · last event 2m ago                                                 ⚑ 3 unread 
```

---

## 6. Anti-patterns to avoid

Each with the failure it causes.

1. **Colour as the only signal.** `NO_COLOR` users, 8-colour terminals and
   colour-blind users lose the state entirely. Every coloured thing gets a
   glyph.
2. **East-Asian *Wide* glyphs inside aligned columns** — `⏰` (U+23F0), `⏸`
   (U+23F8), most emoji. They occupy two cells and shear every column to their
   right. Ambiguous-width glyphs (`● ○ ◆ ✓ ✗`) are fine in the Western default
   but belong in a fixed status column, never mid-text.
3. **A mode with no indicator.** If letters stop being text, the screen must say
   so *before* the user types a sentence into a list and fires six commands.
4. **`Esc` meaning different things on different screens.** One back key, one
   meaning, always.
5. **A panel you can only look at.** Jod already names this in `ui.rs:466` — "A
   panel you can only look at makes you leave the UI to do anything about what
   you saw." A schedules screen without `run now` and `pause` is that panel.
6. **A `?` that lists only global keys.** Help that omits the focused screen's
   verbs sends you to the source. Make it screen-aware.
7. **Blocking the input while work runs.** Jod fixed this once already
   (`mod.rs:545` — prompts are queued, not refused). Do not reintroduce it in a
   form or a confirm dialog; `Esc` must always be live.
8. **Re-sorting a list under the cursor.** The fleet list refreshes every four
   ticks; when a run finishes and the sort key changes, the cursor must stay on
   the *item*, not the *index*. Track selection by id, not by row.
9. **I/O on the render path.** `draw()` must stay a pure function of state. The
   250 ms tick is what refreshes, and `refresh_team` already swallows store
   errors rather than taking the UI down (`mod.rs:874`) — extend that discipline
   to memory, schedules and hooks.
10. **Wrapping cursor movement.** In a list that changes under you, overshooting
    lands somewhere unrelated. `App::step` clamps; keep it.
11. **Mouse-required affordances.** No click targets, no drag, no hover-only
    information. Full mouse capture also breaks native text selection, which is
    how people copy a run id off the screen.
12. **A cron expression with no gloss and no next-run time.** `0 2 * * *` in a
    table is a puzzle. Show `02:00 every day` and `Aug 11 02:00 · in 9h14m`.
13. **Truecolor-only theming.** Breaks on 256-colour terminals, fights the
    user's palette, and needs its own light/dark switch.
14. **Toast-only endings.** An ending that scrolled off the transcript while you
    were away did not happen.
15. **Drawing a global node-link graph.** Impressive at 20 nodes, unusable at
    200 — in a terminal or out of it.
16. **A destructive verb on a bare letter with no confirmation.** `x` deleting a
    webhook silently is one fat-fingered `Ctrl-K h x` away from losing a secret.
17. **Assuming ≥ 120 columns.** Declare the column-drop order; test at 80×24 and
    at absurd sizes, as `ui.rs` already does at `(10,4)`.
18. **Stealing keys the terminal owns.** `Ctrl-S`/`Ctrl-Q` (XON/XOFF),
    `Ctrl-Z` (job control), `Ctrl-H`/`Ctrl-I`/`Ctrl-J`/`Ctrl-M` (aliases of
    Backspace/Tab/Enter). Jod's free list already excludes them; keep it that
    way.
19. **Two prefixes for one job.** Adding `:` beside `/` doubles what has to be
    remembered and halves the chance either is discovered.
20. **A form that discards your input on a validation error.** Re-open it with
    the error attached — `visudo`'s loop — never throw the work away.

---

## 7. What this research does not settle

- **Whether goals need their own screen at all**, or are a schedule with a
  `done_when` field and a progress column. The wireframes assume separate
  because the verbs genuinely differ (`a` answer-escalation has no schedule
  analogue), but that is a judgement, not a finding.
- **How the memory graph ranks neighbours** when a node has 40 edges and the
  pane holds 6. Degree? Recency? Edge-kind priority? The wireframe shows "5 of
  17" and a ranked list; *what* ranks it is a memory-model question, not a UI
  one — see the memory-taxonomy research.
- **Whether `Ctrl-F` for `$EDITOR` is worth the divergence from Claude Code's
  `Ctrl+G`.** The alternative is moving the team panel off `Ctrl-G`, which is a
  documented, tested binding. Owner's call.
- **No usability measurement exists for any of this.** Every grade above is
  convergence-of-practice or a cited argument. Nobody has A/B-tested a keybar
  against a help modal in a terminal, and this report does not pretend
  otherwise.

---

## 8. Sources

**Tools studied — primary documentation**

- lazygit keybindings — https://github.com/jesseduffield/lazygit/blob/master/docs/keybindings/Keybindings_en.md
- lazygit — https://github.com/jesseduffield/lazygit · https://lazygit.dev/features/
- k9s command mode and filters — https://k9scli.io/topics/commands/
- k9s — https://k9scli.io/ · https://www.baeldung.com/ops/k9s-kubernetes-cluster-management
- k9s CronJob next-schedule column request — https://github.com/derailed/k9s/issues/766
- Helix keymap (modal model, space and goto minor modes) — https://docs.helix-editor.com/keymap.html
- Zellij keybindings and locked mode — https://zellij.dev/documentation/keybindings.html
- yazi quick start (Miller columns, vim keys, `F1` cheatsheet) — https://yazi-rs.github.io/docs/quick-start
- posting (command palette `Ctrl+P`, jump mode `Ctrl+O`, tabbed request editor) — https://posting.sh/guide/
- harlequin (split panes, searchable catalog tree, results grid) — https://harlequin.sh/
- atuin (`Ctrl-R`, filter modes, inline vs fullscreen) — https://github.com/atuinsh/atuin
- lazydocker keybindings — https://github.com/jesseduffield/lazydocker/blob/master/docs/keybindings/Keybindings_en.md
- gitui — https://github.com/gitui-org/gitui
- btop panel toggles and presets — https://www.thetechbasket.com/best-tui-apps/
- Claude Code interactive mode — the full shortcut reference, `Ctrl+B` backgrounding, `Ctrl+G` / `Ctrl+X Ctrl+E` editor handoff, `Ctrl+O` transcript viewer, `Ctrl+T` task list, `?` panel on empty input, `Ctrl+R` search — https://code.claude.com/docs/en/interactive-mode
- 2026 agent CLI landscape (opencode, crush, aider, gemini-cli, codex) — https://www.tembo.io/blog/coding-cli-tools-comparison · https://amux.io/blog/best-terminal-ai-coding-agents-2026/

**Usability writing**

- Nielsen, *10 Usability Heuristics for User Interface Design* — https://www.nngroup.com/articles/ten-usability-heuristics/
- Nielsen, *Response Times: The 3 Important Limits* — https://www.nngroup.com/articles/response-times-3-important-limits/
- vim-which-key / emacs which-key (leader popup after `timeoutlen`) — https://github.com/liuchengxu/vim-which-key · https://liuchengxu.github.io/vim-which-key/
- WikEmacs, *Discoverability* — https://wikemacs.org/wiki/Discoverability

**Graphs in text**

- Obsidian's graph view vs. the local graph and backlinks — https://pjordan.substack.com/p/a-pkm-revelation-obsidian-local-graphs · https://knodegraph.com/blog/obsidian-graph-view-alternative/
- `zk` note filtering: `--link-to`, `--linked-by`, `--recursive`, fzf interactive mode — https://zk-org.github.io/zk/notes/note-filtering.html
- ascii-dag (Sugiyama layered layout for fixed-width terminals) — https://github.com/AshutoshMahala/ascii-dag
- Graph::Easy — https://metacpan.org/pod/Graph::Easy
- PHART hierarchical ASCII renderer — https://github.com/scottvr/phart
- Terminal-space visualisation constraints — https://arxiv.org/pdf/1908.07544

**Time, schedules, run history**

- `systemctl list-timers` NEXT / LEFT / LAST / PASSED — https://wiki.archlinux.org/title/Systemd/Timers · https://linuxconfig.org/how-to-schedule-tasks-with-systemd-timers-in-linux
- Temporal Schedules UI — https://docs.temporal.io/web-ui · https://docs.temporal.io/schedule
- Airflow Grid view — https://www.astronomer.io/blog/everything-you-should-know-about-airflow-2-3s-new-grid-view/ · https://airflow.apache.org/docs/apache-airflow/stable/ui.html
- Unicode sparklines `▁▂▃▄▅▆▇█` — https://blog.jonudell.net/2021/08/05/the-tao-of-unicode-sparklines/ · https://github.com/tv42/sparkbar

**Forms and editor handoff**

- charmbracelet/huh — grouped forms, `Validate`, accessibility mode — https://github.com/charmbracelet/huh
- Textual modal dialogs — https://blog.pythonlibrary.org/2024/02/06/creating-a-modal-dialog-for-your-tuis-in-textual/

**Colour, notifications, terminal reality**

- Julia Evans, *Terminal colours are tricky* — https://jvns.ca/blog/2024/10/01/terminal-colours/
- NO_COLOR — https://no-color.org/
- termstandard/colors (truecolor support, `COLORTERM`) — https://github.com/termstandard/colors
- Terminal colour detection: `NO_COLOR`, `COLORTERM`, OSC probes — https://terminfo.dev/fundamentals/color-detection
- OSC 9 / OSC 777 desktop notifications and tmux passthrough — https://github.com/gdamore/tcell/issues/499 · https://github.com/ferologics/pi-notify

**Jod's own code, read at `356548a`**

- `cli/src/tui/mod.rs` — key handling, `Action`, panel keys, slash dispatch, `announce`
- `cli/src/tui/app.rs` — state, `step()`, history recall, prompt queueing, `short_duration`
- `cli/src/tui/ui.rs` — layout, `window_start()`, status-bar collision handling, wrapping
- `cli/src/tui/command.rs` — slash parsing, `HELP`, completions
- `docs/jod-system.md` — §The interface, §Running several agents at once

