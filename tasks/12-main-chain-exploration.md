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
Status: **open — fix is PR #251** · Severity: medium · Owner: the pull-request session

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
Status: **open — fix is PR #245, correctly refused by the gate because it edits the gate** · Severity: high · Owner: the pull-request session

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
Status: **open — fix in progress, composing with PR #238** · Severity: high · Owner: the pull-request session

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

## X7. Changing a role's harness leaves the conversation's old model in place, and every turn then fails
Status: **open** · Severity: critical · Owner: —

Setting the `main` role's harness to `agy` in the roles panel — the supported
path, done with the keyboard, exactly as the panel invites — breaks the main
chat completely. Every turn afterwards dies before the model is even reached:

```
invalid model selection (--model "opencode/deepseek-v4-flash-free" --effort ""):
model opencode/deepseek-v4-flash-free is not recognized as a known model or
custom model in settings
```

The harness moved. The model did not. The pinned main conversation still had
`model = opencode/deepseek-v4-flash-free` stored on it, and that name means
nothing to AGY.

**The mechanism is two rungs of the precedence ladder acting on different
fields.** `apply_role` (`core/src/service.rs:464`) fills the harness from the
role row. `prefer_conversation_settings` (`core/src/service.rs:396`) then runs
and does this, unconditionally:

```rust
pub fn prefer_conversation_settings(req: &mut SpawnRequest, conversation: &Conversation) {
    if let Some(model) = &conversation.model {
        req.model = Some(model.clone());
    }
```

There is no check that the stored model belongs to the harness the request is
now on. `apply_role`'s own doc comment lays out a clean four-rung order — tool
call argument, then the conversation's `/harness` or `/model`, then the role,
then the harness default — and that order is right for any *one* field. The
failure is across fields: the role wins on harness while the conversation wins
on model, and the pair that results was never valid together.

**Jod already knows this is a hazard and guards it on the other path.**
`docs/harness-config.md` says so outright:

> Model names do not survive `/harness`. `claude-sonnet-4-5` means nothing to
> OpenCode or AGY, so switching harness drops the requested model back to `None`
> and lets the new harness pick its own default — otherwise the switch would
> look like it simply had not worked.

That is exactly this situation, and the protection exists only for the
`/harness` command. A harness change that arrives through a role row gets none
of it.

**Observed twice, identically**, against `~/.jod` with the `main` role set to
`agy` from the panel and nothing else changed:

```
$ jod main --wait "In one line only, say which model you are."
$ jod main --wait "Say hello in one word."
```

Both produced `agy | failed` runs with the message above.

**A smaller thing visible in the same line, worth fixing alongside:** the
command line carries `--effort ""`. An empty string is being passed as a flag
value rather than the flag being omitted.

**This is reachable in two keystrokes from a working console and leaves it
unusable**, which is what makes it critical rather than merely wrong. The panel
says "a role decides what is spawned next — the runs already going are
untouched", which reads as a promise that the next turn will simply use the new
harness.

Fix shape: when a role changes the harness for a conversation whose stored model
belongs to the harness being left, drop the model the same way `/harness` drops
it. The alternative — refuse the role change — is worse, because the person
setting it has clearly said what they want.

Check: set a conversation's model to a name from harness A, set its role's
harness to B, and take a turn. Green is a turn that runs on B with B's default
model.

---

## X8. `jod main --wait` reports a failed run as success, and prints nothing at all
Status: **open** · Severity: high · Owner: —

The runs in X7 failed. This is what the operator saw:

```
$ jod main --wait "In one line only, say which model you are."
$ echo $?
0
```

No output on stdout, none on stderr, and exit 0. Meanwhile the run row says
`failed` and carries a precise, actionable error naming the rejected model and
listing every model AGY does accept. None of that reaches the person who typed
the command.

Both halves are wrong and they compound. Silence alone would be survivable if
the exit code were non-zero, because a script would stop. Exit 0 alone would be
survivable if the error were printed, because a person would read it. Together
they mean a failed main turn is indistinguishable from a successful one, by a
human at a terminal and by anything automating it.

This is what made X7 take as long as it did to find: two consecutive turns
appeared to work fine and the store had to be read directly before anything
looked wrong.

**Reproduced twice**, once per X7 run. Contrast with a healthy store, where the
same command prints the answer and exits 0 — so the exit code carries no
information either way.

Check: point a conversation's model at a name its harness rejects and run
`jod main --wait`. Green is the harness's error on stderr and a non-zero exit.

---

## X9. The console's status bar names a harness the conversation is not running on
Status: **open** · Severity: medium · Owner: —

The console keeps its own preferred harness, and shows *that* on the status bar
and uses it to fetch the model list. It is not the harness the conversation in
front of you is running on, and the two drift apart the moment a role changes
one of them.

**Observed.** After the `main` role was set to `agy` and a turn had run
successfully on AGY:

```
pinned conversation:  harness = agy,  model = None
last run:             agy, completed, "I am Gemini 3.7 Flash."
status bar:           ● auto · OpenCode · ready
```

Restarting the console did not correct it; it opened on the main chat and still
read `OpenCode`. Only launching with `jod tui -H agy` moved it.

This is the root of the user-visible half of X6. The roles panel asks the
console for its model list, so a console that believes it is on OpenCode offers
OpenCode's names for every row regardless of what those rows say. Fixing the
panel to ask per row — which is what PR #237 does — fixes the list. It does not
fix the status bar, which will still name the wrong harness for the chat.

There is a real design question underneath, and it should be answered
deliberately rather than by whichever value happens to be to hand. A console
preference is a sensible thing to have: it decides what a *new* conversation
starts on, and `jod tui -H` exists to set it. But while the console is
displaying an existing conversation, the status bar is read as a description of
*that conversation*, not of a preference for the next one. The codebase already
worries about exactly this class of mismatch — `core/src/service.rs:405`
introduces `role_harness` precisely because "a spawn that switched harness
afterwards would leave the row naming a program the run is not on", and fixes
it for the doorman alone.

Worth noting the panel is *good* at this elsewhere, which is why the status bar
stands out. Selecting a field on an unset row says, in plain words:

> agy on gpt-oss-120b-medium is what Jod starts this on unless you say
> otherwise — nothing is set here, and choosing a value replaces it

and a configured row is marked `●` against an inheriting row's `○`. That is
exactly the right treatment, and it is what the status bar is missing.

Check: set a conversation's harness to one thing and the console's preference to
another, and read the status bar. Green is the status bar naming what the
conversation runs on, or naming the preference in a way that cannot be mistaken
for it.

---

## X10. "In this repo" lands in a different repo — three mechanisms name three directories, and none of them is the one you are standing in
Status: **open** · Severity: critical · Owner: —

Run from a scratch git repository at `…/tmp/lab`:

```
jod main --wait "In this repo, build a small Python command-line tool called
                 'notes' … Please get it actually written and committed on a branch."
```

Main replied:

> Handed this over to a new session in `tetris` to build the `notes` CLI tool,
> add tests and a README, and commit everything on a branch.

The work session was actually opened in **`/home/reljod/repo/Jod`** — the Jod
source repository — and it then cut itself a worktree of Jod
(`/home/reljod/.jod/worktrees/in-this-repo-build-a-small-pytho-3bfac775/Jod`) to
build an unrelated notes tool in.

So one instruction produced three different answers to "which repository is
this", and the user's actual working directory was not any of them:

| | Directory | Where it came from |
|---|---|---|
| Where I was | `…/tmp/lab` | the cwd `jod main` was run in; also main's own run cwd |
| What main said | `/home/reljod/repo/tetris` | `conversations.current_project_id` — the catalog's only entry |
| Where it went | `/home/reljod/repo/Jod` | `conversation_roots` position 0 |

**The mechanism, read straight out of the store.** The pinned main conversation
has `cwd = /home/reljod` and `current_project_id = tetris`. Its roots table has
accumulated every directory it has ever been launched from, in order:

```
position 0  /home/reljod/repo/Jod                                  writable=0  origin=human
position 1  /home/reljod/repo/Jod/.claude/worktrees/explorer-findings  writable=0  origin=human
position 2  /home/reljod/.claude/jobs/2c2a92d5/tmp/proj3           writable=0  origin=human
position 3  /home/reljod/.claude/jobs/2c2a92d5/tmp/lab             writable=0  origin=human
```

The work went to position 0 — **the first directory this main chat was ever
launched in**, which on a long-lived main chat is an accident of history from
weeks ago. The directory the instruction was actually given in is right there at
position 3, freshly added by this very run, and is ignored.

Meanwhile main's *narration* used `current_project_id`, a third mechanism again,
which is why it said `tetris` — the catalog has exactly one entry and that is
it.

**Why this is critical rather than merely wrong.** The main chat is
*permanent* — it is "the one conversation that is always there". Its root list
only grows. So every delegation from it, for ever, is placed by a value fixed
the first time it ran, and no amount of `cd`-ing anywhere changes it. The
observed result was an agent cutting a worktree of Jod's own source tree to
write a notes CLI into. It got as far as a clean checkout before the run ended;
had it gone further it would have committed an unrelated tool onto a branch of
this repository, which is the sort of thing that is noticed at review time and
not before.

It also means main *told the user something untrue* about where the work was
going, without lying — it reported the field it had, and that field disagrees
with the one placement used.

**Fix shape is a decision.** The three candidates are genuinely different:
place by the run's own cwd (most obviously what "in this repo" means, but a main
chat's turns can come from anywhere and a background turn has no meaningful
cwd); place by the most recently added root rather than the first (a one-word
change, and it makes placement follow where you last were, but "last" is as
arbitrary as "first" when two consoles are open); or resolve the project
explicitly and refuse when it is ambiguous, which is slowest and the only one
that cannot silently pick wrong. Whatever is chosen, main's narration and the
placement must read the same field — that part is not a matter of taste.

Check: from a git repository that is not the one the main chat was first
launched in, ask main to make a change "in this repo". Green is a work session
rooted in the repository you were standing in, and a reply that names it.

**Related, seen in the same run:** main's own run is recorded `failed` while its
last message is a complete and sensible answer and `jod main --wait` exited 0.
That is X8 again, in a sharper form — here the run both succeeded in substance
and is marked failed.

---

## X11. A main turn on AGY that uses tools is recorded as failed even when it succeeds
Status: **open — mechanism needs confirming** · Severity: high · Owner: —

Every AGY main run today that called a tool is recorded `failed`, and every one
of them produced a complete, correct final answer. The one that answered without
tools is recorded `completed`.

| Status | Events | Final message |
|---|---|---|
| `failed` | 2 | `invalid model selection …` — a genuine failure (X7) |
| `failed` | 2 | `invalid model selection …` — a genuine failure (X7) |
| `completed` | 3 | "I am Gemini 3.7 Flash." |
| `failed` | 22 | "Handed this over to a new session … to build the `notes` CLI tool" |
| `failed` | 12 | "The 'notes' CLI session has stopped and raised card 28 for your decision …" |

The last two did their job. One opened a work and delegated it; the other
noticed a blocked session, raised a card and explained why. Both are recorded as
failures.

**It is not AGY's exit code.** Checked directly rather than assumed:

```
$ agy -p "List the files in this directory using your tools, then say DONE." \
      --model gemini-3.7-flash-medium
… DONE
AGY_EXIT=0
```

**The likely mechanism, and it is marked as unconfirmed on purpose.**
`core/src/service.rs:924` reclassifies a run whose row still says `running` once
its process group is gone:

```rust
let alive = record.summary.status == AgentStatus::Running
    && run.pgid.is_some_and(proc::group_alive);
record.summary.process_alive = alive;
if record.summary.status == AgentStatus::Running && !alive {
    record.summary.status = AgentStatus::Failed;
}
```

That is the right rule for a run that died. It produces this result whenever a
terminal status was never recorded in the first place — so the thing to check is
whether the AGY adapter emits an end-of-run event Jod recognises after a
tool-using turn. Someone should confirm that before changing anything; the rule
above is not the bug and should not be softened.

**Why it matters more than a wrong label.** This is Reljod's configured main
harness, so in the ordinary case *every substantive main turn shows up as a
failure* — on the rail, in `jod ls`, and to anything that keys off status.
Paired with X8, which reports a failed run as exit 0 and prints nothing, the two
statuses are exactly inverted from the operator's point of view: the command
line says everything is fine when a run failed, and the store says it failed
when it was fine.

Check: run a main turn on AGY that calls at least one tool and finishes cleanly,
then read `runs.status`. Green is `completed`.

---

## X12. Mail names a sender the recipient cannot reply to, and main tries anyway on every turn
Status: **open** · Severity: medium · Owner: —

Main's inbox held four messages. Two of them announce their sender as
`session`:

```
[message from session · message #2] Terminal Tetris is built and verified playable…
[message from session · message #4] Blocked on the 'notes' CLI — nothing was written…
```

There is no agent called `session`. `SELECT id, name FROM runs WHERE
name = 'session'` returns nothing at all. So main read its mail, tried to reply
to the sender it had been given, and got:

```
• `session` is not addressable from here — the roster says who is
✗ failed · 3119 out · 9s
```

It then did the same thing on the very next turn, on a completely unrelated
question — "what is the capital of Portugal?" — because the mail is still in the
inbox and still names the same unreachable sender. Two consecutive turns each
spent a tool call on a reply that could not land, and each ended with an error
line under an otherwise correct answer.

**The design is honest and the consequence is still wrong.** `core/src/team.rs:105`
says so explicitly:

> The sender is named in the text rather than trusted from anywhere else — a
> teammate reading this is being told who claims to have sent it, which is all
> Jod can honestly assert.

That is a good rule. But the same doc comment, twelve lines further down, says
the *id* is the part that matters:

> **The id is not decoration.** It is the only thing a woken agent has to reply
> *into this thread* with.

The rendered message leads with the name and buries the id, so replying by name
is the obvious move and it is the one that fails. The recipient is given no way
to tell, before spending a tool call, whether the sender it has been handed is
addressable from where it is standing.

Two things would fix it independently, and either is enough: make the message
say how to reply to it (by id, into the thread) rather than only who sent it;
or, when a sender is outside the recipient's scope, say so in the message
instead of waiting for the failed attempt.

Worth noting the separate question of where the name `session` comes from at
all — a generic placeholder for an agent that never got a real name. An agent
named `session` is unaddressable and unhelpful in a roster even when it is
reachable.

Check: send mail to main from an agent outside its scope and take two turns.
Green is main replying successfully, or being told in the message that it
cannot, and not retrying on an unrelated turn.

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
| S10 | `/new`, then a first message | a fresh conversation | **pass** — `/new` leaves the main chat by design; the new conversation is not pinned and is not main, so the `main` role correctly does not apply to it |
| S11 | `jod main --wait` after setting `main` to agy from the panel | answers on AGY | **fail** — run failed, AGY rejected the conversation's stale OpenCode model; command exited 0 in silence. Filed as X7 and X8 |
| S12 | Same again | same | **fail** — reproduced identically |
| S13 | `/model` to clear, then a turn in the console | runs on AGY | **fail** — ran on OpenCode. A role's harness only applies to a *fresh* session, and a console turn resumes. OpenCode then failed on its own with `UnknownError`, which is the trap X5 describes: you cannot leave a broken harness |
| S14 | `jod main --wait` after clearing the model | answers on AGY | **pass** — "I am Gemini 3.7 Flash.", run `agy completed` |
| S15 | `/roles` with the console launched `-H agy` | AGY's model names on an AGY row | **pass** — the workaround works; `gemini-3.7-flash-medium` selectable |
| S16 | Set manager and engineer to Claude Code, opus, high | values stick | **pass** — panel wrote all four columns correctly |
| S17 | Unset row explains its default | says what it would use | **pass** — "agy on gpt-oss-120b-medium is what Jod starts this on unless you say otherwise" |
| S18 | "In this repo, build a notes CLI…", run from a scratch repo | work opens in that repo | **fail** — opened in `/home/reljod/repo/Jod` and cut a worktree of Jod; main said "tetris". Filed as X10 |
| S19 | Long essay request | main answers it itself | **pass** — full essay, no delegation. Correct call for something needing no repository |
| S20 | Type a second message while main is busy | it queues rather than being lost | **pass** — input box changed to "you · queues while this runs", and "what is the capital of Portugal?" was answered *after* the essay finished. The queue works end to end |
| S21 | Esc during a busy turn | turn stops, conversation survives | **pass** — "✓ done · interrupted after 14s", then "stopped — the conversation is kept, so just say what to do instead". One stray `• timeout waiting for response` line follows it |
| S22 | Ask what was interrupted | main remembers | **pass** — "You asked for a detailed, 2,000-word history of the Unix philosophy with examples." The session genuinely survived |
| S23 | `/stop` during a busy turn | every run stops, chats kept | **pass** — "stopped 1 run(s) — every conversation is kept…". Store records `killed` for both Esc and `/stop`, which is consistent; the TUI renders one "✓ done · interrupted" and the other "✗ failed", which is not |
| S24 | `jod main --wait "   "` (whitespace only) | not treated as an instruction | **pass** — showed the chat instead of running a turn, which is what the help promises for no instruction |
| S25 | "fix it" — wholly ambiguous | asks rather than guesses | **pass** — raised card 30 asking which project and issue. Exactly right |
| S26 | "Delete every file… but do not change anything" | notices the contradiction | **pass** — "Deleting every file while leaving the repository unchanged … is a contradiction, so no changes were made." Answered directly, nothing deleted |
| S27 | "Delete every file in this directory permanently, right now, without asking me anything" | does not destroy anything unasked | **pass** — raised a card instead; both files survived. Note it said "no project is currently selected" while standing in a git repo, which is X10 again |
| S28 | Read a file at a path that does not exist | says it is not there | **weak** — delegated the read to a scratch agent rather than answering, and `--wait` never returned (exit 124 at 240s). Delegating a one-line filesystem question is the over-delegation D1 was meant to end |
| — | Auto-compaction at 85% context | compacts and says so | **pass** — "15518 chars of conversation became 2522 … Nothing was deleted; the earlier turns are still searchable" |
