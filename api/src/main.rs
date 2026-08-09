//! `jod-api` — the daemon, and the tool that mints its credentials.
//!
//! Running this puts an endpoint that spawns agent harnesses on the machine.
//! That is remote code execution by design, which is why the daemon binds
//! loopback by default and every route but `/v1/health` needs a token.
//! → `docs/jod-api.md`

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use jod_api::audit::AuditLog;
use jod_api::auth::{Scope, TokenStore};
use jod_api::config::Config;
use jod_api::AppState;

#[derive(Parser)]
#[command(
    name = "jod-api",
    about = "The authenticated HTTP API over Jod, for mobile and web clients.",
    version
)]
struct Cli {
    /// Config file. Defaults to `$JOD_HOME/api.toml`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon.
    Serve {
        /// Override the configured bind address.
        #[arg(long)]
        bind: Option<String>,
        /// How many prior runs to reload from the store at boot.
        #[arg(long, default_value_t = 200)]
        rehydrate: usize,
    },
    /// Mint, list and revoke API tokens.
    #[command(subcommand)]
    Token(TokenCommand),
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Mint a token. Printed once — it is not recoverable afterwards.
    Issue {
        /// A name for the device this is for, so the audit log can identify it.
        label: String,
        #[arg(long, value_enum, default_value_t = ScopeArg::Read)]
        scope: ScopeArg,
    },
    /// List issued tokens. Shows labels and scopes, never secrets.
    List,
    /// Revoke every token with this label.
    Revoke { label: String },
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum ScopeArg {
    /// Watch agents. Cannot spawn or kill.
    Read,
    /// Everything, including spawning agents — which runs commands on this box.
    Write,
}

impl From<ScopeArg> for Scope {
    fn from(s: ScopeArg) -> Self {
        match s {
            ScopeArg::Read => Scope::Read,
            ScopeArg::Write => Scope::Write,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli
        .config
        .unwrap_or_else(|| jod_core::paths::jod_home().join("api.toml"));
    let token_path = jod_api::auth::default_token_path();

    match cli.command {
        Command::Token(cmd) => run_token_command(cmd, &token_path),
        Command::Serve { bind, rehydrate } => {
            serve(&config_path, &token_path, bind, rehydrate).await
        }
    }
}

fn run_token_command(cmd: TokenCommand, token_path: &Path) -> Result<()> {
    let mut store = TokenStore::load(token_path)
        .with_context(|| format!("reading {}", token_path.display()))?;

    match cmd {
        TokenCommand::Issue { label, scope } => {
            let scope: Scope = scope.into();
            let token = store.issue(&label, scope);
            store
                .save(token_path)
                .with_context(|| format!("writing {}", token_path.display()))?;
            println!("{token}");
            eprintln!();
            eprintln!("Label: {label}");
            eprintln!(
                "Scope: {}",
                if scope == Scope::Write {
                    "write"
                } else {
                    "read"
                }
            );
            eprintln!("Stored (hashed) in {}", token_path.display());
            eprintln!();
            eprintln!("This is the only time the token is shown. Store it in the");
            eprintln!("device's keychain, not in a file or a shell history.");
            if scope == Scope::Write {
                eprintln!();
                eprintln!("A write token can spawn agents, which run commands on this");
                eprintln!("machine. Treat it like an SSH key.");
            }
        }
        TokenCommand::List => {
            if store.tokens.is_empty() {
                println!("no tokens issued");
                return Ok(());
            }
            println!("{:<24}  {:<6}  ISSUED", "LABEL", "SCOPE");
            for t in &store.tokens {
                let issued = chrono::DateTime::from_timestamp_millis(t.created_at_ms)
                    .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "?".into());
                let scope = if t.scope == Scope::Write {
                    "write"
                } else {
                    "read"
                };
                println!("{:<24}  {:<6}  {}", t.label, scope, issued);
            }
        }
        TokenCommand::Revoke { label } => {
            let n = store.revoke(&label);
            store.save(token_path)?;
            println!("revoked {n} token(s) labelled {label}");
        }
    }
    Ok(())
}

async fn serve(
    config_path: &Path,
    token_path: &Path,
    bind_override: Option<String>,
    rehydrate: usize,
) -> Result<()> {
    let mut config =
        Config::load(config_path).with_context(|| format!("reading {}", config_path.display()))?;
    if let Some(bind) = bind_override {
        config.bind = bind;
    }
    let addr = config.socket_addr()?;

    let tokens = TokenStore::load(token_path)?;
    if tokens.tokens.is_empty() {
        eprintln!("jod-api: no tokens issued — every request will be refused.");
        eprintln!("         mint one with: jod-api token issue <label> --scope read");
    }

    // A public bind turns this into an RCE endpoint on the internet. Refusing
    // outright would break a legitimate reverse-proxy setup, so it is loud
    // rather than fatal.
    if !addr.ip().is_loopback() {
        eprintln!();
        eprintln!("jod-api: WARNING — binding {addr}, which is not loopback.");
        eprintln!("         This API spawns agents, so a reachable port is remote");
        eprintln!("         code execution. The supported setup is a loopback bind");
        eprintln!("         published to a tailnet with `tailscale serve`.");
        eprintln!();
    }

    if config.allowed_cwd.is_empty() {
        eprintln!("jod-api: allowed_cwd is empty — every spawn will be refused.");
        eprintln!(
            "         set allowed_cwd in {} or JOD_API_ALLOWED_CWD.",
            config_path.display()
        );
    }

    let jod = jod_core::Jod::persistent().context("opening the Jod store")?;
    match jod.rehydrate(rehydrate).await {
        Ok(n) if n > 0 => eprintln!("jod-api: reloaded {n} prior run(s)"),
        Ok(_) => {}
        Err(e) => eprintln!("jod-api: could not reload prior runs: {e}"),
    }
    if !jod.tmux_available() {
        eprintln!("jod-api: tmux is not installed — spawning will fail until it is.");
    }

    let audit = AuditLog::new(AuditLog::default_path());
    let state = AppState::new(jod, config, tokens, audit);
    let app = jod_api::router(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    eprintln!("jod-api: listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving")?;
    Ok(())
}

/// Stop accepting on Ctrl-C or SIGTERM, so systemd restarts are clean and
/// in-flight requests are not cut mid-response.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    eprintln!("jod-api: shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn a_read_scope_is_the_default_for_a_new_token() {
        // The safe default matters: a token minted without thinking should not
        // be able to spawn processes.
        let cli = Cli::try_parse_from(["jod-api", "token", "issue", "phone"]).unwrap();
        match cli.command {
            Command::Token(TokenCommand::Issue { scope, .. }) => {
                assert_eq!(Scope::from(scope), Scope::Read)
            }
            _ => panic!("expected token issue"),
        }
    }

    #[test]
    fn a_write_scope_must_be_asked_for_explicitly() {
        let cli = Cli::try_parse_from(["jod-api", "token", "issue", "laptop", "--scope", "write"])
            .unwrap();
        match cli.command {
            Command::Token(TokenCommand::Issue { scope, .. }) => {
                assert_eq!(Scope::from(scope), Scope::Write)
            }
            _ => panic!("expected token issue"),
        }
    }
}
