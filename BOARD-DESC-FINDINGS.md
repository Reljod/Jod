# board-desc — findings

**Verdict: schema, not plumbing.** The `tasks` table that backs the agent-team
board has no description/body column and no other free-text field beyond
`title`. Nothing is merely unwired — there is nowhere to put the text. Per
AGENTS.md, migrations wait for human review, so no migration was written, no
schema was designed into the tree, and the storage layer was not touched.

Second finding, independent of the above: **the change would collide.** The
`jod team task` arg definition (`cli/src/main.rs:1210-1214`) and its handler
(`cli/src/main.rs:1814-1819`) are the exact region the positional-help sweep is
editing right now. Even the plumbing half of this task cannot proceed until that
PR lands.

## 1. The schema as it actually is

Read live and read-only from `/home/reljod/.jod/jod.db`
(`select sql from sqlite_master where name='tasks'`), which is the post-migration
truth rather than a reading of the source:

```sql
CREATE TABLE tasks (
      id         TEXT PRIMARY KEY,
      owner      TEXT,
      claimed_at INTEGER,
      status     TEXT NOT NULL DEFAULT 'open'
    , team TEXT, title TEXT, work_id TEXT REFERENCES works(id) ON DELETE CASCADE, created_at_ms INTEGER, completed_at_ms INTEGER)
```

`pragma table_info(tasks)`:

| # | name | type | notnull | default |
|---|------|------|---------|---------|
| 0 | id | TEXT | 0 | — (PK) |
| 1 | owner | TEXT | 0 | — |
| 2 | claimed_at | INTEGER | 0 | — |
| 3 | status | TEXT | 1 | `'open'` |
| 4 | team | TEXT | 0 | — |
| 5 | title | TEXT | 0 | — |
| 6 | work_id | TEXT | 0 | — |
| 7 | created_at_ms | INTEGER | 0 | — |
| 8 | completed_at_ms | INTEGER | 0 | — |

Where each column comes from, in `core/src/store.rs`:

- `core/src/store.rs:69-74` — `0001_initial` creates `tasks` (id, owner,
  claimed_at, status).
- `core/src/store.rs:170-174` — `0002_teams` adds `team` and `title` plus
  `ix_tasks_team`, with the comment that the shared board deliberately reuses
  `tasks` so claiming stays one atomic statement.
- `core/src/store.rs:1075-1083` — `0014_works_and_leases` adds `work_id`,
  `created_at_ms`, `completed_at_ms`, so the *work* board is the same table.

That is the complete set of `ALTER TABLE tasks` statements in the repo. There is
no `description`, `body`, `detail`, `notes` or `summary` column, and no
side-table keyed on a task id.

The in-memory shape agrees — `core/src/team.rs:125-133`:

```rust
pub struct TeamTask {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub owner: Option<String>,
    pub status: String,
}
```

Two comments in the tree already record this gap as known:
`cli/src/tui/app.rs:807-813` and `cli/src/tui/data.rs:1339-1343` both say
`TeamTask` carries "an id, a title, an owner and a status and nothing else" and
that screen columns are waiting on it.

## 2. Exact locations

**CLI arg definition** — `cli/src/main.rs:1209-1214`

```rust
    /// Put a task on the team's board.
    Task {
        team: String,
        id: String,
        title: Vec<String>,
    },
```

`title` is a greedy trailing `Vec<String>`, which matters for option 1 below: a
description cannot be a second positional.

**CLI handler / insert path** — `cli/src/main.rs:1814-1819`

```rust
                TeamCommand::Task { team, id, title } => {
                    let title = title.join(" ");
                    let title = if title.is_empty() { id.clone() } else { title };
                    store.add_team_task(&team, &id, &title)?;
                    println!("{id} on {team}'s board");
                }
```

**Storage write** — `core/src/store.rs:2072-2083`, `add_team_task`, a single
`INSERT INTO tasks (id, status, team, title) ... ON CONFLICT(id) DO NOTHING`.
Its only non-test callers are `cli/src/main.rs:1817` and the console `/task`
command at `cli/src/tui/mod.rs:1268` (`Action::AddTask`). The sibling writer for
the work board is `core/src/works.rs:1073-1097`, `add_work_task`.

**Storage read** — `core/src/store.rs:2085-2100`, `team_tasks`:
`SELECT id, COALESCE(title, id), owner, status FROM tasks WHERE team = ?1 ORDER BY rowid`.
Sibling: `core/src/works.rs:1102-1115`, `work_tasks`.

**Display path** — `jod team show` dispatch at `cli/src/main.rs:2021-2023`
(`render::team(&store.team_members(&team)?, &store.team_tasks(&team)?)`), and the
board loop itself at `cli/src/render.rs:262-288`, which prints one fixed-width
line per task: `"{:<10} {:<8} {}{}"` = id, mark, title, dim owner. The work
board repeats this inside `render::work` (`cli/src/render.rs:594` onward).

**Other consumers that would want the field**

- `api/src/routes.rs:144-206` — `TeamTask` is serialised straight out over HTTP
  as the team view's `tasks`, so any new field is an API change for `apps/`.
- `cli/src/tui/data.rs:1615` (`task_row`) and `cli/src/tui/app.rs:811`
  (`task_row_from`) build the TUI task rows. Both sit in files adjacent to ones
  other agents own right now (`tui/ui.rs`, `tui/command.rs`, `tui/mention.rs`
  are off limits), so a full wire-through touches contested ground.

## 3. Options for holding a description

### Option 1 — new nullable `description TEXT` column on `tasks` (recommended)

One additive migration, then `TeamTask` gains `pub description: Option<String>`
(or `String` with `#[serde(default)]`), `add_team_task` gains a parameter, the
CLI gains a **flag** (`-d/--description`, plus a `--description -` or
`--from-file` form so a repro or an owned-file list can be piped in — a shell
one-liner is the wrong shape for a paragraph), and `render::team` prints the
body indented under the one-line row.

- Cost: a migration, which is exactly the thing gated on human review.
- Benefit: it is the only option where the board is the system of record. It
  serves the *work* board for free, because `works` shares this table. Every
  existing read names its columns explicitly, so nothing breaks. `ON CONFLICT
  DO NOTHING` semantics are untouched. Serde `default` keeps `apps/` clients
  reading old and new payloads.
- Risk: near zero. Nullable, no backfill, no rewrite, no index.

### Option 2 — reuse `title` (stuff the body in after a separator)

No migration. The CLI already joins a greedy `Vec<String>`, so a multi-line title
technically stores today.

- Cost: it breaks every display at once. `cli/src/render.rs:279` is a
  fixed-width single-line `println!`; a multi-line title destroys the column
  layout, the TUI row builders, and the API payload's meaning. Every reader
  would have to learn a parsing convention, and `COALESCE(title, id)` stops
  meaning "a short label". It is a schema change disguised as no schema change —
  the same data, minus the type safety and minus the review.
- Not recommended.

### Option 3 — a side table (`task_details`) or a side file

A `task_details(task_id, body)` table keyed on `tasks.id` keeps `tasks` narrow
and would suit a future append-only comment thread.

- Cost: still a migration, so it buys nothing against the gate, and adds a join
  to a read that is currently one statement. A *file*
  (`~/.jod/boards/<team>/<id>.md`) avoids the migration but puts board state
  outside the single SQLite file the charter names as the store, loses
  `ON DELETE CASCADE` with `works`, loses atomicity with the claim statement,
  and creates a shadow copy — against principle 2.
- Worth revisiting only if descriptions turn into threaded comments, which is a
  different feature.

### Option 4 (considered, rejected) — file the detail as a `team_messages` row

`team_messages` already carries `kind`, `detail` and `thread_id` (added in
`0015_agent_mail`, `core/src/store.rs:1180-1197`), so a `kind='task_detail'`
message referencing the task id needs no migration at all.

- Cost: messages are delivered-once inbox items that get drained
  (`drain_inbox`, `core/src/store.rs:2044`). A board description must be
  re-readable forever by anyone, not consumed by the first reader. Wrong
  lifetime, wrong addressing, and `jod team show` would need a join into the
  bus. Rejected.

**Recommendation: option 1.** It is one additive nullable column that fixes both
boards, matches the existing "reuse `tasks`, don't build a second board"
decision recorded at `core/src/store.rs:170-171` and
`core/src/store.rs:1075-1079`, and is the only option that leaves the board as
the system of record.

## 4. What a migration would have to do (described, not written)

- Append **one new entry** to the `MIGRATIONS` array in `core/src/store.rs:38`,
  after `"0016_projects"` (`core/src/store.rs:1210`), named in the existing
  `NNNN_snake_case` style — e.g. `0017_task_descriptions`. Existing entries must
  never be edited: `migrate()` (`core/src/store.rs:1477-1503`) records applied
  migrations **by name** in a `migrations` table and skips any name already
  present, so a change to an old entry silently never runs on an existing DB.
  Note `0013` is already used twice (`0013_heartbeats`, `0013_roots_and_cards`),
  so uniqueness is by full name, not by number.
- Body: a single additive statement, `ALTER TABLE tasks ADD COLUMN description TEXT;`
  — nullable, no `NOT NULL`, no default, so SQLite rewrites no rows and existing
  rows read `NULL`.
- **No backfill.** Existing tasks legitimately have no description; `NULL` and
  `""` should not be made to differ.
- **No index.** Descriptions are fetched by task id along with the row. If
  searching the board is wanted later, that is a separate FTS decision, like
  `facts_fts`.
- Each migration runs inside its own immediate transaction and is committed with
  its name (`core/src/store.rs:1494-1500`), so this is atomic and idempotent
  across restarts with no extra work.
- Forward-only. Rolling back means either leaving the column unread or a
  `DROP COLUMN` on a modern SQLite; nothing in this repo has a down-migration
  path, so a reviewer should assume the column is permanent.

Downstream edits a reviewer should expect in the same PR, once the migration is
approved: `TeamTask` (`core/src/team.rs:127`), `add_team_task`
(`core/src/store.rs:2074`) and `team_tasks` (`core/src/store.rs:2085`),
optionally `add_work_task`/`work_tasks` (`core/src/works.rs:1073`, `:1102`), the
CLI arg + handler (`cli/src/main.rs:1210`, `:1814`), the console `/task` path
(`cli/src/tui/mod.rs:1268`), and the printer (`cli/src/render.rs:262-288`). The
runnable check would be a round-trip test — file a task with a description,
assert it survives storage and appears in `jod team show` — which cannot be
written honestly before the column exists.
