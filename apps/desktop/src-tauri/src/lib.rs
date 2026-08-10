//! Tauri shell for Jod.
//!
//! Deliberately thin: every command here is a one-liner over [`jod_core::Jod`].
//! Anything with real logic belongs in the core crate, so the iOS client and a
//! headless VPS daemon can reuse it without touching this file.

use std::path::PathBuf;
use std::sync::Arc;

use jod_core::service::{AgentSummary, HarnessInfo, Report};
use jod_core::{AgentEnvelope, HarnessKind, Jod, PermissionPolicy, SpawnRequest};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

/// Event name the frontend listens on for live agent activity.
const AGENT_EVENT: &str = "jod://agent-event";

struct AppState {
    jod: Arc<Jod>,
}

/// What the machine can currently do — rendered as a banner when something is
/// missing, so a failed spawn is never a mystery.
#[derive(Serialize)]
struct SystemStatus {
    harnesses: Vec<HarnessInfo>,
    supervisor_available: bool,
    default_workdir: String,
}

#[derive(Deserialize)]
struct SpawnArgs {
    name: String,
    harness: HarnessKind,
    prompt: String,
    cwd: Option<String>,
    model: Option<String>,
    permission: Option<PermissionPolicy>,
}

/// Tauri commands must return `Result<_, String>`; core errors carry their own
/// human-readable text, so this is the whole conversion.
fn to_msg(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
fn system_status(state: tauri::State<'_, AppState>) -> SystemStatus {
    SystemStatus {
        harnesses: state.jod.harnesses(),
        supervisor_available: state.jod.supervisor_available(),
        default_workdir: jod_core::service::default_cwd().to_string_lossy().to_string(),
    }
}

#[tauri::command]
async fn spawn_agent(
    state: tauri::State<'_, AppState>,
    args: SpawnArgs,
) -> Result<AgentSummary, String> {
    let cwd = args
        .cwd
        .filter(|c| !c.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(jod_core::service::default_cwd);

    if !cwd.is_dir() {
        return Err(format!("working directory does not exist: {}", cwd.display()));
    }
    if args.prompt.trim().is_empty() {
        return Err("prompt is empty".into());
    }

    state
        .jod
        .spawn_agent(SpawnRequest {
            name: if args.name.trim().is_empty() {
                "agent".into()
            } else {
                args.name
            },
            harness: args.harness,
            prompt: args.prompt,
            cwd,
            model: args.model.filter(|m| !m.trim().is_empty()),
            permission: args.permission.unwrap_or_default(),
        })
        .await
        .map_err(to_msg)
}

#[tauri::command]
async fn list_agents(state: tauri::State<'_, AppState>) -> Result<Vec<AgentSummary>, String> {
    Ok(state.jod.agents().await)
}

#[tauri::command]
async fn agent_events(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Vec<AgentEnvelope>, String> {
    state.jod.events(&id).await.map_err(to_msg)
}

#[tauri::command]
async fn kill_agent(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
    state.jod.kill_agent(&id).await.map_err(to_msg)
}

#[tauri::command]
async fn report(state: tauri::State<'_, AppState>) -> Result<Report, String> {
    Ok(state.jod.report().await)
}

/// Open a real terminal already following the agent.
///
/// `jod watch` reads the run out of the database, so unlike `tmux attach` this
/// works for a finished run too — it replays the transcript rather than
/// refusing. There is no session left behind to be "closed", so there is
/// nothing to check before opening the window.
#[tauri::command]
async fn open_in_terminal(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
    let agent = state.jod.agent(&id).await.map_err(to_msg)?;
    let command = agent.watch_command;

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("osascript")
            .args(["-e", &terminal_script(&command)])
            .status()
            .map_err(to_msg)?;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(format!(
            "opening a terminal is macOS-only for now — run `{command}` yourself"
        ))
    }
}

/// AppleScript that opens `command` in the user's terminal.
///
/// Prefers iTerm2 when it is installed, because someone who has it does not
/// want Terminal.app windows appearing instead.
#[cfg(target_os = "macos")]
fn terminal_script(command: &str) -> String {
    // AppleScript string literals escape with backslashes. `jod watch <id>`
    // carries no quotes today, but escaping is kept — and tested directly —
    // so a command that grows one does not silently break the script.
    let escaped = command.replace('\\', r"\\").replace('"', "\\\"");

    if std::path::Path::new("/Applications/iTerm.app").exists() {
        format!(
            r#"tell application "iTerm"
  activate
  set w to (create window with default profile)
  tell current session of w to write text "{escaped}"
end tell"#
        )
    } else {
        format!(
            r#"tell application "Terminal"
  activate
  do script "{escaped}"
end tell"#
        )
    }
}

/// The wire contract between `src/api.ts` and this file.
///
/// The frontend is plain TypeScript with hand-written types, so nothing else
/// catches a rename. These tests fail the moment the shapes drift apart.
#[cfg(test)]
mod contract {
    use super::*;
    use jod_core::event::{AgentEnvelope, AgentEvent, Usage};

    #[test]
    fn a_spawn_payload_from_the_ui_deserializes() {
        // Exactly what SpawnForm sends through `invoke("spawn_agent", { args })`.
        let json = serde_json::json!({
            "name": "scout",
            "harness": "claude_code",
            "prompt": "summarise this repo",
            "cwd": "/Users/x/code",
            "model": "claude-haiku-4-5",
            "permission": "accept_edits"
        });
        let args: SpawnArgs = serde_json::from_value(json).expect("UI payload must parse");
        assert_eq!(args.harness, HarnessKind::ClaudeCode);
        assert_eq!(args.permission, Some(PermissionPolicy::AcceptEdits));
    }

    #[test]
    fn the_optional_fields_may_be_omitted_entirely() {
        let json = serde_json::json!({
            "name": "scout",
            "harness": "open_code",
            "prompt": "hi",
            "cwd": null,
            "model": null,
            "permission": null
        });
        let args: SpawnArgs = serde_json::from_value(json).expect("nulls must parse");
        assert_eq!(args.harness, HarnessKind::OpenCode);
        assert!(args.cwd.is_none() && args.model.is_none() && args.permission.is_none());
    }

    #[test]
    fn an_envelope_serializes_flat_with_a_kind_discriminant() {
        // types.ts models AgentEnvelope as `AgentEvent & { agent_id, at_ms, seq }`.
        let envelope = AgentEnvelope {
            agent_id: "a1".into(),
            at_ms: 1700000000000,
            seq: 3,
            event: AgentEvent::Message { text: "PONG".into() },
        };
        let v = serde_json::to_value(&envelope).unwrap();
        assert_eq!(v["agent_id"], "a1");
        assert_eq!(v["seq"], 3);
        assert_eq!(v["kind"], "message");
        assert_eq!(v["text"], "PONG");
        assert!(v.get("event").is_none(), "the event must be flattened, not nested");
    }

    #[test]
    fn every_event_variant_uses_the_kind_tag_the_ui_switches_on() {
        let usage = Usage { output_tokens: Some(1), ..Default::default() };
        let cases = [
            (AgentEvent::Started { session_id: None, model: None }, "started"),
            (AgentEvent::Thinking { text: "t".into() }, "thinking"),
            (AgentEvent::Message { text: "m".into() }, "message"),
            (AgentEvent::ToolCall { name: "Bash".into(), input: None }, "tool_call"),
            (
                AgentEvent::ToolResult { name: "Bash".into(), summary: None, is_error: false },
                "tool_result",
            ),
            (
                AgentEvent::Finished { text: None, exit_code: Some(0), is_error: false, usage },
                "finished",
            ),
            (AgentEvent::Raw { line: "x".into() }, "raw"),
            (AgentEvent::Error { message: "e".into() }, "error"),
        ];
        for (event, expected) in cases {
            let v = serde_json::to_value(&event).unwrap();
            assert_eq!(v["kind"], expected, "unexpected tag for {event:?}");
        }
    }

    /// A command embedded in an AppleScript string literal must have its double
    /// quotes escaped, or they terminate the literal and the script fails to
    /// compile. `jod watch <id>` has no quotes today, so the escaping is tested
    /// directly rather than through it — otherwise the guard would quietly stop
    /// guarding anything.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_terminal_script_escapes_quotes_in_the_command() {
        let script = terminal_script(&jod_core::service::watch_command("a1"));
        assert!(script.contains("jod watch a1"), "{script}");

        let script = terminal_script(r#"echo "hi""#);
        assert!(
            script.contains("\\\"hi\\\""),
            "quotes must be escaped:\n{script}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_terminal_script_targets_an_installed_terminal() {
        let script = terminal_script("echo hi");
        let iterm = std::path::Path::new("/Applications/iTerm.app").exists();
        if iterm {
            assert!(script.contains(r#"tell application "iTerm""#), "{script}");
        } else {
            assert!(script.contains(r#"tell application "Terminal""#), "{script}");
        }
    }

    #[test]
    fn status_and_harness_ids_match_the_typescript_unions() {
        for (kind, id) in [
            (HarnessKind::ClaudeCode, "claude_code"),
            (HarnessKind::OpenCode, "open_code"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), id);
        }
        for (policy, id) in [
            (PermissionPolicy::Ask, "ask"),
            (PermissionPolicy::AcceptEdits, "accept_edits"),
            (PermissionPolicy::Bypass, "bypass"),
        ] {
            assert_eq!(serde_json::to_value(policy).unwrap(), id);
        }
        for (status, id) in [
            (jod_core::AgentStatus::Running, "running"),
            (jod_core::AgentStatus::Completed, "completed"),
            (jod_core::AgentStatus::Failed, "failed"),
            (jod_core::AgentStatus::Killed, "killed"),
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), id);
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // `block_on` puts us inside Tauri's Tokio runtime, which the
            // constructor needs in order to spawn its event-collector task.
            //
            // Persistent, not `Jod::new()`: a run is supervised by a separate
            // process that reports through the database, so an app without one
            // could start agents it would then never hear from again.
            let jod = tauri::async_runtime::block_on(async { Jod::persistent() })
                .expect("opening ~/.jod/jod.db");

            // Bridge core events onto the webview's event bus.
            let mut rx = jod.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(envelope) => {
                            let _ = handle.emit(AGENT_EVENT, envelope);
                        }
                        // A lagging client missed events; it refetches history
                        // on selection, so keep going rather than dying.
                        Err(jod_core::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(jod_core::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            app.manage(AppState { jod });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            system_status,
            spawn_agent,
            list_agents,
            agent_events,
            kill_agent,
            report,
            open_in_terminal,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Jod");
}
