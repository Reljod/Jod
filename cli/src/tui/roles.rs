//! The roles panel — what each layer of the chain of command is spawned on.
//!
//! Six roles, and the shape they are drawn in is the *delegation edge* rather
//! than anything the `roles` table knows: `main` hands to `assistant`, which
//! hands to `scratch` and to `manager`, which hands to `engineer`.
//! `housekeeping` hangs off the root because nothing delegates to it — the
//! titler and the compaction run start themselves.
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
/// `main` → `assistant` → {`scratch`, `manager`} → `engineer`, then
/// `housekeeping` at the root.
const CHAIN: [Layer; 6] = [
    Layer {
        role: Role::Main,
        depth: 0,
        branch: "",
    },
    Layer {
        role: Role::Assistant,
        depth: 1,
        branch: "└ ",
    },
    Layer {
        role: Role::Scratch,
        depth: 2,
        branch: "  ├ ",
    },
    Layer {
        role: Role::Manager,
        depth: 2,
        branch: "  └ ",
    },
    Layer {
        role: Role::Engineer,
        depth: 3,
        branch: "    └ ",
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
    /// What one column holds, or [`INHERIT`] when nobody has said.
    pub fn cell(&self, field: RoleField) -> &str {
        self.value(field).unwrap_or(INHERIT)
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
        self.harness.as_deref().and_then(HarnessKind::from_id)
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
        /// Where the model column's options came from, kept beside them so the
        /// screen and the list cannot disagree about whose names are on it.
        /// Ignored by the other three columns.
        models: Models,
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

/// The model list a row may be offered, and why there is none when there is
/// none.
///
/// A model id belongs to exactly one harness. Claude Code calls Opus
/// `claude-opus-5`, OpenCode calls it `opencode/claude-opus-5` and AGY calls it
/// `claude-opus-4-6-thinking`, so a list borrowed from another harness is a
/// list of names that fail the run rather than a rough guide. That is why this
/// is an enum and not a slice: the caller cannot hand [`options`] a list
/// without also saying whose names they are, which is the mistake this panel
/// shipped with — it offered the console session's models on every row,
/// whatever harness the row named.
///
/// A harness that has not answered and a harness that could not answer are kept
/// apart because the person is owed different sentences: one is worth waiting a
/// second for, and the other never resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Models {
    /// The row names this harness, and this is what it said it accepts.
    Named {
        kind: HarnessKind,
        models: Vec<Model>,
    },
    /// The row names no harness, so it will run on whatever the caller, the
    /// conversation or the console hands it. These are the console's own
    /// harness's names, offered as the best available guess and labelled as
    /// one — see [`super::App::role_models`].
    Inherited {
        kind: HarnessKind,
        models: Vec<Model>,
    },
    /// The row names a harness that has not answered yet. Asking OpenCode costs
    /// a subprocess and asking AGY costs a network round trip, so this is what
    /// the first second on the panel looks like.
    Waiting(HarnessKind),
    /// The row names a harness that was asked and could not answer — no binary
    /// on the machine, a failed subcommand, a timeout. Nothing is offered,
    /// because a list of somebody else's names is worse than no list.
    Unreadable(HarnessKind),
}

impl Models {
    /// The names to offer, which is nothing at all unless a harness produced
    /// them.
    pub fn ids(&self) -> &[Model] {
        match self {
            Models::Named { models, .. } | Models::Inherited { models, .. } => models,
            Models::Waiting(_) | Models::Unreadable(_) => &[],
        }
    }

    /// The line drawn under the list, saying whose names these are or why there
    /// are none.
    ///
    /// Always said, even when the list is the row's own harness's: the bug this
    /// panel had was invisible precisely because nothing on the screen said
    /// which harness the names came from.
    pub fn note(&self) -> String {
        match self {
            Models::Named { kind, .. } => format!(
                "these are {}'s own names, which is what this row runs on",
                kind.label()
            ),
            Models::Inherited { kind, models } if models.is_empty() => format!(
                "this row inherits its harness, and {} — what this console is on — \
                 has not said what it accepts",
                kind.label()
            ),
            Models::Inherited { kind, .. } => format!(
                "this row inherits its harness, so these are {}'s names — what this \
                 console is on. Set the harness column to be offered that harness's own",
                kind.label()
            ),
            Models::Waiting(kind) => format!(
                "asking {} what it accepts — press m again in a moment",
                kind.label()
            ),
            Models::Unreadable(kind) => format!(
                "{} could not be asked what it accepts, so there is nothing to offer \
                 here rather than another harness's names",
                kind.label()
            ),
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
/// **Both the model list and the effort list belong to the row's own harness.**
/// `xhigh` and `max` exist on Claude Code and nowhere else, so offering them on
/// an AGY row would be offering a setting that is silently dropped at spawn
/// time; a model id is worse, because it is not dropped — it is handed to
/// `--model` and fails the whole run. A row that names no harness is offered
/// only the levels every harness takes, because which one will run it is not
/// knowable from here.
pub fn options(field: RoleField, harness: Option<HarnessKind>, models: &Models) -> Vec<Choice> {
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
                .ids()
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
    row.harness_kind() == Some(HarnessKind::OpenCode) && row.thinking.is_some()
}

/// Whether one column holds something the row's own harness will not take.
///
/// A row is edited one column at a time, so it is easy to leave behind: set the
/// harness to `agy` on a row whose model column still says `claude-opus-5` and
/// the row now describes a run that cannot start. Nothing clears the other
/// columns for you — a settings screen that quietly deletes what you typed is
/// worse than one that leaves it there — so the panel marks the cell instead
/// and [`objections`] says what will happen to it.
///
/// Only ever true when the harness is actually known to refuse the value. A row
/// naming no harness is judged by nobody, and a model list that could not be
/// read convicts no name, for the reason
/// [`jod_core::harness::models::accepts`] gives: not being in a list nobody
/// could read is not a fact about the name.
pub fn column_refused(row: &Row, field: RoleField, models: &Models) -> bool {
    let Some(kind) = row.harness_kind() else {
        return false;
    };
    match field {
        RoleField::Model => {
            let Some(name) = row.model.as_deref() else {
                return false;
            };
            match models {
                Models::Named { kind: whose, models } if *whose == kind && !models.is_empty() => {
                    !jod_core::harness::models::accepts(name, models)
                }
                _ => false,
            }
        }
        RoleField::Thinking => match row.thinking.as_deref().and_then(Effort::parse) {
            Some(level) => !level.accepted_by(kind),
            None => false,
        },
        RoleField::Harness | RoleField::Permission => false,
    }
}

/// What is wrong with this row, in sentences, given what its harness accepts.
///
/// Empty for the row every machine starts with, and for every row that holds
/// only values its harness has. Each sentence names the harness, the value it
/// will not take, and what happens at spawn — because both of these settings
/// fail quietly otherwise. The effort level is dropped with a line on stderr
/// nobody is reading, and the model id is handed straight to `--model`, where
/// OpenCode answers `UnknownError: Unexpected server error` and AGY fails the
/// turn.
pub fn objections(row: &Row, models: &Models) -> Vec<String> {
    let mut said = Vec::new();
    let Some(kind) = row.harness_kind() else {
        return said;
    };
    if column_refused(row, RoleField::Model, models) {
        let name = row.model.as_deref().unwrap_or_default();
        let harness = kind.label();
        // One sentence a line, and short ones. The panel draws these inside a
        // box and does not wrap, so a sentence wide enough to be cut loses its
        // end — which here is the half that says what to do about it.
        said.push(format!(
            "{harness} has no model called {name}, so a run from this row would fail."
        ));
        match jod_core::harness::models::nearest(name, models.ids()).as_slice() {
            [] => said.push(format!("Press m for the models {harness} does have.")),
            [only] => said.push(format!("{harness} calls that one {only} — press m to pick it.")),
            near => said.push(format!("The closest {harness} has are {}.", near.join(", "))),
        }
    }
    if column_refused(row, RoleField::Thinking, models) {
        let level = row.thinking.as_deref().unwrap_or_default();
        said.push(format!(
            "{} has no word for `{level}`, so it is dropped at spawn — press t.",
            kind.label()
        ));
    }
    said
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_named(rows: &[Row], role: Role) -> &Row {
        rows.iter()
            .find(|r| r.role == role)
            .expect("every role is drawn")
    }

    /// For the three columns that are not the model column, where the model
    /// list is never read.
    fn unused() -> Models {
        Models::Waiting(HarnessKind::Agy)
    }

    /// A harness's own list, the way [`super::App::role_models`] hands one over.
    fn named(kind: HarnessKind, models: Vec<Model>) -> Models {
        Models::Named { kind, models }
    }

    /// The rows `agy models` prints, cut down to what these tests need.
    fn agy_list() -> Vec<Model> {
        jod_core::harness::models::parse(
            HarnessKind::Agy,
            "claude-opus-4-6-thinking\tClaude Opus 4.6 (Thinking)\n\
             gemini-3.6-flash-high\tGemini 3.6 Flash (High)\n",
        )
    }

    /// One role's row, configured however the test needs it.
    fn configured(harness: Option<&str>, model: Option<&str>, thinking: Option<&str>) -> Row {
        let drawn = rows(&[RoleRow {
            role: "scratch".into(),
            harness: harness.map(str::to_string),
            model: model.map(str::to_string),
            thinking: thinking.map(str::to_string),
            permission: None,
        }]);
        row_named(&drawn, Role::Scratch).clone()
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
                "assistant",
                "scratch",
                "manager",
                "engineer",
                "housekeeping"
            ]
        );
        assert_eq!(row_named(&drawn, Role::Main).depth, 0);
        assert_eq!(row_named(&drawn, Role::Assistant).depth, 1);
        assert_eq!(row_named(&drawn, Role::Manager).depth, 2);
        assert_eq!(row_named(&drawn, Role::Scratch).depth, 2);
        assert_eq!(row_named(&drawn, Role::Engineer).depth, 3);
        assert_eq!(
            row_named(&drawn, Role::Housekeeping).depth,
            0,
            "nothing delegates to housekeeping, so it hangs off the root"
        );
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
            options(RoleField::Thinking, harness, &unused())
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
            let offered = options(field, Some(HarnessKind::ClaudeCode), &unused());
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
        let offered = options(
            RoleField::Model,
            Some(HarnessKind::ClaudeCode),
            &named(HarnessKind::ClaudeCode, models.clone()),
        );
        let ids: Vec<Option<String>> = offered.into_iter().map(|c| c.value).collect();
        assert_eq!(ids[0], None, "inherit is always first");
        assert_eq!(ids.len(), models.len() + 1);
        for model in &models {
            assert!(ids.contains(&Some(model.id.clone())), "{ids:?}");
        }
    }

    /// The bug this panel was reported with. Reljod set a row's harness to
    /// `agy` and the model list still offered `opus`, `claude-opus-5` and the
    /// rest of Claude Code's names, because the list came from the console
    /// session rather than from the row. Every one of those names fails an AGY
    /// run: AGY spells that model `claude-opus-4-6-thinking`.
    #[test]
    fn an_agy_row_is_offered_agys_names_and_none_of_claude_codes() {
        let offered: Vec<String> = options(
            RoleField::Model,
            Some(HarnessKind::Agy),
            &named(HarnessKind::Agy, agy_list()),
        )
        .into_iter()
        .filter_map(|c| c.value)
        .collect();

        assert_eq!(
            offered,
            ["claude-opus-4-6-thinking", "gemini-3.6-flash-high"],
            "the row's own harness named these"
        );
        for claude in HarnessKind::ClaudeCode.models() {
            assert!(
                !offered.contains(&claude.id),
                "{} is Claude Code's name and this row runs on AGY: {offered:?}",
                claude.id
            );
        }
    }

    /// A harness that has not answered yet, and one that could not answer,
    /// offer nothing rather than somebody else's names — and each says which of
    /// the two happened, because one is worth waiting for and the other is not.
    #[test]
    fn a_harness_that_gave_no_list_offers_inherit_and_says_why() {
        for models in [
            Models::Waiting(HarnessKind::Agy),
            Models::Unreadable(HarnessKind::Agy),
        ] {
            let offered = options(RoleField::Model, Some(HarnessKind::Agy), &models);
            assert_eq!(offered.len(), 1, "{models:?}");
            assert_eq!(offered[0].value, None, "{models:?}");
        }
        assert!(Models::Waiting(HarnessKind::Agy).note().contains("asking"));
        assert!(Models::Unreadable(HarnessKind::Agy)
            .note()
            .contains("could not be asked"));
    }

    /// A row that names no harness runs on whatever the caller hands it, so no
    /// list can be exactly right. The console's own is the best guess — it is
    /// what a run started from this console inherits — and the note says so
    /// rather than passing the names off as the row's own.
    #[test]
    fn a_row_naming_no_harness_is_offered_the_consoles_list_and_told_so() {
        let models = Models::Inherited {
            kind: HarnessKind::OpenCode,
            models: jod_core::harness::models::parse(
                HarnessKind::OpenCode,
                "opencode/claude-opus-5\n",
            ),
        };
        let offered: Vec<String> = options(RoleField::Model, None, &models)
            .into_iter()
            .filter_map(|c| c.value)
            .collect();
        assert_eq!(offered, ["opencode/claude-opus-5"]);
        let note = models.note();
        assert!(note.contains("inherits its harness"), "{note}");
        assert!(note.contains("OpenCode"), "{note}");
    }

    /// Whose names these are is said even when they are the right ones. The
    /// bug was invisible because nothing on the screen named the harness the
    /// list came from.
    #[test]
    fn the_list_always_says_which_harness_named_it() {
        let note = named(HarnessKind::Agy, agy_list()).note();
        assert!(note.contains("AGY"), "{note}");
    }

    /// Editing one column at a time leaves rows that cannot run: the harness
    /// goes to `agy` and the model column still holds a name only Claude Code
    /// has. Nothing clears it — deleting what somebody typed is worse — so the
    /// panel marks the cell and says what will happen to it.
    #[test]
    fn a_model_the_rows_harness_does_not_have_is_marked_and_explained() {
        let row = configured(Some("agy"), Some("claude-opus-5"), None);
        let models = named(HarnessKind::Agy, agy_list());
        assert!(column_refused(&row, RoleField::Model, &models));

        let said = objections(&row, &models);
        assert!(
            said[0].contains("AGY has no model called claude-opus-5"),
            "{said:?}"
        );
        assert!(said[1].contains("Press m"), "{said:?}");

        // And where there is an id the name was plainly reaching for, that id
        // is what the second line carries — the rule `/model` already follows,
        // because being told a name is wrong without being told the right one
        // is a dead end.
        let near = configured(Some("agy"), Some("claude-opus-4-6"), None);
        let said = objections(&near, &models);
        assert!(
            said[1].contains("AGY calls that one claude-opus-4-6-thinking"),
            "{said:?}"
        );
    }

    /// The same for the thinking column, which fails more quietly still: the
    /// level is dropped at spawn with a line on stderr nobody reads, and the
    /// panel goes on showing `xhigh` as though it were in force.
    #[test]
    fn a_level_the_rows_harness_cannot_spell_is_marked_and_explained() {
        let row = configured(Some("agy"), None, Some("xhigh"));
        let said = objections(&row, &Models::Waiting(HarnessKind::Agy));
        assert!(column_refused(&row, RoleField::Thinking, &unused()));
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("no word for `xhigh`"), "{said:?}");
        assert!(said[0].contains("dropped at spawn"), "{said:?}");
    }

    /// Nothing is convicted on a list that could not be read, and nothing at
    /// all is said about a row whose harness is not known — which is every row
    /// on a machine nobody has configured.
    #[test]
    fn a_row_nobody_can_judge_draws_no_objection() {
        let unreadable = Models::Unreadable(HarnessKind::Agy);
        let row = configured(Some("agy"), Some("claude-opus-5"), None);
        assert!(!column_refused(&row, RoleField::Model, &unreadable));
        assert!(objections(&row, &unreadable).is_empty());

        let inherits = configured(None, Some("claude-opus-5"), Some("xhigh"));
        assert!(objections(
            &inherits,
            &Models::Inherited {
                kind: HarnessKind::Agy,
                models: agy_list(),
            }
        )
        .is_empty());
        assert!(objections(&rows(&[])[0], &unused()).is_empty());
    }

    /// A row holding what its harness actually has is silent. An objection on
    /// a correct row would be noise on the one screen that has to be trusted.
    #[test]
    fn a_coherent_row_draws_no_objection() {
        let row = configured(Some("agy"), Some("gemini-3.6-flash-high"), Some("high"));
        assert!(objections(&row, &named(HarnessKind::Agy, agy_list())).is_empty());
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
            }])[2]
                .clone()
        };
        assert!(variant_caveat_applies(&with("open_code", Some("high"))));
        assert!(!variant_caveat_applies(&with("open_code", None)));
        assert!(!variant_caveat_applies(&with("claude_code", Some("high"))));
    }
}
