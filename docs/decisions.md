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
[`REVIEW.md`](../REVIEW.md) briefs the automated first pass;
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

## The CLI asks; the script still takes flags

`jod setup-project` with no choices walks a human through them (↑/↓, space,
enter) rather than making them hand-assemble `--skills`. The wizard only *fills
in* flags, so scripts, CI, and agents keep the same deterministic entry point;
no-tty falls back to `--list` instead of hanging on a prompt.

## Toolkit distribution is a curlable installer, not a required clone

`install.sh` + `bin/jod` bootstrap the toolkit on any Linux/macOS box and run
`jod setup-project` against a repo without cloning Jod into every project.

## Releases are semver tags, cut manually

`vMAJOR.MINOR.PATCH` via the Release Action's `workflow_dispatch`, never on
every push. Install pins to latest; `jod update` only takes newer patches within
the installed MAJOR.MINOR, so a minor/major bump can't yank the rug out from
under an existing install.

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

## The four agents exist twice, and a test keeps them identical

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
exists — `claude-code-review.yml` reads every PR — but it advises, and the thing
that can actually merge is deterministic.

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

## Five guards were green, and none of them were guarding

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
- **An assertion pointed at the wrong half of the screen.**
  `text_that_was_shortened_says_that_it_was` checked `!row.contains(name)` — but
  past ninety columns the detail pane shares the rendered line and prints the
  name in full, so it was reading the *other* pane's copy and would have passed
  through any amount of silent clipping in the pane under test.

The shape is one thing, and it is not carelessness — all five were written
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

A test you have never seen fail is a test you have never seen.
