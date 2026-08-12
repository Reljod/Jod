//! The desktop's own `jod-api`, served on loopback with the HUD in front of it.
//!
//! The desktop used to reach `jod_core::Jod` through Tauri IPC commands. That
//! made it the one client with its own contract: the web app and the phone
//! spoke HTTP to `jod-api`, and the desktop spoke something else, so a change to
//! one had to be made twice and the desktop's copy silently rotted.
//!
//! So the desktop now runs the **real API in-process** and talks to it over
//! real HTTP. Same routes, same auth, same SSE — `packages/hud` cannot tell the
//! difference between this and a daemon on a VPS, which is the point.
//!
//! ## Why the HUD is served from the API's own origin
//!
//! `jod-api` sets no CORS headers and its session cookie is
//! `SameSite=Strict; Secure`. Both are deliberate: a credential for this API
//! spawns processes, so the daemon assumes same-origin and nothing else.
//! A webview loading bundled assets is `tauri://localhost` — a *different*
//! origin from `http://127.0.0.1:port`, so every request would be cross-origin.
//!
//! The fix is to remove the mismatch rather than weaken the API: this server
//! serves the built HUD too, so the window's origin *is* the API's origin.
//! Nothing about `jod-api` is relaxed for the desktop's benefit.
//!
//! ## Why a launch key, and why the token is not simply embedded
//!
//! The listener is on loopback, but loopback is not private — every process on
//! the machine can reach it. That is exactly why `jod-api` demands a token at
//! all, and serving one to whoever asks for `/` would hand any local process a
//! write credential.
//!
//! So `/` yields the token only when the request carries the launch key minted
//! for this run. The key never leaves the machine and is unguessable, and the
//! window is opened with it already in the URL. A local process that cannot
//! guess it gets a plain `404` from `/` and a `401` from every `/v1` route,
//! which is the same answer it would get from a daemon it has no token for.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use jod_api::audit::AuditLog;
use jod_api::auth::{generate_token, Scope, TokenStore};
use jod_api::config::Config;
use jod_api::AppState;
use jod_core::Jod;
use rust_embed::RustEmbed;
use serde::Deserialize;

/// The built HUD, compiled into the binary so a bundled app has no directory to
/// lose. Populated by `pnpm build` — see `tauri.conf.json`'s `beforeBuildCommand`.
#[derive(RustEmbed)]
#[folder = "../dist/"]
struct Assets;

/// Where the window should point. Carries the launch key, so it is a secret.
pub struct Link {
    pub entry: String,
    pub origin: String,
}

#[derive(Clone)]
struct Shell {
    /// Released to the HUD only against a correct launch key.
    api_token: Arc<str>,
    launch_key: Arc<str>,
}

#[derive(Deserialize)]
struct Entry {
    k: Option<String>,
}

/// Start the API and the asset server, and say where to point a window.
///
/// Binds port 0: the desktop has no business squatting a well-known port, and
/// a second copy of the app must not fail to start because the first one holds
/// one. The window is told the port, and nothing else needs to know it.
pub async fn start(jod: Arc<Jod>) -> Result<Link> {
    let config = Config {
        // Empty denies every spawn, which for the desktop would mean a UI whose
        // primary verb never works. The person at this keyboard already has a
        // shell on this machine, so gating them below their own privileges buys
        // nothing — this is not the remote-caller case that default guards.
        allowed_cwd: vec![home_dir()],
        ..Config::default()
    };

    let mut tokens = TokenStore::default();
    // Never written to disk. It dies with the process, so there is no desktop
    // credential left behind to leak or revoke.
    let api_token = tokens.issue("desktop", Scope::Write);
    let launch_key = generate_token();

    let audit = AuditLog::new(AuditLog::default_path());
    let state = AppState::new(jod, config, tokens, audit);

    let shell = Shell {
        api_token: api_token.into(),
        launch_key: launch_key.clone().into(),
    };

    let app = Router::new()
        .route("/", get(index))
        .with_state(shell)
        // The API owns `/v1`. Merged rather than nested so its routes match
        // exactly as they would in the standalone daemon.
        .merge(jod_api::router(state))
        // Only reached when nothing above matched, so this cannot shadow `/v1`.
        .fallback(asset);

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .context("binding the desktop API to loopback")?;
    let addr = listener.local_addr()?;

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("jod-desktop: the local API stopped: {e}");
        }
    });

    let origin = format!("http://127.0.0.1:{}", addr.port());
    Ok(Link {
        entry: format!("{origin}/?k={launch_key}"),
        origin,
    })
}

/// The HUD, with this run's API token injected — but only for a caller holding
/// the launch key.
///
/// A wrong or absent key is a plain `404`, identical to a path that does not
/// exist. Saying "wrong key" would confirm the endpoint is worth attacking.
async fn index(State(shell): State<Shell>, Query(entry): Query<Entry>) -> Response {
    let presented = entry.k.unwrap_or_default();
    // Length-independent comparison is not the concern here — the key is
    // compared once per window, not in a loop an attacker can time — but the
    // check must still be exact.
    if presented.is_empty() || presented != *shell.launch_key {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Some(html) = Assets::get("index.html") else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The HUD is not built. Run `pnpm build` in apps/desktop.",
        )
            .into_response();
    };

    let html = String::from_utf8_lossy(&html.data);
    // `bootstrap` is read once by the shell entry point and never stored, so
    // the token lives in a closure rather than anywhere a later script can find
    // it. Injected before anything else runs, so the HUD never renders unauthed.
    let bootstrap = format!(
        "<script>window.__JOD_BOOTSTRAP__={{token:{}}};</script>",
        serde_json::to_string(&*shell.api_token).unwrap_or_else(|_| "\"\"".into())
    );
    let injected = match html.split_once("</head>") {
        Some((head, rest)) => format!("{head}{bootstrap}</head>{rest}"),
        // No `</head>` means a template we do not recognise. Prepending still
        // runs before the app's module script, which is all that is required.
        None => format!("{bootstrap}{html}"),
    };

    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // The token is in this body. It must never sit in a disk cache.
            (header::CACHE_CONTROL, "no-store"),
        ],
        injected,
    )
        .into_response()
}

/// Everything else the built HUD needs. Plain public files — the credential is
/// not in any of them.
async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref())],
                Body::from(file.data.into_owned()),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Where an agent may be spawned. `$HOME` keeps the desktop's reach the same as
/// the user's own, and no wider.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(jod_core::service::default_cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Start a server on an in-memory Jod and hand back its entry URL.
    ///
    /// `Jod::new` rather than `persistent`: these tests are about the shell in
    /// front of the API, and touching the developer's real `~/.jod/jod.db` to
    /// assert a 404 would be indefensible.
    async fn serve() -> Link {
        start(Jod::new()).await.expect("server should start")
    }

    fn key_of(link: &Link) -> String {
        link.entry
            .split_once("?k=")
            .expect("entry carries the launch key")
            .1
            .to_string()
    }

    /// Pull the token back out the way the HUD's entry point does.
    fn token_in(html: &str) -> String {
        let start = html.find("token:").expect("token is injected") + "token:".len();
        let rest = &html[start..];
        let open = rest.find('"').expect("token is a JSON string") + 1;
        let close = rest[open..].find('"').expect("token is terminated") + open;
        rest[open..close].to_string()
    }

    #[tokio::test]
    async fn the_launch_key_is_required_to_get_the_token() {
        let link = serve().await;
        let http = reqwest::Client::new();

        // No key, and a wrong one, are both indistinguishable from a path that
        // does not exist. Any other answer tells a local process that this
        // endpoint is worth attacking.
        for url in [
            format!("{}/", link.origin),
            format!("{}/?k=", link.origin),
            format!("{}/?k=not-the-key", link.origin),
        ] {
            let res = http.get(&url).send().await.unwrap();
            assert_eq!(
                res.status(),
                reqwest::StatusCode::NOT_FOUND,
                "{url} should not reveal the token"
            );
        }

        let res = http.get(&link.entry).send().await.unwrap();
        assert!(res.status().is_success(), "the real launch key should open");
        let body = res.text().await.unwrap();
        assert!(
            body.contains("__JOD_BOOTSTRAP__"),
            "the HUD is served with its token injected"
        );
    }

    /// The token is a live credential, so a cache would be a credential on disk.
    #[tokio::test]
    async fn the_bootstrap_page_is_never_cached() {
        let link = serve().await;
        let res = reqwest::get(&link.entry).await.unwrap();
        assert_eq!(
            res.headers().get("cache-control").and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
    }

    /// The whole point of the rewrite: this is the real API, not a shim.
    #[tokio::test]
    async fn the_api_is_mounted_and_health_needs_no_credential() {
        let link = serve().await;
        let res = reqwest::get(format!("{}/v1/health", link.origin)).await.unwrap();
        assert!(res.status().is_success());
        assert_eq!(res.text().await.unwrap(), r#"{"status":"ok"}"#);
    }

    /// Loopback is not private. Another process on this machine reaching the
    /// port must be refused exactly as it would be by a daemon on a VPS.
    #[tokio::test]
    async fn an_unauthenticated_v1_request_is_refused() {
        let link = serve().await;
        let res = reqwest::get(format!("{}/v1/agents", link.origin)).await.unwrap();
        assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    /// End to end: take the token the way the browser does, then spend it.
    #[tokio::test]
    async fn the_injected_token_authenticates_against_the_api() {
        let link = serve().await;
        let html = reqwest::get(&link.entry).await.unwrap().text().await.unwrap();
        let token = token_in(&html);

        let res = reqwest::Client::new()
            .get(format!("{}/v1/agents", link.origin))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();

        assert!(
            res.status().is_success(),
            "the token the HUD is handed must work: {}",
            res.status()
        );
    }

    /// A read token could not spawn, and the desktop's primary verb is spawning.
    #[tokio::test]
    async fn the_desktop_token_carries_write_scope() {
        let link = serve().await;
        let html = reqwest::get(&link.entry).await.unwrap().text().await.unwrap();
        let token = token_in(&html);

        let res = reqwest::Client::new()
            .post(format!("{}/v1/session", link.origin))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();

        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["scope"], "write");
    }

    /// Two windows, or two copies of the app, must not fight over a port.
    #[tokio::test]
    async fn each_launch_gets_its_own_port_and_its_own_secrets() {
        let a = serve().await;
        let b = serve().await;
        assert_ne!(a.origin, b.origin, "port 0 means never squatting a port");
        assert_ne!(key_of(&a), key_of(&b), "a launch key is per-run");
    }

    /// The launch key opens the door; it is not itself an API credential.
    #[tokio::test]
    async fn the_launch_key_is_not_accepted_as_an_api_token() {
        let link = serve().await;
        let res = reqwest::Client::new()
            .get(format!("{}/v1/agents", link.origin))
            .bearer_auth(key_of(&link))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
}
