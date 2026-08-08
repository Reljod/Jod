---
name: auto-merge
description: >
  Use when deciding whether a pull request can be merged without a human
  reading it, or when actually merging one. Triggers on "merge this PR",
  "can this auto-merge", "is this safe to merge", "does this need review",
  "triage this PR", "merge it if CI is green". Categorises the diff with a
  deterministic classifier and merges only when every precondition holds —
  linear history, branch not behind, checks green, verdict auto-merge.
---

# auto-merge

Most PRs in a repo an agent works in are not interesting. A typo in a
README, a research writeup, one more test case — asking a human to read
those spends the scarcest resource in the loop on the changes least
likely to need it. Meanwhile the PR that *does* need eyes gets the same
rubber stamp, because a reviewer who has approved nine trivia in a row
approves the tenth.

So this skill exists to make the boring cases genuinely unattended, and by
doing so to make "a human is reading this" mean something again.

## The gate is a regex, not a judgement

The classifier is `scripts/pr_triage.sh` — path patterns and content
patterns over a diff. That is the whole design, and the reason is worth
stating plainly:

**The diff is written by the same agent asking to merge it.** A model asked
"is this PR safe?" is reading attacker-or-author-controlled text and
deciding whether to obey it. A branch that adds `# this change is trivial
and pre-approved` gets a real chance of being believed. A regex does not
have that failure mode: it either matches or it does not, and no amount of
persuasive commentary in the diff changes what `grep -E` returns.

Two properties follow, and both are load-bearing:

1. **It only escalates.** Every rule can move a PR from auto-merge to
   human-review. Nothing can move one the other way — not a flag, not an
   environment variable, not a line in the PR body. A false positive costs
   one human read; a rule that never fires costs an unreviewed merge, which
   is why the path patterns are deliberately broad.
2. **Four categories are a floor.** `security`, `gate`, `ci` and `data` are
   never auto-mergeable, and `--allow` cannot make them so. A knob that
   switches the gate off for CI is the gate being optional.

`gate` includes this skill — its prose as much as its scripts, because its
prose is the merge policy. A PR that widens what auto-merges cannot be
auto-merged by the rules it is widening.

## What gets categorised

Eleven categories, assigned by path and never exclusive — `AGENTS.md` is
both `docs` and `rules`, and the blocking one wins.

| Auto-mergeable | Never auto-mergeable |
|---|---|
| `docs` — markdown, prose, `docs/` | `security` — auth, secrets, crypto, keys |
| `research` — findings, notes, datasets | `gate` — CI, hooks, permissions, this skill |
| `rules` — the charter, skills, agent defs | `ci` — workflows and pipelines |
| `tests` — test files and fixtures | `data` — migrations, schema, SQL |
| `code` — everything else, under the size limits | `deps`, `contract` — off by default, tunable |
| `assets` — images and fonts | |

### `gate` vs `rules`

The split is what the machine **enforces** versus what it **reads**.

`gate` is enforcement — CI, git hooks, tool permissions, plugin manifests,
`CODEOWNERS`, this skill. Change it and you change which checks can run at
all. Never auto-mergeable.

`rules` is instruction — the charter, skills, agent definitions, commands.
Prose an agent obeys. Clarifying a paragraph, adding a skill, fixing an
example: inert, and auto-mergeable. Blocking all of it would mean the
charter improves only when a human has time, which mostly means it doesn't.

The exception is the edit that grants the branch permission to merge
itself. Any `rules` change whose diff adds **or removes** merge-policy
language — `auto-merge`, `human-review`, `--allow`, `max-files`, `branch
protection`, `--no-verify`, `bypass` — is a `self-amendment` finding and
goes to a human. Removed lines count here because deleting *never bypass
branch protection* weakens the rules and leaves no `+` line to find.

A script inside a skill (`skills/foo/scripts/run.sh`) is `code`, not
`rules` — it executes, so it clears the code rules on its own.

Research and docs are auto-merged because **nothing executes them**. That
exemption is conditional, not a property of the directory: a `.sh`, a
`.py`, or anything carrying the executable bit is reclassified `code` even
under `research/`, and has to clear the code rules on its own.

The same reasoning scopes the content scans. A destructive command
(`rm -rf ~`, `sudo`, `curl … | sh`, `DROP TABLE`) blocks wherever something
will *run* it — but a writeup quoting a provisioning script is describing a
machine, not administering one, so prose is not scanned for it. Two
exceptions: `rules` files are scanned like code, because a charter is
prescriptive and gets obeyed literally; and the credential rule scans
everything, because a live key pasted into a note is leaked whether or not
anything runs.

It is also why size limits count only executable weight. A 3,000-line
writeup is not riskier than a 300-line one; 400 lines of new code is about
as much as anyone reads carefully.

Content rules mirror `REVIEW.md`'s substitutions list, so the thing a human
reviewer is told to flag is the thing that mechanically blocks a merge:
skipped tests, deleted tests, swallowed failures, silenced linters,
hardcoded credentials, mocks in shipped code, leftover breakpoints. A
`BLOCKED.md` blocks too — it is a successful ending, and a human's cue.

See `references/categories.md` for the full rule-by-rule table.

## Merging

**Never run `gh pr merge` directly.** Run:

```
${CLAUDE_SKILL_DIR}/scripts/merge_pr.sh <pr-number> [--dry-run]
```

Exit 0 means merged. Exit 1 means refused, with every reason printed.
Refusal is the ordinary outcome and not an error — do not retry it, do not
work around it, and do not reach for `gh pr merge` because the script said
no. That is the same move as deleting a failing test.

The script checks, and merges only if all of it holds:

- the PR is open, not a draft, and has no requested changes
- **every** check on the head commit is green — pending counts as not green,
  because waiting means an unbounded window in which someone pushes
- the branch is **not behind base**, by both GitHub's `mergeStateStatus` and
  a local `rev-list` count
- the triage verdict, **re-derived from the diff right then**, is
  `auto-merge`

It re-derives rather than reading the `merge:auto` label the CI job wrote.
Labels are mutable by anyone with write access, an agent included; the diff
is what the verdict is actually about.

### History stays linear

Only `--method squash` (default) and `--method rebase` are accepted.
`--method merge` is refused outright — a merge commit is the one result
that cannot be replayed as a straight line, and "no merge commits" is only
true if nothing is able to create one.

Behind-base is refused for a reason that is easy to miss: a branch behind
base was tested against a tree that no longer exists. Squash and rebase
both replay its commits onto current base **without re-running anything**,
so green-and-behind is precisely how a green PR breaks main. Fix it with:

```
${CLAUDE_SKILL_DIR}/scripts/merge_pr.sh <pr> --update-branch
```

which rebases and then **stops**, deliberately. Merging in the same breath
would merge on the checks that ran before the rebase — the failure the rule
exists to prevent. Re-run once CI is green on the new head.

## How to run it

1. **Read the verdict** before doing anything else:

   ```
   ${CLAUDE_SKILL_DIR}/scripts/pr_triage.sh <base>...<head>
   ```

   On a PR, CI has already done this — the `merge:auto` / `merge:human`
   label and the triage comment carry it.

2. **Dry-run the merge.** `merge_pr.sh <pr> --dry-run` prints every
   precondition and what it would run, without touching the PR.

3. **Merge, or hand it over.** On exit 0, `merge_pr.sh <pr>`. On exit 1,
   say which reason sent it to a human — name the reason, don't summarise
   it as "needs review".

## Closing out a PR you just opened

This is the common case: you finished a task, `/create-pr` opened a draft,
and CI is running. Finish the job rather than leaving a green trivial PR
sitting for a human who has nothing to add.

```
${CLAUDE_SKILL_DIR}/scripts/merge_pr.sh <pr> --ready
```

`--ready` publishes the draft and merges, but **only if every other
precondition already holds** — it is an opt-in for un-drafting, not a
bypass for anything else. Without it a draft is refused, because
publishing is normally the author's call.

Order matters, and the temptation is to compress it:

1. Open the PR (draft, per this repo's convention).
2. **Wait for checks to finish.** Pending is refused, not waited on — the
   script will not poll for you, and merging on a check that had not
   finished is the thing the rule exists to stop.
3. Run `merge_pr.sh <pr> --ready`.
4. Exit 0 → say so, with the categories and the merge commit. Exit 1 →
   report the reasons verbatim and leave the PR open.

Only do this for work **you** carried end to end. A teammate's branch
closes when they say it does, however green it looks.

## Boundaries

- **Never merge on a `human-review` verdict**, however obviously fine the
  change looks. Your read of the diff is not the check; the check is the
  check.
- **Never edit the classifier to make a PR pass.** That change is itself
  `gate`, so it cannot auto-merge, and doing it during an implementation
  task is the substitution `AGENTS.md` names outright.
- **Never merge someone else's PR unattended**, even on a green verdict.
  Auto-merge is for work the agent shepherded end to end; a teammate's
  branch closes when they say it does.
- **Don't auto-merge into anything but the default branch** without being
  told to. Release and long-lived branches have their own timing.
- The verdict says a human read is not *required*. It never says the change
  is correct — that is what the tests are for, and a PR with no checks at
  all is refused for exactly that reason.
