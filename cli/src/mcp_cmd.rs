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
    let server = Server::new(jod)
        .with_access(access)
        .with_max_permission(max_permission);

    // Said on stderr before the first request, so a misconfigured launch is
    // visible in the harness's own log rather than only as tools that are
    // mysteriously absent.
    eprintln!(
        "jod mcp · {} access · permission ceiling {} · {} tools",
        access.as_str(),
        permission_id(max_permission),
        server.tools().len()
    );

    mcp::serve(&server, std::io::stdin().lock(), std::io::stdout().lock())
        .await
        .context("serving MCP over stdio")
}

/// The spelling `parse_permission` reads back, from the one definition of it.
fn permission_id(p: PermissionPolicy) -> &'static str {
    p.as_str()
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
