//! Durable state: one SQLite file in WAL mode.
//!
//! This is the layer that turns Jod from a task runner into an assistant. A
//! process that restarts still knows which agents it launched, what they said,
//! and what it has learned about the person it works for.
//!
//! The design is taken from [`research/agent-db-2026`], which benchmarked nine
//! engines with real concurrent OS processes. Three results drive the code here:
//!
//! - **SQLite was both fastest and the only engine that never lost a write.**
//!   Postgres silently discarded 47% of contended updates when used the obvious
//!   way, LanceDB 51%, Qdrant 46% — every one of them reporting zero errors.
//! - **`BEGIN IMMEDIATE` is mandatory for writes.** Deferred transactions
//!   upgrade their lock late and collide; that was a 98% failure rate in the
//!   benchmark. Every write here goes through [`Store::write`].
//! - **Never hold a write transaction across a model call.** The whole argument
//!   rests on write transactions costing microseconds. Nothing in this module
//!   opens a transaction that outlives a single function call.
//!
//! Markdown stays the source of truth for prose; this database is an index over
//! it and can be deleted and rebuilt.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::event::AgentEnvelope;
use crate::harness::HarnessKind;
use crate::team::{Member, MemberStatus, Message, TeamTask};

/// Applied in order; each is recorded so it runs exactly once.
const MIGRATIONS: &[(&str, &str)] = &[
    (
    "0001_initial",
    r#"
    -- Agent event streams: append-only, many writers, tail-follow.
    CREATE TABLE events (
      id          INTEGER PRIMARY KEY,
      run_id      TEXT NOT NULL,
      seq         INTEGER NOT NULL,
      kind        TEXT NOT NULL,
      at_ms       INTEGER NOT NULL,
      payload     TEXT NOT NULL,
      UNIQUE(run_id, seq)
    );
    CREATE INDEX ix_events_run ON events(run_id, seq);

    -- One row per delegation, so a restarted process can list and reattach.
    CREATE TABLE runs (
      id            TEXT PRIMARY KEY,
      name          TEXT NOT NULL,
      harness       TEXT NOT NULL,
      status        TEXT NOT NULL,
      cwd           TEXT NOT NULL,
      session_id    TEXT,
      tmux_session  TEXT NOT NULL,
      created_at_ms INTEGER NOT NULL,
      summary       TEXT NOT NULL
    );
    CREATE INDEX ix_runs_created ON runs(created_at_ms DESC);

    -- Contended state. Claimed atomically, never with a read-then-write.
    CREATE TABLE tasks (
      id         TEXT PRIMARY KEY,
      owner      TEXT,
      claimed_at INTEGER,
      status     TEXT NOT NULL DEFAULT 'open'
    );

    -- What Jod has learned. Bitemporal: `valid_from`/`valid_to` are about the
    -- world, `recorded_at` is about when Jod found out. A fact is never
    -- deleted, only superseded, so history stays answerable.
    CREATE TABLE facts (
      id             INTEGER PRIMARY KEY,
      -- Which part of life this belongs to. A *partition*, applied before
      -- ranking — never a ranking signal. Measured: scope used as a boost
      -- leaked cross-domain facts 79% of the time; as a hard filter, 0%.
      scope          TEXT NOT NULL DEFAULT 'default',
      subject        TEXT NOT NULL,
      predicate      TEXT NOT NULL,
      object         TEXT NOT NULL,
      -- Who asserted this: owner | agent | untrusted | system. Its own column
      -- and never part of the fact text, so an ingested page cannot forge its
      -- own trust level by writing "origin: owner" into its content.
      origin         TEXT NOT NULL DEFAULT 'agent',
      source         TEXT,
      valid_from     TEXT,
      valid_to       TEXT,
      recorded_at_ms INTEGER NOT NULL,
      state          TEXT NOT NULL DEFAULT 'accepted',
      invalidated_by INTEGER REFERENCES facts(id)
    );
    CREATE INDEX ix_facts_subject ON facts(scope, subject);

    -- Proof that a deletion happened, kept after every version of the fact is
    -- physically gone.
    CREATE TABLE tombstones (
      id         INTEGER PRIMARY KEY,
      scope      TEXT NOT NULL,
      subject    TEXT NOT NULL,
      predicate  TEXT NOT NULL,
      deleted_at_ms INTEGER NOT NULL,
      versions   INTEGER NOT NULL
    );

    -- Recall over the facts. Brute-force text search is enough: the research
    -- measured vector search as barely mattering below ~150k memories, and
    -- untuned ANN indexes returned as little as 43% of true neighbours.
    CREATE VIRTUAL TABLE facts_fts USING fts5(
      subject, predicate, object,
      content='facts', content_rowid='id'
    );
    CREATE TRIGGER facts_ai AFTER INSERT ON facts BEGIN
      INSERT INTO facts_fts(rowid, subject, predicate, object)
      VALUES (new.id, new.subject, new.predicate, new.object);
    END;
    CREATE TRIGGER facts_ad AFTER DELETE ON facts BEGIN
      INSERT INTO facts_fts(facts_fts, rowid, subject, predicate, object)
      VALUES ('delete', old.id, old.subject, old.predicate, old.object);
    END;
    CREATE TRIGGER facts_au AFTER UPDATE ON facts BEGIN
      INSERT INTO facts_fts(facts_fts, rowid, subject, predicate, object)
      VALUES ('delete', old.id, old.subject, old.predicate, old.object);
      INSERT INTO facts_fts(rowid, subject, predicate, object)
      VALUES (new.id, new.subject, new.predicate, new.object);
    END;
    "#,
    ),
    (
        "0002_teams",
        r#"
    -- Who is on a team. One row per member; the team is part of the key so a
    -- name can mean different agents on different teams.
    CREATE TABLE team_members (
      team         TEXT NOT NULL,
      name         TEXT NOT NULL,
      harness      TEXT NOT NULL,
      role         TEXT NOT NULL DEFAULT '',
      status       TEXT NOT NULL DEFAULT 'ready',
      -- The run currently embodying this member, and the harness-side
      -- conversation to resume for its next turn. Both change every turn.
      agent_id     TEXT,
      session_id   TEXT,
      joined_at_ms INTEGER NOT NULL,
      PRIMARY KEY (team, name)
    );

    -- The message bus. Already addressed to one recipient: a broadcast is
    -- fanned out on send, so reading an inbox is one query over one table.
    CREATE TABLE team_messages (
      id        INTEGER PRIMARY KEY,
      team      TEXT NOT NULL,
      sender    TEXT NOT NULL,
      recipient TEXT NOT NULL,
      body      TEXT NOT NULL,
      at_ms     INTEGER NOT NULL,
      -- Set when the message has been handed to the agent, so the same
      -- instruction is never injected into two turns.
      delivered INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX ix_team_messages_inbox
      ON team_messages(team, recipient, delivered, id);

    -- The shared board reuses the existing `tasks` table, so claiming stays the
    -- single atomic statement it already was.
    ALTER TABLE tasks ADD COLUMN team TEXT;
    ALTER TABLE tasks ADD COLUMN title TEXT;
    CREATE INDEX ix_tasks_team ON tasks(team);
    "#,
    ),
    (
        "0003_process_supervision",
        r#"
    -- Runs are no longer tmux sessions. A run is a detached process group led
    -- by a `jod-run` supervisor, and that group's id is the whole control
    -- surface: any process can ask whether the run is alive and stop it.
    --
    -- `tmux_session` was NOT NULL, which SQLite cannot relax in place, so the
    -- table is rebuilt. Every existing row is carried over — history is the
    -- point of this table — and the dead session name is simply dropped.
    CREATE TABLE runs_new (
      id            TEXT PRIMARY KEY,
      name          TEXT NOT NULL,
      harness       TEXT NOT NULL,
      status        TEXT NOT NULL,
      cwd           TEXT NOT NULL,
      session_id    TEXT,
      -- The supervisor's pid, and the group it leads. Null until it starts,
      -- and left in place afterwards so a finished run still says what ran it.
      pid           INTEGER,
      pgid          INTEGER,
      created_at_ms INTEGER NOT NULL,
      summary       TEXT NOT NULL
    );

    INSERT INTO runs_new (id, name, harness, status, cwd, session_id,
                          pid, pgid, created_at_ms, summary)
      SELECT id, name, harness,
             -- A run inherited from the tmux era cannot still be running: its
             -- session is gone with the transport. Recording that here means
             -- rehydrate never has to guess about a row it cannot probe.
             CASE WHEN status = 'running' THEN 'failed' ELSE status END,
             cwd, session_id, NULL, NULL, created_at_ms, summary
        FROM runs;

    DROP TABLE runs;
    ALTER TABLE runs_new RENAME TO runs;
    CREATE INDEX ix_runs_created ON runs(created_at_ms DESC);
    "#,
    ),
    (
        "0004_memory_graph",
        r#"
    -- A traversable index over `facts`. Not a second source of truth: both
    -- tables can be dropped and rebuilt by rescanning `facts`, which is the
    -- same property `facts_fts` already has and the reason markdown stays
    -- authoritative.
    --
    -- Measured in `research/sqlite-graph-2026`: plain tables plus a recursive
    -- CTE answer a 3-hop neighbourhood over a million edges in 0.37 ms p50.
    -- No SQLite graph extension exists that is maintained, permissively
    -- licensed *and* statically linkable into a Rust binary, so the extension
    -- the question asked for is not purchasable at any price — and at these
    -- numbers it would buy nothing.

    -- A thing facts talk about. Interned once so traversal compares integers
    -- rather than repeating the subject text on every edge.
    CREATE TABLE entities (
      id            INTEGER PRIMARY KEY,
      -- The same hard partition `facts` uses, applied *before* traversal and
      -- never as a ranking signal: measured leaking 79% cross-domain as a
      -- boost, 0% as a filter.
      scope         TEXT NOT NULL DEFAULT 'default',
      kind          TEXT NOT NULL DEFAULT 'thing',
      name          TEXT NOT NULL,
      first_seen_ms INTEGER NOT NULL,
      last_seen_ms  INTEGER NOT NULL,
      UNIQUE(scope, kind, name)
    );

    -- One edge per fact: `subject --predicate--> object`, which is the shape
    -- `facts` already stores. This table only makes it walkable.
    CREATE TABLE relations (
      id             INTEGER PRIMARY KEY,
      scope          TEXT NOT NULL DEFAULT 'default',
      src            INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
      dst            INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
      predicate      TEXT NOT NULL,
      weight         REAL NOT NULL DEFAULT 1.0,
      -- The fact this edge came from. `ON DELETE CASCADE` is what makes
      -- `forget` reach the graph: destroying every version of a fact destroys
      -- its edges with it. Without this, a forgotten thing stays traversable
      -- and "Jod forgot that" stops meaning "Jod says it forgot that".
      fact_id        INTEGER NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
      -- Milliseconds, not the ISO text `facts` keeps. A derived index may pick
      -- its own representation, and an integer comparison inside a recursive
      -- step is the difference between pruning an edge and parsing it.
      valid_from_ms  INTEGER,
      valid_to_ms    INTEGER,
      recorded_at_ms INTEGER NOT NULL
    );

    -- Both are *covering* for one traversal step: the recursive term reads
    -- only these five columns, so k-hop never touches the table itself. The
    -- column order is the query's order — scope partitions first, then the
    -- endpoint, then validity, then the far end.
    CREATE INDEX ix_relations_out
      ON relations(scope, src, valid_to_ms, valid_from_ms, dst);
    CREATE INDEX ix_relations_in
      ON relations(scope, dst, valid_to_ms, valid_from_ms, src);

    -- Not optional, and not obvious. Without it the hybrid query's FTS5-seed
    -- join degenerates to a full scan of `relations` per seed row: measured
    -- 533 ms against 6.5 ms at 10k edges, an 82x difference from one index.
    CREATE INDEX ix_relations_fact ON relations(fact_id);

    -- Communities are recomputed by a periodic job, never at query time: one
    -- label-propagation pass over 100k edges measured 46.8 s.
    CREATE TABLE entity_community (
      entity_id      INTEGER PRIMARY KEY REFERENCES entities(id) ON DELETE CASCADE,
      community      INTEGER NOT NULL,
      computed_at_ms INTEGER NOT NULL
    );
    CREATE INDEX ix_entity_community ON entity_community(community);
    "#,
    ),
];

/// Who asserted a fact. Kept out of the fact's text so that content Jod
/// ingested cannot claim a trust level it was not given.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Reljod said so. The highest trust there is.
    Owner,
    /// An agent concluded it while working.
    #[default]
    Agent,
    /// Read from somewhere outside — a web page, an email, a document.
    Untrusted,
    /// Jod itself recorded it, e.g. a run's outcome.
    System,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::Owner => "owner",
            Origin::Agent => "agent",
            Origin::Untrusted => "untrusted",
            Origin::System => "system",
        }
    }

    pub fn parse(s: &str) -> Origin {
        match s {
            "owner" => Origin::Owner,
            "untrusted" => Origin::Untrusted,
            "system" => Origin::System,
            _ => Origin::Agent,
        }
    }
}

/// The default partition, for facts that belong to no particular domain.
pub const DEFAULT_SCOPE: &str = "default";

/// A thing Jod believes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub id: i64,
    pub scope: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub origin: Origin,
    pub source: Option<String>,
    pub valid_from: Option<String>,
    /// `None` means still believed.
    pub valid_to: Option<String>,
    pub recorded_at_ms: i64,
    pub state: String,
}

/// A fact on its way in, before the store assigns it an id and a timestamp.
#[derive(Debug, Clone)]
pub struct NewFact {
    pub scope: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub origin: Origin,
    pub source: Option<String>,
    pub valid_from: Option<String>,
}

impl Default for NewFact {
    fn default() -> Self {
        NewFact {
            scope: DEFAULT_SCOPE.to_string(),
            subject: String::new(),
            predicate: String::new(),
            object: String::new(),
            origin: Origin::default(),
            source: None,
            valid_from: None,
        }
    }
}

impl NewFact {
    pub fn new(
        subject: impl Into<String>,
        predicate: impl Into<String>,
        object: impl Into<String>,
    ) -> Self {
        NewFact {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            ..Default::default()
        }
    }

    pub fn in_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    pub fn from(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }
}

pub struct Store {
    /// Write transactions cost microseconds, so one lock over one connection is
    /// cheaper than a pool and makes "one writer" explicit.
    conn: Mutex<Connection>,
    /// Where this database lives, when it lives anywhere. A run's supervisor is
    /// a separate process and has to be told which file to write into, so the
    /// store has to be able to say. `None` for an in-memory store — which is
    /// exactly the case where no other process could ever share it.
    path: Option<std::path::PathBuf>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Canonicalised, because the supervisor runs elsewhere and a relative
        // path would resolve against the wrong directory.
        let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        Store::build(Connection::open(path)?, Some(absolute))
    }

    /// An ephemeral database. Used by tests, and by any caller that wants the
    /// orchestrator without a file on disk.
    pub fn in_memory() -> Result<Store> {
        Store::build(Connection::open_in_memory()?, None)
    }

    /// The file this store is backed by, if any.
    pub fn path(&self) -> Option<std::path::PathBuf> {
        self.path.clone()
    }

    fn build(conn: Connection, path: Option<std::path::PathBuf>) -> Result<Store> {
        let store = Store::from_connection(conn)?;
        Ok(Store { path, ..store })
    }

    fn from_connection(conn: Connection) -> Result<Store> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;      -- readers never block the writer
             PRAGMA busy_timeout = 5000;     -- wait for the lock, don't fail
             PRAGMA synchronous = NORMAL;    -- durable across crashes
             PRAGMA foreign_keys = ON;",
        )?;
        let store = Store {
            conn: Mutex::new(conn),
            path: None,
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let mut conn = self.conn.lock().expect("store lock poisoned");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS migrations (
               name TEXT PRIMARY KEY, applied_at_ms INTEGER NOT NULL);",
        )?;
        for (name, sql) in MIGRATIONS {
            let done: Option<String> = conn
                .query_row(
                    "SELECT name FROM migrations WHERE name = ?1",
                    params![name],
                    |r| r.get(0),
                )
                .optional()?;
            if done.is_some() {
                continue;
            }
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO migrations (name, applied_at_ms) VALUES (?1, ?2)",
                params![name, now_ms()],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Run `f` inside an immediate transaction.
    ///
    /// Immediate, not deferred: a deferred transaction takes its write lock only
    /// when it first writes, and two of them that both started by reading will
    /// collide on upgrade. That is the failure the benchmark measured at 98%.
    fn write<T>(&self, f: impl FnOnce(&rusqlite::Transaction) -> Result<T>) -> Result<T> {
        let mut conn = self.conn.lock().expect("store lock poisoned");
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    // ---- agent runs and their events ----------------------------------

    /// Record one event. Re-recording the same `(run_id, seq)` is ignored, so a
    /// replayed stream cannot duplicate history.
    pub fn append_event(&self, env: &AgentEnvelope) -> Result<()> {
        let payload = serde_json::to_string(&env.event)?;
        let kind = event_kind(env);
        self.write(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO events (run_id, seq, kind, at_ms, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![env.agent_id, env.seq as i64, kind, env.at_ms, payload],
            )?;
            Ok(())
        })
    }

    pub fn events(&self, run_id: &str) -> Result<Vec<AgentEnvelope>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT run_id, seq, at_ms, payload FROM events
             WHERE run_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![run_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (agent_id, seq, at_ms, payload) = row?;
            out.push(AgentEnvelope {
                agent_id,
                seq: seq as u64,
                at_ms,
                event: serde_json::from_str(&payload)?,
            });
        }
        Ok(out)
    }

    /// Events after `after`, oldest first. `None` means "I have seen nothing",
    /// and returns the run from its very first event.
    ///
    /// This is what lets a client that dropped its connection replay only the
    /// tail: it remembers the last `seq` it saw and asks for what followed,
    /// rather than re-fetching a transcript it already has.
    ///
    /// The cursor is an `Option` rather than a plain number because sequences
    /// start at 0, so no integer can mean "nothing yet". Taking `0` for that
    /// would skip `seq` 0 — the `Started` event, the one carrying the session
    /// id and the model — and a client would render a run that never began.
    pub fn events_since(
        &self,
        run_id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<AgentEnvelope>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT run_id, seq, at_ms, payload FROM events
             WHERE run_id = ?1 AND (?2 IS NULL OR seq > ?2) ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![run_id, after.map(|s| s as i64), limit as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (agent_id, seq, at_ms, payload) = row?;
            out.push(AgentEnvelope {
                agent_id,
                seq: seq as u64,
                at_ms,
                event: serde_json::from_str(&payload)?,
            });
        }
        Ok(out)
    }

    /// Insert or update the record of one delegation.
    ///
    /// Two fields resist being overwritten, because two processes write this
    /// row and only one of them knows the truth about each.
    ///
    /// `pid`/`pgid` are not overwritten by an update that does not carry them.
    /// The supervisor records them once, and the process that spawned the run
    /// may keep saving summaries long afterwards from an in-memory copy that
    /// never learned them; `COALESCE` keeps the launch facts from being erased
    /// by a later save that simply did not know.
    ///
    /// **A terminal `status` is never overwritten.** Anyone following a run
    /// derives a status from its events, and the events cannot tell a killed
    /// run from a completed one — both end in a `Finished` with no exit code.
    /// Only the supervisor saw the signal, and it records that through
    /// [`Store::set_run_status`], which is unconditional. Without this guard a
    /// follower's save landing afterwards reported every killed run as
    /// `completed`; it did, and that is what this clause is for.
    pub fn save_run(&self, run: &StoredRun) -> Result<()> {
        let summary = serde_json::to_string(&run.summary)?;
        self.write(|tx| {
            tx.execute(
                "INSERT INTO runs
                   (id, name, harness, status, cwd, session_id, pid, pgid,
                    created_at_ms, summary)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name,
                   status = CASE
                     WHEN runs.status IN ('completed', 'failed', 'killed')
                       THEN runs.status
                     ELSE excluded.status
                   END,
                   session_id = excluded.session_id,
                   pid = COALESCE(excluded.pid, runs.pid),
                   pgid = COALESCE(excluded.pgid, runs.pgid),
                   summary = excluded.summary",
                params![
                    run.id,
                    run.name,
                    run.harness,
                    run.status,
                    run.cwd,
                    run.session_id,
                    run.pid,
                    run.pgid,
                    run.created_at_ms,
                    summary,
                ],
            )?;
            Ok(())
        })
    }

    /// Record which process group is supervising a run.
    ///
    /// Its own statement rather than part of `save_run`, because the pgid is
    /// known only after the launch succeeded, and it must not depend on the
    /// caller holding an otherwise up-to-date summary.
    pub fn set_run_process(&self, run_id: &str, pid: u32, pgid: u32) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE runs SET pid = ?2, pgid = ?3 WHERE id = ?1",
                params![run_id, pid, pgid],
            )?;
            Ok(())
        })
    }

    /// Record the harness-assigned conversation id for a run.
    ///
    /// Written by the supervisor the moment the harness reports it, because
    /// `--resume` depends on it and the process that launched the run may be
    /// long gone by the time anyone wants to continue the conversation.
    pub fn set_run_session(&self, run_id: &str, session_id: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE runs SET session_id = ?2 WHERE id = ?1",
                params![run_id, session_id],
            )?;
            Ok(())
        })
    }

    /// Set a run's terminal status. Used by the supervisor, which is the only
    /// process that knows how the harness actually exited.
    pub fn set_run_status(&self, run_id: &str, status: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE runs SET status = ?2 WHERE id = ?1",
                params![run_id, status],
            )?;
            Ok(())
        })
    }

    /// One run by id, or `None`.
    pub fn run(&self, run_id: &str) -> Result<Option<StoredRun>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT id, name, harness, status, cwd, session_id, pid, pgid,
                        created_at_ms, summary
                   FROM runs WHERE id = ?1",
                params![run_id],
                run_from_row,
            )
            .optional()?)
    }

    /// Most recent runs first.
    pub fn runs(&self, limit: usize) -> Result<Vec<StoredRun>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, name, harness, status, cwd, session_id, pid, pgid,
                    created_at_ms, summary
               FROM runs ORDER BY created_at_ms DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], run_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The harness session id of the most recent run, so `--continue` can be
    /// resolved to a specific conversation rather than "whatever was last".
    pub fn last_session_for(&self, harness: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT session_id FROM runs
                  WHERE harness = ?1 AND session_id IS NOT NULL
                  ORDER BY created_at_ms DESC LIMIT 1",
                params![harness],
                |r| r.get(0),
            )
            .optional()?)
    }

    // ---- contended state ----------------------------------------------

    /// Take ownership of a task. Returns whether this caller won.
    ///
    /// One statement, not a read then a write: the `owner IS NULL` guard is what
    /// makes two agents racing produce one winner instead of two.
    pub fn claim_task(&self, task_id: &str, owner: &str) -> Result<bool> {
        self.write(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO tasks (id, status) VALUES (?1, 'open')",
                params![task_id],
            )?;
            let changed = tx.execute(
                "UPDATE tasks SET owner = ?2, claimed_at = ?3
                  WHERE id = ?1 AND owner IS NULL",
                params![task_id, owner, now_ms()],
            )?;
            Ok(changed == 1)
        })
    }

    // ---- teams ----------------------------------------------------------

    /// Add a member, or update the role and harness of one already there.
    pub fn join_team(
        &self,
        team: &str,
        name: &str,
        harness: HarnessKind,
        role: &str,
    ) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO team_members (team, name, harness, role, status, joined_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'ready', ?5)
                 ON CONFLICT(team, name) DO UPDATE SET harness = ?3, role = ?4",
                params![team, name, harness.id(), role, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn set_member_status(&self, team: &str, name: &str, status: MemberStatus) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE team_members SET status = ?3 WHERE team = ?1 AND name = ?2",
                params![team, name, status.as_str()],
            )?;
            Ok(())
        })
    }

    /// Record which run embodies a member now, and which conversation to resume.
    ///
    /// A `None` session is ignored rather than written: a later turn that does
    /// not report an id must not erase the one the member needs to keep its
    /// context.
    pub fn bind_member(
        &self,
        team: &str,
        name: &str,
        agent_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE team_members
                    SET agent_id = ?3,
                        session_id = COALESCE(?4, session_id)
                  WHERE team = ?1 AND name = ?2",
                params![team, name, agent_id, session_id],
            )?;
            Ok(())
        })
    }

    pub fn team_members(&self, team: &str) -> Result<Vec<Member>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT team, name, harness, role, status, agent_id, session_id
               FROM team_members WHERE team = ?1 ORDER BY joined_at_ms, name",
        )?;
        let rows = stmt.query_map(params![team], |r| {
            let harness: String = r.get(2)?;
            let status: String = r.get(4)?;
            Ok(Member {
                team: r.get(0)?,
                name: r.get(1)?,
                // A row naming a harness this build does not know still lists,
                // as the harness Jod would have to be told about.
                harness: HarnessKind::from_id(&harness).unwrap_or(HarnessKind::ClaudeCode),
                role: r.get(3)?,
                status: MemberStatus::parse(&status),
                agent_id: r.get(5)?,
                session_id: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every team that has a member.
    pub fn teams(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt =
            conn.prepare("SELECT DISTINCT team FROM team_members ORDER BY team")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Put a message on the bus. `to` of `None` is a broadcast to every member
    /// except the sender. Returns who it was addressed to.
    pub fn send_team_message(
        &self,
        team: &str,
        from: &str,
        to: Option<&str>,
        text: &str,
    ) -> Result<Vec<String>> {
        let recipients: Vec<String> = match to {
            Some(one) => vec![one.to_string()],
            None => self
                .team_members(team)?
                .into_iter()
                .map(|m| m.name)
                .filter(|n| n != from)
                .collect(),
        };
        let at = now_ms();
        self.write(|tx| {
            for name in &recipients {
                tx.execute(
                    "INSERT INTO team_messages (team, sender, recipient, body, at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![team, from, name, text, at],
                )?;
            }
            Ok(())
        })?;
        Ok(recipients)
    }

    /// Everything ever addressed to a member, delivered or not.
    pub fn team_inbox(&self, team: &str, member: &str) -> Result<Vec<Message>> {
        self.messages(team, member, false)
    }

    /// Messages waiting to be shown, without consuming them.
    pub fn team_unread(&self, team: &str, member: &str) -> Result<Vec<Message>> {
        self.messages(team, member, true)
    }

    fn messages(&self, team: &str, member: &str, only_pending: bool) -> Result<Vec<Message>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let sql = if only_pending {
            "SELECT id, team, sender, recipient, body, at_ms FROM team_messages
              WHERE team = ?1 AND recipient = ?2 AND delivered = 0 ORDER BY id"
        } else {
            "SELECT id, team, sender, recipient, body, at_ms FROM team_messages
              WHERE team = ?1 AND recipient = ?2 ORDER BY id"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![team, member], |r| {
            Ok(Message {
                id: r.get(0)?,
                team: r.get(1)?,
                from: r.get(2)?,
                to: r.get(3)?,
                text: r.get(4)?,
                at_ms: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Take every pending message and mark it delivered, in one transaction.
    ///
    /// Read-then-write would let two turns of the same agent pick up the same
    /// instruction and act on it twice.
    pub fn drain_inbox(&self, team: &str, member: &str) -> Result<Vec<Message>> {
        self.write(|tx| {
            let mut stmt = tx.prepare(
                "SELECT id, team, sender, recipient, body, at_ms FROM team_messages
                  WHERE team = ?1 AND recipient = ?2 AND delivered = 0 ORDER BY id",
            )?;
            let pending: Vec<Message> = stmt
                .query_map(params![team, member], |r| {
                    Ok(Message {
                        id: r.get(0)?,
                        team: r.get(1)?,
                        from: r.get(2)?,
                        to: r.get(3)?,
                        text: r.get(4)?,
                        at_ms: r.get(5)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.execute(
                "UPDATE team_messages SET delivered = 1
                  WHERE team = ?1 AND recipient = ?2 AND delivered = 0",
                params![team, member],
            )?;
            Ok(pending)
        })
    }

    /// Put a task on a team's board. Re-adding an id leaves the original alone,
    /// so a retry cannot orphan work someone already claimed.
    pub fn add_team_task(&self, team: &str, id: &str, title: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO tasks (id, status, team, title) VALUES (?1, 'open', ?2, ?3)
                 ON CONFLICT(id) DO NOTHING",
                params![id, team, title],
            )?;
            Ok(())
        })
    }

    pub fn team_tasks(&self, team: &str) -> Result<Vec<TeamTask>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, COALESCE(title, id), owner, status FROM tasks
              WHERE team = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![team], |r| {
            Ok(TeamTask {
                id: r.get(0)?,
                title: r.get(1)?,
                owner: r.get(2)?,
                status: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Whether this id names a task on some team's board.
    ///
    /// `claim_task` is a lease primitive and will claim an id that names
    /// nothing, creating it — right for a lease, wrong for a team command,
    /// where a mistyped id would otherwise report success and leave behind a
    /// task no board ever shows.
    pub fn is_team_task(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE id = ?1 AND team IS NOT NULL",
            params![id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Mark a task done. Returns whether it actually changed anything, so a
    /// caller can tell "finished it" from "there was no such task".
    pub fn complete_task(&self, id: &str) -> Result<bool> {
        self.write(|tx| {
            let changed = tx.execute(
                "UPDATE tasks SET status = 'done' WHERE id = ?1",
                params![id],
            )?;
            Ok(changed > 0)
        })
    }

    // ---- memory ---------------------------------------------------------

    pub fn remember(&self, fact: NewFact) -> Result<i64> {
        self.write(|tx| insert_fact(tx, &fact))
    }

    /// Remember many facts in one transaction.
    ///
    /// An agent that has just read a conversation writes what it learned all at
    /// once, and a transaction per fact would pay the commit cost per fact for
    /// no isolation anyone asked for. Either every fact from one extraction
    /// lands or none does, which is also the honest unit: half an extraction is
    /// a memory of something that did not happen.
    pub fn remember_all(&self, facts: &[NewFact]) -> Result<Vec<i64>> {
        self.write(|tx| facts.iter().map(|f| insert_fact(tx, f)).collect())
    }

    /// Replace a belief. The old fact is closed, never deleted, and points at
    /// the one that replaced it — so "what did Jod think last month" stays
    /// answerable.
    pub fn supersede(&self, old_id: i64, fact: NewFact) -> Result<i64> {
        self.write(|tx| {
            let new_id = insert_fact(tx, &fact)?;
            tx.execute(
                "UPDATE facts SET valid_to = ?2, invalidated_by = ?3
                  WHERE id = ?1 AND valid_to IS NULL",
                params![old_id, iso_now(), new_id],
            )?;
            // Close the superseded belief's edge too, or a traversal would keep
            // walking through something Jod has stopped believing.
            tx.execute(
                "UPDATE relations SET valid_to_ms = ?2
                  WHERE fact_id = ?1 AND valid_to_ms IS NULL",
                params![old_id, now_ms()],
            )?;
            Ok(new_id)
        })
    }

    /// Full-text recall over currently-believed facts, best match first.
    ///
    /// `scope` filters *before* ranking rather than nudging the score: the
    /// research measured scope-as-a-boost leaking facts across domains 79% of
    /// the time, and scope-as-a-partition leaking none.
    pub fn recall_in(&self, scope: Option<&str>, query: &str, limit: usize) -> Result<Vec<Fact>> {
        self.recall_from(scope, query, limit, false)
    }

    /// Recall across every scope.
    ///
    /// Cross-scope on purpose, for a person searching their own memory from the
    /// TUI. It is *not* the call an agent's retrieval should make: the research
    /// measured scope-blind answers leaking a fact from another domain on 20% of
    /// queries. Anything answering on Jod's behalf passes a scope.
    pub fn recall(&self, query: &str, limit: usize) -> Result<Vec<Fact>> {
        self.recall_in(None, query, limit)
    }

    /// Recall, saying explicitly whether untrusted material may answer.
    ///
    /// `untrusted` facts came from outside — a fetched page, an email, a Linear
    /// comment — and are excluded by default, which is the whole point of
    /// storing origin in its own column. Including them measured an attack
    /// success rate of 0.17–0.25; excluding them, 0.00.
    ///
    /// `include_untrusted` exists for the memory browser, where the question is
    /// "what did that page claim" rather than "what is true". A caller that
    /// wants it has to say so at the call site, where the decision is visible.
    pub fn recall_from(
        &self,
        scope: Option<&str>,
        query: &str,
        limit: usize,
        include_untrusted: bool,
    ) -> Result<Vec<Fact>> {
        let Some(expr) = fts_query(query) else {
            return Ok(vec![]);
        };
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT f.id, f.scope, f.subject, f.predicate, f.object, f.origin, f.source,
                    f.valid_from, f.valid_to, f.recorded_at_ms, f.state
               FROM facts_fts JOIN facts f ON f.id = facts_fts.rowid
              WHERE facts_fts MATCH ?1
                AND f.valid_to IS NULL
                AND (?2 IS NULL OR f.scope = ?2)
                AND (?4 OR f.origin <> 'untrusted')
              ORDER BY bm25(facts_fts) LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![expr, scope, limit as i64, include_untrusted],
            row_to_fact,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Everything currently believed about one subject.
    pub fn facts_about(&self, subject: &str) -> Result<Vec<Fact>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, scope, subject, predicate, object, origin, source,
                    valid_from, valid_to, recorded_at_ms, state
               FROM facts WHERE subject = ?1 AND valid_to IS NULL
              ORDER BY recorded_at_ms DESC",
        )?;
        let rows = stmt.query_map(params![subject], row_to_fact)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Forget something completely. Returns how many versions were destroyed.
    ///
    /// This physically deletes *every* version, not just the current one, and
    /// leaves a tombstone recording that it happened. Closing only the head —
    /// the obvious implementation — leaves the withdrawn fact fully readable to
    /// any question phrased about the past, which the research measured leaking
    /// on 56% of historical queries. "Jod forgot that" and "Jod says it forgot
    /// that" have to be the same thing.
    pub fn forget(&self, scope: &str, subject: &str, predicate: &str) -> Result<usize> {
        self.write(|tx| {
            let versions = tx.execute(
                "DELETE FROM facts WHERE scope = ?1 AND subject = ?2 AND predicate = ?3",
                params![scope, subject, predicate],
            )?;
            if versions > 0 {
                tx.execute(
                    "INSERT INTO tombstones (scope, subject, predicate, deleted_at_ms, versions)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![scope, subject, predicate, now_ms(), versions as i64],
                )?;
            }
            Ok(versions)
        })
    }

    // ---- the memory graph -----------------------------------------------

    /// Everything within `depth` hops of `name`, nearest first.
    ///
    /// Undirected, because the question a person asks is "what is related to
    /// this", which does not care which way the fact was phrased. That needs
    /// *two* recursive terms, one per index — a single
    /// `ON (src = node OR dst = node)` defeats both and falls back to a scan.
    ///
    /// `at_ms` selects the instant to believe: `now` for what is true, any past
    /// instant for what Jod believed then. The predicate sits inside the
    /// recursive step so an edge that was not valid then is never expanded —
    /// and because that prunes about a third of the edges, the filtered
    /// traversal measured *faster* than the unfiltered one.
    pub fn neighbourhood(
        &self,
        scope: &str,
        name: &str,
        depth: u32,
        at_ms: i64,
    ) -> Result<Vec<Neighbour>> {
        let Some(start) = self.entity_id(scope, name)? else {
            return Ok(vec![]);
        };
        let depth = depth.clamp(1, MAX_HOPS) as i64;
        let conn = self.conn.lock().expect("store lock poisoned");
        // Two things in this query are load-bearing, and neither is obvious.
        //
        // `UNION` rather than `UNION ALL` deduplicates, so a cycle terminates
        // without a visited table.
        //
        // `CROSS JOIN` rather than `JOIN` pins the join order. A recursive CTE
        // has no statistics, so the planner guesses — and it guesses wrong:
        // measured, it made `relations` the outer loop matching on `scope=?`
        // alone, which selects every row, then scanned the frontier inside it.
        // That is a cross product per step, and it used the in-edge index for
        // both directions, so the out-edge index was never touched at all.
        // 2-hop over 10k edges took 903 ms. With the order pinned, the frontier
        // drives and each step is a covering-index probe: 14 ms. Same schema,
        // same indexes, 64x.
        let mut stmt = conn.prepare(
            "WITH RECURSIVE reach(node, depth) AS (
               SELECT ?1, 0
               UNION
               SELECT r.dst, x.depth + 1
                 FROM reach x CROSS JOIN relations r
                      ON r.src = x.node AND r.scope = ?3
                WHERE x.depth < ?2
                  AND (r.valid_to_ms   IS NULL OR r.valid_to_ms   > ?4)
                  AND (r.valid_from_ms IS NULL OR r.valid_from_ms <= ?4)
               UNION
               SELECT r.src, x.depth + 1
                 FROM reach x CROSS JOIN relations r
                      ON r.dst = x.node AND r.scope = ?3
                WHERE x.depth < ?2
                  AND (r.valid_to_ms   IS NULL OR r.valid_to_ms   > ?4)
                  AND (r.valid_from_ms IS NULL OR r.valid_from_ms <= ?4)
             )
             SELECT e.id, e.name, e.kind, MIN(reach.depth) AS hops
               FROM reach JOIN entities e ON e.id = reach.node
              WHERE reach.node <> ?1
              GROUP BY e.id
              ORDER BY hops, e.name
              LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![start, depth, scope, at_ms, MAX_NEIGHBOURS],
            |r| {
                Ok(Neighbour {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    hops: r.get(3)?,
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// How two entities are connected, as the shortest chain of names.
    ///
    /// A bidirectional breadth-first search in Rust, one indexed query per
    /// level, expanding whichever side is smaller. The obvious alternative —
    /// threading the route through a recursive CTE as a string and using it as
    /// a per-branch visited set — is exponential: it measured 7,983 ms against
    /// 81 ms here, and hit a ten-second ceiling on 6 of 20 queries.
    pub fn path_between(
        &self,
        scope: &str,
        from: &str,
        to: &str,
        max_depth: u32,
    ) -> Result<Option<Vec<String>>> {
        let (Some(start), Some(goal)) = (self.entity_id(scope, from)?, self.entity_id(scope, to)?)
        else {
            return Ok(None);
        };
        if start == goal {
            return Ok(Some(vec![from.to_string()]));
        }

        // Each side maps a reached node to the node it was reached from, so a
        // meeting point can be unwound into a route without a second search.
        let mut seen_fwd: HashMap<i64, Option<i64>> = HashMap::from([(start, None)]);
        let mut seen_bwd: HashMap<i64, Option<i64>> = HashMap::from([(goal, None)]);
        let mut frontier_fwd = vec![start];
        let mut frontier_bwd = vec![goal];

        for _ in 0..max_depth.clamp(1, MAX_PATH_DEPTH) {
            // Expand the cheaper side. On a scale-free graph one frontier is
            // routinely orders of magnitude larger than the other.
            let forward = frontier_fwd.len() <= frontier_bwd.len();
            let (frontier, seen, other) = if forward {
                (&mut frontier_fwd, &mut seen_fwd, &seen_bwd)
            } else {
                (&mut frontier_bwd, &mut seen_bwd, &seen_fwd)
            };

            let mut next = Vec::new();
            for (node, reached) in self.step(scope, frontier, forward)? {
                if seen.contains_key(&reached) {
                    continue;
                }
                seen.insert(reached, Some(node));
                if other.contains_key(&reached) {
                    return Ok(Some(self.route(&seen_fwd, &seen_bwd, reached)?));
                }
                next.push(reached);
            }
            if next.is_empty() {
                return Ok(None);
            }
            *frontier = next;
        }
        Ok(None)
    }

    /// One breadth-first level: every node adjacent to `frontier`.
    ///
    /// Chunked, because SQLite's bound-parameter ceiling is 32,766 and a hub's
    /// neighbourhood can exceed it — a limit that only shows up on the graphs
    /// that matter.
    fn step(&self, scope: &str, frontier: &[i64], forward: bool) -> Result<Vec<(i64, i64)>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let (near, far) = if forward { ("src", "dst") } else { ("dst", "src") };
        let mut found = Vec::new();
        for chunk in frontier.chunks(512) {
            let holes = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT r.{near}, r.{far} FROM relations r
                  WHERE r.scope = ? AND r.{near} IN ({holes}) AND r.valid_to_ms IS NULL"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut binds: Vec<&dyn rusqlite::ToSql> = vec![&scope];
            binds.extend(chunk.iter().map(|n| n as &dyn rusqlite::ToSql));
            let rows = stmt.query_map(binds.as_slice(), |r| Ok((r.get(0)?, r.get(1)?)))?;
            found.extend(rows.collect::<std::result::Result<Vec<_>, _>>()?);
        }
        Ok(found)
    }

    /// Unwind both halves of a bidirectional search through their meeting node.
    fn route(
        &self,
        fwd: &HashMap<i64, Option<i64>>,
        bwd: &HashMap<i64, Option<i64>>,
        meeting: i64,
    ) -> Result<Vec<String>> {
        let mut left = vec![meeting];
        while let Some(Some(prev)) = fwd.get(&left[left.len() - 1]).copied() {
            left.push(prev);
        }
        left.reverse();
        let mut at = meeting;
        while let Some(Some(next)) = bwd.get(&at).copied() {
            left.push(next);
            at = next;
        }
        self.names(&left)
    }

    fn names(&self, ids: &[i64]) -> Result<Vec<String>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare("SELECT name FROM entities WHERE id = ?1")?;
        ids.iter()
            .map(|id| Ok(stmt.query_row(params![id], |r| r.get(0))?))
            .collect()
    }

    /// Text recall, then one graph hop, ranked together.
    ///
    /// BM25 picks the entities the words point at and the graph supplies what
    /// they connect to; scope partitions before either. The second hop is what
    /// the prior retrieval research measured as worth 0.00 → 0.42 on multi-hop
    /// questions, and it costs about 1.3x the text query alone.
    pub fn recall_expanded(
        &self,
        scope: &str,
        query: &str,
        hops: u32,
        limit: usize,
    ) -> Result<Vec<Neighbour>> {
        let Some(expr) = fts_query(query) else {
            return Ok(vec![]);
        };
        let hops = hops.clamp(0, MAX_HOPS) as i64;
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "WITH seeds AS (
               SELECT r.src AS node, bm25(facts_fts) AS rank
                 FROM facts_fts
                 JOIN facts f     ON f.id = facts_fts.rowid
                 JOIN relations r ON r.fact_id = f.id
                WHERE facts_fts MATCH ?1
                  AND f.scope = ?2
                  AND f.valid_to IS NULL
                  -- Untrusted material must not seed an expansion either. A
                  -- page that cannot answer directly must not be able to steer
                  -- which part of the graph gets walked.
                  AND f.origin <> 'untrusted'
                ORDER BY rank LIMIT 20
             ),
             reach(node, depth, rank) AS (
               SELECT node, 0, rank FROM seeds
               UNION
               -- CROSS JOIN for the same reason as `neighbourhood`: without it
               -- the planner drives from `relations` and the covering index is
               -- never used.
               SELECT r.dst, x.depth + 1, x.rank
                 FROM reach x CROSS JOIN relations r
                      ON r.src = x.node AND r.scope = ?2
                WHERE x.depth < ?3 AND r.valid_to_ms IS NULL
             )
             SELECT e.id, e.name, e.kind, MIN(reach.depth) AS hops
               FROM reach JOIN entities e ON e.id = reach.node
              GROUP BY e.id
              -- A hop away is worth less than a direct hit, but not nothing.
              ORDER BY MIN(reach.rank) / (1.0 + MIN(reach.depth)), e.name
              LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![expr, scope, hops, limit as i64], |r| {
            Ok(Neighbour {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                hops: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn entity_id(&self, scope: &str, name: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT id FROM entities WHERE scope = ?1 AND name = ?2",
                params![scope, name],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// How many entities and relations the index holds.
    pub fn graph_size(&self) -> Result<(usize, usize)> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let entities: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))?;
        let relations: i64 = conn.query_row("SELECT COUNT(*) FROM relations", [], |r| r.get(0))?;
        Ok((entities as usize, relations as usize))
    }

    /// Throw the graph away and fold it back out of `facts`.
    ///
    /// The index is derived, so this must always be possible — it is what makes
    /// "the graph drifted" a repairable state rather than a corrupt one.
    pub fn rebuild_graph(&self) -> Result<usize> {
        self.write(|tx| {
            tx.execute("DELETE FROM relations", [])?;
            tx.execute("DELETE FROM entities", [])?;
            let facts: Vec<(i64, String, String, String, String, Option<String>, Option<String>)> = {
                let mut stmt = tx.prepare(
                    "SELECT id, scope, subject, predicate, object, valid_from, valid_to
                       FROM facts ORDER BY id",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            let total = facts.len();
            for (id, scope, subject, predicate, object, valid_from, valid_to) in facts {
                link(
                    tx, id, &scope, &subject, &predicate, &object,
                    valid_from.as_deref(), valid_to.as_deref(),
                )?;
            }
            Ok(total)
        })
    }
}

/// How far a traversal may go.
///
/// Not a performance limit — undirected 3-hop is 22 ms at a million edges. A
/// product one: four undirected hops from a well-connected entity returned 20%
/// of every node in the database, and a neighbourhood that size is not a
/// retrieval result, it is the graph.
const MAX_HOPS: u32 = 3;

/// Paths may run longer than a neighbourhood, because a route is one chain
/// rather than an expanding front.
const MAX_PATH_DEPTH: u32 = 6;

/// The most neighbours one traversal will hand back.
///
/// Traversal from a hub is the case that decides whether this is usable: three
/// undirected hops from the highest-degree node reaches most of a scale-free
/// graph. Nothing reads two thousand neighbours, so the cap is about what a
/// caller can use rather than what SQLite can produce — and it bounds the sort,
/// which is the part that grows fastest.
const MAX_NEIGHBOURS: i64 = 500;

/// An entity reached from another, and how far away it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Neighbour {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub hops: i64,
}

/// Intern an entity, returning its id.
fn intern(tx: &rusqlite::Transaction, scope: &str, name: &str) -> Result<i64> {
    let at = now_ms();
    tx.execute(
        "INSERT INTO entities (scope, kind, name, first_seen_ms, last_seen_ms)
              VALUES (?1, 'thing', ?2, ?3, ?3)
         ON CONFLICT(scope, kind, name)
              DO UPDATE SET last_seen_ms = ?3",
        params![scope, name, at],
    )?;
    Ok(tx.query_row(
        "SELECT id FROM entities WHERE scope = ?1 AND kind = 'thing' AND name = ?2",
        params![scope, name],
        |r| r.get(0),
    )?)
}

/// Add one fact to the graph as a single edge between its subject and object.
fn link(
    tx: &rusqlite::Transaction,
    fact_id: i64,
    scope: &str,
    subject: &str,
    predicate: &str,
    object: &str,
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> Result<()> {
    // A fact with no subject or object is not a relationship between two
    // things, so it stays a fact and never becomes an edge.
    if subject.trim().is_empty() || object.trim().is_empty() {
        return Ok(());
    }
    let src = intern(tx, scope, subject)?;
    let dst = intern(tx, scope, object)?;
    tx.execute(
        "INSERT INTO relations
           (scope, src, dst, predicate, fact_id, valid_from_ms, valid_to_ms, recorded_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            scope,
            src,
            dst,
            predicate,
            fact_id,
            valid_from.and_then(iso_to_ms),
            valid_to.and_then(iso_to_ms),
            now_ms()
        ],
    )?;
    Ok(())
}

/// An ISO instant as epoch milliseconds.
///
/// `facts` stores validity as text because that is what a human writes; the
/// graph stores it as an integer because a recursive step compares it on every
/// edge. A date with no time is taken at midnight UTC, which is what someone
/// writing `2026-08-10` means.
fn iso_to_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(t.timestamp_millis());
    }
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|t| t.and_utc().timestamp_millis())
}

fn insert_fact(tx: &rusqlite::Transaction, fact: &NewFact) -> Result<i64> {
    tx.execute(
        "INSERT INTO facts
           (scope, subject, predicate, object, origin, source, valid_from, recorded_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            fact.scope,
            fact.subject,
            fact.predicate,
            fact.object,
            fact.origin.as_str(),
            fact.source,
            fact.valid_from,
            now_ms()
        ],
    )?;
    let id = tx.last_insert_rowid();
    // In the same transaction as the fact, so the graph can never be missing an
    // edge for a fact that committed.
    link(
        tx,
        id,
        &fact.scope,
        &fact.subject,
        &fact.predicate,
        &fact.object,
        fact.valid_from.as_deref(),
        None,
    )?;
    Ok(id)
}

fn row_to_fact(r: &rusqlite::Row) -> rusqlite::Result<Fact> {
    Ok(Fact {
        id: r.get(0)?,
        scope: r.get(1)?,
        subject: r.get(2)?,
        predicate: r.get(3)?,
        object: r.get(4)?,
        origin: Origin::parse(&r.get::<_, String>(5)?),
        source: r.get(6)?,
        valid_from: r.get(7)?,
        valid_to: r.get(8)?,
        recorded_at_ms: r.get(9)?,
        state: r.get(10)?,
    })
}

/// One persisted delegation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredRun {
    pub id: String,
    pub name: String,
    pub harness: String,
    pub status: String,
    pub cwd: String,
    pub session_id: Option<String>,
    /// The supervising `jod-run` process, and the group it leads. `None` until
    /// the run has been launched.
    pub pid: Option<u32>,
    pub pgid: Option<u32>,
    pub created_at_ms: i64,
    /// The full client-facing summary, kept verbatim so adding a field to it
    /// never needs a migration here.
    pub summary: serde_json::Value,
}

fn run_from_row(r: &rusqlite::Row) -> rusqlite::Result<StoredRun> {
    Ok(StoredRun {
        id: r.get(0)?,
        name: r.get(1)?,
        harness: r.get(2)?,
        status: r.get(3)?,
        cwd: r.get(4)?,
        session_id: r.get(5)?,
        pid: r.get(6)?,
        pgid: r.get(7)?,
        created_at_ms: r.get(8)?,
        summary: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
    })
}

fn event_kind(env: &AgentEnvelope) -> String {
    serde_json::to_value(&env.event)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Turn free text into an FTS5 expression that cannot be a syntax error.
///
/// User text reaches this straight from a prompt, and bare FTS5 treats `"`,
/// `*`, `:`, `-` and `NEAR` as operators — so an innocent question like
/// `what's the plan?` would otherwise fail the query rather than search for it.
/// Each word becomes a quoted term; terms are OR-ed and ranked, so a long
/// question still matches the facts sharing most of its words.
fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\""))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AgentEvent;

    fn store() -> Store {
        Store::in_memory().expect("in-memory store")
    }

    fn envelope(run: &str, seq: u64, text: &str) -> AgentEnvelope {
        AgentEnvelope {
            agent_id: run.into(),
            at_ms: 1_700_000_000_000,
            seq,
            event: AgentEvent::Message { text: text.into() },
        }
    }

    #[test]
    fn a_fresh_store_has_no_teams() {
        let s = store();
        assert!(s.teams().unwrap().is_empty());
        assert!(s.team_members("crew").unwrap().is_empty());
        assert!(s.team_tasks("crew").unwrap().is_empty());
        assert!(s.team_inbox("crew", "nobody").unwrap().is_empty());
        assert!(s.drain_inbox("crew", "nobody").unwrap().is_empty());
    }

    /// The capability the whole design exists for: one team, three harnesses.
    /// No harness can put a teammate from another harness on its own team.
    #[test]
    fn one_team_can_span_every_harness() {
        let s = store();
        for (name, harness) in [
            ("lead", HarnessKind::ClaudeCode),
            ("builder", HarnessKind::OpenCode),
            ("scout", HarnessKind::Agy),
        ] {
            s.join_team("crew", name, harness, "r").unwrap();
        }
        let kinds: Vec<HarnessKind> = s
            .team_members("crew")
            .unwrap()
            .iter()
            .map(|m| m.harness)
            .collect();
        assert_eq!(kinds.len(), HarnessKind::ALL.len());
        for kind in HarnessKind::ALL {
            assert!(kinds.contains(&kind), "{kind:?} must be able to join a team");
        }
    }

    #[test]
    fn members_are_listed_in_the_order_they_joined() {
        let s = store();
        s.join_team("crew", "lead", HarnessKind::ClaudeCode, "coordinator")
            .unwrap();
        s.join_team("crew", "scout", HarnessKind::Agy, "research")
            .unwrap();

        let members = s.team_members("crew").unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].name, "lead");
        assert_eq!(members[0].role, "coordinator");
        assert_eq!(members[0].status, MemberStatus::Ready);
        assert_eq!(members[1].harness, HarnessKind::Agy);
    }

    #[test]
    fn rejoining_updates_rather_than_duplicating() {
        let s = store();
        s.join_team("crew", "scout", HarnessKind::OpenCode, "old")
            .unwrap();
        s.join_team("crew", "scout", HarnessKind::Agy, "new").unwrap();

        let members = s.team_members("crew").unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].role, "new");
        assert_eq!(members[0].harness, HarnessKind::Agy);
    }

    #[test]
    fn teams_are_separate_namespaces() {
        let s = store();
        s.join_team("alpha", "scout", HarnessKind::OpenCode, "r")
            .unwrap();
        s.join_team("beta", "scout", HarnessKind::ClaudeCode, "r")
            .unwrap();

        assert_eq!(s.teams().unwrap(), vec!["alpha", "beta"]);
        assert_eq!(
            s.team_members("alpha").unwrap()[0].harness,
            HarnessKind::OpenCode
        );
        assert_eq!(
            s.team_members("beta").unwrap()[0].harness,
            HarnessKind::ClaudeCode
        );
    }

    #[test]
    fn status_and_binding_land_on_the_member() {
        let s = store();
        s.join_team("crew", "scout", HarnessKind::OpenCode, "r")
            .unwrap();
        s.set_member_status("crew", "scout", MemberStatus::Busy)
            .unwrap();
        s.bind_member("crew", "scout", Some("run-1"), Some("ses-1"))
            .unwrap();

        let m = &s.team_members("crew").unwrap()[0];
        assert_eq!(m.status, MemberStatus::Busy);
        assert_eq!(m.agent_id.as_deref(), Some("run-1"));
        assert_eq!(m.session_id.as_deref(), Some("ses-1"));
    }

    /// A later turn that reports no session must not erase the one the member
    /// needs, or it silently loses its context on the next resume.
    #[test]
    fn rebinding_without_a_session_keeps_the_known_one() {
        let s = store();
        s.join_team("crew", "scout", HarnessKind::OpenCode, "r")
            .unwrap();
        s.bind_member("crew", "scout", Some("run-1"), Some("ses-1"))
            .unwrap();
        s.bind_member("crew", "scout", Some("run-2"), None).unwrap();

        let m = &s.team_members("crew").unwrap()[0];
        assert_eq!(m.agent_id.as_deref(), Some("run-2"));
        assert_eq!(m.session_id.as_deref(), Some("ses-1"));
    }

    #[test]
    fn a_direct_message_reaches_only_its_recipient() {
        let s = store();
        s.join_team("crew", "lead", HarnessKind::ClaudeCode, "r")
            .unwrap();
        s.join_team("crew", "scout", HarnessKind::OpenCode, "r")
            .unwrap();

        let to = s
            .send_team_message("crew", "lead", Some("scout"), "look at the parser")
            .unwrap();

        assert_eq!(to, vec!["scout".to_string()]);
        assert_eq!(s.team_inbox("crew", "scout").unwrap().len(), 1);
        assert!(s.team_inbox("crew", "lead").unwrap().is_empty());
    }

    #[test]
    fn a_broadcast_reaches_everyone_but_the_sender() {
        let s = store();
        for name in ["lead", "a", "b"] {
            s.join_team("crew", name, HarnessKind::ClaudeCode, "r")
                .unwrap();
        }
        let to = s.send_team_message("crew", "lead", None, "ship it").unwrap();

        assert_eq!(to.len(), 2);
        assert!(s.team_inbox("crew", "lead").unwrap().is_empty());
        assert_eq!(s.team_inbox("crew", "a").unwrap().len(), 1);
        assert_eq!(s.team_inbox("crew", "b").unwrap().len(), 1);
    }

    /// Draining twice must not replay a message, or a teammate acts on the same
    /// instruction on every turn.
    #[test]
    fn draining_yields_each_message_exactly_once() {
        let s = store();
        s.join_team("crew", "scout", HarnessKind::OpenCode, "r")
            .unwrap();
        for i in 0..3 {
            s.send_team_message("crew", "lead", Some("scout"), &format!("m{i}"))
                .unwrap();
        }

        let first = s.drain_inbox("crew", "scout").unwrap();
        assert_eq!(first.len(), 3);
        assert_eq!(first[0].text, "m0", "delivered in order");
        assert!(s.drain_inbox("crew", "scout").unwrap().is_empty());

        s.send_team_message("crew", "lead", Some("scout"), "m3")
            .unwrap();
        let second = s.drain_inbox("crew", "scout").unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text, "m3");
    }

    #[test]
    fn unread_peeks_without_consuming_but_the_inbox_keeps_everything() {
        let s = store();
        s.join_team("crew", "scout", HarnessKind::OpenCode, "r")
            .unwrap();
        s.send_team_message("crew", "lead", Some("scout"), "peek")
            .unwrap();

        assert_eq!(s.team_unread("crew", "scout").unwrap().len(), 1);
        assert_eq!(s.team_unread("crew", "scout").unwrap().len(), 1);

        s.drain_inbox("crew", "scout").unwrap();
        assert!(s.team_unread("crew", "scout").unwrap().is_empty());
        assert_eq!(
            s.team_inbox("crew", "scout").unwrap().len(),
            1,
            "the transcript survives delivery"
        );
    }

    #[test]
    fn a_delivered_message_carries_who_sent_it() {
        let s = store();
        s.join_team("crew", "scout", HarnessKind::OpenCode, "r")
            .unwrap();
        s.send_team_message("crew", "lead", Some("scout"), "status?")
            .unwrap();

        let m = &s.drain_inbox("crew", "scout").unwrap()[0];
        assert_eq!(m.from, "lead");
        assert_eq!(m.to, "scout");
        assert_eq!(m.as_prompt(), "[message from lead]\nstatus?");
    }

    #[test]
    fn a_broadcast_to_an_empty_team_addresses_nobody() {
        let s = store();
        assert!(s
            .send_team_message("ghost", "lead", None, "anyone?")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_task_board_tracks_claiming_and_completion() {
        let s = store();
        s.add_team_task("crew", "t1", "port the parser").unwrap();
        s.add_team_task("crew", "t2", "write the docs").unwrap();

        assert!(s.claim_task("t1", "scout").unwrap());
        s.complete_task("t1").unwrap();

        let tasks = s.team_tasks("crew").unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title, "port the parser");
        assert_eq!(tasks[0].owner.as_deref(), Some("scout"));
        assert!(tasks[0].is_done());
        assert!(!tasks[1].is_claimed());
    }

    /// `jod team claim` uses this to refuse an id that names nothing, rather
    /// than letting the lease primitive invent it and report success.
    #[test]
    fn only_a_task_on_a_board_counts_as_a_team_task() {
        let s = store();
        s.add_team_task("crew", "t1", "port the parser").unwrap();
        assert!(s.is_team_task("t1").unwrap());

        // A team *name* is not a task id — the mistake that reported success.
        assert!(!s.is_team_task("crew").unwrap());
        assert!(!s.is_team_task("typo").unwrap());

        // A lease claimed outside any team stays off the boards, so it is not
        // a team task either.
        s.claim_task("loose", "someone").unwrap();
        assert!(!s.is_team_task("loose").unwrap());
    }

    #[test]
    fn completing_says_whether_there_was_anything_to_complete() {
        let s = store();
        s.add_team_task("crew", "t1", "port the parser").unwrap();
        assert!(s.complete_task("t1").unwrap());
        assert!(!s.complete_task("no-such-task").unwrap());
    }

    /// The race the atomic claim exists for: two teammates going for the same
    /// task must produce exactly one winner.
    #[test]
    fn a_contested_task_has_exactly_one_winner() {
        let s = store();
        s.add_team_task("crew", "t1", "the only job").unwrap();

        let winners = ["a", "b", "c", "d"]
            .iter()
            .filter(|who| s.claim_task("t1", who).unwrap())
            .count();

        assert_eq!(winners, 1, "exactly one member may own a task");
        assert!(s.team_tasks("crew").unwrap()[0].is_claimed());
    }

    /// Re-adding an id must not orphan work someone already claimed.
    #[test]
    fn re_adding_a_task_leaves_the_claim_alone() {
        let s = store();
        s.add_team_task("crew", "t1", "original").unwrap();
        s.claim_task("t1", "scout").unwrap();
        s.add_team_task("crew", "t1", "different title").unwrap();

        let tasks = s.team_tasks("crew").unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "original");
        assert_eq!(tasks[0].owner.as_deref(), Some("scout"));
    }

    #[test]
    fn tasks_belong_to_their_own_team() {
        let s = store();
        s.add_team_task("alpha", "t1", "alpha work").unwrap();
        s.add_team_task("beta", "t2", "beta work").unwrap();
        assert_eq!(s.team_tasks("alpha").unwrap().len(), 1);
        assert_eq!(s.team_tasks("beta").unwrap()[0].id, "t2");
    }

    /// A task claimed through the pre-existing API, with no team, must not
    /// appear on any team's board.
    #[test]
    fn a_teamless_task_stays_off_every_board() {
        let s = store();
        assert!(s.claim_task("loose", "someone").unwrap());
        assert!(s.team_tasks("crew").unwrap().is_empty());
    }

    #[test]
    fn teams_survive_reopening_the_database() {
        let dir = std::env::temp_dir().join(format!("jod-team-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("jod.db");

        {
            let s = Store::open(&path).unwrap();
            s.join_team("crew", "scout", HarnessKind::Agy, "research")
                .unwrap();
            s.send_team_message("crew", "lead", Some("scout"), "still here?")
                .unwrap();
        }

        let s = Store::open(&path).unwrap();
        assert_eq!(s.team_members("crew").unwrap()[0].role, "research");
        assert_eq!(s.team_unread("crew", "scout").unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `0003` rebuilds `runs` to drop a `NOT NULL` column SQLite cannot relax
    /// in place. A rebuild that lost rows would delete a person's history, so
    /// this opens a genuine pre-`0003` database rather than a simulation of one.
    #[test]
    fn a_database_from_the_tmux_era_still_opens_and_keeps_its_history() {
        let dir = std::env::temp_dir().join(format!("jod-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("jod.db");

        // Exactly what an installed 0.1 wrote: migrations 0001 and 0002, and a
        // `runs` table whose `tmux_session` column is NOT NULL.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS migrations (
                   name TEXT PRIMARY KEY, applied_at_ms INTEGER NOT NULL);",
            )
            .unwrap();
            for (name, sql) in &MIGRATIONS[..2] {
                conn.execute_batch(sql).unwrap();
                conn.execute(
                    "INSERT INTO migrations (name, applied_at_ms) VALUES (?1, 0)",
                    params![name],
                )
                .unwrap();
            }
            for (id, status) in [("done", "completed"), ("interrupted", "running")] {
                conn.execute(
                    "INSERT INTO runs (id, name, harness, status, cwd, session_id,
                                       tmux_session, created_at_ms, summary)
                     VALUES (?1, 'old', 'claude_code', ?2, '/tmp', 'sess-1',
                             'jod-old', 1, '{}')",
                    params![id, status],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO events (run_id, seq, kind, at_ms, payload)
                 VALUES ('done', 0, 'message', 1, '{\"kind\":\"message\",\"text\":\"hi\"}')",
                [],
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();

        let runs = store.runs(10).unwrap();
        assert_eq!(runs.len(), 2, "the rebuild must carry every row over");
        assert_eq!(store.events("done").unwrap().len(), 1, "events are untouched");

        let done = store.run("done").unwrap().unwrap();
        assert_eq!(done.status, "completed");
        assert_eq!(done.session_id.as_deref(), Some("sess-1"));
        assert_eq!(done.pid, None, "there is no process to claim it had");

        // A run left mid-flight by the old transport cannot still be running:
        // its tmux session went away with tmux. Saying so here means nothing
        // downstream has to guess about a row it can no longer probe.
        assert_eq!(
            store.run("interrupted").unwrap().unwrap().status,
            "failed",
            "a run inherited as `running` has no process group to check"
        );

        // And the new columns work on the migrated table.
        store.set_run_process("done", 111, 111).unwrap();
        assert_eq!(store.run("done").unwrap().unwrap().pgid, Some(111));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression, found by killing a real detached run: a follower's
    /// event-derived save landed after the supervisor's authoritative one and
    /// turned `killed` into `completed`.
    #[test]
    fn a_terminal_status_is_not_overwritten_by_a_later_save() {
        let s = store();
        s.save_run(&run("a", "agy", 1)).unwrap();
        s.set_run_status("a", "killed").unwrap();

        // A follower saw `Finished` and concluded the run completed. It did
        // not see the signal, so it does not get to say.
        let mut derived = run("a", "agy", 1);
        derived.status = "completed".into();
        s.save_run(&derived).unwrap();
        assert_eq!(s.run("a").unwrap().unwrap().status, "killed");

        // The supervisor may still correct itself outright, because it is the
        // one process that knows.
        s.set_run_status("a", "failed").unwrap();
        assert_eq!(s.run("a").unwrap().unwrap().status, "failed");
    }

    /// The guard must not freeze a run before it has finished, or a live run
    /// would never leave `running`.
    #[test]
    fn a_running_status_is_still_updated_by_a_save() {
        let s = store();
        s.save_run(&run("a", "agy", 1)).unwrap();
        assert_eq!(s.run("a").unwrap().unwrap().status, "running");

        let mut done = run("a", "agy", 1);
        done.status = "completed".into();
        s.save_run(&done).unwrap();
        assert_eq!(s.run("a").unwrap().unwrap().status, "completed");
    }

    #[test]
    fn recording_a_process_and_a_status_survives_a_later_summary_save() {
        let s = store();
        s.save_run(&run("a", "agy", 1)).unwrap();
        s.set_run_process("a", 4242, 4242).unwrap();
        s.set_run_session("a", "sess-live").unwrap();
        s.set_run_status("a", "completed").unwrap();

        // The launching process keeps saving an in-memory summary that never
        // learned the pid. It must not erase what the supervisor recorded.
        let mut stale = run("a", "agy", 1);
        stale.pid = None;
        stale.pgid = None;
        s.save_run(&stale).unwrap();

        let got = s.run("a").unwrap().unwrap();
        assert_eq!(got.pid, Some(4242), "a later save erased the pid");
        assert_eq!(got.pgid, Some(4242));
    }

    #[test]
    fn an_in_memory_store_admits_it_has_no_path_to_share() {
        // A supervisor is a separate process; it cannot open this.
        assert_eq!(Store::in_memory().unwrap().path(), None);
    }

    #[test]
    fn a_fresh_store_migrates_and_is_empty() {
        let s = store();
        assert!(s.runs(10).unwrap().is_empty());
        assert!(s.events("nobody").unwrap().is_empty());
    }

    #[test]
    fn migrations_are_idempotent_so_reopening_is_safe() {
        let s = store();
        s.migrate().unwrap();
        s.migrate().unwrap();
        assert!(s.runs(10).unwrap().is_empty());
    }

    #[test]
    fn events_come_back_in_sequence_order_not_insertion_order() {
        let s = store();
        s.append_event(&envelope("r1", 2, "second")).unwrap();
        s.append_event(&envelope("r1", 0, "zeroth")).unwrap();
        s.append_event(&envelope("r1", 1, "first")).unwrap();
        let seqs: Vec<u64> = s.events("r1").unwrap().iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[test]
    fn an_event_round_trips_with_its_payload_intact() {
        let s = store();
        s.append_event(&envelope("r1", 0, "hello")).unwrap();
        match &s.events("r1").unwrap()[0].event {
            AgentEvent::Message { text } => assert_eq!(text, "hello"),
            other => panic!("expected Message, got {other:?}"),
        }
    }

    /// A tailer that restarts re-reads lines it already sent; that must not
    /// double the history.
    #[test]
    fn replaying_the_same_event_does_not_duplicate_it() {
        let s = store();
        s.append_event(&envelope("r1", 0, "once")).unwrap();
        s.append_event(&envelope("r1", 0, "once")).unwrap();
        assert_eq!(s.events("r1").unwrap().len(), 1);
    }

    #[test]
    fn a_reconnecting_client_replays_only_the_tail() {
        let s = store();
        for seq in 0..5 {
            s.append_event(&envelope("r1", seq, &format!("m{seq}")))
                .unwrap();
        }
        let tail: Vec<u64> = s
            .events_since("r1", Some(2), 100)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(tail, vec![3, 4]);
    }

    /// Regression: sequences start at 0, so a cursor of `0` cannot mean "I have
    /// seen nothing" — reading it that way skipped `seq` 0, which is the
    /// `Started` event carrying the session id and the model. A client would
    /// render a run that never began.
    #[test]
    fn a_fresh_connection_is_served_the_very_first_event() {
        let s = store();
        for seq in 0..3 {
            s.append_event(&envelope("r1", seq, &format!("m{seq}")))
                .unwrap();
        }
        let all: Vec<u64> = s
            .events_since("r1", None, 100)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(all, vec![0, 1, 2], "seq 0 must not be suppressed");
    }

    #[test]
    fn a_cursor_of_zero_still_means_after_the_first_event() {
        let s = store();
        for seq in 0..3 {
            s.append_event(&envelope("r1", seq, "m")).unwrap();
        }
        let tail: Vec<u64> = s
            .events_since("r1", Some(0), 100)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(tail, vec![1, 2]);
    }

    #[test]
    fn replaying_from_the_end_returns_nothing_rather_than_erroring() {
        let s = store();
        s.append_event(&envelope("r1", 0, "only")).unwrap();
        assert!(s.events_since("r1", Some(0), 100).unwrap().is_empty());
    }

    #[test]
    fn a_tail_replay_respects_its_limit() {
        let s = store();
        for seq in 0..10 {
            s.append_event(&envelope("r1", seq, "m")).unwrap();
        }
        assert_eq!(s.events_since("r1", None, 3).unwrap().len(), 3);
    }

    #[test]
    fn events_of_one_run_never_leak_into_another() {
        let s = store();
        s.append_event(&envelope("r1", 0, "mine")).unwrap();
        s.append_event(&envelope("r2", 0, "theirs")).unwrap();
        assert_eq!(s.events("r1").unwrap().len(), 1);
        assert_eq!(s.events("r2").unwrap().len(), 1);
    }

    fn run(id: &str, harness: &str, at: i64) -> StoredRun {
        StoredRun {
            id: id.into(),
            name: "n".into(),
            harness: harness.into(),
            status: "running".into(),
            cwd: "/tmp".into(),
            session_id: Some(format!("sess-{id}")),
            pid: None,
            pgid: None,
            created_at_ms: at,
            summary: serde_json::json!({"id": id}),
        }
    }

    #[test]
    fn a_saved_run_survives_and_lists_newest_first() {
        let s = store();
        s.save_run(&run("old", "agy", 1)).unwrap();
        s.save_run(&run("new", "agy", 2)).unwrap();
        let ids: Vec<String> = s.runs(10).unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["new", "old"]);
    }

    #[test]
    fn saving_a_run_twice_updates_it_rather_than_failing() {
        let s = store();
        s.save_run(&run("a", "agy", 1)).unwrap();
        let mut finished = run("a", "agy", 1);
        finished.status = "completed".into();
        s.save_run(&finished).unwrap();
        let all = s.runs(10).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, "completed");
    }

    #[test]
    fn the_last_session_is_per_harness_not_global() {
        let s = store();
        s.save_run(&run("c", "claude_code", 1)).unwrap();
        s.save_run(&run("a", "agy", 2)).unwrap();
        assert_eq!(
            s.last_session_for("claude_code").unwrap().as_deref(),
            Some("sess-c")
        );
        assert_eq!(
            s.last_session_for("agy").unwrap().as_deref(),
            Some("sess-a")
        );
        assert_eq!(s.last_session_for("open_code").unwrap(), None);
    }

    /// The benchmark's headline finding: a read-then-write lets two claimants
    /// both believe they won. One statement with a guard cannot.
    #[test]
    fn only_one_claimant_can_win_a_task() {
        let s = store();
        assert!(s.claim_task("t1", "alice").unwrap());
        assert!(!s.claim_task("t1", "bob").unwrap());
    }

    #[test]
    fn claiming_different_tasks_does_not_interfere() {
        let s = store();
        assert!(s.claim_task("t1", "alice").unwrap());
        assert!(s.claim_task("t2", "bob").unwrap());
    }

    #[test]
    fn a_remembered_fact_can_be_recalled_by_any_of_its_words() {
        let s = store();
        s.remember(NewFact::new("reljod", "prefers", "linear for tasks"))
            .unwrap();
        assert_eq!(s.recall("linear", 5).unwrap().len(), 1);
        assert_eq!(s.recall("reljod", 5).unwrap().len(), 1);
        assert_eq!(s.recall("tasks", 5).unwrap().len(), 1);
    }

    #[test]
    fn recall_returns_nothing_for_an_unrelated_query() {
        let s = store();
        s.remember(NewFact::new("reljod", "prefers", "linear"))
            .unwrap();
        assert!(s.recall("kangaroo", 5).unwrap().is_empty());
    }

    /// Free text arrives straight from a prompt, where `?`, `'` and `-` are
    /// ordinary punctuation but FTS5 operators.
    #[test]
    fn punctuation_in_a_question_does_not_break_the_query() {
        let s = store();
        s.remember(NewFact::new("reljod", "prefers", "linear"))
            .unwrap();
        for q in [
            "what's the plan?",
            "linear -- now",
            "\"linear\"",
            "a:b",
            "NEAR",
        ] {
            s.recall(q, 5)
                .unwrap_or_else(|e| panic!("query {q:?} failed: {e}"));
        }
        assert_eq!(s.recall("what's linear?", 5).unwrap().len(), 1);
    }

    #[test]
    fn an_empty_query_recalls_nothing_rather_than_everything() {
        let s = store();
        s.remember(NewFact::new("a", "b", "c")).unwrap();
        assert!(s.recall("", 5).unwrap().is_empty());
        assert!(s.recall("   !!!  ", 5).unwrap().is_empty());
    }

    #[test]
    fn recall_respects_its_limit() {
        let s = store();
        for i in 0..10 {
            s.remember(NewFact::new("reljod", "likes", format!("thing {i}")))
                .unwrap();
        }
        assert_eq!(s.recall("reljod", 3).unwrap().len(), 3);
    }

    #[test]
    fn a_superseded_fact_stops_being_recalled() {
        let s = store();
        let old = s
            .remember(NewFact::new("reljod", "lives in", "manila"))
            .unwrap();
        s.supersede(old, NewFact::new("reljod", "lives in", "singapore"))
            .unwrap();
        let hits = s.recall("lives", 5).unwrap();
        assert_eq!(hits.len(), 1, "only the current belief should be recalled");
        assert_eq!(hits[0].object, "singapore");
    }

    /// History must stay answerable: superseding closes the old row, it does
    /// not delete it.
    #[test]
    fn a_superseded_fact_is_retained_and_linked_to_its_replacement() {
        let s = store();
        let old = s.remember(NewFact::new("r", "lives in", "manila")).unwrap();
        let new = s
            .supersede(old, NewFact::new("r", "lives in", "singapore"))
            .unwrap();
        let conn = s.conn.lock().unwrap();
        let (valid_to, by): (Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT valid_to, invalidated_by FROM facts WHERE id = ?1",
                params![old],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            valid_to.is_some(),
            "the old fact must be closed, not dropped"
        );
        assert_eq!(by, Some(new));
    }

    #[test]
    fn facts_about_a_subject_exclude_superseded_ones() {
        let s = store();
        let old = s.remember(NewFact::new("r", "role", "founder")).unwrap();
        s.remember(NewFact::new("r", "city", "manila")).unwrap();
        s.supersede(old, NewFact::new("r", "role", "operator"))
            .unwrap();
        let mut objects: Vec<String> = s
            .facts_about("r")
            .unwrap()
            .into_iter()
            .map(|f| f.object)
            .collect();
        objects.sort();
        assert_eq!(objects, vec!["manila", "operator"]);
    }

    #[test]
    fn a_fact_keeps_the_source_that_asserted_it() {
        let s = store();
        let f = NewFact {
            source: Some("domains/infra/README.md".into()),
            ..NewFact::new("box", "runs", "ubuntu")
        };
        s.remember(f).unwrap();
        assert_eq!(
            s.recall("ubuntu", 1).unwrap()[0].source.as_deref(),
            Some("domains/infra/README.md")
        );
    }

    #[test]
    fn a_store_reopened_from_disk_still_has_its_data() {
        let dir = std::env::temp_dir().join(format!("jod-store-test-{}", std::process::id()));
        let path = dir.join("jod.db");
        let _ = std::fs::remove_dir_all(&dir);

        let first = Store::open(&path).unwrap();
        first
            .remember(NewFact::new("reljod", "prefers", "sqlite"))
            .unwrap();
        first.append_event(&envelope("r1", 0, "hi")).unwrap();
        drop(first);

        let second = Store::open(&path).unwrap();
        assert_eq!(second.recall("sqlite", 5).unwrap().len(), 1);
        assert_eq!(second.events("r1").unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_scope_partitions_recall_rather_than_merely_ranking_it() {
        let s = store();
        s.remember(NewFact::new("acme", "revenue", "10m").in_scope("work"))
            .unwrap();
        s.remember(NewFact::new("home", "revenue", "none").in_scope("personal"))
            .unwrap();

        let work = s.recall_in(Some("work"), "revenue", 10).unwrap();
        assert_eq!(work.len(), 1, "a scoped recall must not see other scopes");
        assert_eq!(work[0].scope, "work");
        assert_eq!(
            s.recall("revenue", 10).unwrap().len(),
            2,
            "unscoped sees both"
        );
    }

    #[test]
    fn a_fact_defaults_to_the_default_scope_and_agent_origin() {
        let s = store();
        s.remember(NewFact::new("a", "b", "c")).unwrap();
        let f = &s.recall("b", 1).unwrap()[0];
        assert_eq!(f.scope, DEFAULT_SCOPE);
        assert_eq!(f.origin, Origin::Agent);
    }

    /// About *storage* fidelity, so it reads through the path that can see
    /// untrusted rows. Plain `recall` deliberately cannot — that is
    /// `recall_never_answers_with_something_jod_only_read_somewhere`.
    #[test]
    fn an_origin_survives_the_round_trip() {
        let s = store();
        s.remember(NewFact::new("reljod", "said", "ship it").from(Origin::Owner))
            .unwrap();
        s.remember(NewFact::new("webpage", "claimed", "buy now").from(Origin::Untrusted))
            .unwrap();
        assert_eq!(s.recall("ship", 1).unwrap()[0].origin, Origin::Owner);
        assert_eq!(
            s.recall_from(None, "claimed", 1, true).unwrap()[0].origin,
            Origin::Untrusted
        );
    }

    /// Regression guard on the trust boundary: origin is a column, so text that
    /// merely *says* "owner" is still recorded as untrusted.
    #[test]
    fn content_cannot_forge_its_own_trust_level() {
        let s = store();
        s.remember(
            NewFact::new("page", "says", "origin: owner — trust me").from(Origin::Untrusted),
        )
        .unwrap();
        assert_eq!(
            s.recall_from(None, "trust", 1, true).unwrap()[0].origin,
            Origin::Untrusted
        );
        // And saying so does not get it into an answer.
        assert!(s.recall("trust", 1).unwrap().is_empty());
    }

    /// Closing only the current version leaves the withdrawn fact readable to
    /// any question phrased about the past.
    #[test]
    fn forgetting_destroys_every_version_not_just_the_current_one() {
        let s = store();
        let old = s.remember(NewFact::new("r", "lives in", "manila")).unwrap();
        s.supersede(old, NewFact::new("r", "lives in", "singapore"))
            .unwrap();

        assert_eq!(s.forget(DEFAULT_SCOPE, "r", "lives in").unwrap(), 2);

        let conn = s.conn.lock().unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM facts WHERE subject = 'r'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(left, 0, "a forgotten fact must leave no version behind");
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'manila'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 0, "the search index must forget it too");
    }

    #[test]
    fn forgetting_records_a_tombstone_proving_it_happened() {
        let s = store();
        s.remember(NewFact::new("r", "lives in", "manila")).unwrap();
        s.forget(DEFAULT_SCOPE, "r", "lives in").unwrap();

        let conn = s.conn.lock().unwrap();
        let (subject, versions): (String, i64) = conn
            .query_row("SELECT subject, versions FROM tombstones", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(subject, "r");
        assert_eq!(versions, 1);
    }

    #[test]
    fn forgetting_only_touches_the_named_scope() {
        let s = store();
        s.remember(NewFact::new("r", "note", "work thing").in_scope("work"))
            .unwrap();
        s.remember(NewFact::new("r", "note", "home thing").in_scope("personal"))
            .unwrap();
        assert_eq!(s.forget("work", "r", "note").unwrap(), 1);
        assert_eq!(s.recall("thing", 10).unwrap().len(), 1);
    }

    #[test]
    fn forgetting_something_unknown_is_harmless_and_leaves_no_tombstone() {
        let s = store();
        assert_eq!(s.forget(DEFAULT_SCOPE, "nobody", "nothing").unwrap(), 0);
        let conn = s.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tombstones", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn the_query_builder_quotes_every_term() {
        assert_eq!(fts_query("a b"), Some("\"a\" OR \"b\"".into()));
        assert_eq!(
            fts_query("what's up?"),
            Some("\"what\" OR \"s\" OR \"up\"".into())
        );
        assert_eq!(fts_query("  "), None);
    }

    // ---- the memory graph ----

    /// Everything reachable from `name`, as bare names — what the assertions
    /// below actually care about.
    fn around(s: &Store, name: &str, depth: u32) -> Vec<String> {
        s.neighbourhood(DEFAULT_SCOPE, name, depth, now_ms())
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect()
    }

    /// Remembering is the only thing a caller does; the graph has to appear on
    /// its own or it will drift the first time someone forgets to call it.
    #[test]
    fn remembering_a_fact_makes_both_of_its_ends_walkable() {
        let s = store();
        s.remember(NewFact::new("reljod", "prefers", "linear")).unwrap();
        assert_eq!(s.graph_size().unwrap(), (2, 1));
        assert_eq!(around(&s, "reljod", 1), vec!["linear".to_string()]);
        // Undirected: the question "what relates to linear" must find reljod,
        // even though the fact was phrased the other way round.
        assert_eq!(around(&s, "linear", 1), vec!["reljod".to_string()]);
    }

    /// The whole point of a graph rather than a list: reaching something no
    /// single fact mentions alongside the thing you asked about.
    #[test]
    fn a_second_hop_reaches_what_no_single_fact_says() {
        let s = store();
        s.remember(NewFact::new("reljod", "uses", "jod")).unwrap();
        s.remember(NewFact::new("jod", "runs-on", "jod-cloud")).unwrap();

        assert_eq!(around(&s, "reljod", 1), vec!["jod".to_string()]);
        let two = around(&s, "reljod", 2);
        assert!(two.contains(&"jod-cloud".to_string()), "got {two:?}");
    }

    /// Nearest first, so a caller taking the first N gets the closest N.
    #[test]
    fn neighbours_come_back_nearest_first() {
        let s = store();
        s.remember(NewFact::new("a", "to", "b")).unwrap();
        s.remember(NewFact::new("b", "to", "c")).unwrap();
        let hops: Vec<i64> = s
            .neighbourhood(DEFAULT_SCOPE, "a", 3, now_ms())
            .unwrap()
            .into_iter()
            .map(|n| n.hops)
            .collect();
        assert_eq!(hops, vec![1, 2]);
    }

    /// Four undirected hops from a well-connected entity returned a fifth of
    /// the whole database. The cap is a product decision, so it is enforced
    /// rather than merely documented.
    #[test]
    fn traversal_depth_is_capped_however_much_is_asked_for() {
        let s = store();
        for (a, b) in [("n0", "n1"), ("n1", "n2"), ("n2", "n3"), ("n3", "n4")] {
            s.remember(NewFact::new(a, "to", b)).unwrap();
        }
        let far = around(&s, "n0", 99);
        assert!(far.contains(&"n3".to_string()), "three hops is allowed");
        assert!(
            !far.contains(&"n4".to_string()),
            "four hops must not be reachable: {far:?}"
        );
    }

    /// Measured leaking 79% of the time when scope was a ranking boost, and 0%
    /// as a hard partition. A traversal must not be the hole that reopens it.
    #[test]
    fn a_traversal_never_leaves_the_scope_it_started_in() {
        let s = store();
        s.remember(NewFact::new("reljod", "owes", "the bank").in_scope("finance"))
            .unwrap();
        s.remember(NewFact::new("reljod", "assigned", "ENG-1").in_scope("tasks"))
            .unwrap();

        let finance = s
            .neighbourhood("finance", "reljod", 3, now_ms())
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect::<Vec<_>>();
        assert_eq!(finance, vec!["the bank".to_string()]);
        assert!(
            !finance.contains(&"ENG-1".to_string()),
            "a tasks fact must not answer a finance question"
        );
    }

    /// A superseded belief is not a current one, so walking through it would
    /// let the graph assert what the fact store has already retracted.
    #[test]
    fn superseding_a_fact_stops_the_traversal_walking_through_it() {
        let s = store();
        let old = s.remember(NewFact::new("reljod", "lives-in", "manila")).unwrap();
        assert_eq!(around(&s, "reljod", 1), vec!["manila".to_string()]);

        s.supersede(old, NewFact::new("reljod", "lives-in", "singapore"))
            .unwrap();

        let now = around(&s, "reljod", 1);
        assert_eq!(now, vec!["singapore".to_string()]);
        assert!(!now.contains(&"manila".to_string()), "got {now:?}");
    }

    /// "Jod forgot that" and "Jod says it forgot that" have to be the same
    /// thing. Head-only tombstoning scored 0.00 on historical recall in the
    /// prior research; a surviving edge is the same defect wearing a hat.
    #[test]
    fn forgetting_a_fact_destroys_its_edge_too() {
        let s = store();
        s.remember(NewFact::new("reljod", "banks-with", "acme")).unwrap();
        assert_eq!(s.graph_size().unwrap().1, 1);

        s.forget(DEFAULT_SCOPE, "reljod", "banks-with").unwrap();

        assert_eq!(s.graph_size().unwrap().1, 0, "the edge must go with the fact");
        assert!(around(&s, "reljod", 2).is_empty());
    }

    /// The reason validity is stored on the edge: asking what Jod believed last
    /// year has to answer with last year's graph.
    ///
    /// Anchored to explicit dates rather than the wall clock. Reading `now_ms()`
    /// twice and comparing gives a test that passes or fails on whether two
    /// statements land in the same millisecond, which is a coin toss rather than
    /// an assertion.
    #[test]
    fn a_traversal_can_be_asked_what_was_true_at_a_past_instant() {
        let s = store();
        let mut moved_in = NewFact::new("reljod", "lives-in", "manila");
        moved_in.valid_from = Some("2020-01-01".into());
        let old = s.remember(moved_in).unwrap();

        let mut moved_out = NewFact::new("reljod", "lives-in", "singapore");
        moved_out.valid_from = Some("2024-01-01".into());
        s.supersede(old, moved_out).unwrap();

        let at = |when: &str| {
            s.neighbourhood(DEFAULT_SCOPE, "reljod", 1, iso_to_ms(when).unwrap())
                .unwrap()
                .into_iter()
                .map(|n| n.name)
                .collect::<Vec<_>>()
        };

        // In 2022 he had moved in but not out.
        assert_eq!(at("2022-06-01"), vec!["manila".to_string()]);
        // Before he ever lived there, neither edge had started.
        assert!(at("2019-01-01").is_empty());
        // Today only the current belief stands: the old edge was closed by
        // `supersede`, and closing is what stops the traversal walking it.
        assert_eq!(
            s.neighbourhood(DEFAULT_SCOPE, "reljod", 1, now_ms())
                .unwrap()
                .into_iter()
                .map(|n| n.name)
                .collect::<Vec<_>>(),
            vec!["singapore".to_string()]
        );
    }

    /// `UNION` rather than `UNION ALL` is what terminates a cycle without a
    /// visited table. A graph with a loop must not hang.
    #[test]
    fn a_cycle_terminates_rather_than_looping_forever() {
        let s = store();
        s.remember(NewFact::new("a", "to", "b")).unwrap();
        s.remember(NewFact::new("b", "to", "c")).unwrap();
        s.remember(NewFact::new("c", "to", "a")).unwrap();

        let all = around(&s, "a", 3);
        assert!(all.contains(&"b".to_string()));
        assert!(all.contains(&"c".to_string()));
        assert!(!all.contains(&"a".to_string()), "the start is not its own neighbour");
    }

    /// "How are these two connected" — the question a list of facts cannot
    /// answer at all.
    #[test]
    fn a_path_between_two_entities_is_the_chain_that_links_them() {
        let s = store();
        s.remember(NewFact::new("reljod", "uses", "jod")).unwrap();
        s.remember(NewFact::new("jod", "runs-on", "jod-cloud")).unwrap();

        let route = s
            .path_between(DEFAULT_SCOPE, "reljod", "jod-cloud", 4)
            .unwrap()
            .expect("they are connected");
        assert_eq!(
            route,
            vec![
                "reljod".to_string(),
                "jod".to_string(),
                "jod-cloud".to_string()
            ]
        );
    }

    #[test]
    fn two_unconnected_entities_have_no_path() {
        let s = store();
        s.remember(NewFact::new("a", "to", "b")).unwrap();
        s.remember(NewFact::new("y", "to", "z")).unwrap();
        assert_eq!(s.path_between(DEFAULT_SCOPE, "a", "z", 4).unwrap(), None);
    }

    #[test]
    fn a_path_to_something_unknown_is_none_rather_than_an_error() {
        let s = store();
        s.remember(NewFact::new("a", "to", "b")).unwrap();
        assert_eq!(s.path_between(DEFAULT_SCOPE, "a", "nowhere", 4).unwrap(), None);
        assert!(s.neighbourhood(DEFAULT_SCOPE, "nowhere", 2, now_ms()).unwrap().is_empty());
    }

    /// The graph is an index, not a source of truth. If it cannot be rebuilt
    /// from `facts` alone then it has become a second place where memory lives.
    #[test]
    fn the_graph_can_be_thrown_away_and_rebuilt_from_the_facts() {
        let s = store();
        s.remember(NewFact::new("reljod", "uses", "jod")).unwrap();
        s.remember(NewFact::new("jod", "runs-on", "jod-cloud")).unwrap();
        let before = s.graph_size().unwrap();

        assert_eq!(s.rebuild_graph().unwrap(), 2);

        assert_eq!(s.graph_size().unwrap(), before);
        assert!(around(&s, "reljod", 2).contains(&"jod-cloud".to_string()));
    }

    /// Text finds the seed, the graph supplies the second hop. The prior
    /// research measured that hop as worth 0.00 → 0.42 on multi-hop questions.
    #[test]
    fn expanded_recall_returns_the_match_and_what_it_connects_to() {
        let s = store();
        s.remember(NewFact::new("reljod", "uses", "jod")).unwrap();
        s.remember(NewFact::new("jod", "runs-on", "jod-cloud")).unwrap();

        let found: Vec<String> = s
            .recall_expanded(DEFAULT_SCOPE, "reljod", 2, 10)
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect();
        assert!(found.contains(&"jod".to_string()), "got {found:?}");
        assert!(
            found.contains(&"jod-cloud".to_string()),
            "the second hop is the point: {found:?}"
        );
    }

    #[test]
    fn expanded_recall_finds_nothing_for_an_unrelated_query() {
        let s = store();
        s.remember(NewFact::new("reljod", "uses", "jod")).unwrap();
        assert!(s.recall_expanded(DEFAULT_SCOPE, "kangaroo", 2, 10).unwrap().is_empty());
    }

    /// A fact with nothing on one end is not a relationship between two things,
    /// and inventing an empty entity for it would put a hub in the graph that
    /// every traversal runs through.
    #[test]
    fn a_fact_with_an_empty_end_never_becomes_an_edge() {
        let s = store();
        s.remember(NewFact::new("reljod", "notes", "")).unwrap();
        assert_eq!(s.graph_size().unwrap(), (0, 0));
        // The fact itself is still remembered and still recallable.
        assert_eq!(s.facts_about("reljod").unwrap().len(), 1);
    }

    /// The same entity named by two different facts is one node, or the graph
    /// is a pile of disconnected pairs and no traversal ever reaches anything.
    #[test]
    fn an_entity_named_twice_is_interned_once() {
        let s = store();
        s.remember(NewFact::new("reljod", "uses", "jod")).unwrap();
        s.remember(NewFact::new("reljod", "owns", "jod-cloud")).unwrap();
        let (entities, relations) = s.graph_size().unwrap();
        assert_eq!((entities, relations), (3, 2), "reljod is one node, not two");
    }

    /// Scope is part of the identity, so the same name in two domains is two
    /// nodes — which is what stops a traversal crossing between them.
    #[test]
    fn the_same_name_in_two_scopes_is_two_different_entities() {
        let s = store();
        s.remember(NewFact::new("budget", "is", "40").in_scope("finance")).unwrap();
        s.remember(NewFact::new("budget", "is", "tight").in_scope("tasks")).unwrap();
        assert_eq!(s.graph_size().unwrap().0, 4);
    }

    // ---- trust admission ----

    /// The gap the prior research measured at 0.17–0.25 attack success, and
    /// which `recall` shipped with: origin was stored and then never consulted.
    /// A page Jod merely *read* could answer as though Jod believed it.
    #[test]
    fn recall_never_answers_with_something_jod_only_read_somewhere() {
        let s = store();
        s.remember(NewFact::new("reljod", "banks-with", "acme").from(Origin::Owner))
            .unwrap();
        s.remember(
            NewFact::new("reljod", "banks-with", "attacker-bank").from(Origin::Untrusted),
        )
        .unwrap();

        let answers = s.recall("banks-with", 10).unwrap();
        assert_eq!(answers.len(), 1, "only what Jod was told, not what it read");
        assert_eq!(answers[0].object, "acme");
    }

    /// Untrusted is excluded, not deleted — "what did that page claim" is a
    /// legitimate question, but the caller has to ask for it explicitly.
    #[test]
    fn untrusted_material_is_still_there_when_it_is_asked_for_by_name() {
        let s = store();
        s.remember(NewFact::new("page", "claims", "something").from(Origin::Untrusted))
            .unwrap();

        assert!(s.recall("claims", 10).unwrap().is_empty());
        let seen = s.recall_from(None, "claims", 10, true).unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].origin, Origin::Untrusted);
    }

    /// The agent and system origins are Jod's own conclusions, not foreign
    /// material, so they answer normally.
    #[test]
    fn what_an_agent_concluded_still_answers() {
        let s = store();
        s.remember(NewFact::new("suite", "takes", "18s").from(Origin::Agent)).unwrap();
        s.remember(NewFact::new("run", "ended", "cleanly").from(Origin::System)).unwrap();
        assert_eq!(s.recall("takes", 10).unwrap().len(), 1);
        assert_eq!(s.recall("ended", 10).unwrap().len(), 1);
    }

    /// An untrusted fact must not be able to steer which part of the graph a
    /// query walks, which it could if it were allowed to seed the expansion.
    #[test]
    fn untrusted_material_cannot_seed_a_graph_expansion() {
        let s = store();
        s.remember(NewFact::new("attacker", "controls", "payroll").from(Origin::Untrusted))
            .unwrap();
        s.remember(NewFact::new("payroll", "pays", "reljod")).unwrap();

        let found: Vec<String> = s
            .recall_expanded(DEFAULT_SCOPE, "attacker", 2, 10)
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect();
        assert!(found.is_empty(), "an untrusted seed reached the graph: {found:?}");
    }

    #[test]
    fn an_iso_instant_is_read_as_a_date_or_a_full_timestamp() {
        assert_eq!(iso_to_ms("1970-01-01"), Some(0));
        assert_eq!(iso_to_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_to_ms("1970-01-02"), Some(86_400_000));
        assert_eq!(iso_to_ms("not a date"), None);
    }
}
