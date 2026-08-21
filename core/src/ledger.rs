//! The delivery ledger: proof that a message Jod owed somebody was sent.
//!
//! Schema is migration `0008_monitors_and_ledger` in [`crate::store`].
//!
//! Jod's stated rule is that a failed run must never look like a successful
//! one. A notification that was never delivered is exactly that failure wearing
//! the wrong face: the run finished, the store says `done`, and the person it
//! was for heard nothing. Today nothing anywhere records the difference, and
//! the whole of this module is the answer — a durable row per outbound message,
//! written *before* the send and closed only when the send is known to have
//! landed. Modelled on Hermes' `gateway/delivery_ledger.py`
//! ([`research/hermes-parity-2026/REPORT.md`] §4.6).
//!
//! ## Three checkpoints, because two would not be enough
//!
//! [`Store::record_obligation`] → `pending`, [`Store::mark_attempting`] →
//! `attempting`, then [`Store::mark_delivered`] or [`Store::mark_failed`]. The
//! middle one is the only reason the ledger is worth having. A crash is not one
//! situation but two, and they demand opposite behaviour:
//!
//! - A **`pending`** row never reached the transport. Nothing was sent, so it
//!   is redelivered plainly — a duplicate is impossible.
//! - An **`attempting`** row was in flight. It may have arrived and it may not,
//!   and nothing Jod can inspect afterwards will say which. It is redelivered
//!   **with [`RECOVERED_MARKER`] in front of it**.
//!
//! That marker is the entire ethic of the module: delivery is honestly
//! at-least-once, and **ambiguity is labelled rather than silently resent**.
//! Dropping the message would be a lie of omission; resending it unmarked would
//! be a lie of commission; saying "this may be a duplicate" is neither.
//!
//! The same honesty has to outlive the send, which is what
//! [`Obligation::recovered_at_ms`] is for. A recovered message ends `delivered`
//! like any other, so for a while the ledger could label a duplicate on its way
//! out and had no answer at all for the person who asked afterwards — and
//! afterwards is when people ask, because holding two copies is how they find
//! out. The row now remembers that it was resent, in every state it can reach.
//!
//! ## Which process may recover a row
//!
//! [`Store::sweep_recoverable`] runs at startup and claims only rows whose
//! owning process is gone. Ownership is a machine *and* a pid, because a pid
//! means nothing without the machine that issued it, and a Jod on one box has
//! no way to tell whether a pid on another is still running. Rows belonging to
//! a live process — or to any process on a machine that is not this one — are
//! left exactly where they are.
//!
//! ## Who may invoke the sweep
//!
//! **Only a process that can actually send.** A rule about the caller rather
//! than the ledger, and worth stating because the obvious startup hook is the
//! daemon, which holds no transport.
//!
//! Claiming is a write: `sweep_recoverable` rewrites the owner in the same
//! transaction that selects, so two Jods starting together cannot both
//! redeliver. A caller that claims a row it cannot send therefore *owns* it
//! while alive, and every later sweep correctly skips it — turning a
//! recoverable message into an unrecoverable one.
//!
//! The same argument decides the `channel` parameter: rows for a channel this
//! caller cannot address are left orphaned, so the process that can address
//! them finds them. There is no handing one back — [`Store::mark_failed`] means
//! "this attempt failed, try again".

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::store::Store;

/// How many times one message may be attempted before it is called failed.
///
/// Hermes' constants, adopted with their values ([`REPORT.md`] §4.6), and
/// deliberately **not configurable** — in either system. Every one of them
/// trades the same pair against each other: how long a message may keep being
/// retried, against how far a duplicate may be from the thing it duplicates.
/// An operator who turns `MAX_ATTEMPTS` up is not buying reliability, they are
/// buying more copies of a message that is failing for a reason; an operator
/// who turns `STALE_AFTER_MS` up is asking to be told about something a day
/// after it stopped mattering. Neither is a preference worth a config key, and
/// a knob that exists gets turned.
pub const MAX_ATTEMPTS: i64 = 3;

/// Past this age an undelivered message is failed rather than sent.
///
/// A day-old "your build broke" is not a notification, it is archaeology, and
/// delivering it as though it were current is its own kind of lie.
pub const STALE_AFTER_MS: i64 = 24 * 60 * 60 * 1_000;

/// How long a settled row is kept, so "did that ever actually send" stays
/// answerable for a week after anybody thinks to ask.
pub const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// The most rows kept, whatever their age. The ledger is evidence, not a log:
/// it must not grow without bound on a box Jod shares with the work.
pub const MAX_ROWS: i64 = 500;

/// Prefixed to a message that was in flight when Jod died.
///
/// Visible, in the message itself, in the recipient's language rather than a
/// header they will never see — the person reading it is the only one who can
/// tell whether they have seen it before, and they can only do that if they
/// are told.
pub const RECOVERED_MARKER: &str = "♻️ Recovered reply — Jod restarted during \
delivery, so this may be a duplicate:\n\n";

/// Where an outbound message has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// Owed, and not yet handed to any transport.
    #[default]
    Pending,
    /// Handed to a transport, outcome unknown. The state that makes a crash
    /// answerable.
    Attempting,
    /// Known to have arrived. Terminal.
    Delivered,
    /// Given up on — too many attempts, or too old to be worth sending.
    /// Terminal, and *not* a silent one: the row is the record that somebody
    /// was owed something they never got.
    Failed,
}

impl DeliveryState {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryState::Pending => "pending",
            DeliveryState::Attempting => "attempting",
            DeliveryState::Delivered => "delivered",
            DeliveryState::Failed => "failed",
        }
    }

    /// Unknown text reads as `failed`, matching [`crate::webhook::DeliveryStatus`]:
    /// a row written by a newer Jod must not make an older one unable to *read*
    /// its ledger, and of the four, failed is the reading that cannot cause a
    /// message to be sent.
    pub fn parse(s: &str) -> DeliveryState {
        match s {
            "pending" => DeliveryState::Pending,
            "attempting" => DeliveryState::Attempting,
            "delivered" => DeliveryState::Delivered,
            _ => DeliveryState::Failed,
        }
    }

    pub fn is_settled(&self) -> bool {
        matches!(self, DeliveryState::Delivered | DeliveryState::Failed)
    }
}

/// The process answerable for a row.
///
/// A pid alone is not an identity. Two Jods on two boxes issue pids from
/// separate spaces, so "is 4821 still running" is only a question the machine
/// that issued it can answer — see [`Processes`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub machine: String,
    pub pid: u32,
}

impl Owner {
    pub fn new(machine: impl Into<String>, pid: u32) -> Owner {
        Owner {
            machine: machine.into(),
            pid,
        }
    }

    /// This process, on this machine.
    pub fn here(machine: impl Into<String>) -> Owner {
        Owner::new(machine, std::process::id())
    }
}

/// A message Jod owes somebody, on its way in.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMessage {
    /// The caller's idempotency key — usually the id of the thing being
    /// reported. Recording the same key twice queues one message, not two.
    pub message_key: String,
    /// `telegram`, `cli`, … Which transport is answerable for it.
    pub channel: String,
    /// Whatever that transport addresses with: a chat id, a thread.
    pub target: String,
    pub body: String,
    /// The run this is reporting, when it is reporting one.
    pub run_id: Option<String>,
}

impl NewMessage {
    pub fn new(
        message_key: impl Into<String>,
        channel: impl Into<String>,
        target: impl Into<String>,
        body: impl Into<String>,
    ) -> NewMessage {
        NewMessage {
            message_key: message_key.into(),
            channel: channel.into(),
            target: target.into(),
            body: body.into(),
            run_id: None,
        }
    }

    pub fn about_run(mut self, run_id: impl Into<String>) -> NewMessage {
        self.run_id = Some(run_id.into());
        self
    }
}

/// One row of the ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Obligation {
    pub id: i64,
    pub message_key: String,
    pub channel: String,
    pub target: String,
    pub body: String,
    pub state: DeliveryState,
    pub attempts: i64,
    pub owner: Owner,
    pub run_id: Option<String>,
    pub detail: Option<String>,
    /// When this row was last resent after a crash, if it ever was.
    ///
    /// The row's *history*, not its state, which is the whole reason it needs a
    /// column of its own: a recovered message ends `delivered` like any other
    /// and `mark_delivered` clears `detail` on the way past, so before
    /// `0012_recovered_deliveries` the one fact somebody needs was erased at the
    /// moment it became useful. "Why did I get this twice" is asked after
    /// delivery, always.
    pub recovered_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl Obligation {
    /// What to actually send, now, given how this row was interrupted.
    ///
    /// The one place the crash semantics are expressed, and pure so that they
    /// can be argued with in a test rather than in production.
    pub fn redelivery_body(&self) -> String {
        redelivery_body(self.may_be_a_duplicate(), &self.body)
    }

    /// Whether this message may reach its recipient twice.
    ///
    /// Two ways, and the second is why [`Obligation::recovered_at_ms`] exists.
    /// A row that is **`attempting`** was in flight and may have landed. A row
    /// that has **been recovered before** may have landed on that earlier pass,
    /// and stays a possible duplicate for the rest of its life however it is
    /// sent afterwards — a later known failure proves the *last* attempt sent
    /// nothing, and says nothing about the one that was interrupted.
    ///
    /// So this reads true after delivery too, which is the point: it is what a
    /// reader shows somebody holding two copies and asking why.
    pub fn may_be_a_duplicate(&self) -> bool {
        self.state == DeliveryState::Attempting || self.recovered_at_ms.is_some()
    }
}

/// A message that may already have arrived is marked; one that certainly has
/// not goes plainly. Nothing else is ever redelivered.
///
/// Takes the answer rather than the state because there are now two ways to
/// reach it — see [`Obligation::may_be_a_duplicate`] — and a second copy of
/// that rule living here is a second copy to get wrong.
pub fn redelivery_body(may_be_a_duplicate: bool, body: &str) -> String {
    if may_be_a_duplicate {
        format!("{RECOVERED_MARKER}{body}")
    } else {
        body.to_string()
    }
}

/// Whether a row is past saving: too many attempts, or too old to send as news.
pub fn is_beyond_recovery(o: &Obligation, at_ms: i64) -> bool {
    o.attempts >= MAX_ATTEMPTS || at_ms - o.created_at_ms >= STALE_AFTER_MS
}

/// Whether the process that owned a row is still running.
///
/// Behind a trait for the same reason [`crate::monitor::Probes`] is: the sweep's
/// judgement — which rows are orphaned — is the interesting part, and it should
/// be testable without killing a real process to find out.
pub trait Processes {
    fn is_alive(&self, owner: &Owner) -> bool;
}

/// This machine's name, as it is written into `owner_machine`.
///
/// One function so that the name a row is *written* with and the name the sweep
/// *judges* it against cannot drift. They have to be the same string or
/// [`LocalProcesses::is_alive`] silently reports every local row as belonging to
/// another machine, and the sweep then recovers nothing at all — a failure that
/// looks exactly like having no orphaned rows.
///
/// Deliberately not shared with `ticker`'s own hostname helper even though the
/// two agree today. That one builds a schedule lease owner, `pid@host`, which is
/// compared only against other lease owners; this one is half of an identity the
/// sweep makes life-and-death decisions with. Merging them would make a change
/// to either format a change to both.
pub fn machine() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into())
}

/// Liveness as this machine can actually observe it.
pub struct LocalProcesses {
    /// This machine's name, as it is written into `owner_machine`. Use
    /// [`machine`], and the same value the rows were written with.
    pub machine: String,
}

impl Default for LocalProcesses {
    fn default() -> LocalProcesses {
        LocalProcesses { machine: machine() }
    }
}

impl Processes for LocalProcesses {
    /// A process on another machine is reported **alive**, which is to say
    /// "not mine to recover".
    ///
    /// That is the conservative direction and the only defensible one: this
    /// process cannot see that machine's process table, so declaring its rows
    /// dead would mean redelivering messages another live Jod is at that moment
    /// sending. Better a message stranded until its own box restarts than one
    /// delivered twice by a Jod that had no way of knowing.
    ///
    /// Pids are recycled, so a dead owner whose number was reissued reads as
    /// alive and its row waits for the next sweep instead. That is the same
    /// trade [`crate::proc::group_alive`] already documents, and it errs the
    /// same way — towards not sending twice.
    fn is_alive(&self, owner: &Owner) -> bool {
        owner.machine != self.machine || crate::proc::group_alive(owner.pid)
    }
}

/// A row the sweep took responsibility for, and what to send for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Recovered {
    pub obligation: Obligation,
    /// [`Obligation::redelivery_body`], already prefixed when it needed to be.
    pub body: String,
    pub may_be_a_duplicate: bool,
}

// ---- storage ---------------------------------------------------------------

const LEDGER_COLUMNS: &str = "SELECT id, message_key, channel, target, body, state, attempts,
                                     owner_machine, owner_pid, run_id, detail,
                                     recovered_at_ms, created_at_ms, updated_at_ms
                                FROM delivery_ledger";

impl Store {
    /// Write down that a message is owed, before anything tries to send it.
    ///
    /// Before, not after, and that ordering is the whole guarantee: a row
    /// written after a successful send records only the sends that succeeded,
    /// which is precisely the set nobody needs a record of.
    ///
    /// Returns the row id. Recording a key that is already in the ledger
    /// returns the existing row untouched — a caller that retries must not
    /// queue a second copy, and must not have its in-flight row reset to
    /// `pending` underneath the process sending it.
    pub fn record_obligation(&self, m: &NewMessage, owner: &Owner, at_ms: i64) -> Result<i64> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO delivery_ledger
                   (message_key, channel, target, body, state, attempts,
                    owner_machine, owner_pid, run_id, created_at_ms, updated_at_ms)
                 VALUES (?1,?2,?3,?4,'pending',0,?5,?6,?7,?8,?8)
                 ON CONFLICT(message_key) DO NOTHING",
                params![
                    m.message_key,
                    m.channel,
                    m.target,
                    m.body,
                    owner.machine,
                    owner.pid,
                    m.run_id,
                    at_ms
                ],
            )?;
            Ok(tx.query_row(
                "SELECT id FROM delivery_ledger WHERE message_key = ?1",
                params![m.message_key],
                |r| r.get(0),
            )?)
        })
    }

    /// Take a message to the transport: `pending` → `attempting`, one attempt
    /// spent, this process answerable for it.
    ///
    /// The state change and the attempt count are one guarded statement, so two
    /// Jods racing over the same row produce one sender and one loser rather
    /// than two sends. Returns whether this caller is the one that took it.
    ///
    /// Refuses a row that is already `attempting`: whoever holds it may be
    /// mid-send, and the honest way to get it back is to wait for the sweep to
    /// establish that they are gone.
    pub fn mark_attempting(&self, id: i64, owner: &Owner, at_ms: i64) -> Result<bool> {
        self.write(|tx| {
            let took = tx.execute(
                "UPDATE delivery_ledger
                    SET state = 'attempting', attempts = attempts + 1,
                        owner_machine = ?2, owner_pid = ?3, updated_at_ms = ?4
                  WHERE id = ?1 AND state = 'pending'",
                params![id, owner.machine, owner.pid, at_ms],
            )?;
            Ok(took == 1)
        })
    }

    /// It arrived. Terminal, and never revisited by a sweep.
    pub fn mark_delivered(&self, id: i64, at_ms: i64) -> Result<bool> {
        self.write(|tx| {
            let changed = tx.execute(
                "UPDATE delivery_ledger
                    SET state = 'delivered', detail = NULL, updated_at_ms = ?2
                  WHERE id = ?1 AND state <> 'delivered'",
                params![id, at_ms],
            )?;
            Ok(changed == 1)
        })
    }

    /// The attempt failed, and the transport knows it did.
    ///
    /// A *known* failure is the good case: nothing arrived, so the message
    /// returns to `pending` and the next attempt goes out plainly, with no
    /// marker, because there is nothing ambiguous to warn anybody about. Only
    /// once it is [`is_beyond_recovery`] — out of attempts, or too old to send
    /// as news — does it become terminally `failed`, with the reason kept.
    ///
    /// Returns whether the row is now settled, so the caller knows whether to
    /// stop trying.
    pub fn mark_failed(&self, id: i64, detail: &str, at_ms: i64) -> Result<bool> {
        self.write(|tx| {
            let found: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT attempts, created_at_ms FROM delivery_ledger
                      WHERE id = ?1 AND state NOT IN ('delivered', 'failed')",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((attempts, created_at_ms)) = found else {
                return Ok(false);
            };
            let done = attempts >= MAX_ATTEMPTS || at_ms - created_at_ms >= STALE_AFTER_MS;
            tx.execute(
                "UPDATE delivery_ledger SET state = ?2, detail = ?3, updated_at_ms = ?4
                  WHERE id = ?1",
                params![id, if done { "failed" } else { "pending" }, detail, at_ms],
            )?;
            Ok(done)
        })
    }

    /// Every unsettled row whose owning process is gone, claimed for this one.
    ///
    /// Run at startup. Claimed as it is read — owner rewritten in the same
    /// transaction that selected it — so two Jods starting together cannot both
    /// decide to redeliver the same message.
    ///
    /// Rows past saving are settled as `failed` here rather than returned:
    /// after a crash the *reason* a message is a day old is usually that Jod
    /// was down, and sending yesterday's alert to prove the ledger works helps
    /// nobody.
    ///
    /// Each returned row carries the body already prepared — plain for
    /// `pending`, marked for `attempting`. The caller sends `body` and never
    /// `obligation.body`; the distinction between the two is the reason this
    /// module exists.
    ///
    /// `channel` is a safety filter, not a convenience one — the sweep is
    /// unsound without it, for the reason the module header gives.
    ///
    /// **"It settles eventually anyway" is the objection to expect from whoever
    /// next tries to remove it, and it is the reason to keep it.** Measured: a
    /// `cli` row in front of a telegram bridge is **`failed` after three
    /// restarts** ([`MAX_ATTEMPTS`]), because the redelivery path spends an
    /// attempt on every sweep before discovering it cannot address the row. No
    /// elapsed time is involved.
    ///
    /// So the row is not stranded, it is **destroyed** — the record that proved
    /// somebody was owed something becomes the record that they were written
    /// off, by a process that never could have delivered. Harder to spot than a
    /// row cycling for ever, which would at least look wrong.
    pub fn sweep_recoverable(
        &self,
        me: &Owner,
        processes: &dyn Processes,
        channel: &str,
        at_ms: i64,
    ) -> Result<Vec<Recovered>> {
        let open: Vec<Obligation> = {
            let conn = self.conn.lock().expect("store lock poisoned");
            let mut stmt = conn.prepare(&format!(
                "{LEDGER_COLUMNS} WHERE state IN ('pending', 'attempting')
                   AND channel = ?1
                 ORDER BY created_at_ms, id"
            ))?;
            let rows = stmt.query_map(params![channel], row_to_obligation)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        let orphaned: Vec<Obligation> = open
            .into_iter()
            // A row this very process owns is not orphaned even if it looks
            // idle: the sweep runs at startup, and the only rows it may touch
            // are the ones nobody is answerable for any more.
            .filter(|o| &o.owner != me && !processes.is_alive(&o.owner))
            .collect();

        self.write(|tx| {
            let mut taken = Vec::new();
            for o in orphaned {
                if is_beyond_recovery(&o, at_ms) {
                    tx.execute(
                        "UPDATE delivery_ledger
                            SET state = 'failed', detail = ?2, updated_at_ms = ?3
                          WHERE id = ?1",
                        params![
                            o.id,
                            format!(
                                "{} died holding this after {} attempt(s), and it is past saving",
                                o.owner.machine, o.attempts
                            ),
                            at_ms
                        ],
                    )?;
                    continue;
                }
                // Recorded here rather than by the transport, in the same
                // statement that claims the row, because this is the only place
                // that knows it. By the time the caller has sent the message the
                // row is `delivered` and the interruption is no longer visible
                // in any column — which is precisely how this fact used to get
                // lost.
                //
                // Only for a row that may actually be a duplicate. A `pending`
                // row never reached a transport, so resending it is not a second
                // copy and recording one would make the reader cry wolf on every
                // clean recovery.
                let duplicate = o.may_be_a_duplicate();
                let recovered_at_ms = duplicate.then_some(at_ms).or(o.recovered_at_ms);
                tx.execute(
                    "UPDATE delivery_ledger
                        SET owner_machine = ?2, owner_pid = ?3, updated_at_ms = ?4,
                            recovered_at_ms = ?5
                      WHERE id = ?1",
                    params![o.id, me.machine, me.pid, at_ms, recovered_at_ms],
                )?;
                taken.push(Recovered {
                    body: o.redelivery_body(),
                    may_be_a_duplicate: duplicate,
                    obligation: Obligation {
                        owner: me.clone(),
                        updated_at_ms: at_ms,
                        // The returned row has to match what was just written,
                        // or a caller that trusts it reports the message as
                        // never recovered while the database says otherwise.
                        recovered_at_ms,
                        ..o
                    },
                });
            }
            Ok(taken)
        })
    }

    pub fn obligation(&self, id: i64) -> Result<Option<Obligation>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                &format!("{LEDGER_COLUMNS} WHERE id = ?1"),
                params![id],
                row_to_obligation,
            )
            .optional()?)
    }

    pub fn obligation_by_key(&self, message_key: &str) -> Result<Option<Obligation>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                &format!("{LEDGER_COLUMNS} WHERE message_key = ?1"),
                params![message_key],
                row_to_obligation,
            )
            .optional()?)
    }

    /// The ledger, newest first.
    pub fn obligations(&self, limit: usize) -> Result<Vec<Obligation>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "{LEDGER_COLUMNS} ORDER BY created_at_ms DESC, id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_obligation)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Drop settled rows that are past [`RETENTION_MS`], then trim the ledger
    /// to [`MAX_ROWS`].
    ///
    /// Only settled rows, ever. An unsettled row is somebody still waiting to
    /// hear something, and deleting it to save space would be the exact failure
    /// the ledger exists to prevent — silently, and with the evidence gone.
    /// Returns how many rows went.
    pub fn prune_ledger(&self, at_ms: i64) -> Result<usize> {
        self.write(|tx| {
            let mut gone = tx.execute(
                "DELETE FROM delivery_ledger
                  WHERE state IN ('delivered', 'failed') AND updated_at_ms < ?1",
                params![at_ms - RETENTION_MS],
            )?;
            gone += tx.execute(
                "DELETE FROM delivery_ledger
                  WHERE state IN ('delivered', 'failed')
                    AND id NOT IN (
                      SELECT id FROM delivery_ledger
                       ORDER BY created_at_ms DESC, id DESC LIMIT ?1)",
                params![MAX_ROWS],
            )?;
            Ok(gone)
        })
    }
}

fn row_to_obligation(r: &rusqlite::Row) -> rusqlite::Result<Obligation> {
    Ok(Obligation {
        id: r.get(0)?,
        message_key: r.get(1)?,
        channel: r.get(2)?,
        target: r.get(3)?,
        body: r.get(4)?,
        state: DeliveryState::parse(&r.get::<_, String>(5)?),
        attempts: r.get(6)?,
        owner: Owner {
            machine: r.get(7)?,
            pid: r.get::<_, i64>(8)? as u32,
        },
        run_id: r.get(9)?,
        detail: r.get(10)?,
        recovered_at_ms: r.get(11)?,
        created_at_ms: r.get(12)?,
        updated_at_ms: r.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Liveness as the test says it is, rather than as this box happens to be.
    struct Fake {
        alive: Vec<Owner>,
    }

    impl Fake {
        fn nobody() -> Fake {
            Fake { alive: Vec::new() }
        }
        fn only(owners: &[&Owner]) -> Fake {
            Fake {
                alive: owners.iter().map(|o| (*o).clone()).collect(),
            }
        }
    }

    impl Processes for Fake {
        fn is_alive(&self, owner: &Owner) -> bool {
            self.alive.contains(owner)
        }
    }

    fn dead() -> Owner {
        Owner::new("jod-cloud", 4821)
    }

    fn me() -> Owner {
        Owner::new("jod-cloud", 9001)
    }

    fn message(key: &str) -> NewMessage {
        NewMessage::new(key, "telegram", "chat-7", "the nightly digest is ready")
    }

    fn ledger_with(key: &str, owner: &Owner) -> (Store, i64) {
        let s = Store::in_memory().unwrap();
        let id = s.record_obligation(&message(key), owner, 1_000).unwrap();
        (s, id)
    }

    // ---- the three checkpoints ---------------------------------------------

    #[test]
    fn a_message_is_owed_from_the_moment_it_is_recorded() {
        let (s, id) = ledger_with("run-1", &dead());
        let o = s.obligation(id).unwrap().unwrap();
        assert_eq!(o.state, DeliveryState::Pending);
        assert_eq!(o.attempts, 0);
        assert_eq!(o.channel, "telegram");
        assert_eq!(o.target, "chat-7");
        assert_eq!(o.created_at_ms, 1_000);
    }

    #[test]
    fn taking_a_message_to_the_transport_spends_an_attempt() {
        let (s, id) = ledger_with("run-1", &me());
        assert!(s.mark_attempting(id, &me(), 2_000).unwrap());
        let o = s.obligation(id).unwrap().unwrap();
        assert_eq!(o.state, DeliveryState::Attempting);
        assert_eq!(o.attempts, 1);
        assert_eq!(o.owner, me());
    }

    /// Two Jods racing must produce one sender, not two sends.
    #[test]
    fn only_one_caller_can_take_a_message_to_the_transport() {
        let (s, id) = ledger_with("run-1", &me());
        let other = Owner::new("jod-cloud", 9002);
        assert!(s.mark_attempting(id, &me(), 2_000).unwrap());
        assert!(!s.mark_attempting(id, &other, 2_001).unwrap());
        assert_eq!(s.obligation(id).unwrap().unwrap().owner, me());
        assert_eq!(s.obligation(id).unwrap().unwrap().attempts, 1);
    }

    #[test]
    fn recording_the_same_key_twice_owes_the_message_once() {
        let (s, first) = ledger_with("run-1", &me());
        s.mark_attempting(first, &me(), 2_000).unwrap();
        let again = s
            .record_obligation(&message("run-1"), &me(), 3_000)
            .unwrap();

        assert_eq!(again, first);
        assert_eq!(s.obligations(10).unwrap().len(), 1);
        // And the in-flight row was not reset underneath the process sending it.
        let o = s.obligation(first).unwrap().unwrap();
        assert_eq!(o.state, DeliveryState::Attempting);
        assert_eq!(o.attempts, 1);
    }

    // ---- crash semantics ---------------------------------------------------

    /// It never left, so there is nothing to warn anybody about.
    #[test]
    fn a_pending_row_left_by_a_dead_process_is_redelivered_plainly() {
        let (s, id) = ledger_with("run-1", &dead());

        let taken = s
            .sweep_recoverable(&me(), &Fake::nobody(), "telegram", 5_000)
            .unwrap();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].obligation.id, id);
        assert!(!taken[0].may_be_a_duplicate);
        assert_eq!(taken[0].body, "the nightly digest is ready");
        assert!(!taken[0].body.contains(RECOVERED_MARKER));
    }

    /// It may have arrived and nothing can now say. Labelled, not guessed.
    #[test]
    fn an_attempting_row_left_by_a_dead_process_is_redelivered_with_the_marker() {
        let (s, id) = ledger_with("run-1", &dead());
        s.mark_attempting(id, &dead(), 2_000).unwrap();

        let taken = s
            .sweep_recoverable(&me(), &Fake::nobody(), "telegram", 5_000)
            .unwrap();
        assert_eq!(taken.len(), 1);
        assert!(taken[0].may_be_a_duplicate);
        assert!(
            taken[0].body.starts_with(RECOVERED_MARKER),
            "{}",
            taken[0].body
        );
        assert!(taken[0].body.ends_with("the nightly digest is ready"));
        // The stored body stays as it was written; the marker belongs to this
        // send, not to the message.
        assert_eq!(taken[0].obligation.body, "the nightly digest is ready");
    }

    #[test]
    fn a_delivered_row_is_never_resent() {
        let (s, id) = ledger_with("run-1", &dead());
        s.mark_attempting(id, &dead(), 2_000).unwrap();
        assert!(s.mark_delivered(id, 3_000).unwrap());

        assert!(s
            .sweep_recoverable(&me(), &Fake::nobody(), "telegram", 5_000)
            .unwrap()
            .is_empty());
        assert_eq!(
            s.obligation(id).unwrap().unwrap().state,
            DeliveryState::Delivered
        );
    }

    #[test]
    fn a_failed_row_is_never_resent_either() {
        let (s, id) = ledger_with("run-1", &dead());
        for at in [2_000, 3_000, 4_000] {
            s.mark_attempting(id, &dead(), at).unwrap();
            s.mark_failed(id, "telegram said 400", at).unwrap();
        }
        assert_eq!(
            s.obligation(id).unwrap().unwrap().state,
            DeliveryState::Failed
        );
        assert!(s
            .sweep_recoverable(&me(), &Fake::nobody(), "telegram", 5_000)
            .unwrap()
            .is_empty());
    }

    // ---- the sweep ---------------------------------------------------------

    #[test]
    fn the_sweep_only_claims_rows_whose_owner_is_gone() {
        let s = Store::in_memory().unwrap();
        let live = Owner::new("jod-cloud", 4822);
        let orphan = s
            .record_obligation(&message("run-1"), &dead(), 1_000)
            .unwrap();
        let held = s
            .record_obligation(&message("run-2"), &live, 1_000)
            .unwrap();

        let taken = s
            .sweep_recoverable(&me(), &Fake::only(&[&live]), "telegram", 5_000)
            .unwrap();
        assert_eq!(
            taken.iter().map(|r| r.obligation.id).collect::<Vec<_>>(),
            vec![orphan]
        );
        // Untouched, still owed by the process that is sending it.
        assert_eq!(s.obligation(held).unwrap().unwrap().owner, live);
    }

    /// This process cannot see another box's process table, so declaring its
    /// rows dead would mean redelivering what a live Jod is sending right now.
    #[test]
    fn a_row_owned_by_another_machine_is_left_where_it_is() {
        let s = Store::in_memory().unwrap();
        let elsewhere = Owner::new("laptop", 4821);
        let id = s
            .record_obligation(&message("run-1"), &elsewhere, 1_000)
            .unwrap();

        let processes = LocalProcesses {
            machine: "jod-cloud".to_string(),
        };
        let taken = s
            .sweep_recoverable(&me(), &processes, "telegram", 5_000)
            .unwrap();
        assert!(taken.is_empty());
        assert_eq!(s.obligation(id).unwrap().unwrap().owner, elsewhere);
    }

    /// Claimed as it is read, so two Jods starting together do not both decide
    /// to redeliver the same message.
    #[test]
    fn a_swept_row_belongs_to_the_process_that_swept_it() {
        let (s, id) = ledger_with("run-1", &dead());
        assert_eq!(
            s.sweep_recoverable(&me(), &Fake::nobody(), "telegram", 5_000)
                .unwrap()
                .len(),
            1
        );

        let o = s.obligation(id).unwrap().unwrap();
        assert_eq!(o.owner, me());
        assert_eq!(o.updated_at_ms, 5_000);
        // A second sweep by the same process finds nothing left to take.
        assert!(s
            .sweep_recoverable(&me(), &Fake::nobody(), "telegram", 6_000)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_row_that_has_exhausted_its_attempts_is_failed_rather_than_swept() {
        let (s, id) = ledger_with("run-1", &dead());
        for at in [2_000, 3_000, 4_000] {
            s.mark_attempting(id, &dead(), at).unwrap();
            if at < 4_000 {
                s.mark_failed(id, "timed out", at).unwrap();
            }
        }
        // Three attempts spent, and the third one died mid-flight.
        assert_eq!(s.obligation(id).unwrap().unwrap().attempts, MAX_ATTEMPTS);

        assert!(s
            .sweep_recoverable(&me(), &Fake::nobody(), "telegram", 5_000)
            .unwrap()
            .is_empty());
        let o = s.obligation(id).unwrap().unwrap();
        assert_eq!(o.state, DeliveryState::Failed);
        assert!(o.detail.unwrap().contains("past saving"));
    }

    /// A day-old alert is archaeology, and sending it as though it were current
    /// is its own kind of lie.
    #[test]
    fn a_message_too_old_to_be_news_is_failed_rather_than_delivered_late() {
        let (s, id) = ledger_with("run-1", &dead());
        let much_later = 1_000 + STALE_AFTER_MS;

        assert!(s
            .sweep_recoverable(&me(), &Fake::nobody(), "telegram", much_later)
            .unwrap()
            .is_empty());
        assert_eq!(
            s.obligation(id).unwrap().unwrap().state,
            DeliveryState::Failed
        );
    }

    // ---- retrying ----------------------------------------------------------

    /// A *known* failure sent nothing, so the retry has nothing to warn about.
    #[test]
    fn a_known_failure_returns_the_message_to_pending_for_a_plain_retry() {
        let (s, id) = ledger_with("run-1", &me());
        s.mark_attempting(id, &me(), 2_000).unwrap();
        assert!(!s
            .mark_failed(id, "telegram was rate limited", 2_100)
            .unwrap());

        let o = s.obligation(id).unwrap().unwrap();
        assert_eq!(o.state, DeliveryState::Pending);
        assert_eq!(o.attempts, 1);
        assert_eq!(o.redelivery_body(), "the nightly digest is ready");
        assert!(!o.may_be_a_duplicate());
        // And it can be taken again.
        assert!(s.mark_attempting(id, &me(), 3_000).unwrap());
    }

    #[test]
    fn the_last_allowed_failure_settles_the_row_and_keeps_the_reason() {
        let (s, id) = ledger_with("run-1", &me());
        for at in [2_000, 3_000] {
            s.mark_attempting(id, &me(), at).unwrap();
            assert!(!s.mark_failed(id, "timed out", at).unwrap());
        }
        s.mark_attempting(id, &me(), 4_000).unwrap();
        assert!(s
            .mark_failed(id, "timed out for the third time", 4_000)
            .unwrap());

        let o = s.obligation(id).unwrap().unwrap();
        assert_eq!(o.state, DeliveryState::Failed);
        assert_eq!(o.attempts, MAX_ATTEMPTS);
        assert_eq!(o.detail.as_deref(), Some("timed out for the third time"));
    }

    #[test]
    fn a_settled_row_cannot_be_reopened_by_a_late_report() {
        let (s, id) = ledger_with("run-1", &me());
        s.mark_attempting(id, &me(), 2_000).unwrap();
        s.mark_delivered(id, 3_000).unwrap();

        assert!(!s.mark_failed(id, "a straggler", 4_000).unwrap());
        assert_eq!(
            s.obligation(id).unwrap().unwrap().state,
            DeliveryState::Delivered
        );
    }

    // ---- pruning -----------------------------------------------------------

    #[test]
    fn pruning_drops_settled_rows_past_their_retention_and_keeps_the_rest() {
        let s = Store::in_memory().unwrap();
        let old = s.record_obligation(&message("old"), &me(), 1_000).unwrap();
        let recent = s
            .record_obligation(&message("recent"), &me(), 1_000)
            .unwrap();
        s.mark_attempting(old, &me(), 1_000).unwrap();
        s.mark_delivered(old, 1_000).unwrap();
        s.mark_attempting(recent, &me(), 1_000).unwrap();
        s.mark_delivered(recent, RETENTION_MS).unwrap();

        assert_eq!(s.prune_ledger(RETENTION_MS + 2_000).unwrap(), 1);
        assert_eq!(s.obligation(old).unwrap(), None);
        assert!(s.obligation(recent).unwrap().is_some());
    }

    /// The ledger is evidence, not a log — but an unsettled row is somebody
    /// still waiting, and it is never what gets dropped to save space.
    #[test]
    fn pruning_never_drops_a_message_somebody_is_still_waiting_for() {
        let s = Store::in_memory().unwrap();
        let owed = s.record_obligation(&message("owed"), &me(), 1_000).unwrap();
        for i in 0..MAX_ROWS + 10 {
            let id = s
                .record_obligation(&message(&format!("done-{i}")), &me(), 1_000 + i)
                .unwrap();
            s.mark_attempting(id, &me(), 1_000 + i).unwrap();
            s.mark_delivered(id, 1_000 + i).unwrap();
        }

        s.prune_ledger(2_000).unwrap();
        assert!(s.obligation(owed).unwrap().is_some());
        let left = s.obligations(MAX_ROWS as usize * 2).unwrap().len() as i64;
        assert!(left <= MAX_ROWS + 1, "{left} rows left");
    }

    // ---- the marker --------------------------------------------------------

    #[test]
    fn only_an_interrupted_send_is_labelled_a_possible_duplicate() {
        assert_eq!(redelivery_body(false, "hello"), "hello");
        assert_eq!(
            redelivery_body(true, "hello"),
            format!("{RECOVERED_MARKER}hello")
        );

        // And through the row, which is where the rule is decided.
        let (s, id) = ledger_with("run-1", &dead());
        let plain = s.obligation(id).unwrap().unwrap();
        assert!(
            !plain.may_be_a_duplicate(),
            "a message that never reached a transport cannot be a second copy"
        );
        assert_eq!(plain.redelivery_body(), plain.body);

        s.mark_attempting(id, &dead(), 2_000).unwrap();
        let in_flight = s.obligation(id).unwrap().unwrap();
        assert!(in_flight.may_be_a_duplicate());
        assert!(in_flight.redelivery_body().starts_with(RECOVERED_MARKER));
    }

    /// The fact `0012_recovered_deliveries` exists for: it has to outlive the
    /// delivery, because "why did I get this twice" is asked afterwards.
    ///
    /// Before the column this was unanswerable — a recovered message ends
    /// `delivered` like any other and `mark_delivered` clears `detail` on the
    /// way past, so the interruption left no trace anywhere in the row.
    #[test]
    fn a_message_resent_after_a_crash_still_says_so_once_it_has_landed() {
        let (s, id) = ledger_with("run-1", &dead());
        s.mark_attempting(id, &dead(), 2_000).unwrap();

        let taken = s
            .sweep_recoverable(&me(), &Fake::nobody(), "telegram", 5_000)
            .unwrap();
        assert_eq!(taken.len(), 1);
        assert!(taken[0].may_be_a_duplicate);
        assert_eq!(
            taken[0].obligation.recovered_at_ms,
            Some(5_000),
            "the row handed back matches what was written"
        );

        // It goes out, and the row settles like any other.
        s.mark_delivered(id, 6_000).unwrap();

        let landed = s.obligation(id).unwrap().unwrap();
        assert_eq!(landed.state, DeliveryState::Delivered);
        assert_eq!(landed.detail, None, "delivery clears the failure reason");
        assert_eq!(
            landed.recovered_at_ms,
            Some(5_000),
            "and does not clear the one fact the recipient needs"
        );
        assert!(
            landed.may_be_a_duplicate(),
            "a delivered row that was recovered still warns"
        );
    }

    /// A `pending` row never reached a transport, so resending it is not a
    /// second copy. Recording one would make every clean recovery cry wolf.
    #[test]
    fn a_message_that_never_left_is_not_recorded_as_a_possible_duplicate() {
        let (s, id) = ledger_with("run-1", &dead());

        let taken = s
            .sweep_recoverable(&me(), &Fake::nobody(), "telegram", 5_000)
            .unwrap();
        assert_eq!(taken.len(), 1);
        assert!(!taken[0].may_be_a_duplicate);
        assert_eq!(s.obligation(id).unwrap().unwrap().recovered_at_ms, None);
    }

    /// Once a message may have arrived, it may have arrived — a later *known*
    /// failure proves the last attempt sent nothing and says nothing about the
    /// one that was interrupted. So the warning survives, and a second recovery
    /// keeps the latest instant rather than counting.
    #[test]
    fn a_known_failure_afterwards_does_not_clear_the_warning() {
        let (s, id) = ledger_with("run-1", &dead());
        s.mark_attempting(id, &dead(), 2_000).unwrap();
        s.sweep_recoverable(&me(), &Fake::nobody(), "telegram", 5_000)
            .unwrap();

        // The redelivery itself fails, cleanly, and the row goes back to
        // pending for another go.
        s.mark_failed(id, "transport refused", 6_000).unwrap();
        let retried = s.obligation(id).unwrap().unwrap();
        assert_eq!(retried.state, DeliveryState::Pending);
        assert!(
            retried.may_be_a_duplicate(),
            "the interrupted first attempt is still unaccounted for"
        );
        assert!(retried.redelivery_body().starts_with(RECOVERED_MARKER));
    }

    #[test]
    fn the_marker_is_visible_text_in_the_message_itself() {
        assert!(RECOVERED_MARKER.contains("may be a duplicate"));
        assert!(RECOVERED_MARKER.ends_with("\n\n"));
    }
}
