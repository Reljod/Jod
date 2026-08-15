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
Status: open · Owner: — · Severity: high — **this is the reported bug**

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
Status: open · Owner: — · Severity: high

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

## L3. `jod main` starts the chat in `$HOME`, whatever directory you run it in
Status: open · Owner: — · Severity: medium

Observed on a fresh `JOD_HOME`, run from a scratch repository: the pinned
conversation came back with `cwd = /home/reljod`, and `jod root ls` reported no
roots at all.

The two entry points disagree about what "here" means:

- `Command::Tui` (`cli/src/main.rs:2006`) uses `console_cwd(cwd)`, which falls
  back to `std::env::current_dir()`. Correct.
- `main_chat` (`cli/src/main.rs:2331`) uses
  `cwd.unwrap_or_else(jod_core::service::default_cwd)`, and `default_cwd`
  (`core/src/service.rs:1359`) returns `$HOME`. Wrong.

There is also no `ensure_launch_root` equivalent on the `jod main` path, so
unlike the TUI it adds no root either.

Fix: `main_chat` should use `console_cwd`, and should seed a root the way the
TUI does. One helper called by both, so they cannot drift again.

Check: fresh `JOD_HOME`, `jod main "hi"` from a scratch directory, assert
`conversations.cwd` is that directory and `jod root ls` lists it.

## L4. A console with no root cannot open work at all
Status: open · Owner: — · Severity: medium — real, but rarer than it looked

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
Status: open · Owner: — · Severity: low

Observed: `jod project current` exits 2 with "unrecognized subcommand". The MCP
tool `project_current` exists and the orchestrator's preamble tells the model to
call it, so the concept is real and only the CLI is missing it. Anyone debugging
which project the router picked has no way to ask from a terminal.

Fix: add the subcommand, printing the conversation's current project and how it
was resolved. `project_resolutions` already records the how.

## L6. `jod team list` where every other noun uses `ls`
Status: open · Owner: — · Severity: low

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
