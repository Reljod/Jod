# Launch, roots and the console's working directory

How this was tested: the binary built from this branch, plus the installed
`jod 0.2.3`, driven under a throwaway `JOD_HOME` with the TUI running inside
tmux on its own socket. The live `~/.jod/jod.db` was read but never written.

**Read this first, because it corrects an earlier version of this file.** The
first draft claimed `jod tui` does not add the directory you launch it in. That
is wrong, and testing is what showed it. `ensure_launch_root`
(`cli/src/tui/mod.rs:5509`) already adds the launch directory as a read-only
root on every console launch, and it works:

```
launch 1, in tui-repo    → root: tui-repo                (position 0)
launch 2, in tui-repo-b  → roots: tui-repo, tui-repo-b   (positions 0, 1)
```

Both binaries behave the same. It landed in a625bec, "open the console where it
was launched, and say so" (#78). Nobody should re-fix this.

The real fault is somewhere else, and it is below.

---

## L1. The console Reljod actually uses is rooted at `$HOME`, and never in a repository
Status: **open — needs Reljod's decision between three options** · Severity: high — **this is the reported bug**

Reljod does not run `jod tui` in a directory. He attaches to a console that was
already running. `jod-tui.service` starts it at boot:

```
ExecStart=/usr/bin/tmux -L jod new-session -d -s jod /usr/local/bin/jod-console
```

There is no `WorkingDirectory=` in the unit, so systemd starts it in `$HOME`.
Confirmed on the live box:

```
$ readlink /proc/3865447/cwd        # the running console
/home/reljod
$ test -d /home/reljod/.git         # is that a repository?
HOME is NOT a git repo
```

`ensure_launch_root` then does its job perfectly and adds `/home/reljod` as the
console's root. That is why the live main chat has exactly one root,
`/home/reljod`, and why every repository has to be added by hand afterwards.
The feature is not broken; it is being handed the wrong directory once, at
boot, and the main chat is a singleton so that first answer sticks for ever.

Fix — needs a decision, so do not just pick one:

1. Give the service a `WorkingDirectory=`. Cheapest, but it only moves the
   problem: one directory is still hard-coded, and Reljod works in several.
2. Give the console a way to change where it is standing — a `/cd` command that
   adds a root and makes it the one new work defaults to. This is the one that
   matches how he actually works, and it composes with L2.
3. Let `jod tui` in a directory adopt that directory as the *default* rather
   than merely adding it. Related to L2 and probably the same change.

Check: on a box whose console started in `$HOME`, opening work about a
repository must not land in `$HOME`.

## L2. New work defaults to the oldest root, which is the wrong one for ever
Status: **blocked on L1's decision** · Severity: high

**Do not start this yet, and do not treat it as merely unclaimed.** It reads
like an independent fix and it is not. Two of L1's three options *are* this
fix:

- L1 option 2 — a `/cd` that adds a root **and makes it the one new work
  defaults to** — defines the default-picking mechanism, and L2 is then
  implemented by it.
- L1 option 3 — a console adopts its directory as the default rather than
  merely adding it — likewise. L1 already says of this option "related to L2
  and probably the same change".
- Only L1 option 1 — setting `WorkingDirectory=` on the service — leaves L2
  standing alone and still needing one of its own three options below.

So in two of three branches, implementing L2 now produces work that is
discarded or has to be unpicked. The right order is: Reljod decides L1, then L2
is either already done or becomes a small, well-specified change.

What is *not* blocked is the observation itself, which is settled and needs no
decision: `roots.first()` against an append-only order means the oldest answer
wins for ever, and it fails silently because an existing directory is a valid
answer.

`open_work` with no explicit `checkout` takes the **first** root
(`core/src/mcp.rs:2149`, `roots.first()`), and roots are ordered by `position`,
which is append-only — `add_root` deliberately never reorders, so the first
root a conversation ever got stays first for the life of the conversation
(`core/src/roots.rs:173`).

Chained with L1, every work the resident console opens defaults its checkout to
`/home/reljod`: not a repository, not what he meant, and no error, because a
directory that exists is a perfectly valid answer to `roots.first()`.

This is the half that turns L1 from an annoyance into the reported symptom. Add
the repository as a root and it still is not the default, because `/home/reljod`
got there first.

Fix: the default checkout should be the directory the console is standing in,
not the oldest root it happens to hold. Options worth weighing: track a
"current" root explicitly; prefer the most recently added; or prefer a root
that is actually a git repository over one that is not. The last one is the
smallest change and fixes the observed case, but it is a heuristic and will be
wrong for a non-git project, so say so if you choose it.

Check: a conversation with roots `[$HOME, some-repo]` must default new work to
`some-repo`.

## L3. Every entry point except the TUI starts in `$HOME`, wherever you ran it
Status: **partially fixed — one of seven sites · check run and it fails** · Severity: medium ·
#122 fixed `main_chat` (`jod main`) only. **Six sites still answer `$HOME`:**
`cli/src/main.rs` lines 1701, 1702, 2147, 2233, 3815 and 4474 — `jod run`
among them. A fix is in progress.

> Marked "fixed" here for a while, and it was not. #122 did exactly what its
> own brief asked and its pull request was honest about its scope; the
> narrowing happened silently between this task and that brief, and the merge
> then marked the whole task done. Verified against main before correcting:
> `console_cwd` has two callers, `:2006` (the TUI) and `:2338` (from #122).
>
> The countermeasure, worth more than this entry: when a task names N sites,
> the brief says N — and if it is deliberately scoped smaller, the status must
> not read fixed when it merges.

Observed twice on a fresh `JOD_HOME`, both times from inside a scratch
repository:

```
$ jod main "hi"            # run from …/orch-repo
conversations.cwd = /home/reljod
conversation_roots = (empty)

$ jod run --detach -n probe "hi"   # run from …/orch-repo
conversations.cwd = /home/reljod
conversation_roots = (empty)
```

`console_cwd` (`cli/src/main.rs:4317`) is the function that means "the
directory you are standing in" — it falls back to `std::env::current_dir()`.
Grepping its call sites, **it is used in exactly one place**: `Command::Tui`
at `cli/src/main.rs:2006`.

Every other entry point calls `jod_core::service::default_cwd` instead, which
returns `$HOME` (`core/src/service.rs:1359`): `jod run`
(`cli/src/main.rs:1701`, `:1702`), `main_chat` (`:2331`), and four more at
`:2147`, `:2233`, `:3787` and `:4396`.

The distinction was understood when it was written — the doc comment on
`console_cwd` at `cli/src/main.rs:4301` opens "Not
`jod_core::service::default_cwd`, which answers `$HOME`". It was simply only
applied to the TUI.

Neither is there an `ensure_launch_root` equivalent off the TUI path, so these
entry points seed no root either, which is what makes L4 reachable.

Fix: use `console_cwd` at every site where the answer should be "here", and
seed the launch root from one helper both the TUI and the others call, so they
cannot drift again. Check each of the seven sites individually — `$HOME` may
genuinely be right for one or two of them, and this should not be a blind
replace.

Check: fresh `JOD_HOME`; run `jod main "hi"` and `jod run` from a scratch
directory; assert `conversations.cwd` is that directory in both cases and that
`jod root ls` lists it.

**Check run against main `730e63b`. It fails, on the half that was never
fixed.**

```
half 1, jod main:  cwd = /tmp/jod-verify-repo-439006   root listed   PASS
half 2, jod run:   cwd = /home/reljod                  roots = []    FAIL
```

`jod root ls` after `jod run` answers "no conversation given and there is no
main chat yet". So the status above is correct and is now verified rather than
inferred.

## L4. A console with no root cannot open work at all
Status: **verified fixed — check run against main, passes** · Severity was: medium

Not inferred from #122. The stated check was run against main `730e63b`: a
fresh `JOD_HOME`, `jod main` inside a scratch repository, one instruction that
should open work. A work opened — `6437fd76`, "add CONTRIBUTING.md with 'PRs
welcome.' and commit" — its session conversation carries a `work_id`, and every
run completed. `open_work` succeeded rather than refusing, because `jod main`
now seeds a root (`/tmp/jod-l4-repo-448730`, confirmed present before the
instruction went out).

`open_work` with no `checkout` and no roots refuses (`core/src/mcp.rs:2149`):

> say which directory this work happens in — `checkout` — because this session
> has no roots of its own to inherit one from

The refusal itself is right; defaulting to whatever directory the daemon was
started in would be worse. What makes it reachable is L3: a console opened with
`jod main` rather than `jod tui` has no roots, so the first instruction it ever
receives cannot route to `open_work` — and the preamble calls `open_work` "the
usual answer for anything about code".

Not reachable through the TUI, which is why this is ranked below L1 and L2.

Fix: falls out of L3. Keep the refusal.

Check: fresh `JOD_HOME`, `jod main` in a repository, one instruction that
should open work, assert it opens rather than refuses.

## L5. `jod project current` does not exist, though the MCP tool does
Status: **fixed — merged as #165** · Severity was: low

Observed: `jod project current` exits 2 with "unrecognized subcommand". The MCP
tool `project_current` exists and the orchestrator's preamble tells the model to
call it, so the concept is real and only the CLI is missing it. Anyone debugging
which project the router picked has no way to ask from a terminal.

Fix: add the subcommand, printing the conversation's current project and how it
was resolved. `project_resolutions` already records the how.

## L7. Re-adding a root you already hold silently takes write access away
Status: **fixed — merged as #155** · Severity was: high

**The wider worry was checked and came back negative, which is worth keeping.**
This task asked whether every `ON CONFLICT DO UPDATE` in the codebase shared the
shape — an upsert guarding some columns and not others. All sixteen upsert sites
were audited: thirteen `DO UPDATE`, three `DO NOTHING`, and none of the
remaining ones needed a fix. So L7 and P1 were two isolated bugs rather than a
pattern.

Recorded because a negative result is a finding. Without it written down, the
next person reads "worth checking every `ON CONFLICT DO UPDATE`" as outstanding
work and audits sixteen sites a second time.

`Store::add_root` (`core/src/roots.rs:179`) upserts:

```sql
ON CONFLICT(conversation_id, path) DO UPDATE SET
  writable = excluded.writable,
  origin   = excluded.origin
```

`position` is deliberately protected — the comment above it explains that a
second add must not move a root to the end of the user's order. `writable` is
not protected at all. So calling `add_root` with `NewRoot::reading` for a
directory the conversation already holds as a **lease** flips it back to
read-only, and the session loses the write it had. Silently: `add_root` returns
the row and reports success.

The upsert's *other* direction is deliberate and tested — re-adding as a lease
is how a read-only root becomes writable
(`a_root_added_twice_does_not_duplicate_the_row`). It is the read direction
that nobody meant.

Found by the agent that fixed L3, which hit it while making `jod main` grant
its launch directory. The TUI never sees it because `ensure_launch_root` grants
once per process and remembers it in a set; `jod main` is a single command, so
running it twice inside a claimed worktree revokes the write.

The L3 fix added a guard at its own call site. **That fixes `jod main` and not
`add_root`**, so the next caller who adds a root twice gets the same surprise.

This is the same shape as P1 in
[`30-project-managers.md`](30-project-managers.md): an upsert that guards some
columns and not others reads as safe when it is not. Both were found the same
week. Worth checking every `ON CONFLICT DO UPDATE` in the codebase for the same
pattern rather than fixing these two and waiting for the third.

Relevant to L1 and L2, which both involve calling `add_root` more than once for
the same directory.

Fix: never let a re-add downgrade `writable`. Either protect the column the way
`position` is protected, or make widening explicit and separate from adding.
`set_root_writable` already exists as the sanctioned way to change it.

**Do not reach for `COALESCE` here.** It is the obvious move, because
`COALESCE` is what protects `remote` in the neighbouring `add_project` upsert —
and it would not work. `COALESCE` returns the first non-NULL argument, and
`writable` is a boolean that is `0` or `1` and never NULL, so the guard would
never fire while looking exactly like a guard that had. Two readers gave that
advice on P1 in [`30-project-managers.md`](30-project-managers.md), where the
columns default to an empty list and an empty string; it was wrong there for
the same reason, and only running it caught it. `CASE` was the answer there.

Fixing this one deserves a test that is red before the change, not just green
after — that is what would have caught the P1 near-miss.

Check: add a root as a lease, add the same path again as reading, assert it is
still writable.

## L8. `merge_pr.sh` exits non-zero after a merge that succeeded
Status: **fix open as #128, waiting on a human** · Severity: medium

Run from inside a git worktree, `merge_pr.sh <n> --ready` merges the pull
request and *then* exits 1, with:

```
failed to run git: fatal: 'main' is already used by worktree at '/home/reljod/repo/Jod'
```

That is `gh pr merge --delete-branch` failing its local cleanup step, after the
merge has already landed. The remote branch has to be deleted by hand.

Seen four times across two sessions, so it reproduces rather than being a
one-off. The fourth was #124, this very task list, and the full output is worth
having because it shows how convincing the failure looks:

```
  base     main is 0 commit(s) ahead of this branch
  triage   auto-merge (categories: docs)
✓ Pull request Reljod/Jod#124 is marked as "ready for review"
Marked PR #124 ready for review.
failed to run git: fatal: 'main' is already used by worktree at '/home/reljod/repo/Jod'
```

The merge had already succeeded — main is `0289cbb` — and the error is the last
line on screen, immediately after a line saying the pull request was marked
ready. An agent reading only the tail would conclude the merge failed at the
final step. It happens whenever the primary checkout holds `main` and the merge is
run from a worktree, which is how every agent on this box works.

Why it matters more than a cosmetic error: the charter tells agents to run
`merge_pr.sh` and **obey its exit code**. An agent that reads exit 1 as a
refusal will either retry a merge that already happened or report a finished
task as blocked. Both are worse than the original problem.

**A second signal from the same gate cannot be taken at face value either.**
It refused a pull request with "a destructive or privilege-escalating command is
introduced (1 added line)". The matching line was a **doc comment in a test** —
``/// Deleting a goal was a bare `DELETE FROM goals`, so an iteration already``
— English prose describing the *old* implementation, caught by the pattern
`DELETE[[:space:]]+FROM`. The real SQL was unchanged from main and is not what
fired.

The agent **did not reword the comment to get past it**, and that restraint is
the behaviour to copy: rewording prose to satisfy a pattern is working around a
check, not passing it. It left the pull request open and named the reason.

On substance the gate's refusals have been right every time. This one was right
in form and wrong in fact, which is a different failure from the cleanup-exit
bug below but the same subject — a gate whose signals need reading rather than
obeying.

**A security classifier has now made the same mistake, which is the strongest
statement of the cost.** An automated security review flagged an agent for
merging without review. It had not: it ran the gate, the gate categorised the
pull request `merge:auto` and merged it, and the script *then* exited 1 on the
local branch cleanup. The classifier read that exit code as a refusal that had
been bypassed.

That is a third distinct victim of one exit code meaning both "I refused, fix
your branch" and "I already merged, ignore me" — after agents reporting finished
work as blocked, and a session falling into it while merging the document
describing it. It is the first time the ambiguity has produced a **false
accusation against correct behaviour** rather than merely confusing someone.

The decisive evidence is the label, not the exit code: a real refusal leaves the
pull request open and labelled `merge:human`, which is what happened to #144,
#154 and #142. Anything diagnosing this should read the label.

**Awareness cannot fix this, so the script must.** The fourth occurrence was
the pull request for the document describing the trap — a session that had read
the write-up, and had itself filed the task, still merged through it and had to
check the PR's real state to be sure. Four for four across two sessions. Any
fix of the form "tell agents to check the state afterwards" has now failed its
own test.

Fix: the cleanup step should not fail the script when the merge succeeded, and
should not try to check out `main` from a worktree in the first place. Deleting
the remote branch does not need a local checkout.

**And the fix cannot be only about the exit code.** If the last thing printed
is a bare `git` error, the output still reads as failure to anyone scanning the
tail — which is what a person does, and what an agent summarising a long
command does. The final line must say what actually happened: the merge
succeeded, and the local branch may need deleting by hand.

Do not weaken the script generally to achieve this. It deliberately makes
`gh pr merge` unreachable so a model cannot merge by going around the gate, and
a script that swallowed errors broadly would trade a reporting bug for a hole
in the enforcement machinery.

Check: run it from a worktree against a mergeable pull request and assert exit 0.

## L9. The tree is not rustfmt-clean, and nothing anywhere checks
Status: **open — needs Reljod's decision** · Severity: medium

> **This task's original premise was wrong and I have rewritten it.** It said
> local rustfmt "disagrees with CI's" and proposed pinning the version so the
> two agree. **There is nothing to agree with: CI has no formatting check.**
> Verified read-only against main — `.github/workflows/tests.yml` runs only the
> shell suites (`*.test.sh` and `tests/test.sh`), and no workflow in
> `.github/workflows/` mentions `cargo fmt` or `rustfmt` at all.
>
> Left uncorrected, this sends whoever picks it up to compare two rustfmt
> versions and discover they cannot — an hour in the wrong place, and the same
> shape as an error that names the harness binary for a bad project path.

`cargo fmt --check` on this box reports differences across most of the
codebase, while CI's formatting check passes. The rustfmt on this machine is a
different version from the one CI runs.

The hazard is not the disagreement, it is what an agent does with it. Any agent
that helpfully runs `cargo fmt` and commits the result produces an enormous
diff touching files it never meant to change, burying its actual work and
making the change unreviewable. It will look like it did something reasonable.

Two agents have now had to be told explicitly to leave the pre-existing
differences alone and to confirm none of them fell in the lines they touched.
That should not depend on somebody remembering to say it.

The hazard is unchanged and still real: an agent that helpfully runs
`cargo fmt` and commits produces an enormous diff burying its actual work, and
enough agents have now had to be told by hand that it should not depend on
somebody remembering.

But the fix is not pinning a version. It is one of two shapes, and which one is
Reljod's call:

1. **Make the tree rustfmt-clean and add a gate.** Large, touches dozens of
   pre-existing files, and buys a guarantee.
2. **Say plainly that the tree is not formatted and nobody should run it.** A
   note in the charter. Cheap, and honest about the state rather than changing
   it.

Check: depends on the shape chosen. Under (1), `cargo fmt --check` is clean and
CI fails when it is not. Under (2), the charter says so and there is nothing to
run — which would make this the second task whose honest check is a note rather
than a command, after P4.

## L6. `jod team list` where every other noun uses `ls`
Status: **fixed — merged as #174** · Severity was: low

Aliased rather than renamed: `ls` is canonical, `list` is a hidden alias, so
nothing already written stops working. `team` was confirmed the only noun out of
step, by enumerating all fourteen listings.

Observed: `jod ls`, `jod work ls`, `jod schedule ls`, `jod goal ls` and
`jod project ls` all work. `jod team ls` exits 2 and suggests `list`.

Fix: accept `ls` on `team`, keeping `list` as an alias so nothing breaks.

---

## Scenarios run

| # | Scenario | Expected | Actual | |
|---|---|---|---|---|
| 1 | `jod root ls`, fresh install, no chat | a clear empty state | names the missing chat and how to start one | pass |
| 2 | `jod main` with no instruction | shows the chat | "the main chat is empty — …" | pass |
| 3 | `jod main` from a repository | chat cwd is that repository | cwd was `$HOME` | **fail — L3** |
| 4 | `jod main`, then `jod root ls` | the launch directory is a root | no roots at all | **fail — L3** |
| 5 | `jod tui` in a repository, fresh `JOD_HOME` | that directory becomes a root | it did, position 0 | pass |
| 6 | `jod tui` in a second directory, same state | both directories are roots | both, positions 0 and 1 | pass |
| 7 | Same, on the installed `jod 0.2.3` | same behaviour | same | pass |
| 8 | The live resident console's directory | a repository | `$HOME`, which is not one | **fail — L1** |
| 9 | Default checkout for new work | the repository in front of you | the oldest root, `$HOME` | **fail — L2** |
| 10 | `jod project current` | prints the current project | unrecognized subcommand | **fail — L5** |
| 11 | `jod team ls` | lists teams | unrecognized subcommand | **fail — L6** |
| 12 | Adding the same project twice | idempotent | idempotent | pass |
| 13 | Adding the same root twice | no duplicate row, position kept | covered by unit tests in `roots.rs` | pass |
