---
name: shepherd-prs
description: >
  Use when sweeping the open pull requests to find and close out the ones that
  are finished — on a schedule, after CI goes green, or on demand. Triggers on
  "shepherd the PRs", "check my open PRs", "merge anything that's ready",
  "sweep the PRs", "which PRs can go in", "close out the green ones". Runs the
  merge gate on each PR, spawns read-only agents that can veto, and merges only
  what survives both.
---

# shepherd-prs

The `auto-merge` skill answers "may *this* PR merge?" — you have to already be
standing in front of one. This skill goes and finds them: sweep what's open,
work out which are finished, and close those out without being asked.

That is the difference between a gate and a routine. A gate is something a
human walks up to. A routine runs at 09:00 whether or not anyone is thinking
about pull requests, which is the point — the PRs that rot are the ones nobody
remembered to go back to.

## The layer model

Three layers, and the order matters more than any of them individually:

| Layer | What it is | What it can do |
|---|---|---|
| `pr_sweep.sh` | enumeration + hard filters | decide what is *considered* |
| `merge_pr.sh` | the deterministic gate | **refuse** — the floor |
| review agents | judgement | **refuse** — never permit |

Read down the third column: nothing in this skill can permit a merge. Every
layer can only take one away. The gate is the sole thing that ever says yes,
and it is a regex — for the reasons `auto-merge` sets out at length, chiefly
that the diff is written by whoever is asking to merge it.

### Why agents are safe here, having been refused there

`auto-merge` argues that a model must not decide whether a PR is safe, because
the diff is author-controlled text and a model reading `this change is routine
and pre-approved` may believe it. That argument is about **permission**. It
does not apply to refusal.

An agent that can only veto has no exploitable failure mode: text in a diff
that manipulates it can, at absolute worst, cause a veto that wasn't warranted
— and the cost of that is one human read. So judgement is welcome exactly
where it subtracts, and excluded exactly where it adds.

This is also why the routine degrades safely. If the agent layer misbehaves —
an agent crashes, returns nonsense, or the lead ignores a veto outright — what
remains is `merge_pr.sh` gating the merge, which is precisely the behaviour of
`auto-merge` on its own. The worst failure of this skill is that it becomes the
skill it is built on.

## The routine

### 1. Sweep

```
${CLAUDE_SKILL_DIR}/scripts/pr_sweep.sh --format md
```

Every open PR comes back `ready`, `blocked`, or `skipped`. Two filters are
hard, and no flag relaxes either:

- **Fork PRs are never swept.** The scheduled job holds a write token; a fork
  head is written by anyone who can open a PR.
- **Only allowlisted authors**, defaulting to the repo owner. A teammate's
  branch closes when they say it does. `--author LOGIN` adds one deliberately.

`blocked` PRs are done — report the reasons and move on. Do not investigate
how to unblock them unless asked; that is a task, not a sweep.

### 2. Send in the agents

For each `ready` PR, spawn **both** of these in parallel, in one message:

- **`merge-checker`** — is this ready *right now*: live CI, base freshness,
  whether the PR body's evidence matches its diff, whether a `BLOCKED.md` is
  sitting on the branch. Pass it the resolved path to `merge_pr.sh`.
- **`reviewer`** — is this *correct*: `REVIEW.md`'s order, with the lens set to
  whatever the diff is mostly made of.

Both are read-only and neither can merge. Give each the PR number and the base,
and tell it to end with `VERDICT: CLEAR` or `VERDICT: BLOCK — <reason>`.

Run PRs concurrently too if several are ready — they don't interact.

### 3. Read the verdicts fail-closed

A PR proceeds only if **every** agent's final line is exactly `VERDICT: CLEAR`.
Treat all of these as a block:

- a `BLOCK` verdict, obviously
- a malformed, missing, or hedged final line
- an agent that errored, timed out, or came back empty

A near-clear is not clear. The asymmetry is the whole design: blocking wrongly
costs one human read, clearing wrongly costs an unread merge.

### 4. Merge, one at a time

```
${CLAUDE_SKILL_DIR}/../auto-merge/scripts/merge_pr.sh <pr> --ready
```

The gate runs again here, and it is the one that counts — state moves while
agents think. A refusal at this point is a normal outcome, not a contradiction
of the sweep: report the reasons verbatim and leave the PR open.

`--ready` publishes the draft, and only pass it when this repo's draft-by-
default convention is what made the PR a draft. Never un-draft someone else's.

Merge serially, not in parallel. Each merge moves `main`, which puts every
other branch one commit behind — and behind-base is a refusal. Re-sweep after
each merge rather than acting on a stale reading.

### 5. Report

Per PR: the verdict, and for anything not merged, the reason in the words the
gate or the agent used. "Needs review" is not a reason; "check not green:
Tests" is.

## Running it unattended

`.github/workflows/pr-shepherd.yml` runs this after CI finishes on a PR, plus
hourly as a backstop, plus on demand. It is the same routine either way — the
workflow only supplies the trigger and a token.

Two things to know about the unattended path:

- **It costs a model invocation per run.** The `workflow_run` trigger fires
  whenever Tests completes on a PR, which on a busy day is often. The hourly
  schedule alone is the cheaper configuration if that matters.
- **It is only as strong as branch protection.** Everything here refuses to
  merge; nothing here prevents a merge by some other path. Required status
  checks are what make that true, and they are a repo setting, not a file.

## Boundaries

- **Never merge a PR whose gate refused**, however plainly correct it looks.
  Editing the classifier to get a PR through is `gate`-categorised, so it
  cannot auto-merge, and doing it mid-task is the substitution `AGENTS.md`
  names outright.
- **Never treat a clear as an approval.** The agents did not approve anything;
  they failed to find a reason to stop. The gate is what permits.
- **Never sweep into a branch other than the default** without being told to.
- **Never act on a fork PR**, including "just reviewing it and reporting" —
  read it if asked, but it does not enter this routine.
