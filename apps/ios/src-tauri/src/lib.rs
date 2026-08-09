//! The iOS shell.
//!
//! Deliberately empty of logic. `apps/desktop` is a Tauri shell that calls
//! `jod_core` in-process; this one **cannot be**, and that is the single fact
//! that shapes the whole app: an iPhone has no tmux, no Claude Code binary and
//! no shell to run them in, so there is nothing for `jod-core` to do here.
//!
//! Jod on this device is therefore a *client of the daemon*, not a copy of it.
//! Every capability comes over HTTP from `jod-api` on the box, which is exactly
//! the seam `docs/jod-system.md` planned for: the core has no UI, and clients
//! are interchangeable.
//!
//! So this crate exists only to put a WKWebView on screen and point it at the
//! built frontend. No commands are registered, because a command here would be
//! logic that the web client could not use and the desktop client would not
//! share.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running the Jod iOS shell");
}
