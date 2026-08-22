//! Conversations and the threads behind the fleet's runs.
//!
//! `core::conversation` holds a full message DAG — a head pointer, real parent
//! edges, forks that share a prefix instead of copying it. This module is the
//! part of it a user can reach: resolving whatever id a screen has into a
//! conversation, listing a thread, and forking one.
//!
//! Everything below is either a plain function over `&Store` or a pure function
//! over rows. Nothing here touches the terminal, so all of it is testable
//! against `Store::in_memory()` — the same seam `data.rs` keeps, and the same
//! reason: the screens have to be provable without a TTY.

use jod_core::conversation::Role;
use jod_core::store::Store;

use super::short;

/// How many conversations [`resolve`] will scan for a prefix. Generous, because
/// a cap that hides one is a thread `/resume` cannot find; small enough that the
/// scan stays a single indexed query.
pub const LIST_LIMIT: usize = 50;

/// How much of a message a row shows. Wide enough that two branches off the
/// same point read as different, narrow enough that a tool payload cannot own
/// the transcript.
const SNIPPET: usize = 64;

// ---- rows --------------------------------------------------------------

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

// ---- loading -----------------------------------------------------------

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
        return Err("name a conversation — the fleet lists them".to_string());
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
    /// One conversation, as a listing of its turns. What the conversation
    /// search lands on when a hit is opened.
    Open(String),
    /// A new conversation starting from this one's head.
    Fork(String),
    /// What became of the reply a run owed somebody — see [`super::delivery`].
    ///
    /// Here rather than in an `Action` of its own, and the reason is a seam
    /// rather than a preference: `Action::Sessions` is already carried through
    /// `perform`, so riding it costs nothing, while a second variant would mean
    /// editing the loop that runs every other verb. The subject keeps its own
    /// module; only the envelope is shared.
    ///
    /// The `String` is a **run** id, not a conversation — this is the one
    /// request that starts from a fleet row rather than a thread, which is why
    /// it does not go through `resolve`.
    Delivery(String),
}

/// Carry out one request and say what happened, one line per line of output.
///
/// Multi-line because one of these is a listing, and folding a thread's turns
/// and branches into a single notice would wrap a conversation into a
/// paragraph — the same reason `/config` answers in lines.
pub fn apply(store: &Store, request: &Request, now_ms: i64) -> Vec<String> {
    match request {
        Request::Open(needle) => match resolve(store, needle) {
            Ok(id) => render_open(store, &id),
            Err(said) => vec![said],
        },
        Request::Fork(needle) => on_conversation(store, needle, |id| fork(store, id)),
        Request::Delivery(run_id) => super::delivery::about_run(store, run_id, now_ms),
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

/// One conversation: the live thread, oldest turn first.
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
    use jod_core::conversation::NewMessage;
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
        let conversations = store.conversations(10).expect("the conversations");
        let forked = conversations
            .iter()
            .find(|c| c.forked_from.is_some())
            .expect("the fork was written down");
        assert_eq!(forked.forked_from.as_deref(), Some(id.as_str()));
        assert_eq!(
            forked.message_count, 0,
            "a fork has minted nothing of its own yet"
        );
        // It shares the prefix rather than copying it, which is the reason Jod
        // keeps its own graph at all.
        assert_eq!(thread_rows(&store, &forked.id).len(), 2);
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

}
