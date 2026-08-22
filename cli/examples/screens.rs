//! `cargo run --example screens` — every workspace, off a real database, as text.
//!
//! The TUI's own tests render fixtures. This renders `~/.jod/jod.db`, through
//! the same loaders the running program uses, and prints the result — so
//! "the screens show what is actually in the store" is something a person can
//! check rather than something a commit message claims. It needs no terminal:
//! ratatui's `TestBackend` is a buffer, so this works over ssh, in CI, and in a
//! pipe.
//!
//! ```text
//! cargo run --example screens                          # $JOD_HOME/jod.db, or ~/.jod/jod.db
//! cargo run --example screens -- /tmp/seed.db          # any other database
//! cargo run --example screens -- /tmp/seed.db prefers  # with the memory filter typed
//! ```
//!
//! The second argument types into the memory screen's `/` filter, which a
//! freshly built app otherwise never has.
//!
//! `jod-cli` is a binary crate, so an example cannot `use` it — the TUI module
//! is compiled in by path instead, which is why the crate root below has to
//! supply the one function that module reaches for.

// The whole TUI is compiled in, and this example calls its loaders and its
// renderer. Everything else in it — the event loop, the key handling, the
// terminal setup — is unreachable from here by design, and warning about that
// would bury the one warning that would matter.
#![allow(dead_code, unused_imports)]

#[path = "../src/tui/mod.rs"]
mod tui;
// Compiled in for the same reason: the TUI's `/update` reaches
// `crate::update`, and the module has to resolve even though nothing this
// example renders will ever call it. The real one, not a stub — it is pure
// code over paths and a subprocess, so compiling it here costs nothing and a
// stub would be one more thing that can drift.
#[path = "../src/update.rs"]
mod update;
// `/upgrade` is the other half of that, reaching `crate::upgrade`, and needs
// to resolve here for exactly the same reason.
#[path = "../src/upgrade.rs"]
mod upgrade;
// And again for dictation: the TUI resolves which engine would transcribe
// before it starts recording, so `crate::voice` has to exist here even though
// nothing this example renders will ever press Ctrl-V.
#[path = "../src/voice.rs"]
mod voice;

use std::path::PathBuf;
use std::sync::Arc;

use jod_core::store::Store;
use jod_core::{paths, HarnessKind, Jod, Resume};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use tui::{App, Workspace};

/// What the TUI names a delegated run.
///
/// The module compiled in above calls `crate::default_name` when it spawns an
/// agent. This example never spawns one, but the call has to resolve — and it
/// now resolves to the real function rather than a copy of it, for the reason
/// the stub below already gives: a local copy is a second thing to keep in step.
use jod_core::harness::default_name;

/// The other symbol the compiled-in module reaches for in the crate root.
///
/// A stub, because this example renders screens and never runs the event loop
/// that would reach it — the same reason `default_name` above is a copy rather
/// than a call. It has to *exist* and it must never be *used*, so it says so
/// rather than quietly returning something plausible.
///
/// The return type is the real one rather than a look-alike: a local copy is a
/// second thing to keep in step, and it drifted the moment the real signature
/// grew a fifth argument.
async fn hand_to_orchestrator(
    _jod: &Jod,
    _instruction: &str,
    _kind: HarnessKind,
    _cwd: PathBuf,
    _carried: Option<String>,
    _run_name: &str,
    _permission: jod_core::PermissionPolicy,
) -> anyhow::Result<jod_core::orchestrator::Handed> {
    anyhow::bail!("the screens example does not run agents")
}

/// The size the screens were designed against: every column of every table is
/// present at 100 wide, and the drop order only starts biting below it.
const WIDTH: u16 = 100;
const HEIGHT: u16 = 30;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(paths::db_path);
    if !path.exists() {
        eprintln!(
            "no database at {} — set JOD_HOME, or pass one as an argument",
            path.display()
        );
        std::process::exit(1);
    }

    let filter = std::env::args().nth(2);

    let jod = Jod::with_store(Arc::new(Store::open(&path)?));
    let mut app = load(&jod, filter).await?;

    println!("{}", path.display());
    println!(
        "{} memory nodes · {} schedules · {} goals · {} hooks · {} activity · {} tasks · \
         {} runs · {} tree nodes",
        app.memory.len(),
        app.schedules.len(),
        app.goals.len(),
        app.hooks.len(),
        app.activity.len(),
        app.board.len(),
        app.agents.len(),
        app.forest.len()
    );

    for workspace in Workspace::MENU {
        app.go(workspace);
        println!();
        println!("── {} {}", workspace.title(), "─".repeat(60));
        println!("{}", render(&app));
    }

    // The traffic log, which has no digit and so is not in the menu above: it
    // is the fleet's second level, reached with `T` on a row. Rendered for the
    // first work that has one, because a work with an empty bus renders the
    // empty state and says nothing about whether the loader works.
    if let Some(store) = jod.store() {
        let busiest = store
            .works(jod_core::works::Filter::All)
            .unwrap_or_default()
            .into_iter()
            .map(|w| {
                let used = store
                    .messages_used(jod_core::team::Scope::Work, &w.id)
                    .unwrap_or_default();
                (used, w.id)
            })
            .max_by_key(|(used, _)| *used);
        match busiest.filter(|(used, _)| *used > 0) {
            Some((_, id)) => {
                app.traffic_of = Some(tui::traffic::Watching::work(&id));
                app.traffic = tui::data::traffic_from(&store, app.traffic_of.as_ref().unwrap());
                app.go(Workspace::Traffic);
                println!();
                println!("── {} {}", Workspace::Traffic.title(), "─".repeat(52));
                println!("{}", render(&app));
            }
            None => {
                println!();
                println!("── fleet · traffic ── no work on this database has any traffic yet");
            }
        }
    }

    // The fleet with a repository opened, which is the screen the tree exists
    // for: a plain render shows every project shut, and shut projects say
    // nothing about whether the roster inside them is right.
    if let Some(project) = app
        .forest
        .iter()
        .find(|n| n.kind == jod_core::tree::NodeKind::Project && n.has_children)
        .map(|n| n.id.clone())
    {
        app.go(Workspace::Fleet);
        app.tree.selected = Some(project);
        let (forest, closed) = (app.forest.clone(), app.closed_works.clone());
        app.tree.expand_or_descend(&forest, &closed);
        println!();
        println!("── fleet · a project opened {}", "─".repeat(46));
        println!("{}", render(&app));
    }

    // The fleet with the cursor walked down into the loose pane, which is the
    // half of that screen a workspace render never shows: the tree's cursor
    // starts in the tree, so a plain render of the fleet cannot say whether the
    // pane below it can be reached at all.
    if !app.loose_rows().is_empty() {
        app.go(Workspace::Fleet);
        let rows = app.tree_rows();
        app.tree.last(&rows);
        println!();
        println!("── fleet · cursor in the loose pane {}", "─".repeat(38));
        println!("{}", render(&app));
        app.tree.first(&rows);
    }

    // The session list, which is an overlay rather than a workspace and so is
    // not in the menu above. Worth its own screen for the same reason the
    // traffic log is: it is the only one whose rows are *conversations*, and
    // the nine above list runs, memories, schedules and tasks. If this comes
    // out empty on a database with a chat history, the loader is wrong.
    if let Some(store) = jod.store() {
        app.go(Workspace::Chat);
        app.overlay = tui::Overlay::Sessions(tui::sessions::Browser {
            rows: tui::sessions::session_rows(store.as_ref(), tui::sessions::LIST_LIMIT),
            loaded: true,
            ..Default::default()
        });
        println!();
        println!("── every conversation {}", "─".repeat(52));
        println!("{}", render(&app));
        app.overlay = tui::Overlay::None;
    }

    // The leader menu, which now has a row that is not a workspace: the
    // session list has no digit and no screen of its own, so the menu is the
    // only place it is named.
    app.go(Workspace::Chat);
    app.overlay = tui::Overlay::WhichKey;
    println!();
    println!("── the leader menu {}", "─".repeat(55));
    println!("{}", render(&app));
    app.overlay = tui::Overlay::None;

    // A chat mid-turn, with the side panel open.
    //
    // Worth a screen of its own because it is the only state in which three
    // things appear at once — the transcript, the panel Shift-Tab opens, and
    // the context box that says when to compact — and none of them are visible
    // on the empty chat above. A reference that only ever shows the resting
    // state is a reference that cannot be checked against the interesting one.
    app.go(Workspace::Chat);
    app.panel = true;
    app.busy = true;
    app.turn_started_ms = Some(app.now_ms - 42_000);
    app.push(tui::Entry::You("what is on my plate this week?".into()));
    app.push(tui::Entry::Agent(
        "Three things are live: the PR sweep, the Linear backlog triage, and \
         the monitor you armed on the deploy log."
            .into(),
    ));
    // Past the threshold, so the recommendation is on screen rather than
    // merely reachable.
    app.context_tokens = 164_000;
    println!();
    println!("── {} {}", "chat · working, panel open", "─".repeat(45));
    println!("{}", render(&app));

    // ...and the same panel with the catalog holding the keyboard, which is the
    // one state where the keybar is neither the screen's nor the rail's. It is
    // its own screen for the reason the one above is: nothing else here shows a
    // cursor inside the panel, and a reference that never draws the focused
    // state cannot be checked against it.
    app.busy = false;
    app.focus_catalog();
    println!();
    println!("── {} {}", "chat · the projects have the keyboard", "─".repeat(34));
    println!("{}", render(&app));

    Ok(())
}

/// Every screen's state, read off one database the way the console reads it.
///
/// A function rather than a block inside `main`, because the claim this whole
/// example makes — that a screen shows what is in the store — is only worth
/// anything if something can check it, and nothing can call a block inside
/// `main`. The test at the bottom of this file calls this.
///
/// The fleet needs three things nothing else here needs, which is why it was
/// the one screen that rendered empty whatever database it was pointed at:
///
///  * `rehydrate` loads the `runs` table into the process. `Jod::agents` reads
///    memory, not SQLite, so a freshly built `Jod` knows about no run at all —
///    `jod tui` calls this before it opens the console, and this has to too.
///  * `App::agents` is filled by `tui::list_agents`, which is a separate call
///    from `refresh_workspaces` in the real program and so was easy to miss
///    when the loaders here were copied from it.
///  * `App::forest` is the tree, and `App::closed_works` says which of its
///    works are archives.
async fn load(jod: &Arc<Jod>, filter: Option<String>) -> anyhow::Result<App> {
    let mut app = App::new(HarnessKind::ClaudeCode, None, Resume::Fresh);
    app.now_ms = chrono::Utc::now().timestamp_millis();
    app.list_mut(Workspace::Memory).filter = filter;

    // What `jod tui` does before it draws its first frame. Without it the
    // fleet's flat list is empty on a database full of runs, because a run
    // reaches `Jod::agents` only by being read back out of the store here.
    // The limit is the console's own.
    jod.rehydrate(200).await?;
    app.agents = tui::list_agents(jod).await;

    // The same calls `tui::refresh_workspaces` makes on the tick.
    app.memory = tui::data::memory(jod);
    app.graph_size = tui::data::graph_size(jod);
    app.schedules = tui::data::schedules(jod);
    app.goals = tui::data::goals(jod);
    app.hooks = tui::data::hooks(jod);
    app.activity = tui::data::activity(jod);
    app.board = tui::data::tasks(jod, None, app.now_ms);
    // The catalog the side panel draws. Missed when these loaders were copied
    // out of `refresh_workspaces`, which is why every panel this example has
    // ever printed said `none yet — /project add <path>` on a database with a
    // full catalog in it — the one claim the example exists to make, failing on
    // the one box a reader would check it against.
    app.projects = tui::data::projects(jod);
    let tree = tui::data::forest(jod, app.tree.show_closed);
    app.forest = tree.nodes;
    app.closed_works = tree.closed;
    app.work_of = tree.works;
    app.tree_runs = tree.runs;
    app.run_of = tree.run_of;
    // The tree's cursor is an id, so it has to be put back on a row that
    // exists before anything is drawn — the same two lines, in the same order,
    // that `refresh_workspaces` runs.
    let rows = app.tree_rows();
    app.tree.reconcile(&rows);
    if let Some(store) = jod.store() {
        app.team = store.teams().unwrap_or_default().first().cloned();
        if let Some(team) = &app.team {
            app.members = store.team_members(team).unwrap_or_default();
            app.tasks = store.team_tasks(team).unwrap_or_default();
        }
    }
    // Puts a cursor on the first row of every list, which is what makes the
    // detail panes render something rather than "nothing selected".
    app.reconcile();
    Ok(app)
}

/// One screen, as the characters that would have reached the terminal.
fn render(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test backend");
    terminal
        .draw(|f| {
            tui::ui::draw(f, app);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does this example render the database it was handed, or a picture of nothing?
///
/// The one claim it makes is in its first line — every workspace, off a real
/// database — and until this test there was nothing holding it to that. The
/// fleet screen quietly failed it: `load` above never filled `App::agents` or
/// `App::forest`, so the screen came out empty on every database, including
/// ones with a work and a run in them. An empty screen is the same thing this
/// example prints when the database really is empty, so the failure looked
/// exactly like an answer, and someone reading it would have concluded there
/// was no fleet rather than that the example had not looked.
///
/// So the assertion is not "the fleet screen renders". It is that the fleet
/// screen off a seeded database is *different* from the fleet screen off an
/// empty one, and that the difference is the rows that were seeded.
///
/// Run with `cargo test -p jod-cli --example screens`, which works because
/// `cli/Cargo.toml` sets `test = true` on this example — cargo does not run
/// tests inside an example otherwise.
#[cfg(test)]
mod tests {
    use super::*;

    use jod_core::conversation::{NewMessage, Role};
    use jod_core::service::watch_command;
    use jod_core::store::StoredRun;
    use jod_core::works::Origin;
    use jod_core::{AgentStatus, AgentSummary, PermissionPolicy, Usage};

    const WORK: &str = "port the parser";
    const SESSION: &str = "port the lexer";
    const RUN: &str = "hello-agent";
    const RUN_ID: &str = "de1e6a7e";

    /// A database holding the three rows the fleet screen is a picture of: a
    /// work, a session under it, and a run that wrote into that session.
    ///
    /// Seeded through the real store API and read back through the real
    /// loaders, like the fleet test in `cli/src/tui/ui.rs` — a hand-made
    /// `App::forest` would prove the renderer works and say nothing about
    /// whether this example ever asks the database anything.
    fn seeded() -> Arc<Jod> {
        let store = Store::in_memory().expect("an in-memory store");
        let work = store.create_work(WORK).expect("a work");
        let session = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
            .expect("a conversation")
            .id;
        store
            .set_conversation_title(&session, SESSION)
            .expect("a session title");
        store
            .attach_conversation(&session, &work.id, None, Origin::Agent)
            .expect("a session under the work");

        // Written whole rather than as `{}`. `rehydrate` rebuilds each row into
        // an `AgentSummary` and skips any run whose summary will not
        // deserialise, so a stub here would leave the fleet empty for a reason
        // that has nothing to do with what is being tested.
        let summary = AgentSummary {
            id: RUN_ID.into(),
            name: RUN.into(),
            harness: HarnessKind::ClaudeCode,
            harness_label: "Claude Code".into(),
            status: AgentStatus::Completed,
            cwd: "/tmp".into(),
            model: None,
            permission: PermissionPolicy::Ask,
            // A finished run, so nothing probes a process group and the screen
            // is the same on every box.
            pid: None,
            pgid: None,
            process_alive: false,
            watch_command: watch_command(RUN_ID),
            created_at_ms: 1,
            session_id: None,
            usage: Usage::default(),
            event_count: 0,
            last_message: None,
        };
        store
            .save_run(&StoredRun {
                id: summary.id.clone(),
                name: summary.name.clone(),
                harness: summary.harness.id().to_string(),
                status: "completed".into(),
                cwd: summary.cwd.clone(),
                session_id: None,
                pid: None,
                pgid: None,
                created_at_ms: summary.created_at_ms,
                summary: serde_json::to_value(&summary).expect("a serialisable summary"),
            })
            .expect("a run");
        // `messages.run_id` is the only join between a run and the session it
        // belongs to, so the tree has no node for a run that never wrote
        // anything.
        store
            .append_message(
                &session,
                NewMessage::new(Role::Assistant, "the lexer builds").from_run(RUN_ID),
            )
            .expect("a message");
        Jod::with_store(Arc::new(store))
    }

    /// The fleet screen, built exactly as `main` builds it.
    async fn fleet_screen(jod: &Arc<Jod>) -> String {
        let mut app = load(jod, None).await.expect("the example's own loader");
        app.go(Workspace::Fleet);
        render(&app)
    }

    /// The run is not named here, and that is the fold rather than a gap: the
    /// tree draws agents, and a run is folded onto the agent that took it — its
    /// status, its stall, and the last thing it said all ride up onto that row.
    #[tokio::test]
    async fn the_fleet_screen_shows_the_work_and_the_agent_in_the_database() {
        let screen = fleet_screen(&seeded()).await;
        assert!(
            screen.contains(WORK),
            "the work has no project, so it is the root of this tree:\n{screen}"
        );
        assert!(
            screen.contains(SESSION),
            "the agent under it is what the tree draws:\n{screen}"
        );
    }

    /// The other half, and the half that makes the first one mean something.
    ///
    /// A screen that says the same thing about a full database and an empty one
    /// is not reporting on the database at all.
    #[tokio::test]
    async fn the_fleet_screen_is_not_the_same_on_an_empty_database() {
        let populated = fleet_screen(&seeded()).await;
        let empty = Jod::with_store(Arc::new(Store::in_memory().expect("an in-memory store")));
        let blank = fleet_screen(&empty).await;

        assert!(
            !blank.contains(WORK),
            "nothing was seeded here, so nothing should be named:\n{blank}"
        );
        assert_ne!(
            populated, blank,
            "the fleet screen renders the same thing whatever the database holds, \
             which is what this example was doing before it loaded the fleet at all"
        );
    }
}
