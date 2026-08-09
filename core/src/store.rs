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

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::event::AgentEnvelope;

/// Applied in order; each is recorded so it runs exactly once.
const MIGRATIONS: &[(&str, &str)] = &[(
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
)];

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
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Store::from_connection(Connection::open(path)?)
    }

    /// An ephemeral database. Used by tests, and by any caller that wants the
    /// orchestrator without a file on disk.
    pub fn in_memory() -> Result<Store> {
        Store::from_connection(Connection::open_in_memory()?)
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

    /// Events after `after_seq`, oldest first.
    ///
    /// This is what lets a client that dropped its connection replay only the
    /// tail: it remembers the last `seq` it saw and asks for what followed,
    /// rather than re-fetching a transcript it already has.
    pub fn events_since(&self, run_id: &str, after_seq: u64, limit: usize) -> Result<Vec<AgentEnvelope>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT run_id, seq, at_ms, payload FROM events
             WHERE run_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![run_id, after_seq as i64, limit as i64], |r| {
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

    /// Insert or update the record of one delegation.
    pub fn save_run(&self, run: &StoredRun) -> Result<()> {
        let summary = serde_json::to_string(&run.summary)?;
        self.write(|tx| {
            tx.execute(
                "INSERT INTO runs
                   (id, name, harness, status, cwd, session_id, tmux_session,
                    created_at_ms, summary)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name,
                   status = excluded.status,
                   session_id = excluded.session_id,
                   summary = excluded.summary",
                params![
                    run.id,
                    run.name,
                    run.harness,
                    run.status,
                    run.cwd,
                    run.session_id,
                    run.tmux_session,
                    run.created_at_ms,
                    summary,
                ],
            )?;
            Ok(())
        })
    }

    /// Most recent runs first.
    pub fn runs(&self, limit: usize) -> Result<Vec<StoredRun>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, name, harness, status, cwd, session_id, tmux_session,
                    created_at_ms, summary
               FROM runs ORDER BY created_at_ms DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(StoredRun {
                id: r.get(0)?,
                name: r.get(1)?,
                harness: r.get(2)?,
                status: r.get(3)?,
                cwd: r.get(4)?,
                session_id: r.get(5)?,
                tmux_session: r.get(6)?,
                created_at_ms: r.get(7)?,
                summary: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
            })
        })?;
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

    // ---- memory ---------------------------------------------------------

    pub fn remember(&self, fact: NewFact) -> Result<i64> {
        self.write(|tx| insert_fact(tx, &fact))
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
            Ok(new_id)
        })
    }

    /// Full-text recall over currently-believed facts, best match first.
    ///
    /// `scope` filters *before* ranking rather than nudging the score: the
    /// research measured scope-as-a-boost leaking facts across domains 79% of
    /// the time, and scope-as-a-partition leaking none.
    pub fn recall_in(&self, scope: Option<&str>, query: &str, limit: usize) -> Result<Vec<Fact>> {
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
              ORDER BY bm25(facts_fts) LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![expr, scope, limit as i64], row_to_fact)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Recall across every scope.
    pub fn recall(&self, query: &str, limit: usize) -> Result<Vec<Fact>> {
        self.recall_in(None, query, limit)
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
    Ok(tx.last_insert_rowid())
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
    pub tmux_session: String,
    pub created_at_ms: i64,
    /// The full client-facing summary, kept verbatim so adding a field to it
    /// never needs a migration here.
    pub summary: serde_json::Value,
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
            s.append_event(&envelope("r1", seq, &format!("m{seq}"))).unwrap();
        }
        let tail: Vec<u64> = s
            .events_since("r1", 2, 100)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(tail, vec![3, 4]);
    }

    #[test]
    fn replaying_from_the_end_returns_nothing_rather_than_erroring() {
        let s = store();
        s.append_event(&envelope("r1", 0, "only")).unwrap();
        assert!(s.events_since("r1", 0, 100).unwrap().is_empty());
    }

    #[test]
    fn a_tail_replay_respects_its_limit() {
        let s = store();
        for seq in 0..10 {
            s.append_event(&envelope("r1", seq, "m")).unwrap();
        }
        assert_eq!(s.events_since("r1", 0, 3).unwrap().len(), 3);
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
            tmux_session: format!("jod-{id}"),
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
        assert_eq!(s.last_session_for("claude_code").unwrap().as_deref(), Some("sess-c"));
        assert_eq!(s.last_session_for("agy").unwrap().as_deref(), Some("sess-a"));
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
        s.remember(NewFact::new("reljod", "prefers", "linear")).unwrap();
        assert!(s.recall("kangaroo", 5).unwrap().is_empty());
    }

    /// Free text arrives straight from a prompt, where `?`, `'` and `-` are
    /// ordinary punctuation but FTS5 operators.
    #[test]
    fn punctuation_in_a_question_does_not_break_the_query() {
        let s = store();
        s.remember(NewFact::new("reljod", "prefers", "linear")).unwrap();
        for q in ["what's the plan?", "linear -- now", "\"linear\"", "a:b", "NEAR"] {
            s.recall(q, 5).unwrap_or_else(|e| panic!("query {q:?} failed: {e}"));
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
        assert!(valid_to.is_some(), "the old fact must be closed, not dropped");
        assert_eq!(by, Some(new));
    }

    #[test]
    fn facts_about_a_subject_exclude_superseded_ones() {
        let s = store();
        let old = s.remember(NewFact::new("r", "role", "founder")).unwrap();
        s.remember(NewFact::new("r", "city", "manila")).unwrap();
        s.supersede(old, NewFact::new("r", "role", "operator")).unwrap();
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
        first.remember(NewFact::new("reljod", "prefers", "sqlite")).unwrap();
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
        assert_eq!(s.recall("revenue", 10).unwrap().len(), 2, "unscoped sees both");
    }

    #[test]
    fn a_fact_defaults_to_the_default_scope_and_agent_origin() {
        let s = store();
        s.remember(NewFact::new("a", "b", "c")).unwrap();
        let f = &s.recall("b", 1).unwrap()[0];
        assert_eq!(f.scope, DEFAULT_SCOPE);
        assert_eq!(f.origin, Origin::Agent);
    }

    #[test]
    fn an_origin_survives_the_round_trip() {
        let s = store();
        s.remember(NewFact::new("reljod", "said", "ship it").from(Origin::Owner))
            .unwrap();
        s.remember(NewFact::new("webpage", "claimed", "buy now").from(Origin::Untrusted))
            .unwrap();
        assert_eq!(s.recall("ship", 1).unwrap()[0].origin, Origin::Owner);
        assert_eq!(s.recall("claimed", 1).unwrap()[0].origin, Origin::Untrusted);
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
        assert_eq!(s.recall("trust", 1).unwrap()[0].origin, Origin::Untrusted);
    }

    /// Closing only the current version leaves the withdrawn fact readable to
    /// any question phrased about the past.
    #[test]
    fn forgetting_destroys_every_version_not_just_the_current_one() {
        let s = store();
        let old = s.remember(NewFact::new("r", "lives in", "manila")).unwrap();
        s.supersede(old, NewFact::new("r", "lives in", "singapore")).unwrap();

        assert_eq!(s.forget(DEFAULT_SCOPE, "r", "lives in").unwrap(), 2);

        let conn = s.conn.lock().unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM facts WHERE subject = 'r'", [], |r| r.get(0))
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
            .query_row(
                "SELECT subject, versions FROM tombstones",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(subject, "r");
        assert_eq!(versions, 1);
    }

    #[test]
    fn forgetting_only_touches_the_named_scope() {
        let s = store();
        s.remember(NewFact::new("r", "note", "work thing").in_scope("work")).unwrap();
        s.remember(NewFact::new("r", "note", "home thing").in_scope("personal")).unwrap();
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
        assert_eq!(fts_query("what's up?"), Some("\"what\" OR \"s\" OR \"up\"".into()));
        assert_eq!(fts_query("  "), None);
    }
}
