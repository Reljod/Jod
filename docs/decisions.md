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

## Agents run in tmux, not as child processes

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

## An agent's tmux session outlives the agent

Jod's sessions used to end when their agent did. That closed the terminal window
of anyone watching, and the chain is worth writing down because every link is a
sensible default on its own:

1. tmux's `detach-on-destroy` defaults to **on** — destroying a session makes an
   attached client *exit* rather than fall back to another session.
2. oh-my-zsh's tmux plugin sets `ZSH_TMUX_AUTOQUIT` from `ZSH_TMUX_AUTOSTART`,
   and runs `exit` the moment its tmux client returns.
3. So: watch an agent → agent finishes → session destroyed → client exits →
   the shell exits → **the terminal window closes.**

Two fixes, because either alone leaves a hole. The launcher now `exec`s a shell
after the agent exits, so a *completed* run never destroys anything. And Jod
sets `detach-on-destroy off` **on its own sessions only**, so an explicit kill
returns the watcher to another session instead of ending their client. The
user's global tmux config is never touched — it is not ours to change.

The cost is that sessions accumulate until closed. That is the behaviour that
was asked for ("kill the session if it's not needed"), it leaves the final
output on screen with the agent's directory ready to inspect, and the UI keeps
its close button live after a run finishes rather than greying it out.

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
