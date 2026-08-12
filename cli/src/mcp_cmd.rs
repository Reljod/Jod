//! `jod mcp` — Jod's tools, over stdio, for a harness to use.
//!
//! Nothing here decides anything. It reads the bounds off the command line,
//! opens the database, and hands both to [`jod_core::mcp`]; the protocol and
//! every refusal live there, where they can be tested without a process.
//!
//! Two things this command must never do, because both would corrupt the
//! stream: print to stdout, and buffer a response. stdout *is* the protocol —
//! one JSON-RPC object per line — so every human-readable word goes to stderr.

use std::sync::Arc;

use anyhow::{Context, Result};
use jod_core::harness::{HarnessKind, ToolAccess};
use jod_core::mcp::{self, Server};
use jod_core::mcp_install;
use jod_core::{Jod, PermissionPolicy};

/// Run the server until the harness closes its end of the pipe.
///
/// `access` and `max_permission` are arguments rather than defaults read from a
/// file because the caller that knows them is whatever launched the harness:
/// the daemon spawning a scheduled run knows it should not be able to schedule
/// more, and a webhook-triggered run knows it was started by a stranger.
pub async fn run(jod: Arc<Jod>, access: ToolAccess, max_permission: PermissionPolicy) -> Result<()> {
    let identity = who_this_is(&jod);
    let server = Server::new(jod)
        .with_access(access)
        .with_max_permission(max_permission)
        .as_identity(identity);

    // Said on stderr before the first request, so a misconfigured launch is
    // visible in the harness's own log rather than only as tools that are
    // mysteriously absent. The run is named too: "this agent could not send a
    // message" is otherwise a silence with no cause anywhere in the log.
    eprintln!(
        "jod mcp · {} access · permission ceiling {} · {} tools · {}",
        access.as_str(),
        permission_id(max_permission),
        server.tools().len(),
        match server.identity() {
            mcp::Identity::Run(id) => format!("run {id}"),
            mcp::Identity::Unknown => "no run — this session cannot send messages".to_string(),
            mcp::Identity::Disputed { group, claimed } => format!(
                "DISPUTED — process group says {}, {} says `{claimed}`; messaging is refused",
                group.as_deref().unwrap_or("no run"),
                jod_core::mcp_config::RUN_ID_ENV
            ),
        }
    );

    mcp::serve(&server, std::io::stdin().lock(), std::io::stdout().lock())
        .await
        .context("serving MCP over stdio")
}

/// The spelling `parse_permission` reads back, from the one definition of it.
fn permission_id(p: PermissionPolicy) -> &'static str {
    p.as_str()
}

/// Which run this server is serving — and therefore who its tools speak as.
///
/// **Neither input here is an argument, and that is the whole point.** Sender
/// identity is the one thing an agent must not be able to choose: a `--run`
/// flag would let any agent that can read its own command line send as anybody
/// on its team. The authority is the process group, which the kernel will not
/// let a process change; the environment variable is enrichment and is never
/// preferred over it. [`jod_core::mcp::identify`] carries the full reasoning,
/// including why a disagreement between the two is refused rather than
/// resolved.
///
/// Without a store there is nothing to resolve against, and the honest answer
/// is that this server is nobody.
fn who_this_is(jod: &Jod) -> mcp::Identity {
    let Some(store) = jod.store() else {
        return mcp::Identity::Unknown;
    };
    let claimed = std::env::var(jod_core::mcp_config::RUN_ID_ENV).ok();
    mcp::identify(store, claimed.as_deref())
}

/// `jod mcp install` — register this binary with the harnesses on this machine.
///
/// Prints to stdout, unlike everything else in this file: this one *is* a
/// command to type, and the person typing it is being told which of their own
/// config files Jod just edited. Naming the path every time is the point —
/// silent edits to a file someone else owns are how a tool loses trust.
pub fn install(
    access: ToolAccess,
    harness: Option<HarnessKind>,
    all: bool,
    dry_run: bool,
) -> Result<()> {
    let home = jod_core::paths::jod_home();

    let results = match harness {
        // An explicit `--harness` is an instruction, so it skips the
        // is-it-installed filter: someone wiring a machine before installing
        // the harness on it has said what they want.
        Some(h) => vec![mcp_install::install(h, access, &home, dry_run)],
        None if all => HarnessKind::ALL
            .into_iter()
            .map(|h| mcp_install::install(h, access, &home, dry_run))
            .collect(),
        None => mcp_install::install_all(access, &home, dry_run),
    };

    if results.is_empty() {
        println!(
            "No harness found on this machine, so there was nothing to register.\n\
             Install Claude Code, OpenCode or AGY first, or name one with --harness."
        );
        return Ok(());
    }

    // A failure against one harness must not hide the successes against the
    // others: a broken OpenCode config is not a reason to leave Claude Code
    // unregistered. Every line is printed, and the command fails at the end if
    // any of them did.
    let mut failed = 0;
    for result in &results {
        match result {
            Ok(r) => println!(
                "{:<12} {:<14} {}",
                r.harness.label(),
                r.outcome.as_str(),
                r.path.display()
            ),
            Err(e) => {
                failed += 1;
                eprintln!("{e}");
            }
        }
    }

    let wrote = results
        .iter()
        .filter(|r| r.as_ref().is_ok_and(|r| r.outcome.wrote()))
        .count();
    if dry_run {
        println!("\nDry run — nothing was written.");
    } else if wrote > 0 {
        println!(
            "\nRegistered as `{}` with {access} access. Restart any open session to pick it up.",
            mcp_install::SERVER_NAME,
            access = access.as_str()
        );
    }

    if failed > 0 {
        anyhow::bail!(
            "{failed} of {} harnesses could not be registered",
            results.len()
        );
    }
    Ok(())
}
