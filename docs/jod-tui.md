# The Jod TUI — the goal

**Jod's primary client is a terminal UI that matches OpenCode's TUI feature for
feature, runs Claude-Code-style agent teams, and streams reasoning live — and
does all three identically across Claude Code, OpenCode and Antigravity.**

That sentence is the goal. This document says what it means concretely, what is
already true, and what has to be built. Reasoning for the two load-bearing
choices lives in [`decisions.md`](decisions.md); the surrounding architecture is
[`jod-system.md`](jod-system.md).

**Status: the spine is built and runs.** `apps/tui` is a working ratatui client
— fleet list, live event stream with reasoning, spawn, kill, multi-turn
follow-up, and a team view — driving all three harnesses through `jod-core`.
Antigravity is a first-class harness, and agent teams have a working bus in
`jod-core::team`.

What is *not* done is the long tail of OpenCode's TUI: inline permission
prompts, file attachment, `@`-references, theming, its config surface, and
per-member tmux panes for a team. The milestones at the end of this document
mark what landed and what remains.

---

## Why this is reachable

The goal reads like a rewrite and isn't, because of one verified fact: **all
three harnesses already expose streaming JSON, session resume, and an
interactive mode.** Jod does not have to drive a pseudo-terminal or reimplement
a chat client. A multi-turn conversation is a headless spawn plus a session id.

Verified 2026-08-09 by running `--help` against each installed binary, and
`agy -p … --output-format stream-json` against a live run.

| | Claude Code | OpenCode | Antigravity |
|---|---|---|---|
| binary | `claude` | `opencode` | `agy` |
| headless | `-p` | `run` | `-p` / `--print` |
| stream | `--output-format stream-json` | `--format json` | `--output-format stream-json` |
| continue last | `-c, --continue` | `-c, --continue` | `-c, --continue` |
| resume by id | `-r, --resume <id>` | `-s, --session <id>` | `--conversation <id>` |
| interactive | default | `-i, --interactive` | `-i, --prompt-interactive` |
| model | `--model` | `-m, --model` | `--model` |
| reasoning effort | model-side | `--variant` | `--effort low\|medium\|high` |
| auto-approve | `--dangerously-skip-permissions` | `--auto` | `--dangerously-skip-permissions` |
| cwd | process cwd | `--dir` | process cwd + `--add-dir` |

The shapes differ; the capabilities don't. That is exactly the situation the
`Harness` trait exists for.

## Reasoning: two thirds already work

`AgentEvent::Thinking` has been in the event vocabulary since the first version,
and two adapters already emit it:

- `crates/jod-core/src/harness/claude.rs:101` — Claude's `thinking` blocks.
- `crates/jod-core/src/harness/opencode.rs:85` — OpenCode's `reasoning` parts.
  OpenCode also has an explicit `--thinking` flag.

So "see the reasoning while the harness runs" is a **rendering** problem for two
harnesses, not a parsing one. Nothing in the core has to change for them.

**Antigravity is the exception, and it is the one real risk in this goal.** A
live `agy` run reports `thinking_tokens: 216` in its usage block and emits no
reasoning text anywhere in the stream. There is no `--thinking` equivalent in
`agy --help`. Until that changes, Antigravity can show *that* the model is
reasoning and how much, but not *what* it reasoned. Do not design the TUI so
that a missing reasoning stream looks like a broken pane — see "Degrade
visibly", below.

## What the Antigravity adapter has to handle

From the live run, and different enough from the other two to write down:

- **Deltas, not blocks.** `step_update` carries `text_delta` fragments keyed by
  `step_index`, with `state` going `ACTIVE` → `DONE`. Claude emits whole blocks;
  OpenCode emits completed parts. The `agy` adapter must accumulate per
  `step_index` or it will emit the same prose twice.
- **`step_type` is a closed vocabulary that is not closed in practice.** The
  observed run produced `user_input`, `agent_response`, `checkpoint` — and
  `unknown`. `AgentEvent::Raw` earns its keep here.
- **Tool calls arrive as `tool_info`** `{name, parameters, output, error}` on a
  `tool` step, so call and result land together rather than needing an id map
  the way Claude's do.
- **The terminal `result` event is real**, unlike OpenCode's, and carries
  `status` (`SUCCESS`/`ERROR`/`CANCELED`/`INTERRUPTED`/…), `num_turns` and
  usage. The runner still owns "the run is over" — but `finalize` gets more to
  work with.
- **No cost field.** Usage is tokens only: `input`, `output`, `thinking`,
  `cache_read`, `total`. `Usage.cost_usd` stays `None` for Antigravity, and the
  TUI must not render a blank where a number belongs.

## Agent teams belong to Jod, not to a harness

All three harnesses are growing their own team feature. Antigravity ships
`define_subagent`, `invoke_subagent`, `manage_subagents`, `send_message`,
`manage_inbox` and `manage_task` as built-in tools today. OpenCode has agent
teams behind `OPENCODE_EXPERIMENTAL_AGENT_TEAMS`. Claude Code shipped the
original.

Jod uses none of them as the mechanism. → [why](decisions.md)

The team model Jod implements is the one already sketched as A2A in
[`jod-system.md`](jod-system.md), which is — independently — almost exactly the
design OpenCode arrived at:

- **Append-only JSONL inbox per member.** One `appendFile` per message, O(1),
  and an audit trail that survives Jod not running. Jod's tailer already follows
  append-only files.
- **Session injection.** A delivered message becomes a synthetic user turn in
  the recipient's next prompt — which, given resume-by-id, works on all three
  harnesses with no harness support at all.
- **Event-driven, not polled.** A message to an idle member wakes it by
  resuming its session, rather than every member burning a loop on a poll.
- **A shared task list with atomic claiming**, so two members can't take the
  same work.
- **Two-level state:** a coarse member lifecycle (ready / busy / shutting down /
  shutdown / error) for recovery logic, and a finer execution status for the UI.

Because the seam is Jod's rather than a harness's, a Jod team can do the thing
none of the three can do alone: **a lead on Claude Code with teammates on
Antigravity and OpenCode, in one team, on one message bus.**

## What "OpenCode-grade" means

The bar, concretely. A pane-per-member layout with `tmux` underneath is already
how Jod runs agents, so the TUI is a client over the existing transport.

**Conversation**
- Multi-turn chat against a live session; history scrollback; interrupt a turn.
- Permission prompts answered inline, mapped onto each harness's policy.
- Model and reasoning-effort switching per session.
- File attachment and `@`-style references where the harness supports them.

**The run, live**
- Reasoning streamed as it arrives, collapsible, visually distinct from prose.
- Tool calls with arguments and results, truncated by `event::summarize`.
- Token and cost counters that update mid-run.
- Raw events shown, never dropped.

**The fleet**
- Every agent, its harness, status, task summary and spend, in one list.
- Switch the TUI into any member's session; attach to its tmux pane.
- Spawn, kill, and resume — including runs that outlived the last TUI process.

**Teams**
- Form a team, assign roles, watch the message bus.
- Per-member panes; the inbox as a first-class view.
- The shared task list, with who claimed what.

## What has to change in `jod-core`

The core has no UI and never will — anything the TUI needs that is logic belongs
below the seam. In rough dependency order:

1. **`HarnessKind::Antigravity`** — a third variant, `discovery::find_binary`
   entry for `agy` (`JOD_AGY_BIN`), and `harness/antigravity.rs`. One file,
   which is the claim the seam has always made; this is the test of it.
2. **Session continuation in `SpawnRequest`.** Today a spawn is one-shot. Add
   the session id and a "continue this" mode, and map it onto the three resume
   flags above. This is what turns headless runs into a conversation.
3. **Interactive turns** — send a follow-up to a live agent and stream the
   reply, rather than spawn-and-wait.
4. **Permission round-trips.** Currently `PermissionPolicy` is set once at
   spawn. Answering a prompt mid-run needs the harness's interactive path.
5. **Team primitives** — inbox/outbox, the shared task list, member state,
   auto-wake. Below the seam, so the desktop app and a future iOS client get
   teams for free.
6. **Run persistence and reattach** — already item 3 on the roadmap, and a hard
   prerequisite for a TUI you can quit and reopen.

## Design rules for the TUI

- **The core has no UI; the TUI has no logic.** If the TUI needs to compute
  something, it belongs in `jod-core`.
- **Degrade visibly.** A harness that cannot do a thing — Antigravity and
  reasoning text, OpenCode and per-turn cost — says so in place. It never shows
  an empty pane that reads as a bug, and it never fabricates a value.
- **One vocabulary.** The TUI renders `AgentEvent`. It never learns which
  harness produced one, except to label it.
- **Unknown input is surfaced.** `Raw` gets a real rendering, not a swallow.
- **tmux stays the transport.** The TUI is a view over sessions that outlive it.

## Non-goals

- Reimplementing a terminal emulator. `tmux attach` already works.
- A model client in `jod-core`. Never. → [why](decisions.md)
- Replacing `apps/desktop`. It keeps working over the same core.
- Matching OpenCode's *theming* and config surface. Feature parity is about
  capability, not chrome.

## Milestones

1. ~~**Antigravity adapter** — `agy` behind the existing seam, with delta
   accumulation and `Raw` handling.~~ **done** — `harness/antigravity.rs`. The
   "one file per harness" claim held: nothing above the seam changed.
2. ~~**TUI skeleton** — fleet list plus event stream, reasoning rendered live.~~
   **done** — `apps/tui`.
3. ~~**Sessions** — resume-by-id in the core; multi-turn conversation in the
   TUI.~~ **done** — `SpawnRequest::resume`, and `i` sends a follow-up turn.
4. **Interaction** — inline permission prompts, interrupt, model switching.
   *Not started.* Permission answers need the harnesses' interactive paths,
   which is the one capability the headless seam does not cover.
5. ~~**Teams** — inbox, shared tasks, member state.~~ **done** —
   `jod-core::team`. Auto-wake and per-member tmux panes remain.
6. **Cross-harness teams** — *the mechanism is done and tested*
   (`one_team_can_span_every_harness`); what remains is the orchestration that
   drives a lead and its teammates through it.

### What live testing changed

Three things only a real run could have found, all now regression-tested:

- **The launcher gave the harness the pane's TTY as stdin.** `agy` on a resumed
  conversation asks a question there and blocks forever with no output. The
  launcher now redirects stdin from `/dev/null`; this affected every harness,
  not just Antigravity.
- **The final answer rendered twice** — once as the last message, once from the
  terminal record. The stream now drops the echo.
- **An idle fleet reported `$-0.0000`.** Summing no costs can land on negative
  zero.

And one undocumented event: Antigravity emits a `system_message` step on every
resumed run, normally empty. It is quiet when empty and surfaced when not.
