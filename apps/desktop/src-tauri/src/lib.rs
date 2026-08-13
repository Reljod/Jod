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

/// Run `build` with Tauri's async runtime entered.
///
/// [`Jod::persistent`] spawns the event pump with `tokio::spawn`, so it panics
/// unless a runtime is in scope — and `setup` runs on the main thread inside
/// AppKit's `did_finish_launching`, an `extern "C"` frame a panic may not
/// unwind through. The panic therefore aborted the process before anything
/// could report it: every launch died as `SIGABRT`, with no window and no
/// message. Entering the runtime for the call is what makes it legal; the
/// service then lives on the same runtime the rest of the shell uses.
fn in_runtime<T>(build: impl FnOnce() -> T) -> T {
    let handle = tauri::async_runtime::handle();
    let _entered = handle.inner().enter();
    build()
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // `persistent` opens the same SQLite file the CLI and the daemon
            // use. The desktop is another window onto one Jod, not its own.
            let jod = in_runtime(Jod::persistent)?;
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

#[cfg(test)]
mod tests {
    use super::in_runtime;

    /// Deliberately a plain `#[test]`, not `#[tokio::test]`: the bug was that
    /// `setup` builds the service from a thread with no runtime of its own,
    /// which is exactly what a bare test thread is. Without [`in_runtime`]
    /// this panics with "there is no reactor running".
    ///
    /// `Jod::new` rather than `Jod::persistent` — same `build` underneath, and
    /// the test has no business opening the developer's `~/.jod/jod.db`.
    #[test]
    fn builds_the_service_off_a_thread_with_no_runtime() {
        // Returning at all is the assertion. The event pump is spawned during
        // the call, so reaching the next line means it found a runtime to be
        // spawned onto.
        let jod = in_runtime(jod_core::Jod::new);
        assert_eq!(std::sync::Arc::strong_count(&jod), 1);
    }
}
