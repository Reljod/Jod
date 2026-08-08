# Jod desktop

A Tauri v2 shell over [`jod-core`](../../crates/jod-core). Delegate a task, watch
the agent stream in, attach to its tmux session, kill it.

The shell is deliberately thin — every command in `src-tauri/src/lib.rs` is a
one-liner over `jod_core::Jod`. Logic lives in the core so the planned iOS client
and VPS daemon reuse it unchanged.

## Requirements

| Thing | Why | Check |
|---|---|---|
| `tmux` | every agent runs in its own session | `tmux -V` |
| Claude Code and/or OpenCode | the harness that actually thinks | `claude --version`, `opencode --version` |
| Rust ≥ 1.88 | Tauri v2's dependency tree | `rustc --version` |
| Node ≥ 20, pnpm | the frontend | `pnpm --version` |

The app finds harness binaries even when launched from Finder with a minimal
`PATH` — it searches `PATH`, then well-known locations (`~/.nvm/versions/node/*/bin`,
`~/.opencode/bin`, Homebrew). Override with `JOD_CLAUDE_BIN`, `JOD_OPENCODE_BIN`
or `JOD_TMUX_BIN`. Missing pieces show as a banner rather than a failed spawn.

## Run it

```bash
cd apps/desktop
pnpm install
pnpm tauri dev          # or: pnpm tauri build  →  src-tauri/target/release/bundle
```

## Without the GUI

The same core, driven from a terminal — useful for debugging a harness adapter:

```bash
cargo run -p jod-core --example delegate -- claude_code "Reply with exactly: PONG"
cargo run -p jod-core --example delegate -- open_code   "Reply with exactly: PONG"
```

`JOD_EXAMPLE_CWD` sets the working directory, `JOD_EXAMPLE_PERMISSION` is
`ask` (default), `accept_edits` or `bypass`.

## Where things go

Runtime state is plain files under `~/.jod` (override with `JOD_HOME`), so a run
stays readable long after the app is closed:

```
~/.jod/runs/<agent-id>/
  prompt.txt      the task, kept out of shell quoting entirely
  run.sh          the generated launcher tmux executes
  stream.jsonl    the harness's raw output
  agent.json      metadata
```

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
