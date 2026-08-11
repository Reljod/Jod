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
use jod_core::harness::ToolAccess;
use jod_core::mcp::{self, Server};
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
