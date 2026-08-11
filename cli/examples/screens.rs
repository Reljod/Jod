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
struct Handed {
    agent: jod_core::service::AgentSummary,
    compaction_due: Option<(&'static str, usize)>,
}

async fn hand_to_orchestrator(
    _jod: &Jod,
    _instruction: &str,
    _kind: HarnessKind,
    _cwd: PathBuf,
) -> anyhow::Result<Handed> {
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
