//! The roles panel — what each layer of the chain of command is spawned on.
//!
//! Six roles, and the shape they are drawn in is the *delegation edge* rather
//! than anything the `roles` table knows: `main` hands to `scratch` and to
//! `manager`, and `manager` hands to `engineer`. `assistant` and
//! `housekeeping` hang off the root because nothing delegates to them — Jod
//! starts the assistant itself when Reljod types into a chat that is already
//! working, and the titler and the compaction run start themselves.
//!
//! That shape lives here as a constant because it is a fact about the code, not
//! about the database. `Store::role_list` answers alphabetically and says so:
//! the panel walks its own tree and asks the store only for the values.
//!
//! ## Nothing here reaches the store
//!
//! Every key on this screen turns into an `Action` the loop carries out, the
//! same discipline the rest of the TUI keeps. What is in this file is the
//! shape, the choices each column offers, and the cursor over them — all of
//! which is testable without a database or a terminal.

use jod_core::harness::models::Model;
use jod_core::harness::{Effort, Role};
use jod_core::store::{RoleField, RoleRow};
use jod_core::{HarnessKind, PermissionPolicy};

/// What a column shows when nobody has set it.
///
/// An em dash rather than an empty cell, because a blank column reads as a
/// value that failed to load. This says *inherit*, which is a decision.
pub const INHERIT: &str = "—";

/// The four columns, in the order the key bar names them.
pub const FIELDS: [RoleField; 4] = [
    RoleField::Harness,
    RoleField::Model,
    RoleField::Thinking,
    RoleField::Permission,
];

/// One layer of the chain, and where it sits under the one that delegates to
/// it.
///
/// `branch` is the drawn prefix rather than something derived from `depth`,
/// because the tree is six fixed rows: computing `└` and `├` from a shape that
/// cannot change would be a small tree-drawing engine with exactly one input.
struct Layer {
    role: Role,
    depth: usize,
    branch: &'static str,
}

/// The chain of command, in the order the panel draws it.
///
/// `main` → {`scratch`, `manager`} → `engineer`, then `assistant` and
/// `housekeeping` at the root.
///
/// **The assistant moved out from under main, and the move is the whole of
/// what changed here.** For one release it sat between main and everything
/// else and made every routing decision, so it was genuinely a layer. It is not
/// one now: it reads a message Reljod typed into a busy chat and decides
/// whether that message can wait. Main does not hand anything to it, and it
/// hands nothing on, so drawing it under main would draw an edge that no longer
/// exists — which is exactly the mistake this constant exists to prevent.
const CHAIN: [Layer; 6] = [
    Layer {
        role: Role::Main,
        depth: 0,
        branch: "",
    },
    Layer {
        role: Role::Scratch,
        depth: 1,
        branch: "├ ",
    },
    Layer {
        role: Role::Manager,
        depth: 1,
        branch: "└ ",
    },
    Layer {
        role: Role::Engineer,
        depth: 2,
        branch: "  └ ",
    },
    Layer {
        role: Role::Assistant,
        depth: 0,
        branch: "",
    },
    Layer {
        role: Role::Housekeeping,
        depth: 0,
        branch: "",
    },
];

/// One row of the panel: a layer of the chain, with whatever has been said
/// about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub role: Role,
    /// How far under `main` this layer sits. `main` is 0 and `engineer` is 3.
    pub depth: usize,
    /// The `└`/`├` prefix drawn before the name.
    pub branch: &'static str,
    /// Whether anything at all is set here. False means every column inherits,
    /// which is the state of every role on a machine whose owner has never
    /// opened this screen.
    pub configured: bool,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub permission: Option<String>,
}

impl Row {
    /// What one column holds: what was configured, then what this layer runs on
    /// by default, then [`INHERIT`] when neither says anything.
    ///
    /// Showing the built-in rather than a dash is the honest answer to "what
    /// will this run on", which is the only question this screen is asked. A
    /// dash over a row that really starts on AGY would be a settings panel
    /// lying about the setting.
    pub fn cell(&self, field: RoleField) -> &str {
        self.value(field)
            .or_else(|| self.default_value(field))
            .unwrap_or(INHERIT)
    }

    /// What this layer runs on when nobody has configured it.
    ///
    /// Only the assistant has one — see [`Role::default_spawn`], where the
    /// reasoning lives. It is deliberately *not* a row in the `roles` table: an
    /// empty table still means "nothing is configured", so Reljod can always
    /// tell what he set from what Jod assumed.
    pub fn default_value(&self, field: RoleField) -> Option<&'static str> {
        let (harness, model) = self.role.default_spawn()?;
        match field {
            RoleField::Harness => Some(harness.id()),
            RoleField::Model => Some(model),
            RoleField::Thinking | RoleField::Permission => None,
        }
    }

    /// Whether this cell is showing a built-in rather than something set here.
    ///
    /// What the panel dims by. A value nobody chose has to read differently
    /// from one somebody did, or the screen cannot be used to answer "what have
    /// I actually changed".
    pub fn is_default(&self, field: RoleField) -> bool {
        self.value(field).is_none() && self.default_value(field).is_some()
    }

    /// What one column holds, as the store keeps it.
    pub fn value(&self, field: RoleField) -> Option<&str> {
        match field {
            RoleField::Harness => self.harness.as_deref(),
            RoleField::Model => self.model.as_deref(),
            RoleField::Thinking => self.thinking.as_deref(),
            RoleField::Permission => self.permission.as_deref(),
        }
    }

    /// The harness this row names, when it names one this build knows.
    ///
    /// `None` covers both "inherit" and a spelling from a newer build, and the
    /// two mean the same thing to every caller here: nothing is known about
    /// which harness will run this role, so only the levels *every* harness
    /// takes may be offered.
    pub fn harness_kind(&self) -> Option<HarnessKind> {
        // The effective harness, built-in included, because the one caller asks
        // in order to decide which effort levels to offer — and an assistant
        // row that says nothing still really starts on AGY.
        self.harness
            .as_deref()
            .and_then(HarnessKind::from_id)
            .or_else(|| self.role.default_harness())
    }
}

/// The six rows of the panel, with the store's answer joined onto the chain.
///
/// A role with no row in `roles` is drawn exactly like one whose columns are all
/// null, because they are the same thing — see [`jod_core::store::RoleRow`].
pub fn rows(configured: &[RoleRow]) -> Vec<Row> {
    CHAIN
        .iter()
        .map(|layer| {
            let stored = configured.iter().find(|r| r.role == layer.role.as_str());
            let set = stored.is_some_and(|r| {
                r.harness.is_some()
                    || r.model.is_some()
                    || r.thinking.is_some()
                    || r.permission.is_some()
            });
            Row {
                role: layer.role,
                depth: layer.depth,
                branch: layer.branch,
                configured: set,
                harness: stored.and_then(|r| r.harness.clone()),
                model: stored.and_then(|r| r.model.clone()),
                thinking: stored.and_then(|r| r.thinking.clone()),
                permission: stored.and_then(|r| r.permission.clone()),
            }
        })
        .collect()
}

/// One value a column will take, and what choosing it costs you.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// What to write. `None` clears the column back to inherit.
    pub value: Option<String>,
    pub label: String,
    pub what: String,
}

impl Choice {
    fn new(value: Option<&str>, label: impl Into<String>, what: impl Into<String>) -> Choice {
        Choice {
            value: value.map(str::to_string),
            label: label.into(),
            what: what.into(),
        }
    }
}

/// The list open over the panel, while one is.
///
/// Two stages rather than one, because `⏎` on a row has to ask *which* column
/// before it can ask what to put in it. `h`, `m`, `t` and `p` skip the first
/// stage, which is the whole reason they are printed on the key bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choosing {
    /// Which of the four columns to change.
    Field { role: Role, selected: usize },
    /// What to put in one column.
    Value {
        role: Role,
        field: RoleField,
        options: Vec<Choice>,
        selected: usize,
    },
}

impl Choosing {
    /// The role being edited, which is what the box says at the top.
    pub fn role(&self) -> Role {
        match self {
            Choosing::Field { role, .. } | Choosing::Value { role, .. } => *role,
        }
    }

    pub fn selected(&self) -> usize {
        match self {
            Choosing::Field { selected, .. } | Choosing::Value { selected, .. } => *selected,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Choosing::Field { .. } => FIELDS.len(),
            Choosing::Value { options, .. } => options.len(),
        }
    }

    /// Move by `delta`, wrapping, because a chooser is short enough to walk
    /// round — the same movement the completion popup makes.
    pub fn step(&mut self, delta: isize) {
        let len = self.len();
        if len == 0 {
            return;
        }
        let at = self.selected() as isize;
        let landed = (at + delta).rem_euclid(len as isize) as usize;
        match self {
            Choosing::Field { selected, .. } | Choosing::Value { selected, .. } => {
                *selected = landed
            }
        }
    }
}

/// Every value a column will take on this row, inherit first.
///
/// The lists are the ones the slash commands already offer, not copies of them:
/// harnesses come from [`HarnessKind::ALL`], models from whatever the harness
/// itself said it accepts — the list `/model` completes against — and permission
/// modes from [`PermissionPolicy::ALL`] with the same sentences `/mode` prints.
///
/// **Effort is filtered by the row's own harness.** `xhigh` and `max` exist on
/// Claude Code and nowhere else, so offering them on an AGY row would be
/// offering a setting that is silently dropped at spawn time. A row that names
/// no harness is offered only the levels every harness takes, because which one
/// will run it is not knowable from here.
pub fn options(field: RoleField, harness: Option<HarnessKind>, models: &[Model]) -> Vec<Choice> {
    let mut out = vec![Choice::new(
        None,
        INHERIT,
        "leave it to the caller, the conversation, and then the harness itself",
    )];
    match field {
        RoleField::Harness => out.extend(
            HarnessKind::ALL
                .into_iter()
                .map(|k| Choice::new(Some(k.id()), k.id(), k.label())),
        ),
        RoleField::Model => out.extend(
            models
                .iter()
                .map(|m| Choice::new(Some(&m.id), &m.id, &m.label)),
        ),
        RoleField::Thinking => out.extend(
            Effort::ALL
                .into_iter()
                .filter(|level| match harness {
                    Some(kind) => level.accepted_by(kind),
                    None => HarnessKind::ALL.iter().all(|k| level.accepted_by(*k)),
                })
                .map(|level| {
                    Choice::new(Some(level.as_str()), level.as_str(), effort_gloss(level))
                }),
        ),
        RoleField::Permission => out.extend(
            PermissionPolicy::ALL
                .into_iter()
                .map(|m| Choice::new(Some(m.label()), m.label(), super::command::mode_gloss(m))),
        ),
    }
    out
}

/// What a level actually asks the harness for.
fn effort_gloss(level: Effort) -> &'static str {
    match level {
        Effort::Low => "the least reasoning the model will do",
        Effort::Medium => "the middle of the range",
        Effort::High => "as much as the first three levels reach",
        Effort::XHigh => "Claude Code only",
        Effort::Max => "Claude Code only, and the top of its range",
    }
}

/// The sentence under the table.
///
/// Said on the screen rather than left to be discovered, because a settings
/// panel whose changes do not touch what you are looking at is one people
/// assume is broken. Nothing here reaches into a process that is already
/// running; the row is read at the next spawn.
pub const WHEN_IT_TAKES_EFFECT: &str =
    "a role decides what is spawned next — the runs already going are untouched";

/// The caveat that belongs to OpenCode, printed on a row that has earned it.
///
/// `--variant` is handed to whichever provider the model comes from, so the
/// words it accepts are that provider's rather than OpenCode's. Jod passes the
/// level through exactly as written and does not pretend to know which ones
/// will be taken.
pub const OPENCODE_VARIANT_CAVEAT: &str =
    "OpenCode passes this straight to the model's provider as --variant, so the \
     provider decides whether it is a word";

/// Whether that caveat applies to a row: OpenCode, with a level actually set.
pub fn variant_caveat_applies(row: &Row) -> bool {
    row.harness.as_deref().and_then(HarnessKind::from_id) == Some(HarnessKind::OpenCode)
        && row.thinking.is_some()
}

/// The line under the table when the selected row is showing a built-in.
///
/// A cell nobody set still shows a value on the assistant's row, and a value
/// with no explanation reads as a setting somebody made and forgot. This says
/// whose it is and how to be rid of it, which is the question a settings screen
/// owes an answer to.
pub fn default_note(row: &Row) -> Option<String> {
    let showing: Vec<&str> = FIELDS
        .into_iter()
        .filter(|f| row.is_default(*f))
        .filter_map(|f| row.default_value(f))
        .collect();
    if showing.is_empty() {
        return None;
    }
    Some(format!(
        "{} is what Jod starts this on unless you say otherwise — nothing is set here, \
         and choosing a value replaces it",
        showing.join(" on ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_named(rows: &[Row], role: Role) -> &Row {
        rows.iter()
            .find(|r| r.role == role)
            .expect("every role is drawn")
    }

    /// Check 30. All six layers, in the chain's own order, with `main` at the
    /// root and `engineer` three deep — which is the shape the panel exists to
    /// show and the one thing an alphabetical `role_list` cannot say.
    #[test]
    fn the_panel_lists_all_six_roles_with_main_at_the_root() {
        let drawn = rows(&[]);
        assert_eq!(drawn.len(), Role::ALL.len());
        let named: Vec<&str> = drawn.iter().map(|r| r.role.as_str()).collect();
        assert_eq!(
            named,
            [
                "main",
                "scratch",
                "manager",
                "engineer",
                "assistant",
                "housekeeping"
            ]
        );
        assert_eq!(row_named(&drawn, Role::Main).depth, 0);
        assert_eq!(row_named(&drawn, Role::Manager).depth, 1);
        assert_eq!(row_named(&drawn, Role::Scratch).depth, 1);
        assert_eq!(row_named(&drawn, Role::Engineer).depth, 2);
        for loose in [Role::Assistant, Role::Housekeeping] {
            assert_eq!(
                row_named(&drawn, loose).depth,
                0,
                "nothing delegates to {loose:?}, so it hangs off the root"
            );
        }
    }

    /// A column nobody has set reads as inheriting rather than as empty. An
    /// empty cell would look like a value that failed to load.
    #[test]
    fn a_role_row_showing_no_value_renders_a_dash() {
        let drawn = rows(&[]);
        let main = row_named(&drawn, Role::Main);
        assert!(!main.configured);
        for field in FIELDS {
            assert_eq!(main.cell(field), INHERIT, "{field:?}");
            assert_eq!(main.value(field), None, "{field:?}");
        }
    }

    #[test]
    fn a_role_with_a_row_shows_what_it_holds() {
        let drawn = rows(&[RoleRow {
            role: "scratch".into(),
            harness: Some("open_code".into()),
            model: Some("gpt-5".into()),
            thinking: None,
            permission: None,
        }]);
        let scratch = row_named(&drawn, Role::Scratch);
        assert!(scratch.configured);
        assert_eq!(scratch.cell(RoleField::Harness), "open_code");
        assert_eq!(scratch.cell(RoleField::Model), "gpt-5");
        assert_eq!(
            scratch.cell(RoleField::Thinking),
            INHERIT,
            "the columns it says nothing about still inherit"
        );
        assert_eq!(
            row_named(&drawn, Role::Main).cell(RoleField::Model),
            INHERIT,
            "and the other five rows are untouched"
        );
    }

    /// A row of nothing but nulls is the same thing as no row at all, so it must
    /// not draw as configured.
    #[test]
    fn a_row_of_nothing_but_nulls_is_the_same_as_no_row() {
        let drawn = rows(&[RoleRow {
            role: "main".into(),
            ..RoleRow::default()
        }]);
        assert!(!row_named(&drawn, Role::Main).configured);
    }

    /// `xhigh` and `max` are Claude Code's words and nobody else's. Offering
    /// them on an AGY row would be offering a setting that is dropped at spawn.
    #[test]
    fn xhigh_is_offered_on_a_claude_code_row_and_on_no_other() {
        let levels = |harness| -> Vec<String> {
            options(RoleField::Thinking, harness, &[])
                .into_iter()
                .filter_map(|c| c.value)
                .collect()
        };
        assert_eq!(
            levels(Some(HarnessKind::ClaudeCode)),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(levels(Some(HarnessKind::Agy)), ["low", "medium", "high"]);
        assert_eq!(
            levels(Some(HarnessKind::OpenCode)),
            ["low", "medium", "high", "xhigh", "max"],
            "OpenCode hands every word to the provider, so none of them is refused here"
        );
        assert_eq!(
            levels(None),
            ["low", "medium", "high"],
            "a row naming no harness is offered only what all three take"
        );
    }

    /// Every column can be cleared from the list itself, so "stop configuring
    /// this one thing" does not need the whole row reset.
    #[test]
    fn every_column_offers_inherit_first() {
        for field in FIELDS {
            let offered = options(field, Some(HarnessKind::ClaudeCode), &[]);
            assert_eq!(offered[0].value, None, "{field:?}");
            assert_eq!(offered[0].label, INHERIT, "{field:?}");
        }
    }

    /// The model column is the harness's own list rather than a second copy of
    /// one — the same names `/model` completes against, which for Claude Code
    /// is a constant this build carries and for the others is what the binary
    /// itself said.
    #[test]
    fn the_models_offered_are_the_ones_the_harness_named() {
        let models = HarnessKind::ClaudeCode.models();
        assert!(!models.is_empty(), "Claude Code's list is compiled in");
        let offered = options(RoleField::Model, Some(HarnessKind::ClaudeCode), &models);
        let ids: Vec<Option<String>> = offered.into_iter().map(|c| c.value).collect();
        assert_eq!(ids[0], None, "inherit is always first");
        assert_eq!(ids.len(), models.len() + 1);
        for model in &models {
            assert!(ids.contains(&Some(model.id.clone())), "{ids:?}");
        }
    }

    #[test]
    fn the_chooser_wraps_at_both_ends() {
        let mut choosing = Choosing::Field {
            role: Role::Main,
            selected: 0,
        };
        choosing.step(-1);
        assert_eq!(choosing.selected(), FIELDS.len() - 1);
        choosing.step(1);
        assert_eq!(choosing.selected(), 0);
    }

    /// The caveat is OpenCode's and only earns its line when a level is
    /// actually set — a row that inherits passes no flag at all.
    #[test]
    fn the_variant_caveat_belongs_to_an_opencode_row_that_set_a_level() {
        let with = |harness: &str, thinking: Option<&str>| {
            rows(&[RoleRow {
                role: "scratch".into(),
                harness: Some(harness.into()),
                thinking: thinking.map(str::to_string),
                ..RoleRow::default()
            }])
            .into_iter()
            .find(|r| r.role == Role::Scratch)
            .expect("scratch is drawn")
        };
        assert!(variant_caveat_applies(&with("open_code", Some("high"))));
        assert!(!variant_caveat_applies(&with("open_code", None)));
        assert!(!variant_caveat_applies(&with("claude_code", Some("high"))));
    }
}
