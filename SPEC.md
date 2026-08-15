# SPEC — main is the CEO, every project gets a manager, and a stuck session says so

Execute this in a fresh session. Delete this file when it has shipped.

## The problem, in Reljod's words

Two things go wrong today.

1. **You cannot tell a finished session from a stuck one.** Sometimes a session
   is just wedged, and nothing on screen says so.
2. **The orchestrator starts a new session when it should carry on with an old
   one.** You ask for a new task on a project you are already working on, and it
   spawns a fresh agent instead of continuing the one that has the context. The
   old session was stuck; the new one finished. So the work got done, but the
   fleet filled up with a hung agent nobody noticed.

The fix he asked for: `jod tui` always opens on **main**, main behaves like a
CEO, main hands work to a **project manager**, and the manager runs its own
engineers.

## What is already true — do not rebuild it

Check these before writing anything. Three of them are easy to duplicate by
accident.

- **The TUI already opens in main.** `cli/src/tui/mod.rs:612` calls `enter_main`
  on every launch unless `--resume` named a conversation. The test is
  `the_launch_position_is_the_main_chat`. Nothing to do here.
- **Stall detection already exists.** `core/src/heartbeat.rs` asks two
  questions: is the process group alive (`kill(pgid, 0)`), and is the run's
  event `seq` still moving. Twenty minutes of silence from a live process is
  `Verdict::Stalled`. The sweep is `Ticker::tick_heartbeats`
  (`core/src/ticker.rs:761`), which runs inside `jod daemon`.
- **Projects already exist and already stick.** `core/src/projects.rs`, table
  `projects` in `core/src/store.rs:1228`. A conversation carries
  `current_project_id`, and every resolution is written to
  `project_resolutions` with how it was reached.
- **Works, sessions and the fleet tree already exist.** `core/src/works.rs`,
  `core/src/tree.rs`. A work is one intent with a board; it closes when the
  board is empty.

## The four real gaps

These are the causes, found by reading the code. Each change below closes one.

1. **A heartbeat is opt-in, so almost nothing has one.** Only three places arm
   one: `jod run --watch` (`cli/src/main.rs:1679`), a keypress in the TUI
   (`cli/src/tui/mod.rs:1301`), and a goal iteration (`core/src/ticker.rs:1809`).
   Every session the orchestrator spawns — `open_work`, `delegate`,
   `continue_agent` — gets none. A wedged one stays `running` for ever.
2. **A stall kills the run.** `Verdict::terminates()` is true for `Stalled`, so
   the sweep signals the process group and marks the run failed. That is right
   for a goal iteration and wrong for a session Reljod is watching.
3. **`list_agents` does not say which project or work an agent belongs to.**
   `AgentView` in `core/src/mcp.rs:961` carries run id, name, harness, status,
   cwd, model, session id, cost and last message. `cwd` is the only project hint,
   and for a session holding a worktree lease the cwd is the *worktree*, not the
   checkout. So the router cannot group agents by project, and cannot see that
   one of them is wedged. It starts a new one.
4. **A work has no project.** The `works` table (`core/src/store.rs:1032`) has no
   `project_id` column. "Which works are on Jod?" is unanswerable except by
   walking each work's sessions and reading their sticky project.

## Decisions already made

Reljod chose these. Do not reopen them.

- **A project manager is a resumed conversation, not a resident process.** One
  pinned conversation per project, the same shape as the main chat. Main resumes
  it for each instruction, it answers, the process exits. Context lives in the
  transcript. Nothing sits idle burning money, and a manager cannot wedge
  between turns because between turns it does not exist.
- **A stalled session is marked and surfaced, never killed.** Status shows
  `stalled` in the fleet and in `list_agents`, a card lands on the rail, and the
  router treats it as not-continuable. Reljod decides whether to stop it.
- **Main may route and may run repo-less one-shots. Nothing else.** Anything
  touching a repository goes through that project's manager. Main can no longer
  call `open_work`. Main keeps schedules, goals, memory, and answering questions
  about Jod itself.

---

## Change 1 — every session gets a heartbeat, and a stuck one says so

### 1a. Arm a heartbeat on every spawn

Arm it in the one place every spawn passes through: `Jod::spawn_agent`
(`core/src/service.rs`). Not in each caller — `delegate`, `open_work`,
`continue_agent`, work sessions and team starts must all get one, and adding it
per caller is how one of them silently misses out.

- Use `Watching::Run` and `DEFAULT_STALL_MS` (20 minutes).
- Arm it *after* the spawn succeeds. A heartbeat for a run that never started is
  a row watching nothing, and the foreign-key cascade only cleans up rows whose
  run exists. `cli/src/main.rs:1667` already says this; follow it.
- Failing to arm one must not fail the spawn. Log it and carry on.

`jod run --watch` and the TUI key keep working. They now change an existing
window rather than create the only one — `watch_run` is already
`INSERT … ON CONFLICT DO UPDATE`, so that works unchanged.

### 1b. A stall marks; it does not kill

Split the sweep's response by what is being watched, in
`Ticker::tick_heartbeats`.

- `Watching::Goal` — unchanged. Terminate and fail. A goal whose iteration
  wedges blocks the goal loop for ever, which is the case heartbeats were
  written for.
- `Watching::Run` — do **not** signal the group, do **not** fail the run, and do
  **not** retire the heartbeat. Record that it is stalled and keep watching.

The same split applies to `Verdict::Expired`. A plain session that runs past a
ceiling is marked, not reaped. Give `Watching::Run` an effectively unbounded
`max_lifetime_ms` rather than special-casing the verdict in two places.

If a stalled run starts producing events again, clear the mark. It went quiet
and came back; that is not a failure and it must stop looking like one.

### 1c. Make it visible

Do not add a variant to `AgentStatus`. `runs.status` is written directly by the
supervisor, a separate process, and a status that process never writes will
drift. Keep `stalled` as a fact derived from the heartbeat row.

- Add `stalled_since_ms INTEGER` to `heartbeats`. Null means healthy.
- Add `Store::stalled_runs() -> Result<HashMap<String, i64>>` returning run id to
  the millisecond it went quiet.
- The fleet tree (`core/src/tree.rs`) shows a `stalled` badge on a run node, with
  how long it has been silent.
- The TUI renders it distinctly from `running`. A spinner that keeps spinning on
  a wedged agent is the bug being fixed here.
- Raise one card on the rail when a run first goes stalled — one, not one per
  tick. `core/src/cards.rs` owns the rail.

### 1d. Say out loud that this needs the daemon

The sweep only runs inside `jod daemon`. Without it, nothing is ever marked
stalled. The TUI already warns about this on the manual heartbeat key
(`cli/src/tui/mod.rs:1303`, "needs `jod daemon`"). Now that every session is
watched, the fleet should say so once when the daemon is not running, rather
than quietly showing every wedged agent as healthy.

---

## Change 2 — the router can see project, work and health

Widen `AgentView` in `core/src/mcp.rs` with four fields:

| Field | Where it comes from |
|---|---|
| `project` | the run's conversation `current_project_id`, resolved to a name |
| `work` | the conversation's `work_id`, with the work's title |
| `stalled_for_ms` | the heartbeat's `stalled_since_ms`, null when healthy |
| `busy` | true when the run is `running` and not stalled |

Add a `project` filter argument to `list_agents`, so a manager can ask only
about its own project instead of reading the whole fleet.

Update the tool description. It currently says "continuing a warm agent beats
starting a cold one". It must now also say a **stalled** agent cannot be
continued, and that starting a fresh one beside it is right — after saying so.

Add `project_id` to the `works` table, set when the work is opened. That is what
makes gap 4 go away, and Change 3 depends on it.

---

## Change 3 — a manager per project

### 3a. The manager conversation

A manager is a pinned conversation attached to a project. It mirrors
`Store::main_conversation` (`core/src/orchestrator.rs:1261`), which is
get-or-create on `pinned = 1`.

- Add `manager_conversation_id TEXT REFERENCES conversations(id) ON DELETE SET
  NULL` to `projects`.
- Add `Store::manager_conversation(project_id, harness) -> Result<String>`.
  Get-or-create, for the same reason main is: a manager that has to be set up is
  one that is missing exactly when you first need it.
- The new conversation gets `current_project_id` set to its project and its
  title set to the project name. Everything it starts inherits the right
  project, so nothing below it has to guess.
- Deleting a project must not orphan a manager conversation. `ON DELETE SET
  NULL` leaves the transcript readable, which is what the catalog is for.

### 3b. The tool main uses to reach it

New MCP tool `ask_manager`:

```
ask_manager { project: string, instruction: string }
```

- Resolve `project` through `projects::resolve` — the same plain string match
  over names, aliases and path basenames the router already uses. Do not ask a
  model to do this.
- Get-or-create the manager conversation, resume it with the instruction, return
  as soon as it is handed over. Non-blocking, like everything else main does.
- Return the run id, the project it resolved to, and whether the manager was
  resumed or started for the first time. Reljod must be able to see which
  project it picked; a routing decision nobody can see is one nobody can
  correct.
- Access level `ToolAccess::Delegate`. It starts an agent, and the thing you
  least want an unattended run to hold is the power to start more.

### 3c. Refuse `open_work` from main

Prompt wording is not enforcement. Refuse it at the tool boundary.

The MCP server already knows which run is calling — it resolves its own process
group against `runs.pgid`, so the caller cannot argue about its identity. When
that run is the main chat, `open_work` refuses and names `ask_manager` instead.

Anything below main — a manager, an engineer — may still call it.

### 3d. Two preambles instead of one

`orchestrator_preamble()` in `core/src/orchestrator.rs:353` becomes main's
preamble, and a second one is added for managers.

**Main.** It routes and it answers. Its tools are `project_list`,
`project_current`, `project_switch`, `ask_manager`, `delegate` (repo-less
one-shots only), `schedule_create`, `goal_create`, `recall`, `related`,
`remember`, `record_decision`, `ask_question`, `list_agents`, `stop_agent`.
Say plainly that anything touching a repository goes to that project's manager,
and that `open_work` is not its to call.

Keep the paragraph about Taglish and dictated speech. "btw, let's fix this" is a
normal instruction and the missing noun is usually the current project.

**A manager.** It owns one project and everything happening in it. Its tools are
`list_agents` (scoped to its project), `continue_agent`, `open_work`,
`delegate`, `stop_agent`, plus the rail and memory tools. Tell it:

- Check `list_agents` for its project first, every time. That is the decision
  that matters most.
- A **stalled** agent cannot be continued. Say so, start a fresh session beside
  it, and leave the stalled one for Reljod to stop.
- Reuse a finished session by resuming its conversation when the new instruction
  carries on what it was doing. Start a new work when the intent is new.
- Report back in one or two sentences: what it did with the instruction and who
  has it now.

### 3e. Where a manager sits in the tree

The fleet tree is works, then sessions, then runs. Add a project level above it.

- `NodeKind::Project` and `NodeKind::Manager` in `core/src/tree.rs`.
- A project row holds its manager conversation as its first child, then its open
  works beneath that — keyed on the new `works.project_id`.
- A `delegate`d run still belongs to no work and stays loose. That is on purpose
  and does not change.
- Pressing enter on a manager row enters that conversation, the way enter on the
  pinned row enters main. `enter_main` in `cli/src/tui/mod.rs:951` is the shape
  to follow.

---

## Migrations

Three, in `core/src/store.rs`, in this order:

1. `ALTER TABLE heartbeats ADD COLUMN stalled_since_ms INTEGER;`
2. `ALTER TABLE works ADD COLUMN project_id TEXT REFERENCES projects(id) ON
   DELETE SET NULL;`
3. `ALTER TABLE projects ADD COLUMN manager_conversation_id TEXT REFERENCES
   conversations(id) ON DELETE SET NULL;`

Existing rows get null in all three, which is the honest starting state. Old
works have no project and no manager exists yet; both are true.

## Files this touches

| File | What changes |
|---|---|
| `core/src/store.rs` | three migrations, `manager_conversation`, `stalled_runs` |
| `core/src/service.rs` | arm a heartbeat in `spawn_agent` |
| `core/src/heartbeat.rs` | the mark-don't-kill split, `stalled_since_ms` |
| `core/src/ticker.rs` | `tick_heartbeats` acts by `Watching`, raises one card |
| `core/src/mcp.rs` | `AgentView` fields, `list_agents` filter, `ask_manager`, refusing `open_work` from main |
| `core/src/orchestrator.rs` | main's preamble, the manager preamble |
| `core/src/works.rs` | a work records its project |
| `core/src/tree.rs` | project and manager nodes |
| `core/src/cards.rs` | the stalled card |
| `cli/src/tui/mod.rs` | entering a manager, the stalled badge, the no-daemon warning |
| `cli/src/tui/ui.rs` | drawing stalled distinctly from running |

## Checks

Every one of these must run and pass. `cargo test -p jod-core -p jod-cli`.

**Change 1**

1. `spawn_agent` arms a heartbeat — spawn through the fake supervisor, assert one
   row in `heartbeats` for that run id with `Watching::Run`.
2. A spawn that fails to start leaves no heartbeat row.
3. `decide` on a `Watching::Run` heartbeat past the silence window returns
   `Stalled`, and the sweep neither signals the group nor changes `runs.status`.
   Assert the run is still `running` afterwards.
4. The same heartbeat under `Watching::Goal` still terminates and fails. This is
   the regression guard on the split.
5. A stalled run that produces a new event clears `stalled_since_ms` on the next
   tick.
6. One card is raised on the first stalled tick and none on the second.

**Change 2**

7. `list_agents` returns `project`, `work`, `stalled_for_ms` and `busy` for a run
   whose conversation has a project and a work.
8. `list_agents` with a `project` filter returns only that project's agents.
9. A work opened by a manager records that manager's project in
   `works.project_id`.

**Change 3**

10. `manager_conversation` is get-or-create: two calls for one project return the
    same id, and two projects get different ids.
11. `ask_manager` on a project with no manager creates one and reports it started
    fresh; a second call reports it resumed, and the conversation id is the same.
12. `ask_manager` on a name that matches nothing refuses and names the projects it
    does know, rather than guessing.
13. `open_work` called by the main chat's run is refused, and the refusal names
    `ask_manager`.
14. `open_work` called by a manager's run succeeds.
15. The tree puts a manager conversation directly under its project, and that
    project's works under it.
16. Enter on a manager row moves the screen to that conversation, and the chat
    box binds to it.

**End to end, on this box**

17. `jod daemon --once` over a store holding one live run and one wedged run
    marks exactly the wedged one and kills neither.

## Out of scope

Say so rather than doing them.

- **Domains.** `domains/` holds life-domains, not repositories. Managers are per
  *project* only. A manager for finance or infra is a separate decision.
- **Killing stalled sessions automatically.** Reljod chose mark-and-surface. Do
  not add a reap-after-ceiling path for `Watching::Run`, even as an option.
- **Migrating existing works to projects.** New works record their project. Old
  ones keep a null and sit outside the project tree, as they do today. A
  backfill is its own task.
- **The old JSON router.** `Decision`, `parse_decision` and `router_prompt` in
  `core/src/orchestrator.rs` are the earlier design, superseded by the tool-using
  orchestrator. Leave them alone. Removing them is its own task.
