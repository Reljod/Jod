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

`gate` includes this skill and its own scripts. A PR that widens what
auto-merges cannot be auto-merged by the rules it is widening.

## What gets categorised

Ten categories, assigned by path and never exclusive — `AGENTS.md` is both
`docs` and `gate`, and the blocking one wins.

| Auto-mergeable | Never auto-mergeable |
|---|---|
| `docs` — markdown, prose, `docs/` | `security` — auth, secrets, crypto, keys |
| `research` — findings, notes, datasets | `gate` — the charter, CI config, this skill |
| `tests` — test files and fixtures | `ci` — workflows, pipelines, hooks |
| `code` — everything else, under the size limits | `data` — migrations, schema, SQL |
| `assets` — images and fonts | `deps`, `contract` — off by default, tunable |

Research and docs are auto-merged because **nothing executes them**. That
exemption is conditional, not a property of the directory: a `.sh`, a
`.py`, or anything carrying the executable bit is reclassified `code` even
under `research/`, and has to clear the code rules on its own. A destructive
command — `rm -rf ~`, `sudo`, `curl … | sh`, `git push --force`,
`DROP TABLE` — blocks anywhere it appears.

The same exemption is why size limits count only executable weight. A
3,000-line writeup is not riskier than a 300-line one; 400 lines of new code
is about as much as anyone reads carefully.

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
