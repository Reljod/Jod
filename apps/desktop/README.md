# Jod desktop

A Tauri v2 shell over [`jod-core`](../../crates/jod-core). Delegate a task, watch
the agent stream in, follow it in a real terminal, kill it.

The shell is deliberately thin — every command in `src-tauri/src/lib.rs` is a
one-liner over `jod_core::Jod`. Logic lives in the core so the planned iOS client
and VPS daemon reuse it unchanged.

## Requirements

| Thing | Why | Check |
|---|---|---|
| `jod-run` | supervises every agent; ships beside `jod` | `jod-run` on `PATH` |
| Claude Code and/or OpenCode | the harness that actually thinks | `claude --version`, `opencode --version` |
| Rust ≥ 1.88 | Tauri v2's dependency tree | `rustc --version` |
| Node ≥ 20, pnpm | the frontend | `pnpm --version` |

The app finds harness binaries even when launched from Finder with a minimal
`PATH` — it searches `PATH`, then well-known locations (`~/.nvm/versions/node/*/bin`,
`~/.opencode/bin`, Homebrew). Override with `JOD_CLAUDE_BIN`, `JOD_OPENCODE_BIN`
or `JOD_SUPERVISOR_BIN`. The supervisor is looked for beside the running
executable first, so a bundled app uses the copy it shipped with rather than an
older one earlier in `PATH`. Missing pieces show as a banner rather than a
failed spawn.

## Run it

```bash
cd apps/desktop
pnpm install
pnpm tauri dev          # or: pnpm tauri build  →  src-tauri/target/release/bundle
```

## Watching an agent

**Watch** opens a new window in iTerm2 (or Terminal.app if iTerm2 is not
installed) already following the agent. By hand it is one command, wherever you
are:

```sh
jod watch <id>
```

**It works on a finished run too**, replaying the transcript rather than
refusing — the run is read out of `~/.jod/jod.db`, not out of a live terminal.
That is also why the same view reaches the web client and the phone
([why](../../docs/decisions.md#a-run-is-a-detached-process-group-and-the-database-is-its-only-transport)).

**Kill** is the button that needs a live run: it signals the run's process
group, which stops the harness and anything the harness started.

Sessions therefore stay until you close them: **Close session** in the app, or
`Ctrl-D` in the pane.

## Without the GUI

The same core, driven from a terminal — useful for debugging a harness adapter:

```bash
cargo run -p jod-core --example delegate -- claude_code "Reply with exactly: PONG"
cargo run -p jod-core --example delegate -- open_code   "Reply with exactly: PONG"
```

`JOD_EXAMPLE_CWD` sets the working directory, `JOD_EXAMPLE_PERMISSION` is
`ask` (default), `accept_edits` or `bypass`.

## Where things go

A run's transcript lives in `~/.jod/jod.db`, because it is contended state that
several processes append to and read. What is left on disk is the record of the
launch (override the location with `JOD_HOME`):

```
~/.jod/runs/<agent-id>/
  prompt.txt      the task, as it was asked
  spawn.json      exactly what was launched, and where its events go
  supervisor.log  the supervisor's own stdout/stderr, for when it fails early
  agent.json      metadata
```

Read a run back with `jod watch <id>`, which works whether or not it is still
going.

## Permissions

| Mode | Claude Code | OpenCode |
|---|---|---|
| Ask | tool calls needing approval are refused | no auto-approval |
| Accept edits | `--permission-mode acceptEdits` | no auto-approval¹ |
| Bypass | `--dangerously-skip-permissions` | `--auto` |

¹ OpenCode has a single auto-approve switch and cannot separate edits from other
tool calls, so "Accept edits" leaves it off rather than granting more than asked.

Use **Bypass** only in a throwaway worktree.

## Icon

`src-tauri/icons/` is generated. To change it:

```bash
python3 scripts/make_icon.py /tmp/jod-icon.png
pnpm tauri icon /tmp/jod-icon.png
```
