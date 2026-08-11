//! Conversations, branches, and the work a revert left behind.
//!
//! `core::conversation` already holds a full message DAG — a head pointer, real
//! parent edges, forks that share a prefix instead of copying it — and none of
//! it reached a user. The wiring audit said so plainly: `tips`, `branch_at`,
//! `children` and `sibling_pager` had no production call site at all. A graph
//! nobody can see is a graph nobody can use, and the specific thing it was
//! failing to deliver is the promise `Store::revert_to` makes in its own doc
//! comment — *"destroy nothing"*. Nothing was destroyed, and nothing was
//! findable either, which to the person who reverted is the same outcome.
//!
//! So the rule this module is written to: **after a revert, the abandoned work
//! is on screen with a name.** Not an integer to remember, not "it's still in
//! the database" — a line that says what that branch was about and how to get
//! back onto it. That is what [`tip_rows`] is for, and it is why every listing
//! here carries its abandoned count rather than making you open a conversation
//! to discover it has one.
//!
//! Everything below is either a plain function over `&Store` or a pure function
//! over rows. Nothing here touches the terminal, so all of it is testable
//! against `Store::in_memory()` — the same seam `data.rs` keeps, and the same
//! reason: the screens have to be provable without a TTY.

use jod_core::conversation::{ConversationSummary, Message, NewMessage, Role};
use jod_core::store::Store;

use super::app::short_duration;
use super::short;

/// How many conversations the list asks for. Generous, because the list is the
/// only way into an old thread and a cap that hides one is a thread you cannot
/// reach; small enough that the per-row tip query below stays cheap.
const LIST_LIMIT: usize = 50;

/// How much of a message a row shows. Wide enough that two branches off the
/// same point read as different, narrow enough that a tool payload cannot own
/// the transcript.
const SNIPPET: usize = 64;

// ---- rows --------------------------------------------------------------

/// One conversation as the list renders it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    /// The 8-character handle every other screen uses for an id.
    pub short: String,
    pub title: String,
    pub harness: String,
    pub model: Option<String>,
    /// Messages this conversation minted — not the length of its thread. A fork
    /// reads 0 on the day it is made, which is the honest number.
    pub messages: i64,
    pub updated_at_ms: i64,
    /// The conversation this one was forked out of, already shortened.
    pub forked_from: Option<String>,
    /// Leaves that are not on the live thread. Non-zero means a revert happened
    /// here and there is work waiting to be picked back up.
    pub abandoned: usize,
}

impl SessionRow {
    /// `⑂` for a fork, `⚑` when something was abandoned here, `●` otherwise.
    ///
    /// A glyph as well as the count, because the count is what you read when
    /// you are already looking at the row and the glyph is what makes you look.
    pub fn glyph(&self) -> &'static str {
        if self.abandoned > 0 {
            "⚑"
        } else if self.forked_from.is_some() {
            "⑂"
        } else {
            "●"
        }
    }
}

/// One message of the live thread.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadRow {
    pub id: i64,
    pub role: Role,
    /// One line, truncated. The whole thing lives in the store.
    pub text: String,
    /// The head — the message the next turn will hang from.
    pub head: bool,
    /// `false` once a compaction has summarised this message out of the live
    /// window. Shown rather than hidden: "this is still here, it is just not
    /// being sent" is the entire difference between compaction and deletion.
    pub active: bool,
    /// `(position, total)` among its siblings, for a `‹2/3›` pager — `None`
    /// when this turn has no alternatives.
    pub pager: Option<(usize, usize)>,
    /// How many lines of conversation leave this point. `1` for an ordinary
    /// turn; more means the thread forked here and the others are still there.
    ///
    /// The pager and this answer different questions and both are needed: the
    /// pager says *which* alternative you are reading, this says a fork exists
    /// at all — and it is readable on the turn *above* the fork, which is where
    /// you are looking when you are trying to find one.
    pub branches: usize,
}

/// A leaf of the DAG, named well enough to go back to.
///
/// The naming is the whole point, and it is why this is not just
/// `Store::tips` reformatted. A tip's *own* text is usually the least useful
/// thing about it — the last message of an abandoned branch is some tool result
/// or a half-finished sentence. What identifies a branch to the person who
/// abandoned it is **where it left the thread and what it tried next**, so that
/// is what [`TipRow::opener`] carries.
#[derive(Debug, Clone, PartialEq)]
pub struct TipRow {
    pub id: i64,
    /// The tip of the thread you are on now, rather than an abandoned one.
    pub live: bool,
    /// The last message this branch and the live thread share. `None` for the
    /// live tip, and for a branch that diverges at the root — two roots in one
    /// conversation share nothing.
    pub diverged_at: Option<i64>,
    /// The first message *after* the divergence: what this branch tried. This
    /// is the name a user recognises.
    pub opener: String,
    /// The last message on it, for "how far did it get".
    pub last: String,
    /// Messages from the divergence down to the tip, inclusive.
    pub turns: usize,
    pub at_ms: i64,
}

impl TipRow {
    /// The one-line name this branch is offered under.
    ///
    /// `opener` first and in quotes, because that is the sentence the user
    /// wrote or read and the only part of the line they will actually scan.
    pub fn label(&self) -> String {
        if self.live {
            return format!("#{} on this thread · “{}”", self.id, self.last);
        }
        format!(
            "#{} abandoned · “{}” · {} turn{} · ended “{}”",
            self.id,
            self.opener,
            self.turns,
            if self.turns == 1 { "" } else { "s" },
            self.last,
        )
    }
}

// ---- loading -----------------------------------------------------------

/// Every conversation, newest first, with its abandoned count already counted.
///
/// The tip query per row is deliberate. `Store::conversations` cannot report it
/// — abandonment is a property of the graph, not of the conversation row — and
/// a list that made you open each thread to find out which ones have work
/// waiting would be a list nobody reads. It is one indexed query per row over a
/// capped list.
///
/// Errors are swallowed the way every loader in `data.rs` swallows them: a
/// locked database costs one stale frame, never the session.
pub fn session_rows(store: &Store, limit: usize) -> Vec<SessionRow> {
    store
        .conversations(limit)
        .unwrap_or_default()
        .into_iter()
        .map(|c| session_row(store, c))
        .collect()
}

fn session_row(store: &Store, c: ConversationSummary) -> SessionRow {
    let live = c.head_id;
    let abandoned = store
        .tips(&c.id)
        .unwrap_or_default()
        .into_iter()
        .filter(|t| Some(t.id) != live)
        .count();
    SessionRow {
        short: short(&c.id),
        // An unnamed conversation falls back to its opening message in SQL, but
        // a conversation with neither — a fresh fork — would render as a blank
        // line, and a blank row in a list is a row you cannot aim at.
        title: if c.title.trim().is_empty() {
            "(untitled)".to_string()
        } else {
            one_line(&c.title, SNIPPET)
        },
        id: c.id,
        harness: c.harness,
        model: c.model,
        messages: c.message_count,
        updated_at_ms: c.updated_at_ms,
        forked_from: c.forked_from.as_deref().map(short),
        abandoned,
    }
}

/// The live thread, oldest first, each turn carrying its sibling pager.
///
/// `Store::siblings` warns that siblings arise from parallel tool results as
/// well as from branching, so a pager drawn on "has siblings" appears on turns
/// nobody edited. The call it says is the caller's is made here: a pager is
/// only shown on a **user** turn. Alternatives to a question are branches —
/// somebody edited a prompt and asked again — while two tool results under one
/// assistant turn are one turn's fan-out, and putting `‹1/2›` on those would
/// teach the user to ignore the marker everywhere.
pub fn thread_rows(store: &Store, conversation_id: &str) -> Vec<ThreadRow> {
    let head = store
        .conversation(conversation_id)
        .ok()
        .flatten()
        .and_then(|c| c.head_id);
    store
        .thread(conversation_id)
        .unwrap_or_default()
        .into_iter()
        .map(|m| ThreadRow {
            head: Some(m.id) == head,
            pager: match m.role {
                Role::User => store.sibling_pager(m.id).unwrap_or_default(),
                _ => None,
            },
            // Counted the same way and for the same reason: alternative
            // *questions* are branches, while the several tool results under
            // one assistant turn are that turn's fan-out. Counting those would
            // put a fork marker on most of a busy transcript.
            branches: store
                .children(m.id)
                .unwrap_or_default()
                .iter()
                .filter(|c| c.role == Role::User)
                .count()
                .max(1),
            text: one_line(&m.text, SNIPPET),
            role: m.role,
            active: m.active,
            id: m.id,
        })
        .collect()
}

/// Every leaf, with the abandoned ones named by where they left the thread.
///
/// Ordered live-tip-first, then most recent abandonment first — you are most
/// likely to want back what you just gave up on, which is the case OpenCode
/// ships `unrevert` for.
pub fn tip_rows(store: &Store, conversation_id: &str) -> Vec<TipRow> {
    let live: Vec<Message> = store.thread(conversation_id).unwrap_or_default();
    let live_ids: Vec<i64> = live.iter().map(|m| m.id).collect();
    let live_tip = live_ids.last().copied();

    let mut rows: Vec<TipRow> = store
        .tips(conversation_id)
        .unwrap_or_default()
        .into_iter()
        .map(|tip| tip_row(store, &tip, &live_ids, live_tip))
        .collect();
    // The tie-break is `Reverse(id)`, not `id`, and it is the difference
    // between an undo/redo pair that works and one that does not. Two tips
    // abandoned in the same millisecond sort by insertion order, so ascending
    // ids made `u` offer back the *oldest* branch immediately after `v` had set
    // aside the newest — pressing undo then redo landed somewhere the user had
    // never been. `Store::conversations` breaks its own tie the same way and
    // says why: at millisecond resolution, insertion order is the only truth
    // available.
    rows.sort_by_key(|r| (!r.live, std::cmp::Reverse(r.at_ms), std::cmp::Reverse(r.id)));
    rows
}

fn tip_row(store: &Store, tip: &Message, live_ids: &[i64], live_tip: Option<i64>) -> TipRow {
    if Some(tip.id) == live_tip {
        return TipRow {
            id: tip.id,
            live: true,
            diverged_at: None,
            opener: one_line(&tip.text, SNIPPET),
            last: one_line(&tip.text, SNIPPET),
            turns: live_ids.len(),
            at_ms: tip.at_ms,
        };
    }

    // Walk up from the leaf until we step onto the live thread. The message we
    // were standing on when that happened is what this branch tried first, and
    // the one we stepped onto is where the two threads part company.
    let mut opener = tip.clone();
    let mut diverged_at = None;
    let mut turns = 1;
    let mut at = tip.parent_id;
    while let Some(parent_id) = at {
        if live_ids.contains(&parent_id) {
            diverged_at = Some(parent_id);
            break;
        }
        let Some(parent) = store.message(parent_id).ok().flatten() else {
            break;
        };
        at = parent.parent_id;
        opener = parent;
        turns += 1;
    }

    TipRow {
        id: tip.id,
        live: false,
        diverged_at,
        opener: one_line(&opener.text, SNIPPET),
        last: one_line(&tip.text, SNIPPET),
        turns,
        at_ms: tip.at_ms,
    }
}

/// The message the head should go back to when the last turn is undone.
///
/// "Undo my last turn" means the head lands on the message *before* the newest
/// user turn — where the thread stood when that question had not been asked
/// yet. Returns `None` at the first question of a conversation, because there
/// is no state before it to go back to and reverting to the root would silently
/// mean something else.
pub fn rewind_target(store: &Store, conversation_id: &str) -> Option<i64> {
    let thread = store.thread(conversation_id).ok()?;
    let last_user = thread.iter().rposition(|m| m.role == Role::User)?;
    thread.get(last_user)?.parent_id
}

/// The abandoned leaf to offer back first — the most recent one.
pub fn restore_target(store: &Store, conversation_id: &str) -> Option<TipRow> {
    tip_rows(store, conversation_id)
        .into_iter()
        .find(|t| !t.live)
}

/// Turn whatever the caller has into a conversation id.
///
/// Three things get typed or passed here and all three have to work, because
/// the id a user can see depends on which screen they were looking at: the
/// fleet lists *runs*, the conversation list shows an 8-character prefix, and
/// anything scripted has the full uuid. Resolving a run id through
/// `conversation_for_run` is what lets the fleet's cursor mean "this thread"
/// without the fleet knowing conversations exist.
///
/// An ambiguous prefix is an error rather than a pick. Guessing which of two
/// threads you meant and then reverting it is the one failure this whole module
/// exists to prevent.
pub fn resolve(store: &Store, needle: &str) -> Result<String, String> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Err("name a conversation — `c` lists them".to_string());
    }
    if matches!(store.conversation(needle), Ok(Some(_))) {
        return Ok(needle.to_string());
    }
    if let Ok(Some(id)) = store.conversation_for_run(needle) {
        return Ok(id);
    }
    let matched: Vec<String> = store
        .conversations(LIST_LIMIT)
        .unwrap_or_default()
        .into_iter()
        .map(|c| c.id)
        .filter(|id| id.starts_with(needle))
        .collect();
    match matched.len() {
        1 => Ok(matched.into_iter().next().expect("exactly one")),
        0 => Err(format!("no conversation matches `{}`", short(needle))),
        n => Err(format!(
            "`{}` matches {n} conversations — type more of it",
            short(needle)
        )),
    }
}

// ---- what the screen asks for ------------------------------------------

/// A verb the sessions layer can carry out, described rather than performed.
///
/// The same discipline the rest of the TUI keeps: a key handler has no store,
/// so it hands back one of these and the loop runs it. Every variant is
/// answered by [`apply`], in sentences, so what the user is told — including a
/// refusal — is testable without a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Every conversation, newest first.
    List,
    /// One conversation: its thread, and every branch hanging off it.
    Open(String),
    /// Undo the last turn. Moves the head back; destroys nothing.
    Rewind(String),
    /// Put the head back on the branch a rewind abandoned — `unrevert`.
    Restore(String),
    /// A new conversation starting from this one's head.
    Fork(String),
    /// Ask the last question again, as a second answer rather than a
    /// replacement — ChatGPT's "regenerate", and the reason `branch_at` exists.
    Retry(String),
}

/// Carry out one request and say what happened, one line per line of output.
///
/// Multi-line because two of these are listings, and folding a list of
/// conversations into a single notice would wrap fifty threads into a
/// paragraph — the same reason `/config` answers in lines.
pub fn apply(store: &Store, request: &Request, now_ms: i64) -> Vec<String> {
    match request {
        Request::List => render_list(&session_rows(store, LIST_LIMIT), now_ms),
        Request::Open(needle) => match resolve(store, needle) {
            Ok(id) => render_open(store, &id),
            Err(said) => vec![said],
        },
        Request::Rewind(needle) => on_conversation(store, needle, |id| rewind(store, id)),
        Request::Restore(needle) => on_conversation(store, needle, |id| restore(store, id)),
        Request::Fork(needle) => on_conversation(store, needle, |id| fork(store, id)),
        Request::Retry(needle) => on_conversation(store, needle, |id| retry(store, id)),
    }
}

fn on_conversation(
    store: &Store,
    needle: &str,
    verb: impl FnOnce(&str) -> Vec<String>,
) -> Vec<String> {
    match resolve(store, needle) {
        Ok(id) => verb(&id),
        Err(said) => vec![said],
    }
}

// ---- the verbs ---------------------------------------------------------

/// Undo the last turn, and immediately say how to undo the undo.
///
/// The second sentence is not politeness. A revert that says only "done" is
/// indistinguishable from a delete to the person who just pressed it, and the
/// whole reason `revert_to` keeps the rows is so that it is not one. Naming the
/// branch in the same breath is the cheapest possible proof.
fn rewind(store: &Store, id: &str) -> Vec<String> {
    let Some(target) = rewind_target(store, id) else {
        return vec![format!(
            "{} has nothing before its first question to go back to",
            short(id)
        )];
    };
    if let Err(e) = store.revert_to(id, target) {
        return vec![format!("could not rewind {}: {e}", short(id))];
    }
    let mut said = vec![format!("{} rewound to #{target}", short(id))];
    match restore_target(store, id) {
        Some(tip) => said.push(format!("  kept: {}", tip.label())),
        // Only reachable when the rewind moved onto a branch that already had
        // no children — nothing was left behind, so nothing is claimed.
        None => said.push("  nothing was left behind".to_string()),
    }
    said
}

fn restore(store: &Store, id: &str) -> Vec<String> {
    let Some(tip) = restore_target(store, id) else {
        return vec![format!(
            "{} has no abandoned branch to go back to",
            short(id)
        )];
    };
    // Not `tip.label()`: that line calls the branch abandoned, which stops
    // being true the instant this succeeds.
    match store.move_head(id, tip.id) {
        Ok(()) => vec![format!(
            "{} back on “{}” — #{}, {} turn{}",
            short(id),
            tip.opener,
            tip.id,
            tip.turns,
            if tip.turns == 1 { "" } else { "s" }
        )],
        Err(e) => vec![format!("could not restore #{}: {e}", tip.id)],
    }
}

/// Ask the last question again, beside the answer it already got.
///
/// `Store::branch_at` in one call, and the reason it is one call rather than a
/// revert followed by an append: the two have to be the same act. A retry that
/// reverted, failed to append, and left the head parked before a question
/// nobody could see would look exactly like a delete.
///
/// The new question lands as a *sibling* of the old one, which is ChatGPT's
/// model: editing appends a node under the same parent instead of mutating the
/// old one, and the answer you did not like stays in the export. So this is
/// non-destructive too, and the sentence says where the first answer went.
fn retry(store: &Store, id: &str) -> Vec<String> {
    let Some(thread) = store.thread(id).ok() else {
        return vec![format!("could not read {}", short(id))];
    };
    let Some(asked) = thread.iter().rev().find(|m| m.role == Role::User) else {
        return vec![format!("{} has no question to ask again", short(id))];
    };
    let Some(target) = asked.parent_id else {
        return vec![format!(
            "{}'s first question has nothing before it to branch from",
            short(id)
        )];
    };
    let text = asked.text.clone();
    match store.branch_at(id, target, NewMessage::user(&text)) {
        Ok(new_id) => {
            let mut said = vec![format!(
                "{} asking again: “{}” — #{new_id}",
                short(id),
                one_line(&text, SNIPPET)
            )];
            // Named off the tip rather than off the old question, because the
            // thing the user wants back is the *answer* they are replacing and
            // the question id points at both branches equally.
            match restore_target(store, id) {
                Some(tip) => said.push(format!("  kept: {}", tip.label())),
                None => said.push("  the first attempt had not answered yet".to_string()),
            }
            said
        }
        Err(e) => vec![format!("could not ask {} again: {e}", short(id))],
    }
}

/// Fork the conversation at its head into a thread of its own.
///
/// At the head rather than at an arbitrary point because that is the request a
/// key can carry: "try something else from here, and keep this". Forking
/// further back is [`Request::MoveTo`] followed by this, which is two keys and
/// no new vocabulary.
fn fork(store: &Store, id: &str) -> Vec<String> {
    let Some(head) = store
        .conversation(id)
        .ok()
        .flatten()
        .and_then(|c| c.head_id)
    else {
        return vec![format!("{} has no messages to fork from", short(id))];
    };
    match store.fork_conversation(id, head, None) {
        Ok(new) => vec![format!(
            "forked {} at #{head} → {} “{}”",
            short(id),
            short(&new.id),
            new.title
        )],
        Err(e) => vec![format!("could not fork {}: {e}", short(id))],
    }
}

// ---- rendering ---------------------------------------------------------

/// The conversation list, one line each plus a heading.
pub fn render_list(rows: &[SessionRow], now_ms: i64) -> Vec<String> {
    if rows.is_empty() {
        return vec!["no conversations yet — every run starts one".to_string()];
    }
    let mut out = vec![format!(
        "{} conversation{}",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    )];
    for row in rows {
        let mut line = format!(
            "  {} {}  {:<40}  {:<12} {:>3} msg  {} ago",
            row.short,
            row.glyph(),
            row.title,
            row.harness,
            row.messages,
            short_duration(now_ms.saturating_sub(row.updated_at_ms)),
        );
        if let Some(from) = &row.forked_from {
            line.push_str(&format!("  ⑂ from {from}"));
        }
        // The count, not just the glyph: "something was abandoned here" is a
        // different message from "two things were", and the second is the one
        // that makes you open the thread.
        if row.abandoned > 0 {
            line.push_str(&format!("  ⚑ {} abandoned", row.abandoned));
        }
        out.push(line);
    }
    out.push("  b opens the branches of the selected run’s thread".to_string());
    out
}

/// One conversation: the live thread, then every branch off it.
pub fn render_open(store: &Store, id: &str) -> Vec<String> {
    let thread = thread_rows(store, id);
    if thread.is_empty() {
        return vec![format!("{} has no messages", short(id))];
    }
    let mut out = vec![format!("{} — {} turns", short(id), thread.len())];
    for row in &thread {
        out.push(format!(
            "  {}#{:<5} {:<10} {}{}{}",
            if row.head { "▶ " } else { "  " },
            row.id,
            row.role.as_str(),
            row.text,
            // A compacted turn is still in the thread and still searchable, so
            // it is marked rather than dropped.
            if row.active { "" } else { "  (compacted)" },
            match row.pager {
                Some((at, of)) => format!("  ‹{at}/{of}›"),
                None => String::new(),
            }
        ));
        // Drawn under the turn it hangs off, so a fork reads as something that
        // happened *here* rather than as a property of the branch you happen to
        // be on. Only when there is more than one, because "1 branch" is just
        // "a conversation".
        if row.branches > 1 {
            out.push(format!("         ⑂ {} lines leave this turn", row.branches));
        }
    }
    out.extend(render_tips(&tip_rows(store, id)));
    out
}

/// The branch list. Written to be readable on its own, because this is what a
/// user is looking at when they are trying to get something back.
pub fn render_tips(tips: &[TipRow]) -> Vec<String> {
    let abandoned = tips.iter().filter(|t| !t.live).count();
    if abandoned == 0 {
        return vec!["  no abandoned branches".to_string()];
    }
    let mut out = vec![format!(
        "  {abandoned} abandoned branch{}",
        if abandoned == 1 { "" } else { "es" }
    )];
    for tip in tips.iter().filter(|t| !t.live) {
        let mut line = format!("    {}", tip.label());
        if let Some(at) = tip.diverged_at {
            line.push_str(&format!(" · left the thread at #{at}"));
        }
        out.push(line);
    }
    out.push("    u goes back to the newest of these".to_string());
    out
}

/// Collapse to one line and truncate. A tool payload is one message like any
/// other and must not be allowed to own the screen.
fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    format!("{}…", flat.chars().take(max).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::HarnessKind;

    fn store() -> Store {
        Store::in_memory().expect("an in-memory store")
    }

    fn conversation(store: &Store, title: &str) -> String {
        let c = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation");
        store.set_conversation_title(&c.id, title).expect("a title");
        c.id
    }

    fn say(store: &Store, id: &str, role: Role, text: &str) -> i64 {
        store
            .append_message(id, NewMessage::new(role, text))
            .expect("an appended message")
    }

    #[test]
    fn the_conversation_list_puts_the_most_recently_touched_first() {
        let store = store();
        let old = conversation(&store, "the old one");
        say(&store, &old, Role::User, "first");
        let new = conversation(&store, "the new one");
        say(&store, &new, Role::User, "second");

        let rows = session_rows(&store, 10);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, new, "the thread just touched leads the list");
        assert_eq!(rows[1].id, old);
        assert_eq!(rows[0].title, "the new one");
        assert_eq!(rows[0].messages, 1);
    }

    #[test]
    fn a_conversation_nobody_named_still_gets_an_aimable_row() {
        let store = store();
        let id = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;

        let rows = session_rows(&store, 10);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(
            rows[0].title, "(untitled)",
            "a blank row cannot be aimed at"
        );
    }

    /// The failure this whole module exists to prevent: reverting, and then
    /// having no way back except remembering an integer.
    #[test]
    fn a_reverted_branch_is_listed_by_name_and_can_be_walked_back_onto() {
        let store = store();
        let id = conversation(&store, "the parser");
        say(&store, &id, Role::User, "fix the parser");
        let keep = say(&store, &id, Role::Assistant, "fixed it");
        say(&store, &id, Role::User, "now rewrite it in one pass");
        let abandoned = say(&store, &id, Role::Assistant, "rewritten, tests pass");

        store.revert_to(&id, keep).expect("a revert");

        // Named on screen, not merely present in the database.
        let tips = tip_rows(&store, &id);
        let lost = tips
            .iter()
            .find(|t| !t.live)
            .expect("the abandoned branch is listed");
        assert_eq!(lost.id, abandoned);
        assert_eq!(
            lost.opener, "now rewrite it in one pass",
            "a branch is named by what it tried, not by its last tool result"
        );
        assert_eq!(lost.last, "rewritten, tests pass");
        assert_eq!(lost.diverged_at, Some(keep));
        assert_eq!(lost.turns, 2);
        assert!(
            lost.label().contains("now rewrite it in one pass"),
            "the line a user reads has to carry the name: {}",
            lost.label()
        );

        // And reachable: the offer the label makes is one the store honours.
        let said = apply(&store, &Request::Restore(id.clone()), 0);
        assert!(
            said[0].contains(&format!("#{abandoned}")),
            "restore says where it went: {said:?}"
        );
        assert_eq!(
            store
                .conversation(&id)
                .expect("the conversation")
                .expect("it exists")
                .head_id,
            Some(abandoned),
            "the head is back on the work that was abandoned"
        );
    }

    #[test]
    fn rewinding_undoes_the_last_question_and_says_what_it_kept() {
        let store = store();
        let id = conversation(&store, "the parser");
        say(&store, &id, Role::User, "fix the parser");
        let before = say(&store, &id, Role::Assistant, "fixed it");
        say(&store, &id, Role::User, "now break it");
        let lost = say(&store, &id, Role::Assistant, "broken");

        let said = apply(&store, &Request::Rewind(id.clone()), 0);

        assert_eq!(
            store
                .conversation(&id)
                .expect("the conversation")
                .expect("it exists")
                .head_id,
            Some(before),
            "the head stands where the last question had not been asked"
        );
        assert!(said[0].contains(&format!("#{before}")), "{said:?}");
        assert!(
            said[1].contains("now break it") && said[1].contains(&format!("#{lost}")),
            "the undo names its own undo in the same breath: {said:?}"
        );
    }

    /// `v` then `u` has to land exactly where `v` started, even when the
    /// conversation already had older branches lying around — which is the
    /// common case, because reverting is a thing people do more than once.
    ///
    /// It did not, at first. Tips abandoned inside the same millisecond sorted
    /// by ascending id, so `u` offered back the oldest branch and undo/redo
    /// walked the user into a thread they had never seen.
    #[test]
    fn undo_then_redo_lands_back_where_it_started_even_among_older_branches() {
        let store = store();
        let id = conversation(&store, "the parser");
        say(&store, &id, Role::User, "port the parser");
        let fork_point = say(&store, &id, Role::Assistant, "done");
        say(&store, &id, Role::User, "now rewrite it in one pass");
        say(&store, &id, Role::Assistant, "rewritten");
        store.revert_to(&id, fork_point).expect("an older revert");
        say(&store, &id, Role::User, "actually just add a test");
        let was_here = say(&store, &id, Role::Assistant, "added tests/lexer.rs");

        let rewound = apply(&store, &Request::Rewind(id.clone()), 0);
        assert!(
            rewound[1].contains("actually just add a test"),
            "the rewind keeps the branch it just left, not an older one: {rewound:?}"
        );

        apply(&store, &Request::Restore(id.clone()), 0);

        assert_eq!(
            store
                .conversation(&id)
                .expect("the conversation")
                .expect("it exists")
                .head_id,
            Some(was_here),
            "redo returns to the branch undo set aside"
        );
    }

    #[test]
    fn the_first_question_of_a_conversation_has_nothing_to_rewind_to() {
        let store = store();
        let id = conversation(&store, "fresh");
        say(&store, &id, Role::User, "hello");

        let said = apply(&store, &Request::Rewind(id.clone()), 0);

        assert!(
            said[0].contains("nothing before its first question"),
            "{said:?}"
        );
    }

    #[test]
    fn the_list_counts_the_branches_a_revert_left_behind() {
        let store = store();
        let id = conversation(&store, "the parser");
        say(&store, &id, Role::User, "one");
        let fork_point = say(&store, &id, Role::Assistant, "done");
        say(&store, &id, Role::User, "two");
        store.revert_to(&id, fork_point).expect("a revert");
        say(&store, &id, Role::User, "three");
        store.revert_to(&id, fork_point).expect("a second revert");

        let rows = session_rows(&store, 10);

        assert_eq!(rows[0].abandoned, 2, "both attempts are counted");
        assert_eq!(rows[0].glyph(), "⚑");
        let listed = render_list(&rows, 0).join("\n");
        assert!(listed.contains("2 abandoned"), "{listed}");
    }

    #[test]
    fn the_sibling_pager_reports_which_of_the_alternatives_you_are_on() {
        let store = store();
        let id = conversation(&store, "the parser");
        say(&store, &id, Role::User, "one");
        let point = say(&store, &id, Role::Assistant, "done");
        let first = say(&store, &id, Role::User, "attempt one");
        store.revert_to(&id, point).expect("a revert");
        let second = say(&store, &id, Role::User, "attempt two");
        store.revert_to(&id, point).expect("a second revert");
        let third = say(&store, &id, Role::User, "attempt three");

        assert_eq!(store.sibling_pager(first).expect("a pager"), Some((1, 3)));
        assert_eq!(store.sibling_pager(second).expect("a pager"), Some((2, 3)));
        assert_eq!(store.sibling_pager(third).expect("a pager"), Some((3, 3)));

        // And the thread view carries it on the turn that is actually on screen.
        let rows = thread_rows(&store, &id);
        let on_screen = rows.last().expect("the head row");
        assert_eq!(on_screen.id, third);
        assert_eq!(on_screen.pager, Some((3, 3)));
        assert!(on_screen.head);
        assert!(
            render_open(&store, &id).join("\n").contains("‹3/3›"),
            "the pager reaches the screen"
        );

        // The turn the three attempts hang off says so, which is what you are
        // looking at when you are hunting for a fork you half-remember making.
        let fork_row = rows.iter().find(|r| r.id == point).expect("the fork point");
        assert_eq!(fork_row.branches, 3);
        assert_eq!(fork_row.pager, None, "the fork point itself has no rivals");
        assert!(
            render_open(&store, &id)
                .join("\n")
                .contains("3 lines leave this turn"),
            "the fork is announced on the turn it happened at"
        );
    }

    /// `Store::siblings` warns that parallel tool results are siblings too. A
    /// pager on those would appear on turns nobody branched, and a marker that
    /// cries wolf is a marker nobody reads.
    #[test]
    fn parallel_tool_results_do_not_get_a_branch_pager() {
        let store = store();
        let id = conversation(&store, "the parser");
        say(&store, &id, Role::User, "run both");
        let call = say(&store, &id, Role::Assistant, "calling two tools");
        say(&store, &id, Role::ToolResult, "first result");
        store.revert_to(&id, call).expect("back to the call");
        say(&store, &id, Role::ToolResult, "second result");

        let rows = thread_rows(&store, &id);
        let last = rows.last().expect("the head row");

        assert_eq!(last.role, Role::ToolResult);
        assert_eq!(
            last.pager, None,
            "two results of one turn are a fan-out, not two answers to a question"
        );
        let called = rows
            .iter()
            .find(|r| r.id == call)
            .expect("the calling turn");
        assert_eq!(
            called.branches, 1,
            "and the turn above them is not marked as a fork either"
        );
        assert!(!render_open(&store, &id)
            .join("\n")
            .contains("lines leave this turn"));
    }

    #[test]
    fn forking_makes_a_conversation_that_reports_where_it_came_from() {
        let store = store();
        let id = conversation(&store, "the parser");
        say(&store, &id, Role::User, "fix the parser");
        let head = say(&store, &id, Role::Assistant, "fixed it");

        let said = apply(&store, &Request::Fork(id.clone()), 0);

        assert!(said[0].contains(&format!("#{head}")), "{said:?}");
        let rows = session_rows(&store, 10);
        let forked = rows
            .iter()
            .find(|r| r.forked_from.is_some())
            .expect("the fork is in the list");
        assert_eq!(forked.forked_from.as_deref(), Some(short(&id).as_str()));
        assert_eq!(forked.glyph(), "⑂");
        assert_eq!(
            forked.messages, 0,
            "a fork minted nothing yet, and the list says so"
        );
        // It shares the prefix rather than copying it, which is the reason Jod
        // keeps its own graph at all.
        assert_eq!(thread_rows(&store, &forked.id).len(), 2);
    }

    #[test]
    fn asking_again_puts_the_second_answer_beside_the_first_rather_than_over_it() {
        let store = store();
        let id = conversation(&store, "the parser");
        say(&store, &id, Role::User, "port the parser");
        say(&store, &id, Role::Assistant, "ported");
        let asked = say(&store, &id, Role::User, "now add a test");
        let first_answer = say(&store, &id, Role::Assistant, "added a bad test");

        let said = apply(&store, &Request::Retry(id.clone()), 0);

        assert!(said[0].contains("now add a test"), "{said:?}");
        assert!(
            said[1].contains(&format!("#{first_answer}")) && said[1].contains("added a bad test"),
            "the retry names the answer it replaced, not the question both share: {said:?}"
        );

        // The question was asked again, not edited: two of it now hang off the
        // same parent, and the old answer is a findable branch rather than an
        // overwrite.
        assert_eq!(store.sibling_pager(asked).expect("a pager"), Some((1, 2)));
        let tips = tip_rows(&store, &id);
        assert!(
            tips.iter().any(|t| t.id == first_answer && !t.live),
            "the first answer survives as a branch: {tips:?}"
        );
        assert_eq!(
            store.thread(&id).expect("the thread").len(),
            3,
            "the live thread carries the new question, not both"
        );
    }

    #[test]
    fn a_conversation_with_nothing_before_its_first_question_cannot_ask_it_again() {
        let store = store();
        let id = conversation(&store, "fresh");
        say(&store, &id, Role::User, "hello");

        let said = apply(&store, &Request::Retry(id.clone()), 0);

        assert!(
            said[0].contains("nothing before it to branch from"),
            "{said:?}"
        );
    }

    #[test]
    fn a_run_id_names_the_thread_it_wrote_into() {
        let store = store();
        let id = conversation(&store, "the parser");
        store
            .append_message(
                &id,
                NewMessage::user("fix the parser")
                    .from_run("run-7")
                    .at_seq(1),
            )
            .expect("a message from a run");

        assert_eq!(resolve(&store, "run-7"), Ok(id.clone()));
        assert_eq!(resolve(&store, &id), Ok(id.clone()));
        assert_eq!(resolve(&store, &short(&id)), Ok(id));
    }

    #[test]
    fn an_id_that_names_two_threads_names_neither() {
        let store = store();
        // Ids are uuids, so a prefix short enough to be ambiguous is the empty
        // one — which every conversation starts with.
        conversation(&store, "one");
        conversation(&store, "two");

        assert!(resolve(&store, "  ").is_err(), "an empty needle is refused");
        assert!(resolve(&store, "definitely-not-an-id")
            .expect_err("no match")
            .contains("no conversation matches"));
    }

    #[test]
    fn a_compacted_turn_is_marked_rather_than_hidden() {
        let store = store();
        let id = conversation(&store, "the parser");
        let first = say(&store, &id, Role::User, "question 0");
        let second = say(&store, &id, Role::Assistant, "answer 0");
        for i in 1..6 {
            say(&store, &id, Role::User, &format!("question {i}"));
            say(&store, &id, Role::Assistant, &format!("answer {i}"));
        }
        store
            .compact(&id, first, second, "we talked about the parser", "test")
            .expect("a compaction");

        let rows = thread_rows(&store, &id);

        assert!(
            rows.iter().any(|r| !r.active),
            "compaction narrows what is sent, not what is stored"
        );
        assert!(
            render_open(&store, &id).join("\n").contains("(compacted)"),
            "and the screen says which turns those are"
        );
    }

    #[test]
    fn an_empty_database_says_so_instead_of_showing_a_blank_box() {
        let store = store();

        assert_eq!(
            apply(&store, &Request::List, 0),
            vec!["no conversations yet — every run starts one".to_string()]
        );
    }
}
