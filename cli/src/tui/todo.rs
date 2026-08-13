//! The agent's plan, in the transcript, updating in place.
//!
//! The board exists as its own screen, and that is the wrong place while a turn
//! is running: what you want then is the current plan changing in front of you,
//! not a screen you have to leave the conversation to visit.
//!
//! ## One block that updates, not a block per revision
//!
//! This is the whole slice. A harness rewrites its todo list constantly — often
//! once per item finished — and every rewrite arrives as another tool call. Fed
//! naively into the transcript that is fifteen near-identical lists between two
//! sentences, and the conversation becomes unreadable at exactly the moment you
//! are trying to follow it. So a revision **replaces** the previous block
//! rather than following it; see `App::apply`.
//!
//! The block stays where it first appeared rather than jumping to the bottom.
//! Its position is a fact about when the agent started planning, and a block
//! that moved on every revision would be a second kind of noise in place of the
//! first.

use serde_json::Value;

/// Where one item has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Pending,
    /// The one being worked on. Harnesses agree there is at most one, and the
    /// renderer leans on that to point at it.
    Doing,
    Done,
}

impl State {
    /// A glyph as well as a colour, because `NO_COLOR` and colour-blind readers
    /// both have to read this column.
    pub fn glyph(&self) -> &'static str {
        match self {
            State::Pending => "○",
            State::Doing => "◐",
            State::Done => "●",
        }
    }

    fn parse(s: &str) -> State {
        match s {
            "completed" | "done" => State::Done,
            "in_progress" | "active" | "doing" => State::Doing,
            _ => State::Pending,
        }
    }
}

/// One line of the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub text: String,
    pub state: State,
}

/// Whether a tool's *name* says it revises the plan.
///
/// Named separately from `from_tool` because a tool's *result* carries no
/// arguments to recognise it by, and the transcript still has to know that the
/// plan block is that result's announcement.
pub fn names_a_plan(name: &str) -> bool {
    name.to_ascii_lowercase().contains("todo")
}

/// The plan a todo tool call carries, or `None` for every other tool.
///
/// Reads the *call's arguments* rather than its result, for the reason
/// `diff::from_tool` does: the arguments are what the agent decided, and they
/// are present whether or not the tool answered.
pub fn from_tool(name: &str, input: &Value) -> Option<Vec<Item>> {
    if !names_a_plan(name) {
        return None;
    }
    let list = input
        .as_object()?
        .iter()
        .find(|(k, _)| {
            let k = k.to_ascii_lowercase();
            k == "todos" || k == "items" || k == "plan"
        })
        .and_then(|(_, v)| v.as_array())?;

    let items: Vec<Item> = list
        .iter()
        .filter_map(|entry| {
            let text = match entry {
                // A bare list of strings is a plan with no progress in it,
                // which is still a plan worth showing.
                Value::String(s) => return Some(Item {
                    text: s.clone(),
                    state: State::Pending,
                }),
                Value::Object(map) => map
                    .iter()
                    .find(|(k, _)| {
                        let k = k.to_ascii_lowercase();
                        k == "content" || k == "task" || k == "text" || k == "title"
                    })
                    .and_then(|(_, v)| v.as_str())?,
                _ => return None,
            };
            let state = entry
                .get("status")
                .or_else(|| entry.get("state"))
                .and_then(|v| v.as_str())
                .map(State::parse)
                .unwrap_or(State::Pending);
            Some(Item {
                text: text.to_string(),
                state,
            })
        })
        .collect();

    // An empty list is not a plan. A harness that clears its todos should
    // leave the last real one on screen rather than replacing it with nothing,
    // which would read as the plan having been abandoned.
    (!items.is_empty()).then_some(items)
}

/// How far through the plan the agent is, as `done / total`.
pub fn progress(items: &[Item]) -> (usize, usize) {
    (
        items.iter().filter(|i| i.state == State::Done).count(),
        items.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_todo_call_becomes_a_plan() {
        let items = from_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    { "content": "port the lexer", "status": "completed" },
                    { "content": "write the docs", "status": "in_progress" },
                    { "content": "cut a release", "status": "pending" },
                ]
            }),
        )
        .expect("a plan");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].state, State::Done);
        assert_eq!(items[1].state, State::Doing);
        assert_eq!(items[2].state, State::Pending);
        assert_eq!(items[1].text, "write the docs");
    }

    /// The harnesses disagree about both key and vocabulary, and a missed
    /// spelling silently costs a whole harness its plan.
    #[test]
    fn the_other_spellings_are_recognised_too() {
        let items = from_tool(
            "todo_write",
            &json!({ "items": [{ "task": "ship it", "state": "done" }] }),
        )
        .expect("a plan");
        assert_eq!(items[0].text, "ship it");
        assert_eq!(items[0].state, State::Done);
    }

    /// A plan with no progress in it is still a plan.
    #[test]
    fn a_bare_list_of_strings_is_a_plan_of_pending_items() {
        let items = from_tool("TodoWrite", &json!({ "todos": ["one", "two"] })).expect("a plan");
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.state == State::Pending));
    }

    #[test]
    fn every_other_tool_carries_no_plan() {
        assert!(from_tool("Bash", &json!({ "command": "cargo test" })).is_none());
        assert!(from_tool("Edit", &json!({ "file_path": "a.rs" })).is_none());
    }

    /// Clearing the list should leave the last real plan on screen: an empty
    /// block would read as the plan having been abandoned.
    #[test]
    fn an_empty_list_is_not_a_plan() {
        assert!(from_tool("TodoWrite", &json!({ "todos": [] })).is_none());
    }

    #[test]
    fn progress_counts_what_is_finished() {
        let items = from_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    { "content": "a", "status": "completed" },
                    { "content": "b", "status": "completed" },
                    { "content": "c", "status": "pending" },
                ]
            }),
        )
        .unwrap();
        assert_eq!(progress(&items), (2, 3));
    }

    #[test]
    fn every_state_has_a_glyph_of_its_own() {
        let glyphs = [State::Pending, State::Doing, State::Done].map(|s| s.glyph());
        assert_eq!(glyphs.len(), 3);
        assert_ne!(glyphs[0], glyphs[1]);
        assert_ne!(glyphs[1], glyphs[2]);
    }
}
