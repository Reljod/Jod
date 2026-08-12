//! Tauri shell for Jod.
//!
//! Now genuinely thin. There are no `#[tauri::command]`s left: the window is
//! pointed at a local `jod-api` and everything the HUD needs travels over HTTP,
//! the same way the web app and the phone reach a daemon on the VPS.
//!
//! What this file does is start that server and open a window at it.
//! → [`server`] for why it is shaped that way.

mod server;

use std::sync::Arc;

use jod_core::Jod;
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// How many past runs to pull back out of the store at launch.
///
/// The HUD opens on a graph, and a graph with nothing in it says the wrong
/// thing — "you have never delegated anything" rather than "this window just
/// opened". Rehydrating means the fleet is already drawn on the first frame.
const REHYDRATE: usize = 200;

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // `persistent` opens the same SQLite file the CLI and the daemon
            // use. The desktop is another window onto one Jod, not its own.
            let jod = Jod::persistent()?;
            let handle = app.handle().clone();

            tauri::async_runtime::spawn(async move {
                if let Err(e) = jod.rehydrate(REHYDRATE).await {
                    // Not fatal: an empty graph is worse than a stale one, but
                    // both beat refusing to open.
                    eprintln!("jod-desktop: could not reload past runs: {e}");
                }
                if let Err(e) = open(handle, jod).await {
                    eprintln!("jod-desktop: {e:#}");
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Start the local API, then point a window at it.
///
/// The window is created here rather than in `tauri.conf.json` because its URL
/// is not known until the listener has a port — and it carries the launch key,
/// which only exists at runtime.
async fn open(handle: tauri::AppHandle, jod: Arc<Jod>) -> anyhow::Result<()> {
    let link = server::start(jod).await?;
    eprintln!("jod-desktop: API on {}", link.origin);

    let url = link.entry.parse()?;
    WebviewWindowBuilder::new(&handle, "main", WebviewUrl::External(url))
        .title("Jod")
        .inner_size(1280.0, 840.0)
        .min_inner_size(900.0, 600.0)
        .build()?;

    Ok(())
}
