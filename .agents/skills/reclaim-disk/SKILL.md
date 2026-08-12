---
name: reclaim-disk
description: >
  Use when a machine is short of disk, when a build or agent fails with "No
  space left on device", or when setting up recurring cleanup. Triggers on
  "check storage", "disk is full", "free up space", "clean up build
  artifacts", "delete unused targets", "ENOSPC", "schedule a cleanup".
  Deletes cargo build output that no worktree is using, decided by a
  deterministic script rather than by judgement, and arms the sweep on jod's
  clock so it does not need asking again.
---

# reclaim-disk

A fleet of agents sharing one box runs out of disk in a way that looks like
nothing. Builds fail with `No space left on device`, tests fail for reasons that
have nothing to do with the diff, and an agent that hits it mid-task usually
reports the symptom rather than the cause — which is why this repo already
carries a commit named *"record why an idle agent is usually a full disk"*.

The cause is arithmetic. A cargo `target/` is per-worktree, six parallel
worktrees is six of them, and the defaults inside each are tuned for a human in
an edit-rebuild loop rather than an agent that builds twice and is then deleted.
Measured on one worktree here: 8.7 GB, of which 3.9 GB was incremental-compile
state that would never be read back.

So there are two halves, and the durable one is not the cleanup:

| | |
|---|---|
| **Produce less** | `[profile.dev]` in `Cargo.toml`: `incremental = false`, `debug = "line-tables-only"`. Takes a worktree's `target/` from ~8.7 GB to ~2 GB. Already committed — nothing to run. |
| **Sweep what's left** | `sweep_targets.sh`, hourly, via jod. Worktrees outlive their tasks, so even small targets accumulate. |

## Freeing space right now

```bash
"${CLAUDE_SKILL_DIR}/scripts/sweep_targets.sh"                    # report only
"${CLAUDE_SKILL_DIR}/scripts/sweep_targets.sh" --apply            # delete
"${CLAUDE_SKILL_DIR}/scripts/sweep_targets.sh" --apply --min-free-gb 0
```

Report-only is the default, so the first command is always safe to run. It
prints nothing and exits 0 when there is nothing to do.

`--min-free-gb 0` sweeps regardless of pressure. Reach for it when a build is
about to start and you want the room now, not when a threshold says so.

**Do not delete a `target/` by hand while agents are working.** The script's
whole value is the two checks a person skips under pressure: no compiler process
inside the directory, and no write to it in the last 90 minutes. A `target/` that
looks abandoned because its branch is merged is routinely one an agent is three
minutes into rebuilding.

## Scheduling it

```bash
"${CLAUDE_SKILL_DIR}/scripts/arm_schedule.sh" --dry-run   # see the commands
"${CLAUDE_SKILL_DIR}/scripts/arm_schedule.sh"             # arm it
```

The wiring is one idea, `jod monitor set --no-agent`:

> The script is the whole job: its stdout is the result and no model is ever
> woken. Empty stdout means stay quiet.

An hourly schedule that fired a *prompt* would wake a model every hour to
re-derive a decision already settled in shell — and would sometimes decide
differently. With `--no-agent` the hourly cost is a `df` and a `find`, and the
ledger gains an entry only in an hour that actually freed something.

Check and inspect without firing anything:

```bash
jod monitor check reclaim-disk    # run the probe, record nothing, start nothing
jod schedule log reclaim-disk     # what it has done
jod schedule pause reclaim-disk   # stop it without forgetting it
```

## What it will and will not delete

Deletes only directories named `target/` — or `node_modules/` with
`--with-node`, off by default because reinstalling costs a network round trip
rather than just CPU — that sit inside the repository, and only when **all** of:

- free space is under `--min-free-gb` (default 8; `0` disables the gate)
- nothing was written inside it for `--idle-minutes` (default 90)
- no `cargo`/`rustc`/`cc`/`ld`/`node` process has its cwd in that worktree, and
  none names the directory on its command line — which is what catches a `rustc`
  whose parent cargo lives elsewhere

It stops as soon as free space is back over the threshold, oldest directory
first, so a sweep deletes as little as it can rather than as much as it may. It
never touches tracked source: `target/` is gitignored, so the cost of a wrong
guess is compile time, not work.

## Protecting a specific path

`--skip GLOB` is repeatable and matches the repo-relative or absolute path:

```bash
"${CLAUDE_SKILL_DIR}/scripts/sweep_targets.sh" --apply --skip 'apps/ios/*'
```

Use it where re-creating the artifact costs more than CPU. `apps/ios/node_modules`
is the worked example: it is that session's only build artifact, and reinstalling
it needs the network — which is precisely what a full disk denies, so it would be
asked to reinstall at the one moment it cannot. It is protected twice over: by
`--skip`, and because `node_modules` needs `--with-node` at all, which the
scheduled probe never passes. **No hourly run can remove a `node_modules`.**

## Threshold changes

`--min-free-gb` and `--idle-minutes` are arguments, not edits. Re-running
`arm_schedule.sh` with new values replaces the monitor rather than adding a
second one:

```bash
"${CLAUDE_SKILL_DIR}/scripts/arm_schedule.sh" --min-free-gb 12 --idle-minutes 120
```

## Checking it still works

```bash
tests/reclaim-disk.test.sh
```

Builds a synthetic tree in a temp directory with a busy target, a
recently-written one and a stale one, and asserts the sweep takes exactly the
stale one. Offline, and it deletes nothing outside its own fixture.
