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
//! - [`proc`] — detached process groups: how a run outlives the process that
//!   started it, and how any process can check on one or stop it.
//! - [`runner`] — writes the launch plan, starts the supervisor, and follows a
//!   run's events out of the store.
//! - [`service::Jod`] — the facade every client drives. The Tauri desktop app is
//!   a thin shell over it; an iOS app or a VPS daemon would be another.
//!
//! A run's transcript lives in one SQLite file ([`store`]); what is left on disk
//! under `~/.jod` ([`paths`]) is the record of the launch. Nothing about a run
//! is held only in the memory of the process that started it, which is why a
//! restarted daemon — or a phone — can pick it straight back up.

pub mod activity;
pub mod approvals;
pub mod cards;
pub mod commands;
pub mod consolidate;
pub mod conversation;
pub mod daemon;
pub mod delivery;
pub mod discovery;
pub mod leases;
pub mod projects;
pub mod prs;
pub mod rank;
pub mod redact;
pub mod roots;
pub mod secrets;
pub mod tree;
pub mod works;
pub mod error;
pub mod event;
pub mod harness;
pub mod heartbeat;
pub mod ledger;
pub mod mcp;
pub mod mcp_config;
pub mod mcp_install;
pub mod monitor;
pub mod orchestrator;
pub mod paths;
pub mod proc;
pub mod recall;
pub mod runner;
pub mod schedule;
pub mod service;
pub mod telegram;
pub mod ticker;
pub mod webhook;
pub mod workdir;
pub mod store;
pub mod team;

/// Re-exported so clients can name the subscription channel's types without
/// taking their own Tokio dependency.
pub use tokio::sync::broadcast;

pub use error::{JodError, Result};
pub use event::{AgentEnvelope, AgentEvent, Usage};
pub use harness::{Harness, HarnessKind, Model, PermissionPolicy, Resume, SpawnRequest};
pub use service::{AgentStatus, AgentSummary, HarnessInfo, Jod, Report};

/// Tests that mutate process-wide environment variables must hold this, or they
/// will corrupt each other — Rust runs tests in parallel threads of one process.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
