# The Jod system

Jod is a local-first personal AI system — a chief of staff that delegates rather
than executes. This document is the architecture for the whole thing, and marks
what is built versus what is planned. Nothing here requires a cloud service: the
same binaries run on a laptop today and on a VPS later, because every dependency
is a local process or a local file.

## The four pillars

| # | Pillar | What it is | Status |
|---|--------|-----------|--------|
| 1 | **Brain nodes** | Flat markdown notes in Open Knowledge Format | Planned |
| 2 | **Brain connections** | A graph over those notes, in SQLite | Planned |
| 3 | **Jod** | The orchestrator that delegates and reports | **Built (core)** |
| 4 | **Agents + A2A** | Harness-run agents that talk to each other | **Built (single-agent)** |

Pillars 3 and 4 come first on purpose. They are the part that produces value on
day one, and they are what the other two will be *built by* — once Jod can
delegate reliably, the knowledge layer can be assembled by agents rather than by
hand.

---

## Pillar 3 — Jod, the orchestrator

**Built.** `crates/jod-core`.

The single rule: **Jod never does the work.** It has no model client, no prompt
templates and no tools. It owns delegation, observation and reporting; the
thinking happens inside an agent *harness* — a CLI that already solved context
management, tool use and permissions.

```
                    ┌──────────────────────────────┐
   TUI (planned)   ─┤                              │
   desktop (Tauri) ─┤          jod-core            │
   iOS (planned)   ─┤   service::Jod  (registry)   │
   VPS daemon      ─┤                              │
   CLI example     ─┤                              │
                    └───────────────┬──────────────┘
                                    │ spawn
                    ┌───────────────▼──────────────┐
                    │   tmux session, one per agent│
                    │   ┌──────────────────────┐   │
                    │   │ claude -p … │ opencode run … │
                    │   └──────────┬───────────┘   │
                    └──────────────┼───────────────┘
                                   │ JSONL via tee
                    ┌──────────────▼───────────────┐
                    │ ~/.jod/runs/<id>/stream.jsonl│
                    └──────────────┬───────────────┘
                                   │ tail + parse
                              AgentEvent
```

### Why tmux is load-bearing

Every agent runs inside its own `tmux` session, not as a child process of the
app. That single choice buys four things that would otherwise each need code:

- **Observability** — `tmux attach -t jod-<id>` shows the live agent. Jod does
  not have to reimplement a terminal.
- **A kill switch that works** — `tmux kill-session` stops an agent whether or
  not Jod is running.
- **Survivability** — closing the desktop app does not kill running work.
- **One transport for every client** — the desktop app, an SSH session on a VPS
  and a future iOS client all watch the same sessions.

The launcher is a generated bash script (`runner::render_script`) that pipes the
harness through `tee`, so the pane a human watches and the JSONL Jod parses are
the same bytes. The prompt is passed via `"$JOD_PROMPT"` read from a file, so a
prompt containing quotes or `$(...)` can never be re-interpreted by the shell.

**The session outlives the agent.** When the harness exits, the pane prints the
status and `exec`s a shell rather than letting the session end — because a
session that destroys itself takes any attached client with it, closing the
terminal window of whoever was watching. Jod also sets `detach-on-destroy off`
on its own sessions, never globally, so an explicit kill returns the watcher to
another session. → [why](decisions.md)

### The harness seam

```rust
pub trait Harness: Send {
    fn kind(&self) -> HarnessKind;
    fn args(&self, req: &SpawnRequest) -> Vec<ArgPart>;
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent>;
    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent;
}
```

Adding a third harness means one file — Antigravity (`agy`) is the next one, and
is the test of that claim. Nothing above the seam changes, because every harness
is normalised into one vocabulary:

`Started · Thinking · Message · ToolCall · ToolResult · Finished · Raw · Error`

`Raw` matters more than it looks: an unrecognised line is *surfaced*, never
dropped. When OpenCode renamed its tool event, the prototype showed the raw line
instead of silently losing the tool call — which is how the mismatch was found.

**The runner owns "the run is over", not the harness.** Claude Code emits a
terminal `result` record; OpenCode emits a `step_finish` per step and nothing at
the end. So completion is detected from the process exit marker, and each
adapter reports its accumulated answer and cost in `finalize`. Both harnesses
then behave identically to every client.

### Reporting

`Jod::report()` returns running/completed/failed/killed counts and total spend.
This is the seed of "Jod checks on the agents and reports back" — a scheduled
caller turns it into a digest without any new machinery.

---

## Pillar 4 — Agents and A2A

**Built:** agents run under Claude Code and OpenCode, each in its own tmux
session, each managing its own context (the harness's job, not Jod's).

**Planned:** agent-to-agent communication — the mechanism underneath **agent
teams**, which Jod owns rather than borrowing from a harness, so a single team
can span all three. → [the goal](jod-tui.md), [why](decisions.md)

The design follows the same local-files principle, so it needs no broker:

```
~/.jod/
  runs/<agent-id>/
    stream.jsonl     # the transcript (built)
    agent.json       # metadata (built)
    inbox.jsonl      # messages addressed to this agent   (planned)
    outbox.jsonl     # messages it sent                   (planned)
  topics/<name>.jsonl # broadcast channels                (planned)
```

An agent sends a message by appending one JSON line; Jod's tailer already knows
how to follow an append-only file, so delivery is the mechanism that exists.
Two integration paths, in order of cost:

1. **Prompt-level** — Jod injects pending inbox messages into the next
   delegation. Works with any harness today, no harness support needed.
2. **MCP-level** — a small `jod-mcp` server exposing `send_message`,
   `read_inbox`, `list_agents`. Both harnesses support MCP, so agents gain
   first-class messaging without Jod parsing anything new.

**Collaboration on code** uses git rather than messages: each agent gets its own
worktree, and integration is a merge. Agents that must not conflict get separate
worktrees; agents that must agree get the same branch and a lock, one owner per
path — the rule already written down in [`teamwork.md`](teamwork.md).

---

## Pillar 1 — Brain nodes

**Planned.** Flat markdown, no hierarchy, one idea per file, as small as it can
be. Following [Open Knowledge Format](https://github.com/openkf) conventions:
frontmatter carries the type and identity, the body carries the content.

```markdown
---
id: ent-reljod
type: Entity
name: Reljod Oreta
tags: [person, self]
---

Founder. Operates through Jod.
```

Categories start deliberately small — `Entity`, `Event`, `Concept`, `Source`,
`Task` — because a taxonomy invented before it is used is a taxonomy that gets
rewritten. Notes live in a plain directory; the system of record for prose stays
Notion, and Jod keeps the graph-shaped subset.

Nodes have no hierarchy because hierarchy is the thing that rots. Everything
structural belongs in pillar 2.

## Pillar 2 — Brain connections

**Planned.** [GraphQLite](https://github.com/colliery-io/graphqlite) — a SQLite
extension adding property-graph storage and openCypher querying, with Rust
bindings and no server. Created at install time as `~/.jod/brain.db`.

The markdown files remain the source of truth; the graph is a derived index that
can be deleted and rebuilt by rescanning the notes. That keeps the knowledge
portable and greppable, and lets the graph schema change without a migration.

```cypher
MATCH (n:Entity {id: 'ent-reljod'})-[:MENTIONED_IN]->(e:Event)
WHERE e.date > '2026-01-01'
RETURN e ORDER BY e.date DESC LIMIT 10
```

Because it is a SQLite extension, agent runs and the knowledge graph can share
one file, and any harness can query it through a plain SQL tool. Pillar 2 stays
behind a small trait so the driver choice remains reversible.

---

## Roadmap

1. ~~Core service: harness seam, tmux runner, event normalisation~~ **done**
2. ~~Desktop prototype (Tauri) driving Claude Code and OpenCode~~ **done**

The current goal is items 3–7: **a TUI that matches OpenCode's feature for
feature, runs agent teams, and streams reasoning live — identically across
Claude Code, OpenCode and Antigravity.** → [`jod-tui.md`](jod-tui.md)

3. Antigravity (`agy`) as the third harness, behind the existing seam
4. TUI skeleton: fleet list, live event stream, reasoning rendered as it arrives
5. Persist and reattach runs across app restarts (`~/.jod/runs` is already there)
6. Sessions and interaction: resume by id, multi-turn chat, inline permissions
7. Agent teams: inbox/outbox, shared task list, auto-wake — plus `jod-mcp`
8. Brain nodes: OKF writer/reader, plus a Notion sync
9. Brain connections: GraphQLite index and query surface
10. Headless daemon for a VPS — the same `jod-core` behind an authenticated API
11. iOS client against that API

## Design rules

- **Jod delegates; harnesses think.** No model client in `jod-core`, ever.
- **Local first.** Every dependency is a local process or a local file.
- **Plain files at the boundaries.** Anything Jod stores stays readable with
  `cat` when Jod is not running.
- **Unknown input is surfaced, not swallowed.** `Raw` over silent drops.
- **The core has no UI.** If a client needs logic, it belongs in `jod-core`.
