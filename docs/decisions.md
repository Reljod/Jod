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
