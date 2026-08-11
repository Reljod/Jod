# `jod main` — end-to-end proof

Four instructions into one pinned chat, against a real `claude` binary and a
real store at `/tmp/jod-main4`. Every line below is copied from that store, not
from a unit test.

Reproduce with:

```sh
cargo build --release --bin jod
rm -rf /tmp/jod-main4
JOD_HOME=/tmp/jod-main4 ./target/release/jod main "<instruction>"
JOD_HOME=/tmp/jod-main4 ./target/release/jod main          # read it back
```

## The four routes

Each instruction was phrased to select a different branch, and the orchestrator
was told nothing about which one to take.

| instruction | tool it called | result in the store |
|---|---|---|
| "Every weekday at 8am Asia/Manila, sweep the open PRs and tell me what needs me." | `mcp__jod__schedule_create` | `weekday-pr-sweep`, `0 8 * * 1-5`, armed |
| "Also, keep working until the README explains what jod main does, then stop." | `mcp__jod__goal_create` | `readme-explains-jod-main`, running, budget $3, max 6 iterations |
| "Count how many Rust source files are in the jod repo and tell me." | `mcp__jod__delegate` | new run `31ca3ac4` named `count-rust-files-jod` |
| "Follow up on that Rust file count — also break it down by crate." | `mcp__jod__continue_agent` | `{"continued": "31ca3ac4…", "run_id": "b21e64f2…"}` |

`list_agents` was called first on every turn, as instructed.

## What the store holds afterwards

```
runs:
  1f0fc870 main                  completed
  e30c18f0 main                  completed
  b96975f4 main                  completed
  31ca3ac4 count-rust-files-jod  completed
  cf190e6d main                  completed
  b21e64f2 count-rust-files-jod  completed

tool calls in the main chat:
  mcp__jod__schedule_create  {"cron":"0 8 * * 1-5","name":"weekday-pr-sweep",…}
  mcp__jod__recall           {"query":"jod main README documentation"}
  mcp__jod__goal_create      {"budget_usd":3,"max_iterations":6,…}
  mcp__jod__delegate         {"name":"count-rust-files-jod",…}
  mcp__jod__continue_agent   {"prompt":"Follow-up on the same 47 `.rs` file count…"}
```

Two runs are named `count-rust-files-jod` because the fourth instruction
*continued* the first rather than starting a rival: `continue_agent` resumes the
harness session, so `b21e64f2` already knew the count was 47 and was asked only
for the breakdown. That is the claim the run's own prompt substantiates — the
number appears in the follow-up prompt because the orchestrator had it from the
first agent, not because it recounted.

## Non-blocking

The command returns before the work does:

```
$ JOD_HOME=/tmp/jod-main ./target/release/jod main "Every weekday at 8am…"
→ aa955d43 · handed to the orchestrator
```

Sent and returned within the same epoch second (1786406762 → 1786406762), with
the run still `running` for minutes afterwards.

## What the chat looks like read back

```
    1 › Every weekday at 8am Asia/Manila, sweep the open PRs and tell me what needs me.
    2 └ [ { "run_id": "1f0fc870-…", "name": "main", "harness": "claude_c…
    4 ⚙ schedule_create weekday-pr-sweep
    5 └ { "name": "weekday-pr-sweep", "next_fire_at_ms": 1786492800000, "state": "armed" }
    6   Armed a new schedule, `weekday-pr-sweep` — `0 8 * * 1-5` in Asia/Manila, running in /home…
    7 › Also, keep working until the README explains what jod main does, then stop.
   ...
  15 ⚙ goal_create readme-explains-jod-main
  16 └ { "name": "readme-explains-jod-main", "state": "running" }

set in motion:
  Aug 11 08:15 (53s ago)  orchestrate  e30c18f0
  Aug 11 08:07 (9m04s ago) orchestrate  1f0fc870
```

Message ids are shown because `jod conv revert` and `jod conv fork` take one.

## Four bugs this found, in order

None were visible to the test suite, which was green throughout.

1. **`Ask` is plan mode, and plan mode refuses the MCP write tools.** The
   orchestrator called `schedule_list`, `list_agents` and `recall`, reached for
   `ExitPlanMode`, and wrote a plan file instead of arming anything.
2. **The `mcp__jod` allowlist entry lived inside the `Ask` arm**, so changing
   the permission mode silently revoked every Jod tool. Four consecutive
   `"requested permissions to use mcp__jod__schedule_create, but you haven't
   granted it yet"`. Regression test: `granted_tools_survive_any_permission_mode`.
3. **`spawn_agent` minted a second conversation**, unpinned and titled with the
   preamble, holding the whole transcript — while the pinned `main` conversation
   stayed empty and `jod main` truthfully reported nothing there. Fixed with
   `spawn_agent_in(.., RunConversation::Existing)`.
4. **The framing was recorded as the user's first chat turn**, because there was
   no system-prompt seam. Added `SpawnRequest::system`; Claude Code takes
   `--append-system-prompt`, and harnesses without one have it folded into the
   prompt by the runner.

A fifth was in the test harness rather than the product: the polling script took
element `[0]` of `jod ls --json` assuming newest-first ordering, which it is not,
and so reported a run "settled" while it was still running. Sorting by
`created_at_ms` fixed it. Worth recording because the wrong instrument briefly
produced a wrong conclusion about the goal branch.
