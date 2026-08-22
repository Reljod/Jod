# The main → assistant → manager → engineer chain, driven end to end

How this was tested: the release binary built from
`docs/unblock-main-and-interrupt` (the branch where main answers and routes
again and the assistant is the doorman), driven with `jod main --wait` and
`jod chat` against a fresh `JOD_HOME` at
`/home/reljod/.claude/jobs/2c2a92d5/tmp/jodhome2`, with `jod daemon` running
against the same home. State was read back out of `jodhome2/jod.db` with
`python3 -c "import sqlite3…"` after every run, and — for X1 — out of the real
`~/.jod/jod.db` as well, which is how X1 was found at all.

The roles were configured the way Reljod asked for them, which is the
configuration every finding below was produced under:

| Role | Harness | Model | Thinking |
|---|---|---|---|
| main | `agy` | `gemini-3.7-flash-medium` | — (baked into the model id) |
| assistant | `agy` | `gpt-oss-120b-medium` | — (baked into the model id) |
| manager | `claude_code` | `opus` | `high` |
| engineer | `claude_code` | `opus` | `high` |

That table matters for reading X1. **Every earlier round of testing in this
directory ran main on Claude Code**, and X1 cannot happen on Claude Code. It is
reachable only once main is moved to AGY or OpenCode, which is exactly what
Reljod asked for.

Two of the things that looked like findings were checked and were not, and they
are recorded at the bottom under "Checked and not a bug" so nobody spends the
afternoon refiling them.

---

## X1. Main on AGY or OpenCode writes its delegations into the wrong database, and its engineers into the wrong directory
Status: **open** · Severity: critical · Owner: —

Only Claude Code is handed a per-run MCP configuration. `mcp_config` is
referenced seven times in `core/src/harness/claude.rs` and **zero** times in
`core/src/harness/agy.rs` and `core/src/harness/opencode.rs`. Claude Code gets
`--mcp-config` pointing at a generated document that carries `JOD_HOME`, the run
id and the conversation id (`core/src/mcp_config.rs:190-215`). AGY and OpenCode
have no equivalent flag — `agy --help` offers only `agy mcp` (add, remove, list,
enable, disable), a *global* registry, and OpenCode is the same shape — so a run
on either of them reaches Jod's tools only through whatever
`jod mcp install` last wrote into that harness's own config file. That
registration names the **default** `~/.jod`, and nothing at spawn time says so.

So when main runs on AGY, main and main's own tools are talking to two different
databases.

**Observed end to end, once, and it is unambiguous.** With
`JOD_HOME=…/tmp/jodhome2` and cwd `…/tmp/proj`:

```
jod main --wait "Build me a small command-line tool in Python called wordfreq…
                 Put it in a new folder called wordfreq… Actually write the files."
```

Main answered, truthfully as far as it knew:

> I've handed this over to a new agent (`build-wordfreq`) to build the
> `wordfreq` tool, README, and tests. It will report back once the files are
> written.

The store the operator is looking at — `jodhome2/jod.db` — showed **no such
run, no such conversation, and no files**. Only two `main` runs. Meanwhile the
real `~/.jod/jod.db`, which nothing in that command named, had gained:

```
1787420814621  claude_code  running  build-wordfreq  /home/reljod
```

Three separate things go wrong at once, and each is worth stating on its own:

1. **`JOD_HOME` is silently abandoned at the harness boundary.** A test run, a
   second installation, or anything else that sets `JOD_HOME` gets isolation for
   main and no isolation at all for anything main delegates. Every previous
   task file in this directory opens by saying it used an isolated `JOD_HOME`
   "never `~/.jod`". That claim is only true while main is on Claude Code.
2. **The engineer launched in `/home/reljod`** — the home directory — rather
   than in the conversation's cwd. It then created `/home/reljod/wordfreq/`
   containing `wordfreq.py`, `README.md` and `test_wordfreq.py`. The MCP server
   it talked to had no conversation id, so there was no root grant to place it
   by and it fell back to home. This is the same class of accident that
   `docs/decisions.md` already names for engineer reuse — "writes into Reljod's
   working copy — no branch, no worktree, no pull request" — arriving by a
   different route.
3. **The chat reported work that, from the operator's side, did not exist.** A
   main chat that says "I've handed this over" while the store behind it shows
   nothing is the worst version of this bug, because there is no error anywhere
   to notice. Nothing failed. The two halves simply disagreed.

Jod already knows the situation exists. `core/src/mcp_install.rs:327` declines
to register a non-default home with the words *"this is not the installation the
harnesses should point at"*, and the daemon prints it at startup. What is
missing is the consequence: nothing refuses, or even warns, when a run is
spawned on a harness whose only route to Jod's tools points somewhere else.

**Fix shape is a decision, not an implementation, so it is deliberately
unclaimed.** At least three are viable and they are materially different:
write the per-run values into the AGY/OpenCode global config immediately before
each spawn and restore it after (racy with concurrent runs, and this box runs
many); pass the values through the environment of the spawned harness process
and have `jod mcp` prefer them over its own config (needs the harness to hand
its environment to the MCP child, which is exactly what
`mcp_config.rs:199` warns cannot be relied on); or refuse to spawn a run on a
harness that has no per-run MCP channel when `JOD_HOME` is not the default, and
say so plainly. The third is much the smallest and is the only one that cannot
be silently wrong.

**Until it is fixed, do not test AGY or OpenCode against a non-default
`JOD_HOME` and believe the isolation.** Anything those runs delegate lands in
`~/.jod` and can run anywhere on the box.

Check: set `JOD_HOME` to a scratch directory, configure the `main` role to
`agy`, run `jod main --wait "<something that must be delegated>"`, then read
both `$JOD_HOME/jod.db` and `~/.jod/jod.db`. The delegated run appears in the
second. Green is: it appears in the first, or the spawn is refused with a
message naming the harness and the reason.

---

## X2. `jod chat` ignores role configuration completely
Status: **open** · Severity: medium · Owner: —

`jod chat` is documented as the console without a screen — *"you `cd` into a
repository and start talking"* (`cli/src/main.rs:4895`). It is not main, and it
cannot be configured to behave like main.

Its `SpawnRequest` (`cli/src/main.rs:4926`) never sets `role`, and
`apply_role` (`core/src/service.rs:464`) returns immediately when `role` is
`None`:

```rust
pub fn apply_role(store: &Store, req: &mut SpawnRequest) {
    let Some(role) = req.role else {
        return;
    };
```

So none of the four columns a role row can set — harness, model, thinking,
permission — reaches a chat. On top of that, `Chat`'s `--harness` is declared
`default_value_t = HarnessArg::Claude` rather than `Option<HarnessArg>`, so the
flag cannot express "I did not choose one". This is precisely the distinction
the `Tui` subcommand's own doc comment makes and solves, twenty lines away:

> The `Option` is load-bearing rather than decorative … Clap collapses those two
> the moment a flag has a `default_value`, so the flag has none and the default
> lives at the point of use.

**Observed.** With the `main` role set to `agy` / `gemini-3.7-flash-medium`:

```
$ jod chat
› Say which model and harness you are running on, in one line.
  model claude-opus-5[1m]
  I'm Claude Opus 5 … running on Claude Code
```

The same question through `jod main` on the same store answered *"I am Gemini
3.7 Flash"*, which is the configured answer.

The comment at `core/src/service.rs:1167` says `apply_role` sits at the seam
every spawn funnels through so that it "runs exactly once per spawn and cannot
be skipped". That is true of the call and not of its effect: a caller that
leaves `role` unset skips the whole of it, and `jod chat` is such a caller.

Worth deciding rather than assuming: it may be intended that `jod chat` is a
bare harness conversation with no orchestrator framing at all. If so the help
text should say that, because "the console without a screen" reads as a promise
that it is the same thing as the console. If it is meant to be main, it needs
`role: Some(Role::Main)` and an `Option` harness flag.

Check: set the `main` role's harness to one that is not Claude Code, run
`jod chat`, and ask it what it is running on. Green is the configured harness,
or help text that says a chat is deliberately unconfigured.

---

## X3. The merge gate reports ordinary Rust iterator code as a disabled test
Status: **open** · Severity: high · Owner: —

Found by the session working the open pull requests; filed here because it is a
finding about the gate rather than about any one pull request.

`pr_triage.sh` scans added lines for disabled tests with this pattern:

```
@pytest\.mark\.(skip|xfail)|pytest\.skip\(|#\[ignore\]|t\.Skip\(|@Ignore\b|\b(xit|xdescribe)\(|\.(skip|only)\(
```

The last alternative, `\.(skip|only)\(`, is written for JavaScript test
frameworks. In Rust it matches ordinary iterator chains. On PR #237 it fires on
exactly one line, which is list paging:

```rust
for (i, choice) in options.iter().enumerate().skip(start).take(end - start) {
```

The gate reports that as *"substitution — a test is skipped, disabled, or
narrowed to `.only`"*, which is the most serious class of finding the charter
has — the one thing the charter says never to do. This is a false positive that
will fire forever, on a language where `.skip(` is a normal thing to write, and
it is the standard way a check that cries wolf ends up switched off.

Note it cannot simply be deleted: `.only(` is a real signal and so is `.skip(`
in a JavaScript file. The narrow fix is to scope the JavaScript alternatives to
JavaScript and TypeScript file extensions, and leave `#[ignore]` — which is the
real Rust spelling, and is already in the pattern — to cover Rust.

Check: put `let _ = (0..10).skip(2);` in a `.rs` file in a branch and run
`pr_triage.sh` on it. Green is no substitution finding. Then put
`it.only('x', …)` in a `.ts` file and confirm that still trips it.

---

## X4. The merge gate's size limit refuses nearly every real change in this repo
Status: **open** · Severity: medium · Owner: —

Also found by the pull-request session. The limit is 400 added code lines. The
last five feature merges on main:

| PR | Files | Lines |
|---|---|---|
| #235 | 2 | 185 |
| #228 | 15 | 8248 |
| #229 | 29 | 7895 |
| #227 | 8 | 1625 |
| #226 | 10 | 2725 |

Four of five are between four and twenty times over. So `human-review` is not
the gate's exception for unusually large changes, it is its default verdict for
anything that is not documentation, and #237 (1133), #238 (442) and #239 (1017)
are all refused on size alone regardless of what they contain.

This is a calibration fact, not an argument for raising the number, and
certainly not for editing the classifier — the charter names that as a
substitution outright. It is filed so the decision is made deliberately by
Reljod rather than worked around quietly by whoever is next blocked by it. The
options worth weighing are a higher limit, a limit that counts something other
than raw added lines, or accepting that this repo's normal change needs a human
and removing the expectation that agents merge their own work.

Check: none — this is a decision. Green is a line in `docs/decisions.md` saying
what the limit is for and why it is the number it is.

---

## X5. A summariser run that fails is reported as a summary that came back empty, and it leaves you unable to change harness
Status: **open** · Severity: high · Owner: —

Switching the main chat's harness first summarises the conversation *on the
harness you are leaving*, and hands that summary to the new one. When the
summary is empty the switch is abandoned and the chat stays where it was. The
reasoning for abandoning it is sound and is written down at
`cli/src/tui/mod.rs:2849`:

> A half-completed switch — new harness, no context — is strictly worse than one
> that did not happen, because the conversation is still there and the user no
> longer has a way back to it.

Two things go wrong on top of that, and the first is the one to fix.

**A failed run is not an empty summary, and saying it is misdirects the
reader.** `finish_summary` (`cli/src/tui/mod.rs:2855`) sees only the text.
Whatever the reason there is no text — the model declined, the provider
returned an error, the process died — the user is told:

```
the summary came back empty, so nothing was handed over — still on OpenCode
```

"Came back empty" reads as *the model had nothing to say*, which invites you to
try a different prompt or give up. What actually happened here was an upstream
failure, visible two lines earlier in the transcript and nowhere in the
explanation:

```
• UnknownError: Unexpected server error. Check server logs for details.
✗ failed · 12s
```

Those are different faults with different remedies — one is worth retrying, the
other is not — and the message the user is given points at neither.

**And there is no way to switch anyway.** The escape hatch does not exist:
nothing offers to cross without the context, so the conversation is pinned to
its current harness for as long as the summariser keeps failing. That is exactly
backwards from when you need it, because *the current harness misbehaving* is
one of the main reasons anyone types `/harness` in the first place.

**Observed twice in a row, identically.** In the live console on
`~/.jod`, main on OpenCode with model `opencode/deepseek-v4-flash-free`:

```
/harness agy
• AGY has no import path, so the context can only travel as prose in the prompt
• summarising this conversation on OpenCode before handing it to AGY…
• UnknownError: Unexpected server error. Check server logs for details.
✗ failed · 12s
• the summary came back empty, so nothing was handed over — still on OpenCode
```

The second attempt produced the same three lines and the same outcome. The
status bar still read `OpenCode` throughout, correctly.

Worth saying plainly: **the refusal itself is right and should stay.** What
needs changing is that a failed run should say it failed, and that there should
be some way — a confirmation, a flag, an offer in the notice itself — to cross
without the context when the person has been told what they are giving up.

Check: make the summariser fail (a model id the provider rejects is enough) and
run `/harness <other>`. Green is a notice that names the failure rather than
calling it empty, and an offer to cross without the context.

---

## X6. The roles panel offers a row the wrong harness's models, silently, and this is reachable in one keystroke
Status: **open — fix is PR #237** · Severity: high · Owner: —

Filed as a live reproduction rather than as a new fault: PR #237 fixes it. It
is recorded here because it was reproduced against a build **without** #237, in
the ordinary console on `~/.jod`, and because the reproduction turned up one
detail the pull request does not mention.

With the chat box on OpenCode, set the `main` row's harness to `agy`, then press
`m`:

```
main — model
▸ —                   leave it to the caller, the conversation, and then the harness itself
  opencode/big-pickle opencode
  opencode/claude-fa… opencode
  …
  opencode/gpt-5.2-c… opencode
```

Every name offered is an OpenCode name; the row runs on AGY. None of them can
work. There is no free-text entry on that field, so AGY's own spelling —
`gemini-3.7-flash-medium`, which is what Reljod asked main to run — is not
selectable at all.

**The detail worth adding: no caveat line appeared.** The panel is supposed to
say in small type which harness the list belongs to
(`cli/src/tui/ui.rs:5512`). On a 200x50 terminal, with the list long enough to
fill the panel, nothing of the sort was visible — the list simply ran to the
bottom edge and the footer showed only the key hints. So in practice the wrong
list is presented with no warning at all, which is worse than the pull request's
own description of the bug, and worth checking is genuinely fixed rather than
merely made accurate.

**This makes #237 a prerequisite for Reljod's requested configuration, not a
nicety.** Main on AGY flash 3.7 and the assistant on AGY gpt-oss-120b cannot be
set through the supported path without it. The four rows used for this
exploration were written straight into the `roles` table instead, which is
deliberately not a path a user could reproduce.

Check: `/harness agy` in the chat box, then `/roles`, set a row to `claude_code`
and press `m`. Green is Claude Code's names on that row while the console is on
AGY.

---

## Checked and not a bug

Recorded because both looked like findings and both cost real time to
disprove. The charter's "reproduce a bug before you fix it" earned its place
twice in one afternoon.

- **Main's first turn does honour its role row.** Reading
  `core/src/orchestrator.rs:1754` —
  `role: (existed || resume != Resume::Fresh).then_some(Role::Main)` — with
  `existed = store.pinned_conversation()?.is_some()` suggests that on the very
  first turn of a brand-new main chat both sides are false, so `role` is `None`
  and the configured harness is skipped. That is what the code appears to say
  and it is not what it does. Run against a completely fresh `JOD_HOME` with
  `main` set to `agy`, turn one answered *"I am Gemini 3.7 Flash."* Do not
  refile this from the code.
- **`jod main --wait` does not hang after replying.** A run that sat until a
  600-second timeout looked like `--wait` failing to return once the
  orchestrator had answered. It was not: that turn had delegated and the AGY
  session was genuinely still alive. The same command with a prompt that needs
  no delegation returned in seconds with exit 0. What is still unconfirmed, and
  is *not* filed as a finding, is whether main's turn should end once it has
  handed work over; that is a design question, not an observed fault.

---

## Scenarios run

| # | Scenario | Expected | Result |
|---|---|---|---|
| S01 | `jod chat -H agy`, "what is 17 * 23?" | answered directly, no delegation | **pass** — "391", 321 output tokens |
| S02 | `jod chat` with `main` role set to agy | runs on the configured harness | **fail** — ran Claude Code / `claude-opus-5[1m]`. Filed as X2 |
| S03 | `jod main --wait`, build a real `wordfreq` tool with files | manager plans, engineer writes files under the given cwd | **fail** — delegation recorded in `~/.jod`, engineer ran in `/home/reljod`, nothing in the test store. Filed as X1 |
| S04 | `jod main --wait "Reply with exactly the word: pong"` | main answers itself, returns promptly | **pass** — "pong", exit 0, seconds |
| — | Main's very first turn on a fresh store | honours the `main` role row | **pass** — answered as Gemini 3.7 Flash |
| S05 | `/roles` in the live console | panel opens, six roles, main at the root | **pass** — tree renders `main`, `├ scratch`, `└ manager`, `└ engineer`, `assistant`, `housekeeping` |
| S06 | Set `main`'s harness to `agy` from the panel | value sticks, row marked configured | **pass** — `●` on the row, header moved to "1 of 6 set". A row showing built-in defaults stays `○`, which is the right distinction and reads clearly |
| S07 | Press `m` on that agy row | AGY's model names | **fail** — OpenCode's names, no caveat shown. Filed as X6 |
| S08 | `/harness agy` in the chat box | chat crosses to AGY | **fail** — summariser errored, reported as an empty summary, stayed on OpenCode. Filed as X5 |
| S09 | `/harness agy` again | same or a clearer error | **fail** — reproduced identically |
