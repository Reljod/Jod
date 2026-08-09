//! # jod-core
//!
//! The part of Jod that has no user interface.
//!
//! Jod is an orchestrator: it never answers a prompt itself, it delegates to an
//! *agent harness* — a CLI like Claude Code or OpenCode that owns its own
//! context, tools and permissions. This crate is the whole of that job:
//!
//! - [`harness`] — the seam. One trait per harness: build a command line, parse
//!   its JSONL back into [`event::AgentEvent`]s.
//! - [`tmux`] — every agent runs in its own tmux session, so a human can attach
//!   and watch, or kill it, without going through Jod.
//! - [`runner`] — generates the launcher, follows the output, emits events.
//! - [`service::Jod`] — the facade every client drives. The Tauri desktop app is
//!   a thin shell over it; an iOS app or a VPS daemon would be another.
//!
//! Runtime state is plain files under `~/.jod` ([`paths`]), so a run stays
//! readable with `cat` long after the app is closed.

pub mod discovery;
pub mod error;
pub mod event;
pub mod harness;
pub mod paths;
pub mod runner;
pub mod service;
pub mod store;
pub mod tmux;

#[cfg(test)]
pub(crate) mod testsupport;

/// Re-exported so clients can name the subscription channel's types without
/// taking their own Tokio dependency.
pub use tokio::sync::broadcast;

pub use error::{JodError, Result};
pub use event::{AgentEnvelope, AgentEvent, Usage};
pub use harness::{Harness, HarnessKind, PermissionPolicy, Resume, SpawnRequest};
pub use service::{AgentStatus, AgentSummary, HarnessInfo, Jod, Report};

/// Tests that mutate process-wide environment variables must hold this, or they
/// will corrupt each other — Rust runs tests in parallel threads of one process.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
