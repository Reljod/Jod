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

// The whole TUI is compiled in, and this example calls the six loaders and the
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
// And again for dictation: the TUI resolves which engine would transcribe
// before it starts recording, so `crate::voice` has to exist here even though
// nothing this example renders will ever press Alt-V.
#[path = "../src/voice.rs"]
mod voice;

use std::path::PathBuf;
use std::sync::Arc;

use jod_core::store::Store;
use jod_core::{paths, HarnessKind, Jod, Resume};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use tui::{App, Workspace};

/// What the TUI names a delegated run, mirrored from `src/main.rs`.
///
/// The module compiled in above calls `crate::default_name` when it spawns an
/// agent. This example never spawns one, but the call has to resolve.
fn default_name(prompt: &str) -> String {
    let words: Vec<&str> = prompt.split_whitespace().take(5).collect();
    let name = words.join(" ");
    if name.is_empty() {
        "agent".to_string()
    } else if name.chars().count() > 48 {
        format!("{}…", name.chars().take(47).collect::<String>())
    } else {
        name
    }
}

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
    let mut app = App::new(HarnessKind::ClaudeCode, None, Resume::Fresh);
    app.now_ms = chrono::Utc::now().timestamp_millis();
    app.list_mut(Workspace::Memory).filter = filter.clone();

    // The same calls `tui::refresh_workspaces` makes on the tick.
    app.memory = tui::data::memory(&jod);
    app.graph_size = tui::data::graph_size(&jod);
    app.schedules = tui::data::schedules(&jod);
    app.goals = tui::data::goals(&jod);
    app.hooks = tui::data::hooks(&jod);
    app.activity = tui::data::activity(&jod);
    app.board = tui::data::tasks(&jod, None);
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

    println!("{}", path.display());
    println!(
        "{} memory nodes · {} schedules · {} goals · {} hooks · {} activity · {} tasks",
        app.memory.len(),
        app.schedules.len(),
        app.goals.len(),
        app.hooks.len(),
        app.activity.len(),
        app.board.len()
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

    Ok(())
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
