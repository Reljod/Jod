# Decisions (the WHYs)

Why the guidelines in [`AGENTS.md`](../AGENTS.md) are what they are, so the
reasoning outlives the session that set it and nobody re-litigates a settled
call. The charter states the rule in a bullet; this file is where the argument
lives.

Add an entry when a choice proves itself. Distill it, don't narrate it — and if
the rule fits in the charter's bullet without this file, it doesn't need an
entry here at all.

## The toolkit stays out of `domains/`

Skills and the charter never reference personal domains, so `.agents/` stays
copyable into any repo. A reusable workflow is not one of Reljod's personal
life-domains.

## Jod delegates to harnesses; it never calls a model

`jod-core` has no model client, no prompt templates and no tools. It shells out
to an agent CLI — Claude Code, OpenCode — and normalises the output.

The tempting alternative is to call the Messages API directly and own the loop.
That means re-solving context management, tool dispatch, permission prompts and
retries, all of which the harnesses already solved and keep improving. Worse, it
makes Jod the thing that must be upgraded every time an agent gets better.

Delegating instead makes the harness a *replaceable part*: adding one is a
single file implementing `Harness`, and the desktop app, the planned iOS client
and a VPS daemon never learn that anything changed. It also means an agent's
token budget is the agent's own problem, which is the only place it can actually
be managed.

The cost is real and accepted: Jod can only do what a harness CLI exposes, and a
harness changing its JSON breaks an adapter. That is why unrecognised output
becomes a `Raw` event rather than being dropped — the prototype found OpenCode's
`tool_use` rename that way, on the first real run.

## A run is a detached process group, and the database is its only transport

**Supersedes "Agents run in tmux, not as child processes" and "An agent's tmux
session outlives the agent."** Both were right about the problem and are kept
below, because the requirements they identified are the ones the replacement had
to meet.

Every delegated task used to get its own `tmux` session, whose pane was piped
through `tee` into `~/.jod/runs/<id>/stream.jsonl`, which Jod tailed and folded
into the `events` table. Three intermediaries between a harness and the store.

Now: Jod writes a plan, starts a detached `jod-run` supervisor on it, and the
supervisor parses the harness's stdout and appends events **straight into
`jod.db`**. tmux is gone, the JSONL file is gone, and — because a plan is
`execve`'d rather than sourced — so is the generated shell script and every
quoting concern that came with it.

What tmux was buying, and where each thing went:

| tmux gave us | now |
|---|---|
| A live view (`tmux attach`) | `jod watch <id>`, reading the store |
| A kill switch that works when Jod is closed | `kill(-pgid, SIGTERM)`, from any process |
| Survival of the launcher quitting | `setsid`, so the run leads its own session |
| One transport for laptop, SSH and API | one SQLite file, which the API already served |

The first row is the one that got *better*. `tmux attach` needed a shell on the
box; `jod watch` is a query, so the same view reaches the web client and the
phone. It also works on a finished run, replaying it instead of refusing.

**Why the supervisor is a separate executable and not a thread.** The whole
promise is that a run outlives its launcher. A thread cannot hold the read end
of the harness's stdout pipe past its own process's death — the harness would
take `EPIPE` on its next write, so closing an SSH session would kill the agent,
which is strictly worse than what tmux did. `fork()`ing in-process is also out:
`jod` is multithreaded, and only async-signal-safe calls are legal in the child,
which rules out running SQLite there. A `setsid`'d binary is the only option
that keeps the promise, and it costs a dependency that ships from the same
`cargo build` rather than one the box has to install.

**A killed run must not read as a clean one.** Found by testing rather than by
reasoning: `SIGTERM` to the group reaches the harness *and* the supervisor at
once, the harness usually dies first, its pipes close, and the supervisor can
finish the whole run before its own signal handler gets a turn — recording a
killed run as `completed`. The status is therefore derived from the child's exit
status, where a signal death is visible as a fact rather than inferred from
having handled one. → [the general rule](#a-failed-run-must-never-look-like-a-successful-one)

**What the old entries got right, and what it cost to keep.** The second one
below is a chain of sensible defaults that closed a user's terminal window; none
of it can happen now, because Jod no longer owns a terminal to close. The
general lesson it drew still stands and still constrains the design: a killed
supervisor is asked with `SIGTERM` and only then `SIGKILL`, precisely so it can
record how the run ended rather than vanishing and leaving it marked running.

### Superseded: Agents run in tmux, not as child processes

Every delegated task gets its own `tmux` session. A child process would have
been less code.

tmux buys four things at once that would otherwise each need building: a live
view (`tmux attach`), a kill switch that works when the app is closed, survival
of the app quitting, and one transport that is identical on a laptop and over
SSH on a VPS. The generated launcher pipes the harness through `tee`, so the
pane a human watches and the JSONL Jod parses are the same bytes — there is no
second code path that can disagree with what was actually shown.

It also makes tmux a hard dependency, which the UI states plainly rather than
discovering at spawn time.

### Superseded: An agent's tmux session outlives the agent

Jod's sessions used to end when their agent did. That closed the terminal window
of anyone watching, and the chain is worth writing down because every link is a
sensible default on its own:

1. tmux's `detach-on-destroy` defaults to **on** — destroying a session makes an
   attached client *exit* rather than fall back to another session.
2. oh-my-zsh's tmux plugin sets `ZSH_TMUX_AUTOQUIT` from `ZSH_TMUX_AUTOSTART`,
   and runs `exit` the moment its tmux client returns.
3. So: watch an agent → agent finishes → session destroyed → client exits →
   the shell exits → **the terminal window closes.**

Two fixes, because either alone leaves a hole. The launcher `exec`s a shell
after the agent exits, so a *completed* run never destroys anything. And Jod
sets `detach-on-destroy off` **on its own sessions only**, so an explicit kill
returns the watcher to another session instead of ending their client. The
user's global tmux config is never touched — it is not ours to change.

The general lesson: **a long-running process Jod spawns must never be able to
take a user's terminal with it when it ends.**

## Blocked is a legal ending

This is the anti-workaround rule, and it is the highest-leverage thing in the
repo.

A gate whose only successful exit is "the check passes" *mathematically
requires* a fake when the check can't pass — no key, no service, no fixture.
Working around the blocker isn't disobedience, it's reward hacking: the task
was bound to "make the check pass", the wall has no legal route over it, so the
cheapest remaining path is to mock the client, invent a key, widen a
`try/except`, or skip the test. From inside the task, that reads as success.

So "done" is redefined as two endings, not one: the check passes with real
output, **or** a `BLOCKED.md` exists naming the missing capability, what was
tried, and what's needed. The incentive to improvise disappears the moment
honesty is a valid exit.

Two supporting details:

- **Enumerate the forbidden workarounds.** "Don't cheat" is too abstract to
  bind behavior, so the charter names them individually. That list is the one
  piece of detail that stays in the always-loaded file.
- **Sanction a specific fake.** Much of this is ambiguity, not deceit. Say
  which mock, where it lives, and when it's allowed (the `SPEC.md` template has
  a section for exactly this). Anything undefined at a decision point gets
  filled by improvisation, and improvisation under a binding goal trends toward
  whatever makes the check pass.

The `TaskCompleted` hook (`.claude/hooks/task-completed-tests.sh`) implements
both halves, because instructions are advisory and hooks are deterministic. It
requires the note to carry `Missing:` / `Tried:` / `Needs:` and to name every
failing suite — which is also what stops a stale note from working as a
permanent bypass.

## The human gate is the spec, not the diff

Verifying is the bottleneck, not writing. So the gate belongs where a decision
is cheapest: one sentence at spec time, one PR at review time — never a plan
someone must read before work can start.

Reviewing every plan before every run is still a synchronous human gate; it
just moves the pain earlier. Instead the spec pre-declares an escalation list
(irreversible actions, migrations, auth, public contracts, money, deletion,
anything guessed at, missing capabilities) and the agent decides everything
else and logs it. Three flagged decisions to read, not forty planned steps.

A spec earns its keep by being self-contained: named files and interfaces,
explicit non-goals, one end-to-end check that proves the feature works, and the
sanctioned fakes. Execution happens in a *fresh* session, because a new reader
is the only real test of whether the spec stands alone. → **`/write-spec`**

Corollary: **gate autonomy on dependency completeness, not task size.** A task
qualifies for an unattended run only if its whole dependency set already exists
in the environment. A missing key or fixture discovered at step nine is the
most common cause of an invented fake.

## Quality by layering, not diligence

Cheap deterministic checks early (git hooks) under mandatory ones later
(required CI) beats relying on remembering to be careful. Two constraints fall
out of it:

- Nothing safety-critical lives *only* in a hook.
- No design may depend on a hook holding against an agent that cannot possibly
  satisfy it. A Stop hook is overridden after 8 consecutive blocks, so a gate
  that's impossible to pass is a gate that eventually isn't there.

## "Tested" means CI ran it, not that an agent says so

`install.sh`'s update logic shipped with only a local run as evidence, which a
reviewer had no way to check without re-running it. A `Tests` Action now runs
every `*.test.sh` on every push/PR, so pass/fail is a status on the PR rather
than a claim in chat.

The same rule at review time: PRs carry the check's **real output** and the
diff-derived deltas from `create-pr`'s evidence bundle. A faked pass is
invisible in a summary and obvious in raw output.

## Review runs on a fresh context, briefed narrowly

A reviewer that never saw the reasoning judges the result on its own terms.

But a reviewer told to "find gaps" will find some even when the work is sound,
and the result is extra abstraction, defensive code, and tests for impossible
states. So the brief is narrow on purpose: the diff, the spec, and the
substitutions checklist — nothing else, and correctness over preference.
[`REVIEW.md`](../REVIEW.md) briefs the agents the shepherd spawns;
[`.claude/agents/reviewer.md`](../.claude/agents/reviewer.md) briefs the
in-session ones.

## Visual means deltas, not a prettier plan

The artifacts that actually cut review time are computed from the diff, after
the run: a blast-radius map, a contract diff, a substitutions scan, a
plan-vs-diff deviation report, before/after visuals for anything user-facing,
and a decision log with confidence per call. Generated afterwards is what makes
them free — a bundle attached to the PR costs no synchronous approval.

Volume is a routing problem, not a rendering one: tier by blast radius, and
batch a stack of related PRs into one digest rather than N context loads.

## Issue keys are opt-in, never the default

The commit gate first required a Linear-style `ENG-123` in every subject, and
the scaffolder baked that into generated charters. That's one team's house rule,
not a property of a good commit — it breaks on repos with no tracker and on the
many real commits that map to no issue. `TICKET_REGEX` ships empty; `--ticket`
is an explicit per-repo decision.

## The scaffolder asks; the script still takes flags

`setup-project.sh` with no choices walks a human through them (↑/↓, space,
enter) rather than making them hand-assemble `--skills`. The wizard only *fills
in* flags, so scripts, CI, and agents keep the same deterministic entry point;
no-tty falls back to `--list` instead of hanging on a prompt.

## `install.sh` installs the binaries; the skills ship as a plugin

`install.sh` used to bootstrap the *toolkit* — a clone at `~/.jod` plus a bash
`jod` that dispatched into `.agents/skills/`. It now clones the source to
`$JOD_HOME/src`, builds the workspace, and installs the real `jod` and
`jod-run` binaries. The skills it used to front are distributed by the Claude
Code plugin, and reached from a checkout by running their scripts directly.

One installer, two different `jod`s on `PATH` was the actual problem. Claude
Code puts an enabled plugin's `bin/` on `PATH`, so the shell shim shadowed the
compiled binary on exactly the machine that needed the compiled one — and the
shim, having no `mcp`, `tui` or `daemon` subcommand, answered every such call
with "unknown command". Deleting it removes the collision rather than ordering
`PATH` around it. Nothing was lost: every command the shim had was a thin call
into a script the plugin already ships.

What this buys is the thing the toolkit installer never could: **the VPS
console can update itself**. `jod update` is what a resident `jod tui` runs to
become a newer build.

## `jod update` shells out to `install.sh` rather than reimplementing it

Version resolution is a set of rules about a tag list — highest patch within
the installed `MAJOR.MINOR`, never a minor jump, a branch install
fast-forwards. Two implementations of those rules that drifted apart would mean
`install.sh` and `jod update` installing different things from the same tags,
which is the one failure a versioning scheme exists to prevent. So the rules
live in the script, and the Rust command answers only what the script cannot
know: where the checkout is (`$JOD_SRC`, else `$JOD_HOME/src`), where the
binaries currently live (the directory of `current_exe`, so an update replaces
what the box is actually running, `/usr/local/bin` included), and which
binaries this machine has — `jod-api` is opt-in at install time, and an update
must not silently drop it from a box that chose it.

Two consequences worth stating:

- **The source lives *inside* the state directory** (`$JOD_HOME/src`), not
  beside it. That is what lets `jod update` find its own checkout with no
  configuration, including when Jod runs as a system user whose `$HOME` is not
  the one that ran the installer.
- **`jod update` is handled before the store is opened.** An update is how you
  recover a build that cannot open its own database; a version of it that
  needed a working store would be useless on the day it was needed.

## The console updates itself, in the background, and asks before restarting

`/update` in the TUI is the same `install.sh` run, started as a background job
with its output streamed into the transcript. Three decisions, each paid for by
the shape of the thing being updated:

- **Backgrounded, not suspended.** The first version took the terminal the way
  `$EDITOR` does. A cold `cargo build` is minutes, and the console is what you
  were in the middle of using — freezing it for the duration contradicts the
  reason the console exists. Agents keep streaming while it builds.
- **Visible.** A job you cannot see is a promise you cannot check, so the
  status row carries a running count and `Ctrl-G j` (`/jobs`) lists every
  background shell with its last line, how long it has run, and how it ended.
  Finished jobs stay listed: "did that update work" is asked afterwards.
- **The restart is asked for, never taken.** Replacing the file does not
  replace the process, so a successful update offers a reload, and `/reload`
  does it on demand. It is an `exec` of the new binary with the same
  arguments — same terminal, same pid, same tmux pane, which is what makes it
  usable as *the* way the always-on VPS console takes an update. Nothing is
  lost that was not already on disk: agents are their own process groups and
  the conversation is in SQLite. It gets its own overlay rather than the
  delete confirmation, because "this cannot be undone" is false here and a
  warning that cries wolf is worse than none.

`/update` takes no version argument. A minor or major move changes what the
console and the daemon *are*, and the console mid-session is the wrong place
to decide that; `jod update --version` at a shell is.

## An update renames the new binary over the old one

Writing a running executable in place fails with `ETXTBSY` on Linux, and the
binary being replaced here is routinely the one running the update — the VPS
console is a long-lived `jod tui`. So each binary is installed under a
temporary name and `mv`'d into place: rename swaps the directory entry while
the running process keeps the inode it started with. It picks the new build up
when it restarts, which the installer says out loud, naming any running
`jod-daemon`/`jod-api` unit and any live console. A replaced binary is not a
restarted process, and silence there reads as "the fix didn't work".
`tests/install.test.sh` holds a process open across an update and asserts both
halves.

## `install.sh` builds; `jod upgrade` downloads

`install.sh` runs `cargo build --release --locked`, and that is still the
install path: the box running an agent supervisor is one that should be able to
rebuild it, and `--locked` means the build is the release rather than whatever
the registry resolved that day.

The reason originally given for building was that releases carried no binary
assets. They do now — [the tag and the binaries are one
act](#the-tag-and-the-binaries-are-one-act) put a build matrix, asset naming
and checksums in the release workflow, which is exactly the cost this decision
said it was deferring. So the condition it named — "worth revisiting when there
is a second machine that cannot build" — has been met, and the revisit is a
*second* command rather than a rewrite of the first.

Re-running `install.sh` is cheap because the install is idempotent: the ref
*and* the commit are recorded (`.jod-version`, `.jod-commit`), and a run whose
target commit is already installed skips the build entirely and says so.

## `update` and `upgrade` are two commands because they are two acts

| | `jod update` | `jod upgrade` |
|---|---|---|
| Gets the bits by | `cargo build` in `$JOD_SRC` | downloading `jod-<target>.tar.gz` |
| Needs | git, a Rust toolchain, a checkout | curl, tar |
| Moves to | newest patch of the installed MAJOR.MINOR | newest release, any major/minor |
| Takes | minutes | seconds |

Neither subsumes the other. `update` cannot run at all on a box installed from
the prebuilt tarball — the README's first install path, the one that advertises
needing no Rust toolchain — because there is no checkout to build from, and
before `upgrade` existed such a box had no way to take a new release short of
reinstalling by hand. `upgrade` in turn cannot install a branch or a commit,
because no tarball is published for one.

They are not folded into one verb with a flag. A single command whose mechanism
depended on what happened to be on the box would mean the safe patch-only move
and the minor-crossing one were the same keystroke, and which you got was a fact
about the machine rather than about what you asked for.

`upgrade` moves across a minor deliberately, which `update` never does. That is
the compensating discipline: it is not what runs unattended, and the console
refuses `/upgrade <version>` outright — landing on a release nobody else is on
is not a decision to take from inside the console the move replaces.

The published `.sha256` is checked before anything is installed, and a download
that cannot be verified — bad checksum, or no checksum on the release at all —
is refused rather than installed with a warning. These binaries are not signed,
so that check is the only integrity guarantee in the path; skipping it when it
is inconvenient would make it decorative.
→ `bin/jod-upgrade.sh`, `cli/src/upgrade.rs`, `tests/upgrade.test.sh`

The script is compiled into the `jod` binary rather than only shipped in the
repo, and writes itself to a private temp directory to run. The box that most
needs to upgrade is precisely the one with no copy of this repo on disk, so an
upgrader that lived only in a checkout would be missing wherever it mattered.
`bin/lib/semver.sh` is embedded beside it, so the version-comparison rules have
one implementation shared with `install.sh` rather than two that can disagree.

## Releases are semver tags, cut manually

`vMAJOR.MINOR.PATCH`, never on every push — pushing a `release/vX.Y.Z` branch
prepares one and a person publishes it (`workflow_dispatch` is still there for
the case with no branch).
→ [how the version is decided](#a-branch-name-is-a-request-the-tag-list-is-the-fact)

Install pins to latest; `jod update` only takes newer patches within the
installed MAJOR.MINOR, so a minor/major bump can't yank the rug out from under
an existing install.

## The tag and the binaries are one act

Building the clients started life as its own workflow, dispatched against the
tag `release.yml` had just created. The separation looked like good hygiene —
deciding a version and shipping binaries *are* different acts — but splitting
them across two workflows bought nothing and cost three things.

Two workflows each had to answer "which version is this?", so `build_target.sh`
existed only to re-confirm a tag `release_version.sh` had minted minutes
earlier, and the two resolvers disagreeing was a reachable state. Each ran the
full suite, so every release paid for it twice. And the second button was
manual, so the interval between "the tag exists" and "the tag has binaries" was
however long it took someone to remember — during which
`/releases/latest/download/…`, the URL the README prints, 404s.

Now one dispatch tags, builds from that tag, and attaches. The property that
mattered is kept as a structural one instead of an organisational one: exactly
one job mints a tag, and the job that uploads assets cannot run unless that job
succeeded. `build_only` covers what the split was really for — proving the
Tauri and iOS builds still compile without shipping anything.
→ `tests/release-version.test.sh` asserts both, job by job.

## Scaffold fitness is checked at release time, not every push

`tests/e2e/run.sh` scaffolds against a spread of fixture repos (greenfield JS,
OSS conventions, a monorepo, a hand-written charter, …) and logs where the
generated `AGENTS.md` doesn't fit as a "gap" rather than failing the run.
N scaffolds is too expensive per PR, and gaps are findings to support later, not
a merge-blocking contract. Wired into `release.yml` / `e2e.yml`.

## Parallelism here is agent teams, and ownership is what makes it safe

Teams beat worktree-isolated subagents because the work that benefits is review
and investigation, where teammates need to argue with each other — exactly what
subagents can't do. The cost is one shared checkout, so the isolation git would
have given us comes from disjoint file ownership plus the `TaskCompleted` gate
instead. → [`teamwork.md`](teamwork.md)

## The plugin is the repo root, not a copy of it

Claude Code installs the toolkit as one plugin, `jod`, whose manifest lives at
`.claude-plugin/plugin.json` and whose component paths point straight at the
trees that already exist — `./.agents/skills/`, `./.claude/agents/`. A
`plugin/` directory holding copies would need a sync step and would drift the
first time someone edited only one side; a generated-then-committed copy is the
same trap with extra machinery. The cost of pointing instead of copying is that
a moved directory ships a plugin with no skills *silently*, which is why
`tests/plugin.test.sh` asserts every declared path resolves.

The same repo root is also the marketplace (`.claude-plugin/marketplace.json`,
entry `source: "./"`), so `/plugin marketplace add Reljod/Jod` and
`/plugin install jod@reljod` need no second repository to maintain.

Not shipped in the plugin: the `SessionStart` hook that sets `user.name` /
`user.email`. Rewriting git identity in every repo the plugin is enabled in is
invasive and wrong in a work checkout — it stays project-local in
`.claude/settings.json`.

## Skills locate their own scripts with `${CLAUDE_SKILL_DIR}`

A plugin's skills run from `~/.claude/plugins/cache/…` while the cwd is the
user's project, so a skill that said `.agents/skills/create-pr/scripts/x.sh`
worked only in this checkout and was dead on arrival for every plugin user.
Claude Code substitutes `${CLAUDE_SKILL_DIR}` in skill content for personal,
project *and* plugin installs, so it is the one form that works in all three.

The `.claude/commands/<skill>.md` wrappers reach a skill by *reading the file*,
which performs no substitution — so each wrapper now states what the variable
resolves to in that repo. That keeps `setup-project`'s copy-into-a-repo install
working, and `tests/plugin.test.sh` fails if a wrapper loses the line.

## The release stamps the plugin manifest onto the tag

Claude Code only offers a plugin update when `version` in `plugin.json`
changes, so a tag-only release would move the tag and tell installed copies
nothing. `release.yml` writes the computed version into the manifest, commits
it onto the tagged commit, and pushes *only the tag* — tags are already the
source of truth here (`install.sh` resolves the newest one), so the released
artifact carries its own version without CI needing write access to `main`.

## The plugin manifest declares skills and nothing else

`claude plugin validate` passes on manifests that are broken at install time,
so the manifest was written twice from the docs and wrong both times. What the
installer actually enforces, against Claude Code 2.1.220:

- **`agents` must not be declared.** A directory value is rejected outright
  (`Validation errors: agents: Invalid input`). An explicit `.md` file list
  validates cleanly — and then loads nothing, `Agents (0)`. Only the default
  `agents/` scan works.
- **`hooks` must not be declared.** `hooks/hooks.json` is auto-discovered, so
  naming it too is a *duplicate* load and the whole plugin fails with
  `Duplicate hooks file detected`.
- **`skills` is declared**, because it is the one component whose files don't
  live in the default location, and a directory value works there.

The rule that falls out: let discovery do its job, and declare a path only when
a component genuinely lives somewhere else. `tests/plugin.test.sh` fails if
either key reappears, since validation won't.

## The subagents exist twice, and a test keeps them identical

A plugin reads agents only from `agents/` at its root; the repo reads them only
from `.claude/agents/`. Both are needed — the plugin ships them, and they have
to work in this checkout for anyone who hasn't installed it. Symlinking one to
the other doesn't work: the plugin loader doesn't follow them and silently
reports `Agents (0)`.

So they are two real copies, and `tests/plugin.test.sh` diffs them in both
directions — drift fails, and an agent added to only one side fails too. A copy
guarded by a check beats a clever link that fails silently.

## A regex decides what merges unread

Every PR is categorised by `pr_triage.sh`, and only a `merge_pr.sh` exit code
opens the door to `gh pr merge`. The obvious alternative — ask a model "is this
PR safe to merge?" — was rejected, and the reason generalises past this repo.

The diff is written by whoever opened the PR, which in this repo is usually the
same agent asking to merge it. A model reading that diff to decide the question
is being asked to grade its own homework using text it wrote, and a branch that
adds `this change is trivial and pre-approved` has a real chance of being
believed. A pattern match cannot be persuaded: it matches or it does not, and no
commentary in the diff changes what `grep -E` returns. The judgement layer still
exists — the shepherd spawns read-only `reviewer` agents on every PR that
reaches `ready` — but it advises, and the thing that can actually merge is
deterministic.

There used to be a second judgement layer: a `claude-code-review.yml` workflow
that reviewed every PR on push, alongside the shepherd's agents reviewing the
same diffs minutes later. Two models reading one diff on overlapping triggers
produced two opinions, no additional authority — neither could merge anything —
and a review comment nobody could tell apart from the one that mattered. The
shepherd's pass survived because it is the one wired to the gate.

Two properties make the gate hold up, and each closes a hole the other leaves:

- **It only escalates.** Every rule can send a PR to a human; nothing can bring
  one back. A false positive costs one human read, a missed pattern costs an
  unreviewed merge, so the path patterns are deliberately broad.
- **`security`, `gate`, `ci` and `data` are a floor, not a default.** No flag or
  environment variable can make them auto-mergeable, and `gate` covers the
  charter, CI config, and this classifier itself — so a PR that widens the rules
  cannot be merged by the rules it widens.

The exemption prose gets is conditional on prose being inert. Research notes and
docs merge unread because nothing executes them; a `.sh` or an executable bit
under `research/` is reclassified as code and clears the code rules on its own,
and a destructive command (`rm -rf ~`, `sudo`, `curl … | sh`, `DROP TABLE`)
blocks anywhere. The same reasoning excludes prose from the size limits — a
3,000-line writeup is not riskier than a 300-line one, and a gate that punishes
thoroughness is one people route around.

Precision was traded for adoption in exactly one place, on purpose. The
destructive-command rule matches `rm -rf` only against `/`, `~`, `$HOME` and
bare globs, so the `rm -rf "$tmpdir"` in every test fixture doesn't fire. A rule
that cries wolf on ordinary cleanup gets the whole gate switched off within a
week, and a rule nobody keeps enabled protects nothing.

## Merges are linear, and never from behind

`merge_pr.sh` accepts `squash` and `rebase` and refuses `merge` outright, and it
refuses any branch behind its base.

The linearity half is ordinary taste. The behind-base half is not, and it is the
one that actually bites: a branch behind base was tested against a tree that no
longer exists. Squash and rebase both replay its commits onto current base
*without re-running anything*, so a green tick plus a stale base is precisely how
a passing PR breaks `main`. GitHub reports this as `mergeStateStatus: BEHIND`
only when branch protection asks for it, so the script also counts commits
locally rather than trusting the API to volunteer it.

`--update-branch` rebases and then stops, which looks unhelpful and isn't:
merging in the same breath would merge on the checks that ran before the rebase,
which is the failure the rule exists to prevent.

## Enforcement is a floor; instruction is not

The merge gate first put the charter, every skill and every agent definition in
the same never-auto-merge bucket as CI config. That was one category doing two
jobs, and the cost showed up immediately: a typo fix in `AGENTS.md` needed the
same human as a workflow rewrite.

The split that survives is **what the machine enforces** vs **what it reads**.
CI, git hooks, tool permissions, plugin manifests, `CODEOWNERS` and the
auto-merge skill are `gate` — change them and you change which checks can run at
all, including the one being asked for an opinion. The charter, skills, agent
definitions and commands are `rules`: prose an agent obeys, inert until someone
reads it, and auto-mergeable.

That leaves exactly one dangerous rules edit — the one granting the branch
permission to merge itself — and it gets its own scan rather than its own
category. Any `rules` diff that adds *or removes* merge-policy language
(`auto-merge`, `--allow`, `branch protection`, `--no-verify`, `bypass`) goes to a
human. Removed lines count because deleting "never bypass branch protection" is
the weakening move and leaves no `+` line to find; every other scan reads added
lines only, since a substitution is something you put in.

The auto-merge skill's *prose* sits in `gate`, not `rules`, for the same reason:
its prose is the merge policy. Blocking the whole category would have been
simpler, and the charter would have stopped improving — a rule that can only be
fixed at a human's convenience mostly isn't.

## Agents finish their own PRs

`create-pr` now ends by running `merge_pr.sh <pr> --ready`, so a green trivial PR
closes itself instead of waiting.

The reason is the same one behind the gate: review attention is the scarce
resource, and a queue of unremarkable green PRs spends it in the worst possible
way — it trains whoever opens them to skim, which is precisely the habit you
don't want when the migration lands.

Two deliberate frictions remain. `--ready` is an explicit flag rather than
implicit behaviour, because un-drafting is what announces to everyone watching
that the work is finished, and a script shouldn't say that on the author's
behalf unless asked. And pending checks are refused rather than waited on: a
script that polls holds an unbounded window in which someone pushes, and the
whole value of "all checks green" is that it was true of the commit being merged.

## Content rules read code, not prose

The destructive-command scan first ran over every added line in a diff. Its
first contact with real history killed that: the 440-line
`research/agent-host-os-2026` writeup already merged into `main` came back
`human-review` for quoting `sudo -u jod …` and `curl … | sh` from a
provisioning guide.

Nothing in that document executes. It describes a machine; it does not
administer one. Blocking it is precisely the failure mode the gate is meant to
remove — a thorough writeup punished for being thorough — so the scans that
describe *what a change does when it runs* now read only the files that can
run.

Two exceptions keep it honest, and both come from asking what is actually true
of the file rather than what directory it sits in:

- **`rules` files are scanned like code.** A charter is prescriptive. "Always
  start by running `rm -rf ~/.cache`" is obeyed more literally than a shell
  script is, because an agent reads it as an instruction rather than executing
  it in a sandbox.
- **The credential rule scans everything.** A live key pasted into a research
  note is leaked exactly as thoroughly as one in a config file. Nothing has to
  run for that to be true.

The general lesson is that "is this file dangerous" is the wrong question, and
"will anything run this" is the right one — the same question that already
decided the size limits, and the reason a `.sh` under `research/` is code no
matter what sits beside it.

## Judgement may subtract, never add

The merge gate refuses to let a model decide whether a PR is safe, and the PR
shepherd spawns two models per PR. Both are right, because they are answering
different questions.

`merge_pr.sh` is the only thing in the system that says **yes**. The sweep that
finds candidate PRs runs it in `--dry-run` and reports; the `merge-checker` and
`reviewer` agents read the PR and return `VERDICT: CLEAR` or
`VERDICT: BLOCK — <reason>`. A `CLEAR` grants nothing — it withholds a veto. So
the diff-written-by-the-author problem that killed "ask a model if this is safe"
does not arise: a diff that talks a reviewer into `CLEAR` has bought exactly the
outcome it would have had if no reviewer existed, and one that talks a reviewer
into `BLOCK` costs a human glance. There is no input that makes the routine
merge something the gate would have refused.

That is also the failure mode worth designing for, because it is the likely one.
If the agent layer is unavailable, hallucinating, or wrong in every direction at
once, what remains is `/auto-merge` running unattended — the behaviour already
shipped and already tested. The routine degrades into the thing it is built on
rather than into something new.

Two filters sit above the gate and no flag relaxes them, because they are
questions of standing rather than of safety:

- **Fork PRs are never swept.** The scheduled job holds a write token, and
  treating a fork head as a candidate hands that token's reach to anyone who can
  open a PR. The author field is not a defence here — the fork's owner writes it.
- **Only the repo owner's PRs are candidates**, plus whoever `--author` names
  explicitly. A teammate's branch closes when they say it does, which is the
  charter's rule, not a security control.

The sweep itself cannot merge, which is what keeps a bug in enumeration cheap: a
mistake there widens what gets *considered* and never what gets *merged*.
Merging stays serial for a duller reason — every merge puts the remaining
branches one commit behind base, and behind-base is itself a refusal, so a batch
merge would be merging PRs against a tree they were never tested on.

## The VPS address lives in SSH config, not in the repo

Reljod calls one machine four things — VPS, Jod Cloud, Jarvis, cloud — so
[`domains/infra/`](../domains/infra/README.md) has to write the vocabulary down
or every session re-asks which host is meant. The obvious way to write it down
is `ssh reljod@<ip>`, and that is the one thing this repo can't do: it is
public, and a real username beside a real address is a free target list for SSH
brute-forcers. Git history makes the mistake permanent — rotating an address is
work, un-publishing one is not possible.

So the repo names an *alias*, `jod-cloud`, and `~/.ssh/config` resolves it. The
docs stay literally correct and copy-pasteable, the address stays off GitHub,
and moving the box to a new IP is a one-line local edit that no document has to
follow. The cost is a machine without the alias gets `Could not resolve
hostname` instead of a working command, which is why the domain note calls that
failure out and says to ask for the entry rather than hard-code an address.

The general rule: a public repo may name a host, never locate one.

## Jod is a CLI on top of other CLIs

The obvious way to build an assistant is to hold an API key and call a model.
Jod does the opposite: it shells out to `claude`, `opencode` and `agy`, and
parses what they print. That looks like a layer of indirection bought for
nothing, and it is worth writing down why it is not.

An agent harness is not a thin wrapper around a model. It is context management,
tool definitions, permission prompts, retry behaviour, MCP support and a
transcript store — years of work each, maintained by teams whose full-time job
that is. A model client in `jod-core` would be a commitment to rebuild all of it
and to keep rebuilding it every time one of them ships something. Piping text to
a program that already works costs one adapter file and stays correct while
those teams improve underneath.

It also makes the choice reversible in the direction that matters. Jod is not
betting on which harness wins. Adding a fourth is one file, and dropping one is
deleting a file, because everything above the seam speaks `AgentEvent` and has
never heard of any harness. When AGY renamed nothing and simply behaved
differently from Claude Code, the cost was a single adapter, not a refactor.

The price is real: Jod can only do what a harness exposes on its command line,
and it inherits every quirk of three separate programs. That price is paid in
the adapters — see below — and it is much smaller than the one it avoids.

## A failed run must never look like a successful one

Each of the three harnesses has at least one way of failing quietly, and all
three were found by running the real binaries rather than reading their docs.

AGY is the worst of them. In headless mode it auto-denies any tool that would
need approval — nothing can prompt — and then reports `status: SUCCESS`, an
empty response, and exit code 0. The only honest signal is a line of English
prose on stderr. Separately, an unknown `--conversation` id does not fail: AGY
starts a brand new conversation and reports success, so an agent that believed
it was resuming a thread has silently lost every prior turn. And its
`--print-timeout` defaults to five minutes, killing long work in a way that
looks like a short answer.

None of these is an error condition the program admits to, so none of them can
be handled by checking for errors. Each needs a positive test for the *absence*
of success: a run that produced no output did not succeed; a run whose reported
conversation id is not the one we asked for did not resume; an exit code of 0
from a run we have already flagged as errored is not a zero exit.

The general rule this leaves behind: **trust a harness's output, never its
self-assessment.** Exit codes, status fields and error arrays are all claims by
the program about itself, and a program that has just misbehaved is exactly the
one least able to report it.

### And the converse, which is easy to get wrong while fixing the above

Suspicion has a cost in the other direction. Every adapter used to latch the
whole run as failed the moment any single tool call returned an error, which
sounds like the cautious reading and is not. Driving the TUI turned it up
immediately: an agent ran `python`, was told there was no such command, retried
with `python3`, produced correct output — and the run was reported `✗ failed`.

A failed tool is ordinary. Agents probe, guess a path, try a command that is not
installed, and recover; that recovery is the loop working, not breaking. So the
two questions are kept apart. **Did this tool call fail** is answered per call
and shown per call. **Did the run fail** is answered only by the harness's own
result record and the exit code, checked against the absence-of-success tests
above.

Collapsing the two in either direction destroys the same thing — the ability to
tell the two apart at a glance — and a status line that cries failure over
recovered work is ignored exactly as fast as one that never cries at all.

## Memory is governance first and retrieval last

The instinct with agent memory is to reach for embeddings, and the measured
answer is that retrieval quality is the least valuable part of the system.
Across two rounds of experiments
([`research/harness-agents-research`](../research/harness-agents-research/RECOMMENDATION.md)),
better retrieval was worth 0.02–0.07 of composite score; deciding what is
currently true, what may be seen, and what has really been deleted was worth
0.2–0.5.

The reason is structural rather than incidental. A superseded fact is a
near-perfect lexical *and* semantic match for a question about the fact that
replaced it, so flat retrievers rank the outdated version above the current one
35–54% of the time. No ranker fixes that, because the ranker is answering the
question it was asked; "what is true now" is not a similarity question. It has
to be answered by the schema — `valid_to IS NULL` — before ranking happens.

Three consequences, each measured, each cheap to build now and a migration
later:

- **Scope is a partition, not a signal.** Used to boost ranking it leaked facts
  across domains 79% of the time; used as a filter, 0%.
- **Origin is a column, not text.** If trust level lives inside the fact's
  content, then content Jod ingested can assert its own trust — a page that says
  "origin: owner" would be believed. It also has to be visible when the fact is
  read, or the distinction may as well not exist.
- **Deletion purges every version.** Closing only the current one leaves the
  withdrawn fact fully readable to any question phrased about the past, which
  leaked on 56% of historical queries. "Jod forgot that" and "Jod says it forgot
  that" have to be the same thing.

So `~/.jod/jod.db` ships with FTS5 and no embeddings. If they are ever needed,
`sqlite-vec` brute force measured 100% recall at 19 ms over 30,000 vectors and
holds to roughly 150,000 memories — which is far past where anything else here
breaks first.

## One SQLite file, and every write is `BEGIN IMMEDIATE`

[`research/agent-db-2026`](../research/agent-db-2026/REPORT.md) benchmarked nine
engines with real concurrent OS processes, and the discriminator was not speed.
Under contended read-modify-write, Postgres on its obvious path silently
discarded 757 of 1,600 updates, LanceDB 822, Qdrant 737 — and **every one of
them reported a 0% error rate**. Meanwhile SQLite misconfigured threw
`database is locked` at 58% of calls and lost nothing at all.

That inverts how the results have to be read. A high error rate next to a
correct result means the engine refused work it could not do safely, which is
the good failure. A clean error rate next to lost data means it accepted every
write and discarded some. Safety is not a property of the engine; it is a
property of the primitive you reach for, and engines differ enormously in
whether the *obvious* primitive is the safe one.

SQLite in WAL mode also measured fastest on this workload — ~44,000 appends/s at
8 writer processes against a need of roughly 1,000 — so the usual trade of
correctness against throughput never arose. Three rules come with it, each
earned by a number rather than by taste:

1. **Every write transaction is `BEGIN IMMEDIATE`.** Deferred transactions take
   their write lock late and collide on upgrade: 98% errors versus 0%.
2. **Never hold a write transaction across a model call.** The entire argument
   rests on write transactions costing microseconds; one held open across a
   30-second model call turns SQLite's single writer into an outage for every
   other agent.
3. **Claim contended rows with one guarded `UPDATE`,** never a read then a
   write. Zero rows changed means you lost the race.

The decision expires against measured triggers, not feelings: above ~5,000
events/s, or past ~150,000 vectors, or the moment a second machine needs to
write, this is the wrong answer and Postgres or libSQL is the right one.

## A remote client watches a team; it does not join one

The TUI's `Ctrl-G` panel reads a team's roster and board straight out of SQLite.
A phone cannot do that, so `jod-api` grew two routes for it — `GET /v1/teams`
and `GET /v1/teams/{team}`, both `read` scope, the second returning members and
tasks in one answer because the sheet draws them together and a board from one
moment against a roster from another is a screen that was never true.

Two routes, and deliberately no more. The obvious next ask is `POST` for join,
claim and message, and it is the wrong ask: a *teammate* is an agent on the box
with a process group and a conversation to resume, and the bus exists so those
agents can coordinate. A phone has none of that. Letting it claim a task would
put an owner on the board that no run is behind, which is worse than not being
able to claim — the board's whole value is that every claim names something
actually working.

So the read/write line here is not about danger, it is about what the client
*is*. Watching is the whole of what a phone can honestly do, and the API says so.

## Port the rule, not the keystroke

Three things in the TUI have no meaning on a phone: byte-cursor editing with
`Ctrl-W`, a line-counted scrollback, and a highlighted completion moved with
`Tab` and the arrows. The temptation each time is to reimplement the mechanism
so the parity table can say "yes" — a fake caret, a computed scroll offset, a
selection index driven by nothing.

Every one of those would be worse than the platform's own answer. iOS has a real
caret, a real scroll view, and a finger that goes straight to the row it wants.
What is worth carrying across is the *rule* the mechanism existed to serve —
**new output never yanks a reader back down** — and that survives without a
single line of scroll arithmetic.

The same reading resolves `/exit`, which cannot quit an iOS app. It was never
about quitting; it was about leaving while the work carries on. Stopping the
stream and saying the agent keeps running is not a compromise, it is the command.

A parity table that says "no, same rule" three times is more honest than one
that says "yes" by reimplementing a terminal inside a WebView.

## Nobody is there to ask, so "ask" grants what only reads

`claude -p` has no terminal to prompt on. `PermissionPolicy::Ask` pushed no
flag at all, which meant the CLI fell back to its own default — deny anything
needing approval — and the policy named "ask" behaved as "deny everything".
The symptom was a question as ordinary as the weather coming back as *I need
permission to search the web*, with no prompt anywhere for the user to answer.

Renaming it would have been the cheap fix and the wrong one. The policy is
right; the flag set was empty. `Ask` now passes `--allowedTools` with the tools
that can only read — `Read`, `Grep`, `Glob`, `WebSearch`, `WebFetch` — and
nothing else. Writes, `Bash` and subagents stay refused, which a test asserts
by name so a future addition to that list has to argue for itself.

The ground given up is none. The alternative was never "the user approves the
web search"; it was a silent refusal. Granting the tools that cannot change
anything costs a guarantee nobody held, and buys back the reason to have a
default policy at all — that the safe setting is still a usable one. Anything
that mutates still needs `--permission accept-edits` or `--permission bypass`,
chosen deliberately.

Only the Claude Code adapter changed. OpenCode and AGY have their own flag
surfaces and were not verified against a live binary here.

## The gate died between deciding and acting

macOS ships bash 3.2, where `"${a[@]}"` on an *empty* array is an
unbound-variable error under `set -u`. bash 4+ and zsh expand it to nothing, so
the bug never fires on CI or on the VPS — only on the machine the maintainer
actually works from.

`merge_pr.sh` built `ready_args=()` and left it empty on every run that did not
pass `--repo`. The gate evaluated all its preconditions, printed `ok` for three
checks, `base 0 commit(s) ahead`, `triage auto-merge` — and then crashed on
`gh pr ready`, exiting 1. That is the worst possible failure for this script:
exit 1 means "refused", the charter says obey the exit code, and the transcript
above it says the opposite. A refusal that is really a crash trains whoever
reads it to stop trusting the exit code, which is the only thing making the gate
a gate.

The fix is the `${a[@]+"${a[@]}"}` form `pr_sweep.sh` already used and
documented; `merge_pr.sh` had simply missed one site.

`tests/shell-arrays.test.sh` now scans every `set -u` script for the pattern.
Its rule has two clauses, and the second is not a loophole: an array must be
guarded at each expansion, **or** the script must test `${#a[@]}` first — taking
a length is safe on an empty array, so a script that checks has genuinely proved
the thing the guard would protect. `tdd-loop.sh` does that, and must not be
"fixed": with `CMD` empty the guarded form would reduce `"${CMD[@]}"; rc=$?` to
a bare `; rc=$?`, trading a clear error for a syntax error in a state its own
check already rules out.

What separated the live bug from the four latent ones was not the syntax but
the reachability — `ready_args` was empty on the ordinary path, every day.

## The busy TUI refused work instead of holding it

Two rules made the interface fight the thing it exists for. Enter while an agent
worked printed *"still working — wait for this turn to finish"* and left the
line sitting in a box that stayed blocked until the turn ended; every event from
an agent other than the one on screen was dropped on the floor. Together they
meant the orchestrator's UI could watch exactly one agent, and while it did, the
only thing for its user to do was sit still.

Both are now the opposite. A prompt typed mid-turn is queued and sent when the
turn ends — the sentence was already written, and blocking on it buys nothing
that holding it does not. An agent that is not on screen still reports: its
ending arrives as a notice naming it, how it went and how long it took, because
the whole point of delegating is that nobody was watching.

`Ctrl-B` is the key that makes the difference — the typed line becomes an agent
that never takes the screen. It always resumes `Fresh`. A background job that
silently continued the conversation on screen would inherit context nobody gave
it, and two agents writing into one harness session is not a conversation.

The agents panel became a cursor rather than a list: `⏎` watch, `s` stop, `r`
continue that run's conversation, `a` attach. A panel you can only read makes
you leave the UI to act on what you saw, which is where the state you were
reading goes stale. It is modal while open, so its letters are commands — the
alternative, reserving Ctrl chords for every verb, runs out of keys and out of
memorability at about four.

Taking `↑`/`↓` for recall cost transcript scrolling its most obvious keys, which
moved to `PageUp`/`PageDown`, the mouse and `Ctrl` with an arrow. Worth it:
re-running a prompt with one word changed is constant, and scrolling has three
other ways to ask.

`Ctrl-X` stops the watched run because `Ctrl-C` is quit; without it the only way
to interrupt a runaway agent is to leave the process that is watching it. And
quitting now warns about every running agent, not just the one on screen —
walking out on four background jobs unwarned is the same mistake, four times.

A 250ms tick pays for all of it. A ten-minute run and a hung one are the same
picture unless something moves, and a fleet that only refreshed when the watched
agent finished was showing state minutes old.

## An idle agent is usually a full disk, not a dropped SSH

"The agent goes idle when my SSH connection drops" describes three unrelated
failures that look identical from the far end, and each wants a different fix.

The first is `SIGHUP`. An agent started directly in an SSH shell belongs to that
session's process group, so the connection dying takes the agent with it. tmux
answers that one — and this box already ran everything in tmux, which is the
first clue that the symptom had another cause.

The second is logind reaping the user slice at logout. `KillUserProcesses` is
`no` here, so it was never it. Linger is enabled now anyway, because the
headless path wants a user manager that outlives the session.

The third is the one that genuinely *idles*. A long-running agent holds a
streaming HTTPS connection to the model API; when the network path breaks — NAT
eviction, host migration, an ISP flap — the peer never sends a FIN, so the read
blocks on a socket nobody will ever speak on again. The process is up, the run
is alive, nothing moves. The obvious remedy, lowering
`net.ipv4.tcp_keepalive_time`, does nothing at all here: Node sets
`TCP_KEEPIDLE` per socket and overrides the sysctl. `ss -tanpo` settles it —
the agent's sockets count down from under a minute while the sysctl still reads
two hours. What Node does *not* set is the probe phase, so `tcp_keepalive_intvl`
and `tcp_keepalive_probes` are the only lines with leverage: 9 × 75s became
4 × 15s, and a dead peer is noticed in about a minute instead of eleven.

None of which was the real problem. The root filesystem was **100% full** — 53MB
free of 45GB — and 29GB of that was Rust `target/` directories, one per
abandoned agent worktree. An agent that cannot write its transcript, its shell
snapshot or a SQLite commit does not crash and does not report; it stalls, which
is indistinguishable from thinking. Check `df` before tuning anything.

That accumulation is structural rather than unlucky. Every background job takes
a worktree, every worktree cargo-builds its own 2–5GB `target/`, and removing it
afterwards is nobody's job. `target/` is gitignored and regenerable, so
reclaiming it costs a rebuild and nothing else — but the sweep has to recur, or
`CARGO_TARGET_DIR` has to point at one shared directory, which trades the disk
for cargo's lock serialising builds across concurrent agents. The sweep is the
cheaper trade while agents run in parallel; it is also the one that gets
forgotten, so it belongs in the box's notes, not in somebody's memory.

## Jod owns the transcript now

This reverses a claim made in [`jod-system.md`](jod-system.md): *"Jod needs no
memory of the transcript: the harness owns it."*

That was right while a conversation was a line you could only continue. Session
resume was normalised behind one `Resume` field, each harness spelled it
differently, and the seam hid it. Nothing above the seam had to know what a turn
contained.

It stops being right the moment you want to **fork, revert, or move a thread to
a different harness**. A session id issued by Claude Code means nothing to
OpenCode. Probing the three binaries directly:

| | Claude Code | OpenCode | AGY |
|---|---|---|---|
| fork | `--fork-session` | `--fork` | **none** |
| assign a session id | `--session-id <uuid>` | none | none |
| export a transcript | — | `opencode export` | none |
| accept one back | `--input-format stream-json` | `opencode import` | none |

So two of the three can fork themselves and Jod should let them. **No two of
them can hand a thread to each other**, and AGY can do none of it. Cross-harness
handoff has no owner unless Jod is the owner.

The shape is the one ChatGPT, LangGraph and git converged on and the harnesses
did not: **one DAG with a moving head pointer**. Claude Code and OpenCode both
fork by copying a prefix into a new container with no parent edge — verified by
forking a real session and reading both files. That is cheap to read, and it
makes branch topology recoverable only by intersecting message ids, so a
"‹ 2/3 ›" sibling pager cannot be drawn from it at all. `leafUuid`,
`current_node` and `HEAD` are the same construct; Jod keeps one.

Two consequences worth stating. The event stream is **not** sufficient as a
transcript: `ToolResult` carried only a truncated `summary`, which is enough to
watch a run and not enough to replay one. And revert is **non-destructive** —
the head moves, the abandoned tail stays reachable — because git's reflog and
OpenCode's `unrevert` both concluded that the recovery window is worth more than
the tidiness.

## The scheduler's claim is one statement, and the lease is not enough

Sixteen processes racing for four due schedules, measured: a read-then-write
claim handed the **same** schedule to two winners **41.26%** of the time. The
guarded single-statement claim under `BEGIN IMMEDIATE` produced **0 duplicates
in 5,408 claims**. This is the same result the database benchmark found for
contended updates, arriving a second time in a different costume.

The part that was *not* obvious is what happens when a claimant dies. A lease
alone looks sufficient — it expires, someone else takes over, the schedule keeps
running. But the next claimant overwrites the lease, and the original claim then
exists nowhere: **52 of 255 claims, one in five, were accounted for in no record
at all.** Whoever displaces a dead lease is the last process that can still see
it existed, so the claim writes the abandonment down *before* taking it. That
brought it to 0 of 270.

Every firing decision gets a row, including the ones where nothing ran. "It
never fired" and "it fired and was skipped" are different bugs with the same
symptom, and a skip nobody recorded is a silent failure.

Jobs are rows rather than a JSON file. Hermes keeps its cron in
`~/.hermes/cron/jobs.json` behind an advisory `flock` and its own source carries
a note about a root-owned copy that failed every tick for fourteen hours. Jod
already had the store that makes this a non-question.

## Jitter sounded prudent and measured worse

Spreading fires to avoid a thundering herd is the obvious move, and it is the
one addition in ten graded scheduler iterations that made things *worse*: a
300 s spread against a 150 s grace window **lost 34 of 72 fires** outright,
because jitter pushed them past the point where they still counted as that fire,
and operator predictability fell from 5 to 3.

It ships defaulting to zero, and a jitter wider than the grace window is
**refused at the boundary** rather than silently losing fires.

The general rule this is an instance of: a safety feature that has not been
measured against the failure it claims to prevent is a guess, and guesses in a
scheduler are paid at 3am.

## A goal that stops moving has to say so

The characteristic failure of an autonomous loop is not crashing. It is
completing iterations for ever while nothing changes — and from outside, a goal
making no progress looks exactly like a goal working hard.

So progress is counted rather than assumed. Every iteration reports whether it
moved; enough that did not, and the goal **stalls itself** instead of running
for weeks. Alongside it sit the two bounds that need no judgement: an iteration
cap and a spend cap, both re-checked immediately *after* an iteration is
recorded, so a goal that has just spent the last of its budget stops there
rather than spending more proving it has run out.

A goal's progress lives in the memory layer rather than in its own columns — the
brief as a prospective fact superseded each iteration, so bitemporal validity
can answer what it thought it was doing last month, and what happened as
episodic facts in a `goal:<id>` scope, because an hourly loop writes far more
than a person does and scope is a hard filter. Only the counters the claim reads
on every tick stay as columns: a claim must not depend on a text index.

## The graph is an index, and the extension was not worth buying

The request was for "the SQLite extension for graph". The benchmark says there
is nothing worth buying: no extension is simultaneously maintained in 2026,
permissively licensed, **and** statically linkable into a single binary — and
Jod ships as one binary onto a VPS. Plain tables plus a recursive CTE answer a
three-hop walk over a million edges fast enough that an extension would buy
nothing.

Two findings shaped the code rather than merely justifying it.

**A recursive CTE has no statistics, and SQLite guesses the join order wrong.**
Written the obvious way it made `relations` the outer loop matched on `scope`
alone — which selects every row — and scanned the frontier inside it, a cross
product per step, using the in-edge index for both directions so the out-edge
index was never touched. `CROSS JOIN` pins the order: **903 ms → 14 ms**, same
schema, same indexes. It fails *silently*, staying correct while going
quadratic, which is the worst way for a performance bug to behave.

**The headline number described a query we do not run.** 0.37 ms for three hops
at a million edges is the *directed* traversal. "What is related to this" is
undirected, needs two recursive terms, and measures 92 ms at 100k. Both are
correct; only one is ours. The conclusion survived the correction, but it had to
be made on the real figure.

The graph stays derived: it rebuilds from `facts` alone, and `ON DELETE CASCADE`
carries `forget` through to the edges — otherwise a forgotten fact stays
walkable, and "Jod forgot that" stops meaning "Jod says it forgot that".

## Origin was stored for months and never consulted

`facts` has carried an `origin` column since the first migration — owner, agent,
untrusted, system — deliberately outside the fact text so ingested content
cannot forge its own trust level. Recall never looked at it. A page Jod merely
*fetched* answered exactly as readily as something Reljod said, and a
hand-labelled corpus put the poisoned fact in the answer set on 10% of queries.

The lesson is not "add a WHERE clause". It is that **a trust boundary nothing
enforces is decoration**, and the way to find out is to measure the shipped
behaviour rather than re-read the design. The column, the doc comment and the
migration comment all described a control that did not exist.

Untrusted material is now excluded by default from answers *and* from seeding a
graph expansion — a page that cannot answer directly must not be able to steer
which part of the graph gets walked. It is excluded, not deleted: "what did that
page claim" stays answerable through an explicit call, where the decision to
believe it is visible at the call site.

## MCP is the seam, and it is what Jod *is*

The charter says `jod-core` has no model client, no prompt templates and no
tools. That was read too narrowly for a while — as though it meant Jod could
only ever ask an agent a question and parse an answer out of its prose.

The harnesses already have a tool mechanism, and all Jod had to do was speak it.
Verified by running them: Claude Code takes `--mcp-config <files...>` and
`--strict-mcp-config`; OpenCode has `opencode mcp add`.

So **Jod is an MCP server**. `jod mcp` exposes what Jod can do — list what is
running, delegate, continue an agent, stop one, schedule, set a goal, remember,
recall, walk the memory graph — and a harness pointed at it thinks *and* acts in
one loop.

This does not weaken the charter, it is the sharpest expression of it yet:

- **The harness supplies judgement.** Whether "fix the CI failure" belongs to the
  agent already looking at CI is a call Jod is not equipped to make and should
  not try to.
- **Jod supplies effects.** Spawning a process group, claiming a schedule,
  superseding a fact. Things with consequences, done under rules the agent
  cannot argue with.

Neither has to become the other, which is what the rule was protecting.

**It belongs on `SpawnRequest`, not on the orchestrator.** Putting it on one
special conversation would have made it a feature; on the spawn it is the seam,
and a scheduled run, a goal iteration, a webhook-triggered agent and a teammate
all get the same tools as the main chat. An agent that can see what else is
running can hand work sideways instead of duplicating it — which is as much of
agent-to-agent as Jod needs to have an opinion about.

**Access is a capability set, not a boolean.** "Can see what is running" and "can
start another agent" are different amounts of trust, and an agent triggered by a
stranger's pull request should get the first and not the second. Three levels:
read-only, delegate, orchestrate. The line that matters is between delegating and
orchestrating: delegating spends money now and is visible, while a schedule
spends it at 2am whether or not anyone is watching, and a goal spends it until
something stops it.

Default is read-only and it is opt-in, because the failure mode of getting this
wrong is an agent that can give itself more agents.

### What this replaces

An earlier design had the orchestrator ask a harness for a JSON decision and
parse it — propose-and-dispose. It works, and it survives as the fallback for
any harness with no MCP support, but it is a weaker version of the same idea: it
allows exactly one decision per turn, cannot ask a follow-up question before
deciding, and turns every capability into a new line in a prompt and a new arm
in a parser. With tools, adding a capability is adding a tool.

## A grant that depends on the permission mode is not a grant

`SpawnRequest::tools` is the seam the whole system turns on, and it took four
runs of the main chat to get one tool call through it. Each failure looked like
a different bug and all four were the same shape: a decision recorded in one
place and consulted in another.

The last two are the instructive pair.

**Plan mode refuses the tools it was given.** `PermissionPolicy::Ask` maps to
`--permission-mode plan`, which is correct — it is what actually confines a run
that must not change anything, as opposed to an allowlist that grants without
denying. But the orchestrator's entire job is calling tools that change
something, and plan mode does not distinguish "writes to the filesystem" from
"writes to Jod's own schedule table". Given `Ask`, the orchestrator dutifully
called `list_agents`, `schedule_list` and `recall`, reached for `ExitPlanMode`,
could not find it, and wrote a plan file instead of arming the schedule it had
been asked for. It looked like a model that would not commit. It was a mode that
would not let it.

**So the confinement axis and the grant axis are separate, and the code has to
say so.** The permission mode bounds what a run may do to the *machine*.
`ToolAccess` bounds what it may do to *Jod*. Neither substitutes for the other,
and an orchestrator wants little of the first and a lot of the second.

Which exposed the fourth bug immediately: the `mcp__jod` entry in `--allowedTools`
had been written inside the `Ask` arm of the permission match. Moving off plan
mode therefore revoked every Jod tool, silently, and the run came back with four
consecutive *"requested permissions to use `mcp__jod__schedule_create`, but you
haven't granted it yet"*. The grant now hangs off `req.tools`, which is the thing
that actually decides whether a run has Jod tools.

The test that missed it is more interesting than the bug. It asserted the grant
appears — under `Ask`, the only mode it ever passed. A test that fixes one value
of the variable your bug lives in will pass forever. It now loops over every
permission mode, because "the grant survives a mode change" is the property, and
the single-mode version was asserting a coincidence.

### The general shape

All four failures were components that were complete, tested, and connected to
nothing: `tools` reached no command line; the allowlist denied what the config
granted; `read_only` on one side met `read-only` on the other; the grant lived
in the wrong branch. Unit tests were green throughout, and each failure produced
a *plausible* symptom — an agent saying it has no tools reads like a missing
feature, not like three broken layers.

Nothing here was found by a unit test. All of it was found by running `jod main`
and reading what the run actually did.

## One byte made a whole module invisible to every audit

`core/src/webhook.rs` contained a literal NUL inside a doc comment — in a line
explaining, of all things, how control bytes are escaped. It is valid UTF-8 and
rustc accepted it, so the file compiled and its 39 tests passed for as long as it
had existed.

`grep` classifies a file containing NUL as binary and silently skips it. Not a
warning, not a partial result — no output and exit 0. So for that whole module:

- every `grep -rn` in this branch returned nothing, including the ones that
  concluded a function had no callers;
- `wiring.py`, the audit that measured "44 of 278 core pub fns have no
  production caller", never saw 1,234 lines of it;
- `match_rules` appeared to be called from `api/` and defined nowhere, which is
  what finally exposed it.

The lesson is not about NUL bytes. It is that a *silent* zero result and a true
zero result are indistinguishable, and every conclusion of the form "nothing
calls this" had been resting on that ambiguity. The audit that reported the
sharpest finding of the branch was running with one file missing and had no way
to say so.

Where a tool can decline to answer, it must be made to say which inputs it
skipped, or its zeroes cannot be read as evidence.

### The same shape again, with the tool skipping everything

Checking whether a change had introduced any `rustfmt` diffs, the working copy
of `cli/src/tui/mod.rs` reported six and the question was how many of those
already existed. So `HEAD`'s copy of the file was extracted to a scratch path
and `rustfmt --check` run against it. It printed no diffs. That was read as a
baseline of zero, and therefore as "all six are mine".

It had not run at all. A standalone `mod.rs` cannot resolve the children it
declares, so `rustfmt` had failed —

```text
Error writing files: failed to resolve mod `app`: …/app.rs does not exist
```

— and exited non-zero having formatted nothing. Every one of the six hunks was
in fact present verbatim in `HEAD`; none of them were new. Checking that, by
grepping `HEAD`'s content for the exact lines, is what settled it.

The NUL byte above is a tool that silently skipped *one input*. This is a tool
that silently skipped *the entire job*, and both produce the same clean zero.
The habit that catches it is small: **check that the check happened, not just
what it said.** An empty result and a failed run are the same text, so a tool's
exit code is part of its answer and a zero is only evidence once you know it was
reached. The same technique was sound for the sibling files in that change —
they declare no child modules — which is the other half of the trap: it works
often enough to be trusted the once it does not.

## Asking and planning were the same mode, so nothing ever acted

`PermissionPolicy` had three levels, and the least of them was `Ask`. It mapped
to Claude Code's `--permission-mode plan`, for a reason that was sound when it
was written: under `-p` there is nobody to answer a prompt, so "ask" would
otherwise mean "deny", and a bare `claude -p` refused even to search the web.
Plan mode grants reading and refuses every write path, which is a better answer
than silent denial.

The trouble is that `Ask` was also `#[default]`. So the default for every spawn
Jod made — the TUI's chat box, a schedule at 2am, a goal iteration, a webhook
run — was *plan mode*. Jod could describe work in any amount of detail and could
not do any of it. That reads as a model being unhelpful, not as a flag, which is
why it survived so long.

Four levels now, and the split is the fix:

| | claude | opencode | agy |
|---|---|---|---|
| `Plan` | `--permission-mode plan` | — | `--mode plan` |
| `Ask` | `--permission-mode manual` | — | — (its default is ask) |
| `AcceptEdits` | `--permission-mode acceptEdits` | — | `--mode accept-edits` |
| `Bypass` ("auto") | `--dangerously-skip-permissions` | `--auto` | `--dangerously-skip-permissions` |

Two things worth keeping:

**The flag names were read off the binaries, not assumed.** `claude --help`
prints exactly six modes; an earlier attempt at this seam had designed around
`--permission-prompt-tool`, which this build does not have. A mode name a
harness does not recognise is not a compile error, not a spawn error, and not
visible until an agent quietly does the wrong amount — so `claude.rs` now has a
test pinning every mode it emits against that list of six.

**OpenCode cannot express three of the four**, and says so rather than
pretending. It has one auto-approve switch and no mode flag at all, so `Plan`,
`Ask` and `AcceptEdits` all collapse to "leave `--auto` off". Emitting a
`--mode plan` OpenCode ignores would have looked like a working plan mode right
up until something got written.

The default is now `Bypass`. That is what Jod is for — a mode that stops to ask
an empty room is a mode that never finishes. The one place that does *not*
follow the new default is `jod-api`'s `SpawnBody`, which pins `Ask`
explicitly: that field is filled in by whatever is on the other end of a socket,
and a caller who omits it has not asked for anything. The dangerous setting
should not be one forgotten JSON key away on the only surface whose callers Jod
does not control.

### The mode belongs to the conversation, not the process

It was set once, at `jod tui` launch, and could never be changed — you quit the
program to change your mind. But it was never a process property to begin with:
Jod respawns the harness once per turn against a resumed session, so
`--permission-mode` is decided afresh at every spawn. The only place the answer
can live and survive a restart is the row the spawn is for.

Two clocks, and the screen has to be honest about both. Jod's own MCP tools are
checked per call and change immediately; the harness's native tools are bounded
by the flag chosen when its process started, so a turn already in flight keeps
the mode it began with. There is no way to tell a running harness otherwise.

The launch flag survives as a *ceiling* rather than a default. `jod tui
--permission plan` is somebody saying "not on this machine, not today", and a
Tab press inside the program must not be able to talk them out of it. Downward
is always allowed: asking for less needs no permission.

## A chat you are watching may schedule; one that wandered off may not

The TUI passed `tools: None` on every spawn from the chat box, with a comment
explaining that "an agent started from the chat box is doing a task, not
orchestrating" — giving it Jod's verbs would let a prompt typed in a hurry
create schedules and spend money every night.

That reasoning is right about delegations and wrong about turns, and the two
were reaching it through one function. This codebase already states the rule:
the main chat gets the full set because it is "you, present, watching". A turn
you just typed into the TUI, whose output is filling your screen, meets that
condition by definition. Withholding the grant there did not make anything
safer; it made "schedule this for me" a request Jod could acknowledge and had no
verb to carry out.

So `tools` became a parameter, and the two call sites are named for the only
thing that separates them — whether anybody is looking. `WATCHED` gets
`Orchestrate`; `DELEGATED` gets `ToolAccess::unattended()`, deferring to the
codebase's own answer rather than a copy of it. A background agent that can
create background agents has no bound at all, and it multiplies while nobody is
reading.

## The phone types into the main chat, not into a chat of its own

Every Telegram message used to spawn with `RunConversation::New`. The bridge
resumed the *harness* session out of `channel_sessions`, so a phone thread felt
continuous — but on Jod's side each message minted a fresh one-turn
conversation, and the main chat, the conversation Jod is actually for, heard
none of it. Ask Jod something from the sofa and there was no trace of it at the
desk; ask at the desk and the phone had never heard of it. `jod conv ls` showed
a pile of one-turn rows and no phone conversation at all, and the
`channel_sessions.conversation_id` column added to record where a chat's turns
went was never written by anything.

A second desk is the wrong model. There is one person here. The main chat is
"the desk you sit at", and which room you are standing in when you say something
is not a reason for Jod to file it somewhere else. So the bridge now calls the
same `hand_to_orchestrator` that `jod main` and the TUI's `/main` call, and that
function moved into `core` for the purpose — the bridge lives there, and a
bridge that could not reach it would have grown its own copy of the three
decisions it makes.

**This widens what a phone message can do, and that is the part worth stating.**
A phone message used to run with `tools: None` and in `Ask` — plan mode. It
cannot stay that way, because the main chat's whole job is calling Jod's own
verbs, and plan mode refuses them; a session where every other turn could not
see those tools is a session whose transcript references tools that are gone.
The discriminator the codebase already settled on is *whether anybody is
looking* — and a phone message is Reljod, waiting, with a progress bubble on his
screen. That meets the condition the TUI's chat box meets. The gate is the
allowlist, which is default-deny and always was.

Three consequences are accepted rather than worked around. A group chat's
allowlisted members write into Reljod's main chat, because the allowlist is his
own ids and a shared desk is the point. `/new` from a phone clears the desk
*everywhere* — so it says so, and it drops only the harness session id, never
the transcript, because Jod owns the transcript and a reset that destroyed the
record would leave the main chat unauditable from whichever surface reset it
last.

**And concurrent turns on the main chat are still unserialized.** Nothing
queues them: two `jod main` invocations seconds apart already resumed the same
session from two processes, and the bridge now joins that — it spawns each
message in its own task on purpose, so the poll loop keeps acknowledging while a
run takes half an hour. The exposure is wider than it was, because a phone makes
it easy to send a second message before the first has answered. It degrades
rather than corrupts: both runs are bound to the conversation, so Jod's own
transcript records both, and it is the harness's session file that may lose a
turn. A per-conversation queue is the fix and is not written yet; it is a real
decision with a cost — a long run would hold up the next message — and it should
be made deliberately rather than smuggled in with the routing change.

## Six guards were green, and none of them were guarding

Every one of these passed. Every one would have kept passing through the change
it existed to catch.

- **A budget test that measured its own arithmetic.** `keys::keybar` budgets the
  verbs; the test rendered a bar and checked it fitted. Self-consistent by
  construction, and green however wrong the padding constant shared with
  `ui::two_ends` was — until the one screen whose verbs ended exactly on the
  boundary, which would have lost its entire left half rather than one verb.
- **A collision test that could only fail from its own fixture.** "Every row the
  sweep claims is `telegram`" fires only if somebody adds a `cli` row *to that
  test*. Writing `cli` rows in production would not have tripped it. A comment
  wearing a test's clothes: found, not finding.
- **A helper whose doc claimed a job it did not do.** `Verdict::is_trouble`
  included `Owed` and said it was "the line the passive marker would be drawn
  from". Anyone who believed the doc would have drawn a glyph on every Telegram
  run for the seconds a reply is in flight, and a routine marker is one nobody
  sees.
- **A test that enumerated the wrong answers instead of naming the right one.**
  `a_run_that_owed_nobody_anything_wears_no_mark` asserted the row carried
  neither `⊘` nor `♻`. Swapping the predicate for a wrong one *passed*, because
  the wrong predicate draws `○` — a third glyph the list had never heard of.
- **A green suite reported as a statement about a window it never measured.**
  A call site was added in one edit and the function it called in the next; in
  between, the file did not compile, and somebody else's `cargo test` caught
  exactly that window. The author ran the suite *after* both edits, saw green,
  and said "the tree was never red" — a claim about an interval backed by a
  reading taken at one later instant.
- **An assertion pointed at the wrong half of the screen.**
  `text_that_was_shortened_says_that_it_was` checked `!row.contains(name)` — but
  past ninety columns the detail pane shares the rendered line and prints the
  name in full, so it was reading the *other* pane's copy and would have passed
  through any amount of silent clipping in the pane under test.

The shape is one thing, and it is not carelessness — all six were written
deliberately, by people who had just been arguing about correctness:

**A guard has to name the property, not enumerate the ways of violating it.**

"The gutter is blank" is a property. "It is not `⊘` and not `♻`" is a list, and
a list goes stale the moment a third possibility exists — silently, because the
test still passes. Likewise "the exit hint is present" is a property; "the bar
is under N columns" is arithmetic the code already did. Likewise "a foreign row
comes back untouched" is a property; "no `cli` row appears" is a statement about
a fixture.

The habit that found all four is cheap and mechanical: **break the thing on
purpose and watch the test fail.** Not once at the end — at the moment you write
the assertion, before you believe it. Report both directions. Three of the four
above were caught by someone swapping in a deliberately wrong implementation to
see what happened, and being surprised.

The fifth is the one to expect. The first four are clever mistakes — a
tautology, a fixture-bound assertion, a doc that overreached, a list that went
stale. The fifth is none of those: the property was named correctly and the
instrument was simply aimed past the thing under test, because two panes render
onto one line and `contains` does not care which. It needed no ingenuity to
write and it will recur, in any suite where the fixture is larger than the
subject. A sibling test in the same file had already hit it, which is the tell
that the shape is structural rather than a slip.

So: **assert against the smallest region that can hold the answer.** A whole
rendered screen is not a unit; it is several, and `contains` will find your
string in any of them.

The sixth is the one that does not look like a testing mistake at all — it
looks like reporting a result, which is why it reached two people in one
exchange. One read a compiler *note* about what else was in scope and inferred
what the author had meant to call; the other read a green suite and inferred
what had been true five minutes earlier. Both outputs were accurate. Both
conclusions were false, and neither was checkable from the thing that had been
read.

In a shared checkout, **"green" is a statement about the instant you ran it.**
Asserting anything about a window means measuring the window — and if you cannot,
say when you looked rather than what has been true.

A test you have never seen fail is a test you have never seen.

## A branch name is a request; the tag list is the fact

Releases used to be one manual dispatch that picked `patch`/`minor`/`major`.
That works and says nothing: the intended version existed only in whoever's head
ran it, and the first written record of it was the tag itself — after the tests,
the e2e check, and the push.

Cutting `release/v1.2.0` says it out loud, in a place that can be reviewed
before anything is published. So the branch name decides the version. The one
case where it cannot be obeyed literally is the interesting one: a branch naming
the version that is *already* the latest tag. There are exactly two things to do
about it — resolve it, or let `git tag` fail at the end of a green run — and
failing there is the worst possible moment, because everything expensive has
already happened and nothing has been recorded. So it patch-bumps, and the PR
says it did.

A branch naming a version *below* the latest tag is refused rather than bumped.
It looks like the same problem and isn't: `install.sh` resolves the **highest**
tag, so cutting `v0.0.9` while `v0.1.0` exists publishes a release that installs
on nobody's machine. A stale branch and a typo both land here, and neither wants
a version chosen for it.

The split that matters is which half runs on its own. **Preparing is automatic
because it is invisible and reversible** — delete the branch and it never
happened. **Publishing is manual because it is neither.** A tag is what
`install.sh` and `jod update` serve to every installed machine, so it waits for
a person, in an environment that can require a second one.

And none of that logic lives in the YAML. A workflow that publishes a tag cannot
be tested by running it — dispatching it *is* the release — so the resolution is
a sourced script with an offline suite, and the workflow's only job is to call
it.

## The model list comes from the harness, except where it cannot

`/model <name>` is only usable by someone who already knows the name, and the
three harnesses do not agree on any of them: `opus`, `opencode/claude-opus-5`
and `claude-opus-4-6-thinking` are one model spelled three ways. A typo is not
rejected at the prompt — it is forwarded to `--model`, which fails the whole
turn. So the completion list has to come from the harness that will be asked.

Two of them will say: `opencode models` and `agy models` both print one model
per line, and `core/src/harness/models.rs` parses each format. Claude Code will
not — `claude models` is read as a *prompt* and opens a session — so its list is
a static catalogue in the same module. Hence a module with both a parser and a
hardcoded list, which looks like indecision and is not.

The catalogue leads with aliases (`opus`, `sonnet`) precisely because they
follow the newest model of each family without this file being edited; the
pinned ids below them are the thing an alias cannot do. Typing filters on any
part of an id rather than its prefix, because the half of
`opencode/claude-sonnet-5` worth typing is the half a prefix cannot reach.

The list is an aid, not a gate — any name is still forwarded, a missing or slow
harness yields an empty list, and the popup simply does not appear. A completion
that reports its own failure would be worse than one that stays quiet.

## An agent definition that pins a model overrides the session that spawned it

Subagent frontmatter accepts `model:`, and all five agents here set it to
`sonnet`. Each was a sensible local choice on the day it was written. Together
they made the session model decorative: a session on Opus spawned every
reviewer, investigator and author on Sonnet, and no setting the caller could
reach changed that.

An agent definition describes a *role* — its lens, its tools, its output
contract. Which model plays the role is the caller's decision, made with the
budget and the deadline in view, and it is the one setting the caller can
already express. So no agent in `agents/` or `.claude/agents/` sets `model:`,
and a subagent inherits the session's. Pin one only when the role genuinely
cannot be done at another tier, and say why in the file.

## A model belongs to a harness, so nothing may hold one without the other

`/model` has always dropped the model when `/harness` moved, because `opus` is a
Claude Code alias, OpenCode wants `opencode/claude-opus-5` and AGY wants
`claude-opus-4-6-thinking` — carrying either name across makes the *next* turn
fail on a `--model` the new harness has never heard of, and the switch looks like
it simply did not work. `default.model` is that same name, stored for longer, so
it inherits the same rule rather than a gentler one: setting `default.harness`
clears it, and a stored model is withheld at launch when `-H` names a harness
other than the one it was chosen beside.

The alternative — one key per harness, `default.model.opencode` — keeps three
answers alive and looks more capable. It is worse. Two of the three are always
stale, nothing ever tells you which one is in force, and the preference stops
being a decision you can read off `/config` in one line. Dropping the model is
lossy and *legible*: the drop is announced with the name it dropped, and the
picker for the new harness opens in the same keystroke.

That picker is the second half. The valid names live in the harness and reaching
them means running a subprocess, so the completion popup is the only thing that
holds the list — which is why the offer is a prefilled `/model ` in the input box
rather than a new overlay. The popup already opens on whatever is typed there. It
is withheld when the box is not empty, and after `/config harness` it is withheld
unless the chosen harness is the one this session is on, because the loaded list
belongs to `app.harness` and offering Claude Code's names for an OpenCode
preference is the exact failure the live list was built to end.

## Two MCP configs, because a run Jod spawns and a session you start are not the same client

`mcp_config.rs` had been read as "the MCP wiring", and it is only half of it. It
writes `~/.jod/mcp/<access>.json` and hands it to a harness on the command line,
which covers exactly the runs Jod launches. The `claude` typed in a repo is
launched by nobody, reads the *user's* config, and therefore held none of Jod's
tools.

That gap does not present as a gap. Asked to schedule something, such a session
answers that there is no tool for it — indistinguishable, from the chair, from a
scheduler that was never built. Jod had `schedule_create`, `schedule_list`,
`schedule_pause` and `schedule_run_now` behind a passing suite for weeks while
the answer to "can you schedule this?" was no.

So `mcp_install.rs` writes the durable half: one entry in each harness's own
user-level config — `~/.claude.json`, `~/.config/opencode/opencode.json*`,
`~/.gemini/config/mcp_config.json`. Three paths and two shapes, because OpenCode
takes one `command` array where the others take `command` plus `args`; the wrong
shape parses, loads, and yields no tools.

Three things follow from these being *the user's* files rather than Jod's:

- **It merges, never writes wholesale.** Every other key and every other server
  survives. `~/.claude.json` also holds session state, and an OpenCode config
  holds model choices.
- **A file it cannot parse is left alone and reported.** Comments are legal in
  an `opencode.jsonc` and this parser rejects them; the failure mode of guessing
  is destroying configuration Jod does not own. `--dry-run` prints the entry to
  paste by hand.
- **It writes through a rename**, so an interrupted install leaves the old
  config or the new one, never half of either.

It runs from the `jod daemon` entrypoint, not from `install.sh` and not from
`Daemon::run` — and both exclusions were paid for. `install.sh` puts binaries
on `PATH`; whether a harness config should point at them is a question about
this machine's agents, not about a file having been copied, and an installer
that edited three configs someone else owns would be doing it on every
re-run. `Daemon::run` is library
code with tests, and hanging registration off it meant `cargo test` wrote three
real harness configs on the developer's own machine, each naming a
`target/debug/deps/jod_core-<hash>` that the next build deleted. An effect on
files this program does not own belongs at the entrypoint of a long-running
binary, where no test goes; `is_a_test_binary` is the guard that no longer
depends on remembering that.

The daemon restart is the right moment for the rest of it: it follows every
build, and it is exactly when `current_exe()` can have changed. Re-registering
then is what keeps a config from naming a binary that has since moved — the same
reason `mcp_config.rs` refuses to name `jod` on the `PATH`. Idempotent, silent
when there is nothing to do, and never fatal: a harness config Jod cannot parse
is not a reason for the daemon that fires every schedule to refuse to start.

Interactive sessions get `orchestrate`, the full set, because someone opened
them and is watching. This cannot widen an unattended run — those are pinned to
read-only where they are spawned, and
[a chat you are watching may schedule; one that wandered off may not](#a-chat-you-are-watching-may-schedule-one-that-wandered-off-may-not).


## The main chat was somewhere you could send to, never somewhere you could be

One conversation is pinned, titled `main`, and never ends. Until now nothing
could open it. `jod main "…"` sent to it; `jod main` with no argument printed
twenty exchanges and exited; `/main <instruction>` in the TUI handed an
instruction over and left the chat box where it was. `jod conv` offered
ls/show/fork/revert/goto and no `open`, and the only code path that ever bound
the chat box to an existing conversation was a harness handoff. The record
outlived every process and no process would let you sit in it.

Attaching was not a substitute. The chat is **one run per instruction**, resumed
through `Store::resume_for`, so every run it has ever had has already exited —
`tmux attach` lands you in a finished session.

So the pinned conversation is a **destination**: the fleet's first row, `⏎` to
enter, `/main` with no argument as the keyboard route, `/new` to leave. Entering
binds `Thread::conversation` and replays `live_window` — what the harness would
actually be sent, so a compacted message is on disk and deliberately not on
screen. Inside it a typed line goes to the orchestrator, which is what being in
it means; everywhere else the chat box is unchanged.

Three details are load-bearing and none is cosmetic.

**`in_main` is derived, never remembered.** A flag set on entry would be wrong
the moment `/harness` ran: `switch_harness` mints a conversation and the pin
moves to it, so the flag would point at the thread that was handed away. It is
a store lookup against `pinned_conversation` every time it is asked.

**The pin follows a harness switch.** `main_conversation` is get-or-create on
`pinned = 1`. A pin left on the conversation a switch just compacted away would
send the next instruction back to a chat nobody can reach, with the handoff
summary stranded in it. Cleared and re-set in the switch's own transaction,
because the partial unique index permits exactly one pinned row. That is also
why `hand_to_orchestrator` gained a `carried` parameter — the target harness has
no session for the thread, so the summary has to travel in the framing.

**The fleet row stands for the conversation, not for a run.** It is outside the
sort and outside the filter — an "always on top" a search term can remove is a
row you have to remember how to get back to — and it collapses every `main` run
into itself. One instruction is one run, so within a day the list was mostly
identical `main` rows burying the delegated work the list exists for, and not
one of them was the chat. The run verbs (`s`, `a`, `r`) say why they do not
apply rather than doing nothing, because a key that silently no-ops is how a
footer stops being believed. The cursor still starts on the first agent:
managing the work is what opening the fleet means.

Nothing deletes a conversation — there is no such verb in the store, and the
fleet is not an editable list, so `x` is not offered on it. The main chat's
permanence rests on that plus get-or-create: if it were ever gone, the next
thing to ask for it would make it again.

## Alive is not working, and only one of them was ever checked

`proc::group_alive` asks the kernel whether a process group exists. It never
lies and it is not enough: a harness blocked on a socket that will never answer,
a tool waiting on input that cannot arrive, a model call retrying for ever — all
of them pass it, indefinitely.

That gap had a specific consequence rather than a theoretical one. `tick_goals`
settles the previous iteration before starting the next, and it settles it by
reading the run's status. A wedged run's status is `running` and *stays*
`running`, because the only process that ever writes a terminal status is the
supervisor watching a harness that is never going to exit. So the goal waits.
Not for an iteration, not for a day: for ever, still listed as `running`, with
nothing anywhere recording that anything was wrong.

So a heartbeat asks the second question too — has this run's high-water event
`seq` moved since the last sweep — and a run has to pass both. The window is
twenty minutes and deliberately generous, because the two errors are not
symmetric: killing a merely-slow run destroys work somebody waited for, while
noticing a wedged one twenty minutes late costs twenty minutes of an idle
process.

**The sweep runs before schedules and goals, and moving it is a bug.** Reaping
first is what lets the same tick that discovers a stall also let the goal move
on. Reaping afterwards costs an extra tick in the ordinary case and, in the case
this exists for, never resolves at all.

**A ceiling beats progress, which looks backwards until you name the failure.**
A run stuck in a retry loop is *busy* — it emits events for ever. A ceiling that
yielded to progress would therefore never fire, against exactly the run it
exists to stop. So `Expired` is checked before `Beating`.

**No claim and no lease, unlike schedules and goals.** Those guard a *spawn*:
two processes acting on one schedule start two harnesses and cost real money. A
sweep starts nothing, and its worst case under two daemons is signalling the
same group twice, which is idempotent. A contended write per tick to prevent
that would buy nothing.

**Retiring deletes the row.** Cleanup was the requirement and a table of
tombstones is not cleanup. Deletion also makes the crash path self-healing: if a
sweep dies between stopping a run and tidying up, the row is still live, and the
next sweep re-decides from scratch — the group is gone, so it reads as
`Vanished`, the status is corrected, and the row goes. A state column would have
had to be crash-correct instead, and would only ever be read by the code that
wrote it. Deletion on *run* deletion is the foreign key's job, not code's, so
parts of the system that have never heard of heartbeats cannot leak one.

The reason a heartbeat retired goes to the memory layer, **not** to the run's
event stream. `events` is `UNIQUE(run_id, seq)` and a duplicate is silently
ignored, while the supervisor allocates `seq` from a counter held in its own
memory — so a watchdog writing `last_seq + 1` would race the one process that
owns that sequence, and losing costs either the explanation or the supervisor's
`Finished`, invisibly. For a stalled run those writes are near-simultaneous by
construction. A fact has no such contention, and for a goal it lands in the
scope the *next iteration's prompt is built from*, so a stall is not merely
recorded — it is told to whatever runs next.

## Reading a web page is not one of Jod's verbs

`ToolAccess` bounds what an agent may do *to Jod*: delegate, schedule, spend
money, write memory. Browsing touches none of that, so the browser MCP is
offered at every level — including to a run granted nothing at all, which used
to mean no MCP config was written whatsoever.

The alternative was worse in a way that only shows up in practice: `jod run`
passes `tools: None` on purpose, so gating the browser behind an access level
would have meant ordinary delegation never got it, and reaching the web would
have required granting the ability to spawn other agents. That is a strange
thing to have to hand somebody so they can read a page.

**The routing is a prompt, not a permission, because no harness offers the
switch it would need.** There is no "deny WebFetch but allow this server" flag;
Claude Code's built-ins are granted or denied by name alongside everything else.
So the instruction is the mechanism, and it is stated in terms of what an agent
*gets* — reaching pages that would otherwise refuse it — rather than as a rule,
because an agent that understands why a tool is better uses it when it matters.
It is applied once in `runner::launch` rather than at the twenty-odd sites that
build a `SpawnRequest`: an instruction that has to be remembered is one that
will be missing from whichever call site is added next.

**A server, not the script next door.** `jodbrowser.py` fetches one URL and
exits, so every page pays a full Firefox launch and nothing can be clicked — the
browser that rendered the page is gone by the time an agent reads it. Resident
means one launch, many pages, and cookies that persist across tool calls, which
is what a login-walled page needs. It starts Firefox lazily, because an MCP
server is launched when the harness starts and most runs never browse at all.

**Everything camoufox does sits behind a four-method seam**, so the protocol,
dispatch, argument validation and truncation are all tested from values with no
browser installed — the same split `monitor::Probes` makes on the Rust side.
What that cannot cover is whether traffic really leaves through the proxy, and
that check found a real bug immediately: `describe()` read the environment but
never loaded `browser.env`, so it answered "direct" while the very next fetch
went through the proxy. Harmless in the one-shot CLI, where `browser_options()`
always ran first — and a lie in `browser_status`, whose entire job is to say
whether traffic is proxied.

## The cheapest disk to reclaim is the disk never written

The box hit 100% — 45G, zero free — with six agents building in parallel. What
that looks like from inside a session is not a full disk. It is a build failing
for a reason unrelated to the diff, or a file that will not write, so the agent
reports its symptom and the cause lands on somebody else's task looking like
theirs. This repo already carried a commit named *"record why an idle agent is
usually a full disk"* before anything was done about it.

The arithmetic is unforgiving. A cargo `target/` is per-worktree, six parallel
worktrees is six of them, and every default inside each is paid for six times.
Measured on one worktree's 8.7 GB `target/debug`: **3.9 GB of it — 44% — was
incremental-compile state**, and summing ELF sections in the largest object file,
**69% of it was `.debug_*`**. Both are correct defaults for a human in an
edit-rebuild loop. Neither describes an agent that builds twice and is then
deleted, which writes the incremental cache, pays for it, and never reads it
back.

So the fix is not cleanup. `[profile.dev]` sets `incremental = false` and
`debug = "line-tables-only"`, which keeps the file and line in a panic backtrace
— the part a failing test actually needs — and drops the type and variable DWARF
that dominates the size. `profile.test` inherits it, so it reaches the check that
runs. `dev-debug` inherits and puts the DWARF back for anyone who does want a
debugger.

**The obvious answer was the wrong one.** A shared `CARGO_TARGET_DIR` across the
worktrees reclaims more — one dependency set instead of six, ~22.7 GB down to
~5 GB — and it was rejected. Cargo takes an exclusive lock on the build
directory, so six agents pointed at one target dir build one at a time; the
fleet's whole purpose is that they do not. The worktrees also build different
feature sets, which would churn each other's fingerprints and force rebuilds. It
trades the thing being optimised for against the thing that matters, and it is
the option a reader would reach for first, which is why it is written down.

What remains is a sweep, because worktrees outlive their tasks — and the sweep is
**shell, not judgement**. Whether a build directory is still wanted has a
determinate answer: a compiler is writing to it, or wrote to it recently, or
neither. A model asked hourly would sometimes answer differently, and the failure
direction is deleting the `target/` a teammate is three minutes into, which
surfaces as an unexplained build failure in *their* session. So
`sweep_targets.sh` decides, and it runs through
`jod monitor set --no-agent` — "the script is the whole job: its stdout is the
result and no model is ever woken. Empty stdout means stay quiet." A quiet hour
costs a `df` and a `find`, wakes nothing, and writes no ledger entry.

Three of its properties are load-bearing and none is the deletion.

**It only acts under pressure.** Above `--min-free-gb` (default 8) it exits 0 and
silently. A build cache is only waste when the space it holds is needed, and a
sweep that runs when there is room is pure loss — it converts free disk into
somebody's next rebuild.

**It stops as soon as there is room.** Oldest directory first, re-checking free
space between each, so a sweep deletes as little as it can rather than as much as
it may.

**It distinguishes an idle agent from an idle build.** Only compiler processes
count toward "busy", and a compiler is identified by its name appearing as the
executable *or* as any token on the command line — `/proc/pid/exe` resolves to
the real inode, so a build driven through a shim or `bash -c 'cargo test …'` has
an `exe` of bash and would otherwise be invisible. A session's cwd deliberately
does *not* count: an agent's cwd sits in its worktree for the session's whole
life, so honouring that would mean a worktree with an idle agent in it is never
swept, which is the exact case being reclaimed.

The guard before the only `rm -rf` re-checks the basename and refuses any
directory containing a `.git` entry, and says so rather than skipping quietly.
The candidate list is already built from `find -name`, so it cannot fire today.
It is there because the asymmetry is total: a wrongly deleted `target/` costs
compile time, and a wrongly deleted checkout costs unpushed work with no way
back. Those two must never be one bug apart. A peer session asked precisely this
question while the sweep was being written — worktrees had vanished, and it
wanted to know whether the sweep had done it. It had not; they were removed
through git by session-exit cleanup, which is why they were absent from
`git worktree list` as well as from disk, where an `rm -rf` would have left the
administrative entry behind. The guard and its test exist because the question
was worth being able to answer with a test instead of an assurance.

## An answer is queued, never spliced into a turn already running

A prompt is assembled once, at spawn. Anything that arrives afterwards reaches
a model whose context was fixed before it existed, so an answer delivered
mid-turn is either ignored or acted on twice — and which one you get depends on
timing nobody controls.

So answering a card is a write and a *queue*, not an interruption. The rail
carries two independent facts about every card: `status`, what the human did,
and `delivery`, whether the agent has heard about it yet. "Answered, queued" is
an ordinary state rather than an inconsistency, and the UI must show it —
pretending an answer landed immediately is a lie the user makes decisions on.
Answer ten cards while a run is mid-turn and none of them touch it until it
comes up for air.

The queue is one table for three sources — card answers, mail from another
agent, a nudge typed by a human — because "is this session ready to be spoken
to" is the same question whoever is speaking, and it was already written down
once, correctly, in `team::wake_order`. A second copy of that judgement is a
second thing to keep right. Delivery itself stays the synthetic user turn the
bus already uses, so nothing new has to be true on any harness.

Batching is part of the design rather than an optimisation of it. Ten queued
answers become one turn carrying ten, not ten turns: that is a cost control,
and an agent reading everything that changed in one go also answers better than
one woken ten times with a line each.

## A secret travels as a name; only the supervisor ever holds the value

Inject at exec, mask on output, reference by name — the model GitHub Actions,
Doppler, Infisical and `op run` all arrived at separately, which is about as
much corroboration as a design gets.

`SpawnRequest` and `SpawnPlan` therefore carry secret *names*, never values.
That is not fastidiousness: `spawn.json` is written to disk precisely so a
person can read afterwards exactly what was launched, so a value in it would be
a second copy of the credential sitting at ordinary permissions — the leak the
whole design exists to prevent, created by the file meant to make the system
auditable. The supervisor resolves each name at exec time from a file outside
every repository at owner-only permissions, verified on read.

Redaction is the belt to injection's braces. The supervisor is the only process
holding both the values it injected and the lines the child printed, so it
scrubs the output before anything is parsed — an agent that echoes the variable
still cannot get the value into the transcript. It runs *before* parsing rather
than after, because a value scrubbed after decoding has already been through a
parser, and a parser is a thing that can log.

Values below a length floor are injected and deliberately **not** redacted: a
four-character secret would match half of ordinary output and replace
legitimate text with the marker. The rail says so when one is stored, because a
silent exception here is a leak nobody was told about.

What this buys is narrower than "the agent cannot leak the key", and worth
having anyway: the value is not in the database, not in the prompt, not in the
transcript, and not in the launch record. A missing key blocks one test rather
than a session — which is the point, and why the agent is told to treat an
absent credential as a *blocked* ending rather than a reason to invent one.

### What the machinery does not do, measured

An earlier version of this entry implied the model never sees the value. It is
not true, and an agent found it rather than a test — asked to print a secret
through a shell command, it reported back:

> the value came back to me **unredacted** in the tool result. So whatever
> scrubbing Jod does, it isn't happening on the tool-output path into the
> model's context — it can only be happening later, at storage time.

It is right, and the reason is structural. The supervisor sits between the
harness and Jod's store, **not** between the harness and the model. A harness
runs its own tool loop: it executes the command, hands the output back to the
model, and only then prints a line that Jod reads and scrubs. By the time
redaction happens the model has already seen it.

So the guarantee to state is **the value never reaches the record** — not the
database, not the transcript, not the launch plan, not a backup. Whether the
*model* sees it is decided by the preamble telling it not to go looking, and by
nothing else. That sentence in the preamble is therefore load-bearing rather
than decorative, and anyone trimming the brief for length should know it is
the whole of that control.

Two smaller limits follow from the same shape. The scrubber replaces exact
occurrences, so an agent that retypes a fragment, or breaks the value across a
line, defeats it — which is a reason to keep values long and opaque rather than
a reason to build a cleverer matcher. And redaction cannot see an outbound
request at all: the exfiltration path that actually matters is the agent
calling something with the key, which no scrubber is positioned to observe.

None of this makes the design worse than the alternative — a credential in the
prompt is seen by the model *and* stored. It makes the claim smaller and true.

## A worktree outlives the work that cut it

Deleting a work removes its sessions, their transcripts, their unanswered cards
and their bus traffic, in one transaction — a half-deleted tree is not a state
that should exist. It does **not** remove the git worktrees or their branches,
and the `leases` row survives with a null work rather than cascading.

The asymmetry is the whole point. Jod's records are cheap to recreate; a branch
with uncommitted work on it is not. And the moment somebody deletes a session's
history is exactly the moment nobody is left to remember what was on that
branch — so the cheap thing goes and the expensive thing stays. The paths are
printed so nothing is orphaned silently, `work_title` is kept on the row so an
orphaned lease can still say what it was for, and removing a tree is a separate
deliberate flag that still refuses one that is dirty or unmerged.

The delete itself refuses the first time whenever a work holds a worktree,
printing each lease's path, branch, and whether it is dirty or merged, and
proceeds only when the same command is repeated. The confirmation is bound to
that work and expires, so a stale one cannot arm a later delete. A work with no
leases deletes on the first command, because there is nothing on disk to lose —
a confirmation that fires when there is no risk is one people learn to type
through without reading.

## A unit test proves a function; only an entry-point test proves a feature

This repository has now produced the same defect at scale twice. The first
entry about it — "Six guards were green, and none of them were guarding" —
turned out not to be enough on its own, so here is the general form.

An audit of one large build found **twenty-four functions with no caller
outside their own tests**, and five user-visible promises broken behind a fully
green suite:

- answering a card never reached the agent — the handler that injects a queued
  answer at a turn boundary had no caller, so an answer sat queued for ever
  unless the agent happened to re-poll for it;
- nothing could claim a worktree — a session was pointed at a read-only
  checkout and told to claim one before writing, with no way to do so, making
  the instruction unfollowable;
- no pull request was ever recorded — schema, stream parser and forge poller
  all built, all tested, referenced by exactly one file: their own;
- discovered commands never reached the palette they were discovered for;
- a work never closed when its last task completed, because the three functions
  that would have noticed were called by nothing.

Every component passed its tests. Every seam between components was missing.

**The reason the tests cannot see it is structural, not careless.** A unit test
calls the function directly, so it proves the function works when called — and
says nothing about whether anything calls it. The test and the missing caller
are independent, so the suite stays green at exactly the moment the feature
stops existing. Adding more unit tests makes this *worse*, because the green
count grows while coverage of the actual product does not.

Three things follow, and they are cheap:

**Wire it before you finish it.** A function with no caller is not a
half-finished feature, it is a zero-finished one with convincing decoration.
Landing the caller in the same change as the function is the only reliable
moment — afterwards there is nothing to notice the gap.

**Write at least one test that fails when the caller is removed.** Not when the
function breaks: when the *wiring* breaks. That is a different test and usually
a coarser one — drive the entry point a person or an agent would actually
reach, and assert the observable effect.

**Grep for callers before believing a green suite.** `grep -rn 'name(' --include=*.rs | grep -v test`
is fifteen seconds and it is the only check that caught any of these. Three of
the five were found not by testing but by *trying to use the feature* — writing
the end-to-end script, or giving the module a caller and watching what it
produced.

The counting error underneath all of it is worth naming: a test count measures
how much code has been exercised, and it is routinely read as measuring how
much of the product works. Those diverge silently, and the gap between them is
invisible from inside the suite.

## A protocol given once at session start does not survive into a later turn

Jod briefs an agent when its run begins. That brief is one user turn among
many, and by the time something arrives that depends on it — a message from a
peer, an answer to a card — it may be several turns back in a resumed
conversation.

Measured, not theorised. In a cross-harness exchange the answerer was told at
session start how to use the bus, and when a question arrived some turns later
it replied **in prose and never touched the bus at all**. The exchange
half-happened, in silence: the asker waited, the answer existed, and nothing
Jod could see had gone wrong. Nothing failed loudly, because nothing failed —
an agent that has forgotten a protocol is an agent behaving reasonably in the
absence of one.

Two consequences.

**A standing protocol belongs in the standing framing**, re-sent every turn,
rather than in the opening prompt. That is what a system prompt is for, and the
harnesses that have one (`--append-system-prompt`) should carry it there; the
ones that do not need it prepended to each prompt rather than only the first.

**Anything delivered as a synthetic turn should carry what the recipient needs
to act on it.** A message that says "reply with `reply(message_id=…)`" is
self-describing; one that assumes the recipient remembers the verb is a message
that works on turn two and quietly stops working on turn twelve. The same
argument the message-id fix already made: the recipient must be able to act
from what it was just handed, not from what it was told once.

The failure mode is worth naming because it is invisible from inside a test
suite. Every unit test gives its agent the instruction and the stimulus in the
same breath, so the gap between them — which is where this lives — never opens.

## A repair belongs in a migration, not on the read path

When a bug leaves rows wrong, the data is often recoverable from something
durable nearby — and the tempting fix is to fold the repair into the reader, so
the wrong value silently becomes right the next time anyone asks.

Do not. Put it in a migration that runs once, or behind an explicit repair
command.

Two costs, and the second is the one that matters. A fold makes a hot read into
a query that sometimes writes, so it acquires new failure modes — a locked
database now breaks a lookup that used to only ever read. And it **permanently
masks the bug it was written for**: if whatever should have populated the value
ever stops again, the reader keeps producing the right answer and nothing
surfaces until something further downstream, with less context, fails instead.

That is the worst failure shape this repository has: silently correct. A
migration is auditable, ordered with the rest of the schema, and — crucially —
does not run tomorrow. If the writer regresses, resume breaks loudly, which is
what makes it fixable.

The general rule: **a fallback that hides a missing writer is not resilience,
it is a second implementation of the writer with nobody watching it.**

## Chords go to the verbs you press mid-sentence; screens go behind the leader

The global chords were on Ctrl, then briefly on Alt, and are on Ctrl again. The
move to Alt was sound about the problem and wrong about the fix: a multiplexer
does take Ctrl chords before Jod sees them — tmux's default prefix is `Ctrl-B`,
which was the delegate key, so it never arrived at all. But macOS composes
Option into accented characters unless the terminal is specially configured, so
on the machine this is actually used from, `Alt-K` typed `˚` into the prompt and
*no* chord arrived. A binding nobody can press is worse than one something else
eats, because the second at least has a workaround.

So the rule is not "Ctrl" or "Alt". It is **take the letters nothing else is
holding, and spend them on the verbs that need to be reachable without stopping
what you are doing.**

Here that leaves eleven: tmux has `a s h j k l`, the terminal has `q z i m`, and
readline has `c d e u w`. Sixteen verbs do not fit in eleven letters, and the
tempting move is to shave the list — drop a verb, or double one letter up under
a modifier the terminal cannot distinguish anyway. Both are worse than the
answer that was already in the program.

A chord buys exactly one thing: reachability while the chat box is turning every
bare key into text. Delegate the line, stop the run, copy the reply, answer the
rail — those need it. A *destination* does not, because arriving somewhere is
not something you do halfway through a sentence. Destinations went behind
`Ctrl-G`, which is what the which-key menu was built for, and it now covers all
nine screens instead of the seven that happened to have no chord left. Four
verbs went with them, and the menu draws every one — a route nothing prints is a
route nobody takes.

Two things fell out of this that were not the point but are worth keeping. The
letters spent on menu verbs have to stay clear of the workspace letters, or a
new screen silently shadows a verb that then has nowhere to go — pinned by
`a_which_key_verb_does_not_shadow_a_screen`. And one letter, `Ctrl-V`, was held
back so the next verb would have somewhere to land that is not someone else's
key.

**The spare was spent within the week, which is the argument for having kept
it.** Rebasing onto main brought two verbs that had been written against the Alt
map: dictation and a projects panel. Dictation went straight to `Ctrl-V` — it is
the strongest claim to a chord in the program, being *only* useful while your
hands are off the keyboard, and `v` is the letter it would have asked for. The
projects panel is a destination and went to `Ctrl-G d`, because `Ctrl-D` is quit.

So the eleven are now spent, and `every_free_letter_is_spent_so_the_next_verb_is_a_decision`
says so out loud. When it fails, the keymap is full and the choice is explicit:
the new verb is a destination and goes behind the leader, or something already
holding a letter is demoted. It is never a licence to take a letter back off the
multiplexer. The menu is the pressure valve, and it has nine free letters left.

**The part that does not generalise:** the six letters tmux holds are *this*
tmux's. A differently-configured multiplexer wants a different six, and nothing
in the program can detect that. What the repo can do is keep the map in one file
and assert the constraint out loud, which is
`no_verb_sits_on_a_chord_a_multiplexer_takes` — so the next person to move it
finds the reasoning rather than the symptom.

## A version that names only the release cannot answer what it is asked

`jod --version` said `jod 0.1.0`, which is true of every binary this repository
has ever produced. The question anyone actually asks it is narrower: *is the
program I am running the code I am reading?* An installed copy on `$PATH` and a
fresh build sat side by side answering identically while differing by seven
subcommands — the user's first sign was `unrecognised subcommand` for a
documented feature, and hand-driving the TUI against an unrebuilt tree produced
bug reports nobody could trust.

So the version carries the commit: `jod 0.1.0 (f4e4c72 2026-08-13)`, stamped by
`cli/build.rs`, with `-dirty` for uncommitted work and `unknown` when there was
no git to ask.

**The date is the commit's, not the wall clock's.** A build script that stamped
`now()` is only honest if it reruns on every `cargo build` — which recompiles
the largest crate in the workspace, times however many worktrees the fleet has
open. And if it does *not* rerun, cargo replays the previous value and the
binary reports the time of an older build with total confidence: a lie in the
one field whose entire job is telling builds apart. A commit date is stable, so
rebuilding the same tree stays cache-hot and the stamp stays true. What the
wall clock would have distinguished — two builds of the same commit — is
already covered by `-dirty`.

**The installer still copies rather than symlinks.** Symlinking `~/.local/bin/jod`
into `target/release/` would track rebuilds automatically, and that is precisely
the objection: installing would stop being an act. `cargo clean` — or the
routine sweep of a full disk, which starts by deleting worktree `target/`
directories — would leave a dangling binary on `$PATH`, and a long-running
console's identity could change underneath it with nothing recorded. The copy
was never the defect. Being unable to *tell which copy* was.

## A console is opened inside something, so `$HOME` is the wrong default

`jod tui` took its working directory from `service::default_cwd`, which answers
`$HOME`. That default is right for the thing it was written for — `jod run` is
a one-shot that a schedule, a webhook or a Telegram message can fire from
anywhere, and inheriting whatever directory the caller happened to be in is not
a decision anybody made. It is wrong for a console, which is *typed inside
something*: you `cd` to a repository and open one there.

The cost was not cosmetic, and it was paid in three places at once. Every
turn's harness process started in `$HOME` rather than the repository on screen.
Nothing on the screen named a directory, so there was no way to notice. And the
picker had always used the launched-in directory — so `Ctrl-P` and the session
itself quietly disagreed about where "here" was.

The launch directory is now granted to the conversation on screen exactly as
`/add-dir` would grant it, and the header band names it. Two rules keep that
from becoming a policy:

**Once per conversation, per process.** `add_root` is idempotent, so re-adding
on every tick would cost nothing and mean something: it would put the directory
back a quarter-second after `/root remove` took it away. A console that undoes
your removals is worse than one that never offered the root. Removal holds for
the session; the next launch grants it again, which is the same bargain
`/add-dir` itself makes.

**Somewhere to put it, or make one.** A machine where nothing has ever run has
no conversation at all — the rail falls back to the pinned main chat and on a
fresh install there is not one to fall back to. The console opens it, which is
what entering the main chat already did. Found by launching the program against
an empty `JOD_HOME` and reading the tables afterwards: `conversations` and
`conversation_roots` were both still empty after minutes, with every unit test
green. → [why that is the only check that finds this](#a-unit-test-proves-a-function-only-an-entry-point-test-proves-a-feature)

## A bare directory name is resolved against the roots, or the launch is refused

"Build it in the tetris directory" is how people talk, and `tetris` is not a
path. Whatever resolved it had to pick something, and what it picked was
`$HOME/tetris` — a whole project, `node_modules` and all, in the home
directory, while the directory the user had actually added stayed empty.

The roots are the answer, because they are the only directories anybody named.
So a relative `cwd` now resolves against them — the root's own name, or an
existing directory inside one — and when it matches none of them the launch is
**refused** with a blocking card that lists what was on offer. There is no
fallback, deliberately: every candidate default here is a guess, and the guess
this replaced was the worst one available. Only a conversation with no roots at
all falls back, and it falls back to the directory the caller is standing in.

The refusal happens before the harness is located, so "`tetris` is not one of
your directories" is what a person is told rather than having it masked by
whichever harness happens to be missing on that machine.

## A run that lands nowhere anybody asked for is a card, not a failed run

Roots are a convention, not a sandbox — passing one grants, withholding one does
not deny — so nothing stops a run writing outside them and nothing should
pretend to. What was missing is narrower and worse: **nothing looked
afterwards.** The harness exited 0, the supervisor recorded `completed`, the
fleet drew a green check against $1.18 of real spend, and every file produced
was somewhere nobody had named. The failure was silent in both directions at
once — the agent believed it had succeeded and the record agreed.

The supervisor already sees every tool call, so it now keeps the paths and, on a
run it is about to call `completed`, compares them against the directory the run
was given plus whatever its conversation declares by then — read at the end, so
a worktree claimed mid-run counts as somewhere it was meant to write.

Two rules keep it from becoming noise:

**A card, not a status.** The run really did complete; relabelling its exit code
would be a lie, and the next person would have to work out which `failed` runs
actually failed. What it did not do is land anywhere anybody asked for, and that
is a thing to tell a person.

**All, not any.** One write inside the workspace and nothing is said, however
many scratch files went elsewhere. A run that found its way is not worth a card,
and a warning that fires on ordinary work is one people learn to dismiss — which
would cost exactly the case it exists for.

`Bash` is the acknowledged gap: `pnpm install` created the `node_modules` tree in
`$HOME` in the original run and no argument inspection would have caught it.
What catches that is the working directory being right to begin with.

## The line dismissed as bookkeeping was the only one on the wire

A build submitted through `jod tui` froze for about nine minutes. The last
rendered line was a `Bash` result from second seven; the status bar showed a
bare spinner and `working 4m49s`. Nothing was wrong — the model was thinking —
but a five-minute blind spinner and a crashed process look exactly alike, and
the only honest thing a person can do with either is kill it.

The harness was not silent. `claude` emits `system` / `thinking_tokens`
throughout a long think, steadily, carrying a running estimate. Jod's adapter
handled `system` only when `subtype == "init"` and dropped the rest under a
comment calling them bookkeeping. They arrived, they were counted by nobody, and
they were thrown away — during the one window in a run where nothing else is
produced at all. A turn that reasons emits no assistant block, no tool call and
no result until it is finished reasoning.

So `AgentEvent::Progress` exists, and three things about it were deliberate.

**A tick, not content.** It carries a token count and no text. Put in the
transcript it would be nine minutes of scrollback saying "still working"; its
place is a status line. `NewMessage::from_event` drops it for the same reason a
`Raw` line is dropped — a thread replayed into another harness must not replay
the first harness's heartbeat.

**Typed, not `Raw`.** The catch-all arm below would have taken it and rendered
the JSON verbatim into the transcript. "Not dropped" and "surfaced" are
different fixes, and only one of them is this one.

**The count is optional; the tick is not.** If a later build renames
`estimated_tokens`, Jod still says *alive* rather than falling silent again. The
failure being repaired is silence, so nothing in the path may be conditional on
a field parsing.

`hook_started` and `hook_response` stay dropped, and that is now a decision
rather than the previous line surviving by inertia. A hook fires around
something Jod already renders — `PreToolUse`/`PostToolUse` bracket a tool call
and its result — so it is a second copy of an event the stream already carries,
and none of them fires inside a think with no tool in it. They also exist only
if the user configured hooks, and a liveness signal cannot be contingent on
that. `hook_response` additionally carries the `stdout` and `stderr` of an
arbitrary user shell command, so surfacing it is a redaction question rather
than a liveness win.

One consequence beyond the UI: [heartbeats](#alive-is-not-working-and-only-one-of-them-was-ever-checked)
ask whether a run is *producing events*, and a long think used to produce none.
Ticks now advance the run's high-water `seq`, so thinking reads as working —
which is what it is.

## Reasoning is shown by default, and hiding it is the flag

Every surface that follows a run had the toggle and three of them started with
it off, so the default answer to "what is it doing" was a list of tool names.
That is the least informative half of a run: `Read`, `Grep`, `Bash`, `Edit`
says which files were touched and never why one branch was taken over another.
`jod main --wait` was worse than off — it matched on `Message` and `ToolCall`
and let every other event fall through, so no toggle would have helped it.

So the default is *shown*, and the flag is `--no-thinking`. The inversion is the
point: the thing worth a flag is turning the noise off, and a person who wants
less reads it in the help. Recording was never in question — a `thinking`
message is stored whatever any of this says, because a conversation read back
tomorrow must not be missing its reasoning because a display toggle was off
yesterday.

One consequence worth stating: on `jod run` and `jod watch` reasoning goes to
stderr with the rest of the progress, so `jod run … > out.txt` still captures
the answer and nothing else. `jod main --wait` puts its whole live view on
stdout and continues to.

And a finding that arrived with it, which is why the ticks above matter more
than they look: on `claude-sonnet-5` the reasoning **has no text**. The block
arrives signed and empty — `{"type":"thinking","thinking":"","signature":"…"}` —
where the same binary on `claude-sonnet-4-6` sends the sentences, so it is the
model withholding rather than the CLI version, and `--include-partial-messages`
does not recover it either: its `thinking_delta`s are empty too. Every one of
the 112 `thinking` rows in the store was the empty string, drawn faithfully as a
blank line between the tool calls. The adapter now drops an empty block — the
guard the `text` arm has always had. So on a Claude 5 model the tick *is* the
whole signal, and no display setting can produce sentences that were never sent.

## The main chat is where the console starts and where every key comes back to

Three separate things made the TUI a place you could get lost in, and they were
one bug wearing three faces: the main chat was reachable but never the default,
and each screen had its own idea of how to leave.

**It is where a launch lands.** The chat box's conversation used to start
*derived* — "the one the run on screen wrote". On a cold start that resolves to
whichever agent this machine most recently finished, so the first sentence typed
after `jod tui` went to a stranger. Being in the main chat is what makes typing
an instruction to Jod rather than a turn to somebody's agent, and that is the
program's whole premise, so it is the starting position and anywhere else is
somewhere you choose to go. `--resume` still wins: naming a conversation is an
explicit choice this must not overrule. A chat nobody has said anything to
leaves the transcript untouched, because `ui::fresh` shows the splash only while
the transcript holds nothing but hints — the empty-state line would otherwise
replace the wordmark on every launch, forever.

**`←` out of a run no longer asks.** The confirmation said *this keeps running
in the background — sure?*, and the honest answer was that it already was: a Jod
run is a detached process group reporting through the database, and the TUI was
only ever a viewer. So the question proposed leaving a thing already left, once
per trip out of a session. What it was actually *for* was the sentence it
printed afterwards — the run survives, `⏎` or `→` reopens it — and that is a
notice, not a question. Keystrokes spent confirming a no-op are how a console
teaches people to stop reading its prompts.

**The fleet tree pins the chat as its first row.** The tree does not extend the
flat agents list, it replaces it — and the flat list is where the pinned chat
lived. Core's forest is works and what hangs off them (`WHERE c.work_id IS NOT
NULL`) and the main chat belongs to no work, so the moment a single work existed
the chat had no row anywhere on the screen and the fleet became somewhere you
could walk into and not back out of except by `Ctrl-G`. It is a sentinel
`NodeId` with a `kind_tag` core never mints, held outside the `/` filter — a
filter narrows the fleet, and the row that is not part of the fleet is the one
you most need when a filter has emptied the screen.

## Three defensible constants in series made the mode on the screen a lie

The console said `auto`. The background session it started refused `git init`,
then `git init -b main`, then `pnpm -v`, each with *"this command requires
approval"*, and gave up on the repository it had been told to create. Nothing
was broken in the sense of a crash: every layer did what it said.

The mode reached the orchestrator and stopped. `hand_to_orchestrator` pinned
`accept_edits` — correctly, once, for a real reason: `plan` refuses the MCP
calls that *are* the chat's job, so a chat below `accept_edits` is inert while
still appearing to work. The per-run MCP config never passed
`--max-permission`, so the server took that flag's own default —
`accept_edits`, agreeing with the run by coincidence rather than by wiring.
`open_work` then asked for `accept_edits` outright and capped it against that
ceiling. Three values, each with a comment explaining why it was right, and no
line anywhere that was wrong on its own.

What made it invisible is that the two ends agreed. The status bar read the
console's mode and told the truth about the console; the failing run was two
levels down. **A default that happens to match the value it stands in for is
indistinguishable from wiring until the value changes** — and the only thing
that ever changed it was somebody pressing Tab.

So the mode is now a parameter at all three, and the floor that motivated the
first constant survives as a floor rather than as a value: `at_least_acting`
raises `plan` and `ask` to `accept_edits` for the chat itself, and passes `auto`
straight through. The test asserts both halves, because a fix that removed the
floor would trade a chat that runs too much for a chat that silently runs
nothing.

## `ask` meant deny, and a mode nobody can answer is not a mode

Under `claude -p` there is no one to put a permission prompt to. `ask` and
`edits` therefore denied silently, and the denial arrived as a failed tool call
— which the model reads as its own mistake, so it retries a variation, fails
again, and reports the task as impossible. The mode was not too strict; it had
no channel.

This build has no `--permission-prompt-tool`, so the only way into that decision
is a `PreToolUse` hook via `--settings`. Jod now writes one per run. It answers
from a standing grant, or raises a blocking card and waits, or — and this is the
part that makes it safe to install in front of every tool call — prints nothing
and lets the harness decide exactly as before. A hook that crashes, or finds a
locked database, or is handed a payload it cannot parse, degrades to the old
behaviour rather than wedging the run.

Two things were only found by running it. The grant was first written by the
hook while it waited, which meant answering the card a minute later — from the
rail, from a phone, the ordinary case — recorded nothing at all, while the card
had promised "every session from now on". The grant belongs to the *answer*, in
the same transaction, so whoever answers keeps the promise. And the first
boundary rule was "a prefix must end at a space", which is right for `git*` and
`gitleaks` and wrong for every URL: it made `https://docs.rs*` unable to cover a
page on that host. The boundary is any character that cannot continue a name,
which refuses `docs.rsevil.com` and `docs.rs.evil.com` for the same reason it
refuses `gitleaks`.

A hook answering `allow` **replaces** the harness's own check rather than adding
to it, so every gap in the matching is a gap in the real boundary. Hence: every
part of a compound command must match separately, and command or process
substitution is never auto-allowed however broad the grant, because no amount of
matching the visible text bounds a command hidden inside it.

## A summary the reader has to decode is not a summary

A research task came back written in the register agents drift into when they
are trying to sound careful: *seam*, *substrate*, *capability*, *compose*,
*authoritative*, and sentences with four clauses each. Every word was defensible
and the findings were correct. Reljod still had to stop and ask what it meant,
which cost more time than plain wording would have taken to write.

The failure is not style, it is delivery. A report exists to move what the agent
learned into the reader's head. Dense wording moves the cost instead of the
information: the agent saves a few seconds by not choosing the ordinary word,
and the reader pays for it with a round trip. On a long unattended run that
round trip may not happen at all, and the summary is simply skipped.

Two habits cause most of it. The first is abstract nouns standing in for things
that have plain names — "the capability seam" for "the code that connects to
each tool". The second is compression: dropping verbs and articles until the
prose reads like commit-message shorthand. Shorthand is fine in a commit
subject, which is one line with a known shape. It is not fine in a paragraph,
where the reader has to reassemble the sentence before they can judge the claim.

So the charter asks for complete sentences and ordinary words in everything a
human reads. Real names stay: a file path, a type, a protocol. What goes is
abstract vocabulary chosen for tone rather than for meaning.

## `ToolAccess` bounds Jod's verbs, not the session

`ToolAccess::Orchestrate` reads like the boundary of what the main chat can do,
and the module comment at the top of `core/src/orchestrator.rs` describes the
orchestrator as confined by it. It is narrower than that. What the level does is
choose which `mcp__jod__*` tools the MCP server offers — `mcp::allows` filters
the catalogue, `mcp_config::config_for` writes the matching document — and that
is the whole of it. The harness keeps its own tools regardless.

Measured, with the flags Jod builds for the main chat: the session comes up
holding 58 tools. Thirty-two are Jod's. The other twenty-six are the harness's,
including a shell, file editors and its own way of starting sub-agents, and Jod
asked for none of them. `--allowedTools` grants without denying, so naming only
`mcp__jod` there changes nothing about the rest; asked to write a file, the same
run wrote one and reported no permission denial at all. The main chat's default
mode is `bypass`, where nothing is refused, which is how a turn that was
supposed to route a question instead sat in a shell loop waiting for its own
child.

The lesson is the same one `docs/harness-support.md` already draws about roots.
Granting is not confining. A flag that hands something over says nothing about
what was withheld, and Jod must not describe either axis as a wall it does not
have.

There is one real deny channel and Jod already writes it: the `PreToolUse` hook
on matcher `*` that calls `jod approve-hook`. A hook that answers `deny` is
obeyed — measured, and the model tried seven ways around it before falling back
to `delegate`, which is exactly the behaviour wanted. It is installed only for
the modes below `bypass`, so making it the orchestrator's boundary is real work
rather than a line of configuration. Until that lands, what stands between the
main chat and a shell is the paragraph in `orchestrator_preamble` telling it not
to, and `tests/e2e/jod/35-orchestrator-toolbox.sh` is how anyone finds out
whether that paragraph is still holding.

## The approval wait is paid per tool call, and buys nothing unattended

`ask` and `edits` put a `PreToolUse` hook in front of every tool call. When no
standing grant covers the call, the hook raises a card and waits 60 seconds —
`APPROVAL_WAIT_SECS` in `core/src/harness/claude.rs`, polled every 400 ms in
`cli/src/approve.rs`. The note beside the constant says the cost is "paid once
per distinct question rather than once per retry". Measured, it is paid once per
tool call.

Every Jod database on the development box, split by whether the run's directory
holds the `settings.json` that carries the hook:

| | runs | call-to-result pairs | median gap |
|---|---|---|---|
| hook installed (`ask`, `edits`) | 28 | 97 | 60.394 s |
| no hook (`auto`, `plan`) | 23 | 129 | 0.033 s |

Seventy percent of the hooked gaps land between 60.205 s and 61.388 s, a
standard deviation of 231 ms around a full minute. Nine more land between 119 s
and 122 s, which is two waits in series. Not one of the 129 unhooked gaps comes
near a minute, and several of those pairs are in the same database file as
hooked ones.

The dedupe meant to bound the cost keys on the exact subject, so `Read a.txt`
and `Read b.txt` are two different questions. A run that opens four one-word
files one at a time takes four minutes. Measured end to end: 60.483 s, 60.438 s,
60.369 s and 60.442 s between one read and the next, four minutes fourteen
seconds of wall clock for four words. That is what puts a ceiling on a single
turn. Seven sequential tool calls is seven minutes, so a suite that stops a turn
at 420 seconds cuts the run off in the middle of ordinary work.

None of the minute is Jod's event pipeline and none of it is the tool. Claude
Code's own transcript records the same gap to within four milliseconds — its
`tool_use` at +2.331 s and `tool_result` at +62.701 s against Jod's +2.227 s and
+62.601 s for the same call. The tool has not started yet. The harness is
waiting for Jod's hook, and Jod's hook is waiting for a person.

When nobody answers, the hook prints nothing and the harness decides, which is
the decision it would have made a minute earlier. Jod passes
`--permission-mode manual` for `ask` and `acceptEdits` for `edits`, so a plain
`Read`, and under `edits` a plain `Write`, are both allowed by the harness on
its own. Both were measured stalling for the full minute first: four reads of
one-word files came back at 60.339 s to 60.381 s with all four cards still open,
and under `edits` — the mode whose whole promise is that file edits go through —
two `Write` calls came back at 60.547 s and 120.923 s.

So the constant does what it says and the hook is as safe as it claims. The cost
is the part nobody had measured. An unattended run pays a minute per distinct
tool call for an answer that is not coming, and then gets for free the answer
the harness was always going to give.

The fix has to settle something not written down anywhere yet, which is why this
is an entry and not a patch. Either Jod learns whether a person is actually at
the rail — approval cards go to the rail and nowhere else today, not to Telegram
— and skips the wait when nobody is there, or it stops asking about calls the
harness would allow by itself, which means Jod keeping its own list of which
tools are sensitive. The first is the smaller change to the boundary. The second
re-opens the argument `plan` already settled, that a list of tool names is not a
boundary. Skipping the wait is not itself a loosening: the hook's silent
fall-through after 60 seconds is the same code path, only later.

Nothing hit this from the console. All 341 run directories under `~/.jod` were
launched in `auto` and none of them has a settings document, so the console has
never paid the wait once. It bites the suites that exercise `ask` and `edits`,
which is where it was found.

## A page that cannot say it is a page

`list_agents` returned a bare JSON array of at most twenty agents. The tool
description called that "every agent Jod knows about", and nothing in the reply
contradicted it. The orchestrator calls this tool first on almost every turn to
decide whether to reuse a warm agent or start a cold one, so a reply that reads
as complete when it is not is a reply that argues for spawning.

Working out when the cut actually bites was the useful half. The listing sorts
running agents ahead of finished ones, so the twenty-row cap drops the oldest
*finished* agents first. Three agents that have been running for days still lead
the page on a box with a hundred runs on it. A running agent only falls off the
default page when more than twenty are running at once. So "past twenty agents
a wedged one silently disappears" was wrong.

The real disappearance was one layer down. Before paging, the tool reads runs
back out of the database with a fixed cap of two hundred, newest first. An agent
that started before the newest two hundred runs never enters memory at all, so
it is absent at *every* limit, and `running_only` positively answers that nothing
is running while three agents are. That is worse than the reported bug, because
the obvious remedy — ask again with a bigger number — does not work.

The fix is both halves. The reply became an object carrying `returned`, `total`,
`hidden` and a note, borrowing the words `jod ls` has always printed for a
person; and the read-back now covers whatever limit was asked for, the same sum
`jod ls` does, so the note points at a remedy that works.

The general shape: a signal that a result was cut is worth nothing if the way
out it names is capped somewhere the caller cannot see.
