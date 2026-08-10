//! The nine workspaces, and the one rule that binds them.
//!
//! Chat is home; every other screen is somewhere you went *from* chat and come
//! back to with `Esc`. Keeping that as data — a digit, a letter, a title, a set
//! of sort orders — rather than as three parallel `match`es in the key handler
//! is what lets the which-key menu, the `?` overlay and the direct-jump digits
//! agree without anyone remembering to update all three.

/// One screen. `Chat` is home and is never on the back stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Workspace {
    Chat,
    Fleet,
    Memory,
    /// Memory's second level: one node with its neighbours. Reached from the
    /// list with `g` and never from a digit, because it means nothing without a
    /// node to focus on.
    MemoryGraph,
    Schedules,
    Goals,
    Hooks,
    Tasks,
    Activity,
    Team,
}

use Workspace::*;

impl Workspace {
    /// Every workspace, including the ones with no digit of their own.
    pub const ALL: [Workspace; 10] = [
        Chat,
        Fleet,
        Memory,
        MemoryGraph,
        Schedules,
        Goals,
        Hooks,
        Tasks,
        Activity,
        Team,
    ];

    /// The which-key menu, in the order it is drawn. This is also the order the
    /// digits follow, so `Ctrl-K s` and `4` are visibly the same destination.
    pub const MENU: [Workspace; 9] = [
        Chat, Fleet, Memory, Schedules, Goals, Hooks, Tasks, Activity, Team,
    ];

    /// The letter that reaches this workspace from the which-key menu.
    ///
    /// `w` for team is "who", because `t` is spoken for by tasks — the report's
    /// choice, kept because the menu prints the letter beside every row.
    pub fn letter(self) -> Option<char> {
        Some(match self {
            Chat => 'c',
            Fleet => 'f',
            Memory => 'm',
            Schedules => 's',
            Goals => 'g',
            Hooks => 'h',
            Tasks => 't',
            Activity => 'a',
            Team => 'w',
            MemoryGraph => return None,
        })
    }

    pub fn from_letter(c: char) -> Option<Workspace> {
        Workspace::MENU.into_iter().find(|w| w.letter() == Some(c))
    }

    /// The digit that jumps straight here from another workspace.
    pub fn digit(self) -> Option<char> {
        let at = Workspace::MENU.iter().position(|w| *w == self)?;
        char::from_digit(at as u32 + 1, 10)
    }

    pub fn from_digit(c: char) -> Option<Workspace> {
        let n = c.to_digit(10)?;
        if n == 0 {
            return None;
        }
        Workspace::MENU.get(n as usize - 1).copied()
    }

    /// What the title bar calls it.
    pub fn title(self) -> &'static str {
        match self {
            Chat => "chat",
            Fleet => "fleet",
            Memory => "memory · list",
            MemoryGraph => "memory · local graph",
            Schedules => "schedules",
            Goals => "goals",
            Hooks => "webhooks",
            Tasks => "tasks",
            Activity => "activity",
            Team => "team",
        }
    }

    /// What the which-key menu calls it — one word, so the letter column reads
    /// as a column.
    pub fn menu_name(self) -> &'static str {
        match self {
            Chat => "chat",
            Fleet => "fleet",
            Memory | MemoryGraph => "memory",
            Schedules => "schedules",
            Goals => "goals",
            Hooks => "hooks",
            Tasks => "tasks",
            Activity => "activity",
            Team => "team",
        }
    }

    /// Where this workspace's list cursor, filter and sort live.
    pub fn slot(self) -> usize {
        Workspace::ALL.iter().position(|w| *w == self).unwrap_or(0)
    }

    /// True for every screen where letters are commands rather than text.
    pub fn is_list(self) -> bool {
        self != Chat
    }

    /// The sort orders `S` cycles through. The first is the default, and every
    /// screen has one so the key never does nothing.
    pub fn sorts(self) -> &'static [&'static str] {
        match self {
            Fleet => &["running first", "newest", "name", "spend"],
            Memory => &["degree", "confidence", "name", "age"],
            Schedules => &["next", "name", "last"],
            Goals => &["progress", "name", "next"],
            Hooks => &["deliveries", "name", "last"],
            Tasks => &["state", "name", "age"],
            Activity => &["newest", "unread first", "source"],
            Team => &["name", "status"],
            Chat | MemoryGraph => &["—"],
        }
    }

    /// The name of the sort currently in force.
    pub fn sort_name(self, at: usize) -> &'static str {
        let sorts = self.sorts();
        sorts[at % sorts.len()]
    }
}

/// One list's cursor, filter and sort order.
///
/// The selection is an **id**, never a row index: the fleet re-sorts under the
/// cursor every four ticks, and an index would silently move the cursor onto a
/// different run at the moment one finishes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListState {
    pub selected: Option<String>,
    /// `Some` once `/` has been pressed, even while still empty — an empty
    /// filter that is *open* still owns the keyboard, and `Esc` still has
    /// something to clear.
    pub filter: Option<String>,
    /// True while the `/` line is being typed into.
    pub editing_filter: bool,
    pub sort: usize,
}

impl ListState {
    /// Does this list have a filter that hides rows?
    pub fn filtering(&self) -> bool {
        self.filter.as_deref().is_some_and(|f| !f.is_empty())
    }

    /// Keep the cursor on a row that still exists, preferring the one it was
    /// on. Called after every refresh and every filter change.
    pub fn reconcile(&mut self, ids: &[String]) {
        if ids.is_empty() {
            self.selected = None;
            return;
        }
        let still_there = self
            .selected
            .as_deref()
            .is_some_and(|id| ids.iter().any(|candidate| candidate == id));
        if !still_there {
            self.selected = Some(ids[0].clone());
        }
    }

    /// Where the cursor is, as a row index into `ids`.
    pub fn index(&self, ids: &[String]) -> usize {
        self.selected
            .as_deref()
            .and_then(|id| ids.iter().position(|candidate| candidate == id))
            .unwrap_or(0)
    }

    /// Move by `delta`, clamped at both ends rather than wrapping: in a list
    /// that changes under you, overshooting lands somewhere unrelated.
    pub fn step(&mut self, delta: isize, ids: &[String]) {
        if ids.is_empty() {
            self.selected = None;
            return;
        }
        let at = self.index(ids) as isize;
        let landed = (at + delta).clamp(0, ids.len() as isize - 1) as usize;
        self.selected = Some(ids[landed].clone());
    }

    pub fn first(&mut self, ids: &[String]) {
        self.selected = ids.first().cloned();
    }

    pub fn last(&mut self, ids: &[String]) {
        self.selected = ids.last().cloned();
    }
}

/// Does `text` match what was typed into a `/` filter?
///
/// Case-insensitive subsequence, which is what "fuzzy" means to everyone who
/// has used one: `prsr` finds `port-the-parser` without anyone learning a
/// syntax. An empty needle matches everything, so an open-but-empty filter
/// hides nothing.
pub fn matches(needle: &str, text: &str) -> bool {
    let mut haystack = text.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| !c.is_whitespace())
        .all(|want| haystack.any(|have| have == want))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_workspace_has_a_letter_and_a_digit_of_its_own() {
        let mut letters: Vec<char> = Vec::new();
        let mut digits: Vec<char> = Vec::new();
        for w in Workspace::MENU {
            let letter = w.letter().expect("a menu row needs a letter");
            let digit = w.digit().expect("a menu row needs a digit");
            assert!(!letters.contains(&letter), "{letter} is claimed twice");
            assert!(!digits.contains(&digit), "{digit} is claimed twice");
            letters.push(letter);
            digits.push(digit);
        }
    }

    #[test]
    fn a_letter_and_its_digit_reach_the_same_workspace() {
        for w in Workspace::MENU {
            assert_eq!(Workspace::from_letter(w.letter().unwrap()), Some(w));
            assert_eq!(Workspace::from_digit(w.digit().unwrap()), Some(w));
        }
    }

    /// The graph is memory's second level, so it has no digit — a digit would
    /// promise you could land there without a node to look at.
    #[test]
    fn the_local_graph_is_not_directly_addressable() {
        assert_eq!(Workspace::MemoryGraph.letter(), None);
        assert_eq!(Workspace::MemoryGraph.digit(), None);
    }

    #[test]
    fn an_unknown_digit_reaches_nothing() {
        assert_eq!(Workspace::from_digit('0'), None);
        assert_eq!(Workspace::from_digit('x'), None);
    }

    #[test]
    fn every_workspace_has_a_slot_of_its_own() {
        let mut seen = Vec::new();
        for w in Workspace::ALL {
            assert!(!seen.contains(&w.slot()), "{w:?} shares a slot");
            seen.push(w.slot());
        }
    }

    /// `S` must never be a key that does nothing, so every screen declares at
    /// least one order.
    #[test]
    fn every_workspace_names_at_least_one_sort_order() {
        for w in Workspace::ALL {
            assert!(!w.sorts().is_empty(), "{w:?}");
            assert!(!w.sort_name(99).is_empty(), "{w:?} wraps rather than panics");
        }
    }

    #[test]
    fn a_filter_matches_a_subsequence_whatever_the_case() {
        assert!(matches("prsr", "port-the-parser"));
        assert!(matches("PORT", "port-the-parser"));
        assert!(matches("", "anything at all"), "an empty filter hides nothing");
        assert!(!matches("zzz", "port-the-parser"));
    }

    #[test]
    fn a_filter_ignores_the_spaces_typed_into_it() {
        assert!(matches("port parser", "port-the-parser"));
    }

    /// The fleet re-sorts under the cursor every four ticks. Tracking a row
    /// index would move the selection onto a different run the moment one
    /// finished.
    #[test]
    fn the_selection_follows_the_item_when_the_list_re_sorts() {
        let mut list = ListState::default();
        let before = ids(&["a", "b", "c"]);
        list.reconcile(&before);
        list.step(1, &before);
        assert_eq!(list.selected.as_deref(), Some("b"));

        let after = ids(&["c", "b", "a"]);
        list.reconcile(&after);
        assert_eq!(list.selected.as_deref(), Some("b"), "still on the same item");
        assert_eq!(list.index(&after), 1);
    }

    #[test]
    fn a_selection_that_disappeared_falls_back_to_the_top() {
        let mut list = ListState {
            selected: Some("gone".into()),
            ..Default::default()
        };
        list.reconcile(&ids(&["a", "b"]));
        assert_eq!(list.selected.as_deref(), Some("a"));
    }

    #[test]
    fn selecting_in_an_empty_list_selects_nothing() {
        let mut list = ListState::default();
        list.reconcile(&[]);
        assert_eq!(list.selected, None);
        list.step(1, &[]);
        assert_eq!(list.selected, None);
    }

    #[test]
    fn the_cursor_stops_at_both_ends_rather_than_wrapping() {
        let mut list = ListState::default();
        let rows = ids(&["a", "b", "c"]);
        list.reconcile(&rows);
        list.step(-1, &rows);
        assert_eq!(list.selected.as_deref(), Some("a"));
        list.step(9, &rows);
        assert_eq!(list.selected.as_deref(), Some("c"));
    }

    #[test]
    fn home_and_end_reach_the_ends_of_the_list() {
        let mut list = ListState::default();
        let rows = ids(&["a", "b", "c"]);
        list.last(&rows);
        assert_eq!(list.selected.as_deref(), Some("c"));
        list.first(&rows);
        assert_eq!(list.selected.as_deref(), Some("a"));
    }

    /// An open-but-empty filter owns the keyboard without hiding anything, so
    /// pressing `/` never makes the list appear to empty itself.
    #[test]
    fn an_open_but_empty_filter_hides_nothing() {
        let mut list = ListState {
            filter: Some(String::new()),
            ..Default::default()
        };
        assert!(!list.filtering());
        list.filter = Some("port".into());
        assert!(list.filtering());
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }
}
