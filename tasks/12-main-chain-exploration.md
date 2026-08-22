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

**Read this before you read anything else in the file.** The title says the
chain was driven end to end. That is true of `main`, of the queue, and of the
assistant's queue-reading, and it is **not** true of the rest. Because of X13,
**the manager tier never ran, in any scenario here.** Main called `ask_manager`
zero times in thirteen turns; it reached for `open_work` and `delegate` every
time, and the guard that should have stopped it failed open. So every scenario
below that looks like it tested main → manager → engineer actually tested
main → work session. What the engineers did was observed; what a *manager*
would have done with the same instruction was never seen once.

That matters for how the passes below should be read. A pass here means "main
did the right thing and the work session did the right thing". It is not
evidence about planning, about a manager deciding whether an instruction is new
work or something an agent of its own is already doing, or about the placement
rules a manager owns. None of that was exercised. The next person to work this
file should assume the manager and engineer tiers are **untested**, not
"tested and fine".

## What was not tested, and should be

Named explicitly, because a list of scenarios that were run says nothing about
the ones that were not, and the gaps here are larger than the coverage.

- **The manager tier, at all.** See above. Everything about `ask_manager`,
  `manager_preamble` and `plan_work` is unexercised. This is the single biggest
  gap and it is a consequence of X13 rather than a choice.
- **Engineer reuse and the roster.** Whether a second instruction on the same
  subject reuses a warm engineer, and whether a different subject correctly
  opens a new one, is the whole of `worktree-engineer-reuse-rules` (#236, merged)
  and none of it was driven.
- **~~The doorman's judgement~~ — since tested, and it is broken. See X14,
  X15 and X16.** This entry originally said the assistant had never been asked to decide
  and that `interrupt_main` had never been seen to fire. Both were true when it
  was written and neither is true now: a later run put an urgent message into a
  busy chat, a doorman read it, judged it correctly, called `interrupt_main`
  five times and was refused every time. What remains untested is a doorman
  whose interrupt *succeeds*, because none has yet.
- **Shift-Esc.** `/stop` was tested and works. Its twin was not, and D8 exists
  because the terminal may not deliver the modified key at all.
- **Multi-turn work with cards answered.** Cards were raised and left sitting.
  Nothing here answers one and watches the work resume, so the card→answer→
  resume loop is unverified from this side.
- **Schedules, goals, memory and webhooks.** Untouched. They have their own area
  files and this run added nothing to them.
- **Concurrency.** One console, one chat. Two consoles on one store, or two
  works in the same repository at once, were never tried — and the pin drift in
  X13 suggests that is exactly where more lives.
- **Recovery.** No harness was killed mid-run, no database was locked, no disk
  filled *deliberately*. The one disk exhaustion that happened was an accident
  and is recorded as such.

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
Status: **fixed — PR #251, merged** · Severity: medium · Owner: the pull-request session

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

**Three live instances from a single night, which is a stronger case than the
historical sample above.** All three are correct, CI-green, independently
reviewed, and blocked by nothing except the line count:

| PR | Code lines | Other findings from the gate |
|---|---|---|
| #237 | 1133 | none |
| #239 | 1017 | none |
| #252 | 686 | none |

#252 is a fix for X5 in this very file. So the limit is now blocking the repair
of a bug the same night it was reported, and the gate has nothing else to say
against any of the three.

Check: none — this is a decision. Green is a line in `docs/decisions.md` saying
what the limit is for and why it is the number it is.

---

## X5. A summariser run that fails is reported as a summary that came back empty, and it leaves you unable to change harness
Status: **open — fix is PR #252, open, composed with PR #238** · Severity: high · Owner: the pull-request session

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
Status: **fixed — PR #254 (`dcef647`), merged** · Severity: critical · Owner: the pull-request session

**How it was fixed.** `prefer_conversation_settings` now takes the
conversation's model only when the run is going to the conversation's own
harness. When the two disagree the model is dropped, the harness picks its own
default, and a line on stderr says so. An unrecognised harness id leaves the
model alone, on the grounds that not knowing what a row says is not the same as
knowing it disagrees. Dropping rather than refusing matches what `/harness`
already does, and `docs/harness-config.md` now states the rule as covering any
harness change rather than that one command.

**Worth recording: PR #237 does not fix this, though it looked as though it
might.** That was checked before the fix was written rather than assumed. #237
changes `apply_role` alone — its `if req.model.is_none()` branch, so that a role
row naming a harness the request is not going to no longer contributes its own
model. X7 lives in `prefer_conversation_settings`, which #237 does not touch,
and the foreign model in the failure below came from the *conversation* row and
was written **after** `apply_role` had already run. The two compose rather than
conflict: #237's hunk is near `core/src/service.rs:475`, this one near `:394`.
The guess that "#237 will probably cover it" was reasonable and it was wrong.

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

**Correction — `--effort ""` was not a second bug, and this finding was wrong
about it.** The original text of X7 claimed that the `--effort ""` visible in
the error above showed an empty string being passed as a flag value. It does
not. Checked against the installed binary rather than reasoned from the code:
`agy --model "opencode/deepseek-v4-flash-free" -p "hi"`, with no effort flag
given at all, prints the identical message with `--effort ""` in it. AGY echoes
its own effort setting as part of its error text, and it is empty because
nothing set it. All three of Jod's adapters gate the flag behind
`req.effort.and_then(|e| e.flag_value(kind))`, so `None` emits nothing.

The claim is corrected here rather than deleted because the mistake is the
instructive part: it was read out of a harness's error text and filed without
being reproduced, and a phantom bug in a finding is a fix waiting to be written
against working code. This repo's own task index opens by describing a near-miss
of exactly that shape. A test asserting `--effort` is absent when no effort is
set has since been added, so a real regression would now be caught in the suite
instead of in a harness error message.

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

## X13. Main never calls `ask_manager` — compaction moves the pin mid-turn, so the guard that should force it fails open
Status: **open — root cause established: compaction forks the main chat and moves the pin** · Severity: critical · Owner: —

Across every scenario run tonight — thirteen main turns, several of them plainly
repository work — **main called `ask_manager` exactly zero times.** It used
`open_work` and `delegate` instead. The manager tier, which #228 and #229 exist
to create, has not run once.

That is supposed to be impossible. `refuse_routing_from_main`
(`core/src/mcp.rs:2931`) exists to refuse exactly this, and there is a test
named `open_work_from_the_main_chat_is_refused_and_names_ask_manager`
(`core/src/mcp.rs:7384`). The refusal text is unambiguous:

> `open_work` is not the main chat's to call. Hand the instruction to the
> project's manager with `ask_manager` instead: it owns the repository, it
> decides whether this is new work or something an agent of its own is already
> doing, and it raises a card that reaches your rail.

**The guard fails open.** Both of its early returns allow the call:

```rust
fn refuse_routing_from_main(&self, tool: &str) -> Result<(), ToolError> {
    let Ok(raiser) = self.raiser() else { return Ok(()); };
    if !self.caller_is_main(&raiser) { return Ok(()); }
    Err(ToolError::Refused(...))
}
```

So anything that stops the caller being recognised as main does not produce an
error — it produces a silent bypass of the whole tier.

**What is established.** `caller_is_main` (`core/src/mcp.rs:2769`) is a single
comparison against `store.pinned_conversation()`. The store now holds **seven**
conversations titled `main`, and the pin moves between them:

```
8ce8211e pin=0 open_code    3035de39 pin=0 open_code   85d68207 pin=0 agy
d2588dcd pin=0 claude_code  ef0405d8 pin=0 agy         3b1035a4 pin=0 agy
c71f36a9 pin=1 agy
```

On the first repository-work run, main's turn executed in `3b1035a4` while a
newly created `c71f36a9` held the pin — so the caller was, by this comparison,
not main, and `open_work` went through. `delegations` records it plainly:
`open_work` at id 58 from conversation `3b1035a4`.

**The cause is compaction, and it is now established rather than guessed.**
Compacting a conversation does not edit it in place. It **forks it into a new
conversation** and moves the pin, titling the new row `main`
(`core/src/conversation.rs:1594-1620`). The comment there explains why the pin
has to move, and is right about it:

> The pin follows the thread across the switch. `main_conversation` is
> get-or-create on `pinned = 1`, so a pin left behind on the conversation this
> switch just compacted away would send the next turn back to the thread that
> was handed over.

So the pin moving is correct. The bug is *when* it moves relative to a turn
already in flight. **A turn that began before a compaction belongs to the old
conversation for its whole life, while the pin has already moved to the new
one.** From that moment `caller_is_main` compares the turn's conversation
against a different pinned id, returns false, and every main-only guard fails
open for the rest of that turn.

**The store shows a perfect one-to-one chain.** Every compaction run is followed
eight to twelve seconds later by a new conversation, forked from the previous
one and titled `main`:

| `summarise to compact` run | new `main` conversation | gap |
|---|---|---|
| 1787423541681 | `3b1035a4` @ 1787423550502 | +9s |
| 1787424527516 | `c71f36a9` @ 1787424535851 | +8s |
| 1787424989059 | `0a1a280c` @ 1787424996981 | +8s |
| 1787425992673 | `2945d706` @ 1787426004731 | +12s |

And `forked_from` links them in one unbroken line, oldest to newest:

```
8ce8211e → 3035de39 → 85d68207 → d2588dcd → ef0405d8 → 3b1035a4 → c71f36a9 → 0a1a280c → 2945d706
```

That is the seven-way pile-up explained: it is not drift or corruption, it is
one chat compacted eight times. It also resolves what looked like a
contradiction above — the run recorded against `c71f36a9` whose work session
hangs under `0a1a280c` is a turn that straddled the third compaction.

**Why it fires so often here, which is a second-order consequence of the
requested configuration.** Main is on `gemini-3.7-flash-medium`. Four
compactions happened in about forty minutes of ordinary use. On a model with a
larger context the window between compactions is wide and a turn rarely
straddles one; on this one it is narrow and turns straddle compactions
routinely. So the same bug that would be intermittent and baffling elsewhere is
close to permanent in Reljod's setup — which is why the manager tier was absent
from *every* scenario rather than from some of them.

**Two fix shapes, and they are not equivalent.** Either resolve "is this main"
against the thread rather than against the current pin — following `forked_from`
so a conversation that *was* main and was compacted still counts for the turn
that was already running — or freeze the answer at the start of a turn and carry
it. The first is more correct and more work; the second is smaller and leaves
the question wrong for anything that outlives a turn. Whichever is chosen,
`caller_is_main` being a single equality against a mutable pin is the thing to
stop doing.

**The blast radius, grepped rather than inferred, is exactly two call sites.**
It was suggested that this widens to every gate that asks "is this main",
including `interrupt_main`'s, and that is **not** so — checked on both branches
before being written here. `caller_is_main` has two callers:

- `core/src/mcp.rs:1730` — the refusal that stops `delegate` being used for work
  inside a registered project;
- `core/src/mcp.rs:2954` — `refuse_routing_from_main`, which gates `open_work`.

Both are routing guards for repository work, which is what this finding already
describes. `interrupt_main` is gated on `caller_is_assistant`, which reads
`conversations.origin` rather than the pin:

```rust
matches!(
    store.conversation_origin(&raiser.conversation_id),
    Ok(Some(origin)) if origin == crate::orchestrator::ASSISTANT_ORIGIN
)
```

Compaction forks *main*, not the doorman, so a doorman's origin is untouched and
that gate does not fail open. Its own doc comment draws the distinction, calling
origin "sender identity the caller cannot argue with" as against
`caller_is_main`. Nobody should go hunting for a hole there or "fix" a gate that
is already right.

Worth noting for whoever takes it: all four `summarise to compact` runs are
recorded `failed`, and X11 applies to them, so do not read those statuses as
evidence that compaction is broken. At least one of them demonstrably worked —
the console reported "15518 chars of conversation became 2522".

**Why this is the most important finding in this file.** It is not a wrong
label or a bad message — an entire designed layer is absent at runtime, on the
configuration Reljod asked for, and nothing anywhere reports it. Every "the
manager will decide where this goes" property is simply not in effect. It also
reframes X10: work landed in the wrong repository partly because no manager —
which owns exactly one project and would have placed it there — was ever
consulted.

It also means the test suite is not covering the real path. The test asserts the
refusal fires; live, with main on AGY, it does not. Whatever the mechanism turns
out to be, a regression test has to exercise it through a real spawn rather than
a constructed `Raiser`, or it will keep passing while the behaviour is absent.

Check: with a registered project and main on AGY, ask main for repository work
in that project. Green is a refusal naming `ask_manager`, a manager conversation
in the store, and a `delegations` row of kind `ask_manager`. Today you get an
`open_work` row and no manager at all.

---

## X14. The doorman decides correctly, main is not interrupted, and the message is stranded in `reviewing` for ever
Status: **open — stop path fixed by #262; the stranding is X15 and X16** · Severity: critical · Owner: —

**A naming note, because the tool this finding names does not exist on main.**
`interrupt_main` is on `feat/interruptible-main` only — `git grep` for it
against `origin/main` returns nothing. Everything below was observed on that
branch, where the doorman really did call a tool by that name. Read outside that
branch, the accurate phrasing is "the doorman's stop path was inert", which is
what was actually seen.

**And the mechanism given below was wrong; the real one is a window.** The text
said `kill_agent` failed because the process serving the tool call had no
in-memory registry of main. The registry part is right and the reason is not:
`Tool::stop_agent` (`core/src/mcp.rs:2165`) *does* call `rehydrate(REHYDRATE)`
first. `REHYDRATE` is `200`, and `Store::runs(limit)` is
`ORDER BY created_at_ms DESC LIMIT ?1` — so rehydration loads the two hundred
**most recent** runs. The main chat is the longest-lived thing on the box and
every new run pushes it further down, until it drops out of that window
entirely. That is why a correct id for a live run came back
`no agent with id 8a93350d…` five times.

Fixed and merged as **#262**. It also means the one-line "rehydrate inside the
MCP server" fix that was considered and rejected would have changed nothing,
because the server already rehydrates.

This mechanism has a second victim that had nothing to do with doormen: see
**X17**.

This is the behaviour Reljod asked for in his own words — *"an assistant read
queue messages and determining when it will interrupt the main"* — and end to
end it does not work. The judgement is right. Everything after it fails.

**What happened, in order.** Main was busy writing a long essay. Typed into the
busy chat:

```
STOP - urgent, forget the essay, I need to know right now: is the lab project
on a branch or on main?
```

1. The message was written to the store correctly: `pending_deliveries` id 12,
   `kind = human`, `state = reviewing`. The console said
   `queued — an assistant is reading it (1 waiting)`.
2. A doorman was started: run `600dee8e`, harness `agy`, 16 events.
3. **The doorman judged it correctly.** Its final message:

   > stopping it — you urgently asked to abort the essay and need an immediate
   > answer about the lab project's branch.

4. **Main was not interrupted.** It carried on for another minute and a half.
5. The doorman run is recorded `failed`, and the console printed
   `✗ doorman STOP - urgent, forget the failed after 24s — Ctrl-F to open it`.
6. **The delivery never left `reviewing`.** `run_id` is still `NULL`. It was
   neither delivered nor returned to the queue.

So the urgent message is gone. Not refused, not deferred, not answered — it sits
in a state that nothing revisits, while the console simultaneously claims *"an
assistant is reading it"* and *"doorman … failed"*. Those two lines were on
screen at the same time and they cannot both be acted on.

**Nothing recovers it.** Checked deliberately rather than assumed. Over the
following minutes: exactly one doorman run was ever created and it was never
retried; main's turn ended and the delivery did not drain; Esc was pressed and
the turn was interrupted, and it still did not drain. Ten minutes after it was
typed the row is unchanged — `state = reviewing`, `run_id = NULL`,
`reviewed_at_ms = NULL` — and the status bar still reads `1 queued`. The only
thing that clears the counter is presumably restarting the console, which loses
the message rather than delivering it.

**The stranding is the part to fix first, and it is structural rather than
model-dependent.** E2.S3 makes "under review" a state so that only one doorman
runs at a time, which is right. But nothing releases the state when the doorman
does not finish cleanly. A run that dies between claiming the queue and acting
on it takes the message with it, permanently, and the only visible symptom is a
console that keeps saying somebody is reading it. Whatever else changes, a
delivery whose doorman ended without delivering or explicitly deferring must go
back to `queued`.

**The interrupt did fire, five times, and was refused every time.** This was
left open when the finding was first written and has since been settled by
reading the doorman's own event log. It called `interrupt_main` with the correct
run id and got back, each time:

```
could not stop `8a93350d-15b1-455e-b2fe-5817564d459d`:
  no agent with id `8a93350d-15b1-455e-b2fe-5817564d459d`
```

That run id is right. `8a93350d` is main, it was `running` at the time, and the
row is in the store. The refusal comes from `Jod::kill_agent`
(`core/src/service.rs:1466`), which looks the run up in **the calling process's
in-memory registry** and errors if it is not there:

```rust
let known = self.state.read().await.agents.contains_key(id);
if !known { return Err(JodError::UnknownAgent(id.to_string())); }
```

Main was started by the console; the process serving the doorman's tool call was
not that process. The comment above that check says it "fails loudly if it is
not there. That is the existing contract for the run a caller named" — a
contract written for an in-process caller, which the doorman is not.

Worth noting the same lookup succeeds from a *long-lived* MCP server: calling
`stop_agent` on a console-started run from a different process worked in this
same session. So the answer is not simply "cross-process never works", and which
server the doorman reaches matters — which is X1's territory, since an AGY
session gets the globally registered server rather than a per-run one.

**And then it reported success anyway.** After five refusals the doorman's final
message was *"stopping it — you urgently asked to abort the essay…"*. Nothing it
said was true of the system, and that sentence is what a reader would act on.
That is the same disease as X10's narration: a layer reporting the action it
chose rather than the action that happened.

The `failed` status on the run is therefore a real failure and not an X11
mislabel — though X11 still applies to AGY runs generally and should not be
ruled out for other doormen.

The stranding half has its own finding and its own mechanism: see **X15**, which
is separately fixable and should be fixed first.

**A prerequisite nobody is told about.** None of this happens without
`jod daemon` running, because `Ticker::tick_deliveries` is what starts a
doorman. There was no daemon on this machine for the whole earlier half of these
runs, and in that state the assistant tier does not exist at all — messages
typed into a busy chat were carried by the console's own in-memory queue
(`App::queued`) and delivered when the turn ended. That works, which is exactly
why it hides the problem: the queue *looks* fine, and the layer Reljod asked for
is simply absent. The console does mention it — "nothing is watching these
sessions for stalls — start `jod daemon`" — but it frames it as a stall-watching
concern, and says nothing about the assistant.

Check: with `jod daemon` running, type an urgent message into a busy main chat.
Green is main's turn stopping, the message delivered, and the delivery row
leaving `reviewing`. Then kill the doorman mid-run and confirm the delivery
returns to `queued` rather than sticking.

---

## X15. A doorman that starts and then dies strands its message for ever
Status: **open** · Severity: critical · Owner: —

The mechanism behind half of X14, established from the code and confirmed
against a live store. It is small, self-contained, and can be fixed without
answering any of X14's harder questions.

**This is about code that has not merged yet, and that matters for who fixes
it.** Neither `finish_review` nor `under_review_for` exists on `main` — checked
with `git grep` against `origin/main`, both return nothing. They live only on
the branch this whole exploration was run against, which is PR #247's. So
nobody should go looking for this on main, and the fix belongs to that pull
request rather than to a separate one landing on top of it. Being found before
the branch merges is the good case: the sweep can be wired up as part of the
feature that needs it, instead of shipping and being discovered by somebody
whose urgent message vanished.

**`finish_review` is the one way out of `reviewing`, and every one of its
callers covers a doorman that never started.** All three are in
`Ticker::tick_deliveries` (`core/src/ticker.rs:2276`, `2283`, `2313`):

1. the turn ended between the busy check and the claim, so there is nothing to
   interrupt;
2. the in-flight turn could not be read;
3. `start_doorman` **failed to spawn**.

The case with no caller is the one that happened: `start_doorman` returned
`Ok(started)`, the run really began — 16 events, a tool call, a considered
answer — and then ended badly. From that moment nothing anywhere calls
`finish_review` for those rows.

**The design anticipated exactly this and the recovery was never wired up.**
`finish_review`'s own doc comment (`core/src/delivery.rs:696`) names it:

> Every path out of a review comes back through here — a doorman that held, a
> doorman that interrupted, and **a doorman that died without saying anything.
> That last one is why this is not folded into the two verdicts: a row left
> `reviewing` by a crashed run would be invisible to `Store::pending_for` for
> ever, and Reljod's message would be lost in a state nothing sweeps.**

And the reader written for that sweep, `Store::under_review_for`
(`core/src/delivery.rs:734`), says in its own comment that it exists because
"the sweep that puts a crashed doorman's rows back needs to find them". **It has
no production caller.** Grepping all of `core/` for it finds two call sites,
both in its own tests. This is the same shape as the `claim_lease` case
`docs/decisions.md` already records: a function that existed, was tested, and
had no caller outside the test.

**The stranded row never recovers.** Polled every twenty seconds for twenty
minutes from a script rather than by eye. Delivery 12 read `reviewing`,
`run_id = NULL`, on every single sample and was still doing so at twenty-two
minutes. Main's turn ended during that window and Esc was pressed during it, and
neither released it. Nothing sweeps it, exactly as `finish_review`'s comment
warns.

**What it costs after that is a lost judgement, not lost messages — an earlier
draft of this finding said the latter and was wrong.** Two more urgent messages
typed while the row was stuck became deliveries 15 and 16. Both were
**delivered** about fourteen minutes later, when main's turn ended, by the
ordinary queue drain. They were not lost. The correction matters because "the
message vanishes" and "the message arrives late and unjudged" call for different
urgency, and only the second is true of the messages after the first.

**And nothing was wrong with those two either — a second correction.** An
earlier draft said they "went unjudged" and treated that as damage. Checked
properly: delivery 15 waited 38 seconds and delivery 16 waited 23 seconds, and
then main's turn ended and both were delivered. No tick landed in that window,
which is ordinary rather than a fault. `reviewed_at_ms` is `NULL` on both, which
also rules out the other candidate — that a failed spawn had stamped them as
seen. They were simply short waits that resolved on their own.

Two candidate explanations for their not being judged were considered and both
are now dead: the "claim blocks later doormen" theory was disproved in code by
the session that owns this feature — a claim is per *row*, not per conversation,
so rows behind a stranded one still plan `Judge` — and the "stamped after a
failed spawn" theory is ruled out by the `NULL`. What is left is that the turn
ended first, and that is not a bug.

So the damage from this finding is **one lost message**, delivery 12, and
nothing more. That is still worth fixing, and the reason it could never be
recovered turns out to be a second fault: see **X16**, where the compaction that
followed orphaned it onto a conversation nobody reads. But the tier is not
degraded, later messages are not lost, and this finding claimed both at
different times before being checked.

**Three drafts, three corrections, and the pattern is worth naming**, because it
is the same each time: generalising from one observation taken at one moment. A
twenty-second poll for twenty minutes settled the part that was real; a single
`SELECT reviewed_at_ms` settled the part that was not. Both were cheap and
neither was done until after the claim had been written down.

The stranding of row 12 is the whole of the bug, and that part is certain.

**Fix shape, and it is genuinely small.** Call `under_review_for` on the tick
and requeue anything whose doorman run is no longer alive. The comments above
describe this sweep as though it exists; making it exist is the whole change.
Worth stamping the requeued rows the way the failed-spawn path already does, and
for the same stated reason — a doorman that failed once will probably fail again,
and a queue that retries it every minute for the length of a long turn is worse
than one that waits.

**A regression test must exercise a doorman that starts and then dies**, not one
that fails to spawn. The existing tests cover the second, which is why this
survived: `start_doorman` returning `Ok` is the boundary the coverage stops at.

Check: start a long main turn, queue a message, kill the doorman run mid-flight,
and wait a tick. Green is the row back in `queued` and a later doorman reading
it. Today the row stays `reviewing` and every subsequent message queues behind
it for ever.

---

## X16. Compaction carries queued deliveries across but not reviewing ones, so a message under judgement is orphaned
Status: **fixed at the root — `7937698` on `feat/interruptible-main`, plus migration `0033`** · Severity: high · Owner: the #247 session

**The fix is the narrow one this finding proposed.** The fork statement now
carries `state IN ('queued', 'reviewing')`. The reasoning went into the comment:
`delivered` must not move because it records where a message actually went, and
that argument does not apply to a message still being judged — which is if
anything *more* owed to the new thread than a queued one, because somebody typed
it while the chat was working and is watching for the answer.

**And delivery 12 came back.** Migration `0033` moves orphaned `reviewing` rows
on the main chain onto the current main, restricted the same way `0027` is, with
a test asserting a `reviewing` row on some other conversation is left alone. In
the live store the row is no longer stranded, and it did not stop at `queued`:

```
12  2945d706  delivered  run=7855dc59-…  delivered_at_ms=1787428173972
```

It reached main about **thirty-nine minutes** after it was typed. Recovered, and
late enough that "STOP, forget the essay" arrived long after the essay had been
forgotten anyway — which is the honest measure of what this bug costs even when
it is repaired.

Worth keeping for whoever reads this later: `run_id` was `NULL` on that row, so
the sweep in X15 would have released it onto the dead thread and no further.
Both halves were needed. That is the strongest available argument that X15 and
X16 were genuinely two bugs rather than one described twice.

The other half of why delivery 12 in X15 could never be recovered, and it
survives the fix for X15 unless that fix is written knowing about it.

Compaction forks the main chat (see X13) and then deliberately carries several
things across to the new conversation: child conversations, open cards, and
queued deliveries. The delivery statement is scoped:

```sql
UPDATE pending_deliveries SET conversation_id = ?2
  WHERE conversation_id = ?1 AND state = 'queued'
```

`state = 'queued'` only. Its comment explains the exclusion it *was* thinking
about — "a delivered one is a record of where it actually went, and moving it
would make the ledger lie" — which is right. But `reviewing` is neither
`queued` nor `delivered`. It is a live message mid-judgement, and it is left
behind.

**Observed, and it explains the permanence in X15.** Delivery 12 was queued
against conversation `0a1a280c` and put into `reviewing` when its doorman
started. The next compaction, fifteen seconds later, forked main into
`2945d706` and moved the pin. Delivery 12 stayed on `0a1a280c` — a conversation
that is no longer main, is not busy, and that nothing revisits. Two later
messages, 15 and 16, were queued against `2945d706` after it existed and were
delivered normally within about thirty seconds.

So the two faults compound and each alone would have been survivable:

1. The doorman died and nothing returned row 12 to `queued` (X15).
2. Because it was sitting in `reviewing`, the next compaction skipped it and
   orphaned it onto an abandoned conversation.

**Correction — an earlier draft said the X15 sweep would miss an orphan, and
that was wrong.** It claimed the sweep asks per conversation via
`under_review_for` and so would not find a row orphaned onto a previous main.
Checked against the branch rather than assumed: `Store::release_stale_reviews`
(`core/src/delivery.rs:789` on `feat/interruptible-main`) takes **no**
conversation id at all. It is one global statement:

```sql
UPDATE pending_deliveries
   SET state = 'queued', reviewed_at_ms = coalesce(reviewed_at_ms, ?1)
 WHERE state = 'reviewing'
   AND (run_id IS NULL
        OR NOT EXISTS (SELECT 1 FROM runs r
                        WHERE r.id = pending_deliveries.run_id
                          AND r.status = 'running'))
```

`under_review_for` is a reader used by `interrupt_main` and by tests, not by the
sweep. So the sweep finds an orphan wherever it sits.

**The point underneath it was right, and it is one step further along.**
Releasing an orphan is not the same as delivering it. The row goes back to
`queued` *on the dead conversation*, and `tick_deliveries` then dutifully
injects it into a thread nobody is reading. Reljod still never sees it. So a
sweep alone does not close this, which is why it had to be fixed at the fork.

**And the window is not rare here.** A doorman takes tens of seconds to judge;
compaction fires every ten minutes or so on `gemini-3.7-flash-medium` (X13). A
message being judged when a compaction lands is an ordinary Tuesday in this
configuration, not a corner case.

The narrow fix is to widen the statement to carry `reviewing` rows as well as
`queued` ones — a message still being judged is exactly as owed to the new
thread as one still waiting, and the comment's reasoning about not moving
`delivered` rows does not apply to it.

Check: queue a message into a busy main chat, let a doorman claim it, force a
compaction before the doorman finishes, and read the row's `conversation_id`.
Green is the new conversation's id, and the row eventually leaving `reviewing`.

---

## X17. A `replace` schedule fails to stop the run it is replacing, records that it did, and starts a second one
Status: **open — belongs in `tasks/40-scheduling.md`, filed here to avoid two owners on one file** · Severity: high · Owner: —

Not found by testing scheduling. It fell out of X14's mechanism, and it is
listed here rather than in the scheduling area file only because that file has
another owner and one owner per path is the rule. Whoever sweeps should move it.

A schedule with the `replace` misfire policy is meant to stop the previous run
before starting the next, so the two never overlap. `Ticker` does this
(`core/src/ticker.rs:983`):

```rust
Decision::Replace { due_at_ms, stop } => {
    // Stop first, so the two never overlap even briefly — the whole
    // point of choosing `replace` over `allow`.
    let _ = self.jod.kill_agent(stop).await;
```

`let _ =` discards the result. It then records the fire as
`FireOutcome::Replaced` with `detail: "stopped {stop}"`, and spawns the new run
regardless.

So when `kill_agent` fails — which, before #262, it did for any run outside the
two-hundred-most-recent window that `rehydrate` loads, and a scheduled run is
exactly the kind of long-lived thing that falls out of it — three things happen
at once:

1. the previous run **keeps going**;
2. the ledger says `Replaced`, `stopped <id>`, which is false;
3. a second run starts anyway.

The result is two runs where the policy exists to guarantee one, and a record
that positively asserts the opposite. Nothing errors, and the symptom — a
schedule quietly running twice over — is not one anybody would attribute to a
stop that failed. It would not be found by testing scheduling, because
scheduling behaves exactly as designed; the failure is in a call whose result is
thrown away.

**#262 removes the cause for now**, but not the swallowed error. The other
callers of `kill_agent` were failing the same way and are worth a look for the
same reason: `DELETE /agents/:id`, `jod kill`, and the TUI's stop key. Of those,
only this one both discards the error *and* writes a record claiming success.

**The fix is not to add a `?`.** A schedule that refuses to fire because it could
not stop its predecessor may be worse than one that overlaps — that is a policy
question. What is not a policy question is the ledger: if the stop failed, the
fire must not be recorded as `Replaced` with `stopped <id>` on it. Say what
happened, then decide whether to fire.

Check: arrange a `replace` schedule whose previous run cannot be stopped, let it
fire, and read the `fires` row. Green is a record that does not claim to have
stopped anything, and either one run or an explicit decision to have two.

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

## What to merge first, and why

Written for Reljod, because on the night these findings were produced four pull
requests sat READY that no agent could merge, and most of the remaining work
touches files those four hold. This is a reading order, not a verdict — the gate
still decides.

1. **#237 — the roles panel offers a row its own harness's models.** Merge this
   first. Without it the configuration Reljod asked for cannot be set through
   the panel in one pass at all (X6), and it holds `service.rs` and the TUI,
   which blocks X11 and X9. Its diff also touches the same `apply_role` seam as
   X7, so whoever fixes X7 needs it landed or they will collide.
2. **#238 — carry the whole thread across a harness switch.** It owns the seam
   X5 lives on. X5's fix has to compose with it rather than undo it, and #238's
   author's session has since died, so the longer it sits the more likely
   somebody rebuilds it by accident.
3. **#239 — tell an agent when a card answer overrules it.** Holds `team.rs`,
   which blocks X12.
4. **#240** — same batch, unblocks the rest of `main.rs`.

Then the findings in this file, in this order:

- **X13** (no manager tier at runtime) and **X7** (a role harness change breaks
  every turn) are the two that make the requested configuration not work. X7 has
  a fix in flight; X13 needs its mechanism pinned down first and should not be
  handed to an agent to "just implement".
- **X1** and **X10** are both decisions rather than implementations, and both
  are deliberately unclaimed. Read X1 first: X13 may turn out to be downstream
  of it.
- **X8** (a failed run reported as exit 0 and silence) is small, contained, and
  the one most likely to waste somebody's night, because an unattended run can
  do nothing all night and report success.

**Do not let a fixing spree near these**, which work and which nobody will
re-test: the queue while main is busy, Esc, `/stop`, the handling of ambiguous,
contradictory and destructive instructions, and auto-compaction's reporting.
Every one was exercised live and behaved correctly. A regression there is the
one that ships.

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
| S29 | Repository work in a **registered** project, project named | goes through a manager | **fail** — `open_work`, no manager. Placement *was* correct this time (worktree cut from `lab`), which is the difference from S18: naming a registered project fixes placement but not the missing manager. Filed as X13 |
| S30 | "Say OK." | trivial answer | **pass** — landed in the pinned main conversation |
| S31 | Repository work again, main provably in the pinned conversation | refusal naming `ask_manager` | **fail** — `open_work` again. Filed as X13 |
| — | Registering a project | `jod project add <path>` | **pass** — note `jod project list` is `jod project ls`; `add` takes a bare path, and `add <name> <path>` is rejected |
| — | Disk filled to 100% mid-run | — | **environment, not a defect** — a run died with "No space left on device". The repo's `reclaim-disk` skill freed 8.6 GB across three idle `target/` directories using its own safety checks. Worth arming the hourly sweep: this fleet makes targets faster than they age out |
