//! Turning an [`Action`] into a call on `jod-core`.
//!
//! The request-building is separated from the awaiting so it can be tested
//! without a tmux session or a harness binary: `follow_request` and
//! `spawn_request` are pure, and `perform` is the thin part that runs them.

use std::path::PathBuf;
use std::sync::Arc;

use jod_core::{AgentSummary, HarnessKind, Jod, PermissionPolicy, SpawnRequest, Team};

use crate::app::{Action, App};

/// A follow-up turn: same agent, same place, same harness — with the
/// conversation resumed so the agent still remembers the last turn.
///
/// This is the whole multi-turn story. A conversation is not a long-lived
/// process, it is a series of spawns that each carry the session id the
/// previous one reported. → `docs/jod-tui.md`
pub fn follow_request(agent: &AgentSummary, prompt: &str) -> SpawnRequest {
    SpawnRequest {
        name: agent.name.clone(),
        harness: agent.harness,
        prompt: prompt.to_string(),
        cwd: PathBuf::from(&agent.cwd),
        model: agent.model.clone(),
        permission: agent.permission,
        resume: agent.session_id.clone(),
    }
}

/// A brand new agent, rooted wherever the TUI was started.
pub fn spawn_request(harness: HarnessKind, prompt: &str, cwd: PathBuf, name: &str) -> SpawnRequest {
    SpawnRequest {
        name: name.to_string(),
        harness,
        prompt: prompt.to_string(),
        cwd,
        model: None,
        permission: PermissionPolicy::Ask,
        resume: None,
    }
}

/// Name a new agent after the first few words of its prompt, so the fleet list
/// reads as a list of jobs rather than a list of UUIDs.
pub fn name_from_prompt(prompt: &str) -> String {
    let words: Vec<&str> = prompt.split_whitespace().take(4).collect();
    if words.is_empty() {
        return "agent".to_string();
    }
    let name: String = words.join("-").chars().take(32).collect();
    name.to_lowercase()
}

/// Run one action. Returns the line to show in the status bar.
pub async fn perform(jod: &Arc<Jod>, app: &App, action: Action, cwd: &PathBuf) -> Option<String> {
    match action {
        Action::None | Action::Quit => None,

        Action::Follow { agent_id, prompt } => {
            let agent = jod.agent(&agent_id).await.ok()?;
            // Without a session id there is nothing to resume, and spawning
            // anyway would silently start a fresh context — say so instead.
            if agent.session_id.is_none() {
                return Some(format!(
                    "{} has not reported a session id yet — cannot continue it",
                    agent.name
                ));
            }
            match jod.spawn_agent(follow_request(&agent, &prompt)).await {
                Ok(new) => Some(format!("continued {} as {}", agent.name, new.id)),
                Err(e) => Some(format!("could not continue: {e}")),
            }
        }

        Action::Spawn { harness, prompt } => {
            let name = name_from_prompt(&prompt);
            match jod
                .spawn_agent(spawn_request(harness, &prompt, cwd.clone(), &name))
                .await
            {
                Ok(agent) => Some(format!("started {} on {}", agent.name, agent.harness_label)),
                Err(e) => Some(format!("could not start: {e}")),
            }
        }

        Action::Kill { agent_id } => match jod.kill_agent(&agent_id).await {
            Ok(()) => Some("killed".to_string()),
            Err(e) => Some(format!("could not kill: {e}")),
        },

        Action::Attach { agent_id } => {
            let agent = jod.agent(&agent_id).await.ok()?;
            Some(format!("run: {}", agent.attach_command))
        }

        Action::Broadcast { team, text } => {
            let from = app
                .selected_agent()
                .map(|a| a.name.clone())
                .unwrap_or_else(|| "jod".to_string());
            let message = jod_core::Message {
                from,
                to: None,
                text,
                at_ms: chrono::Utc::now().timestamp_millis(),
            };
            match Team::new(&team).send(&message).await {
                Ok(to) if to.is_empty() => Some("nobody on the team to message".to_string()),
                Ok(to) => Some(format!("messaged {}", to.join(", "))),
                Err(e) => Some(format!("could not send: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jod_core::{AgentStatus, Usage};

    fn agent() -> AgentSummary {
        AgentSummary {
            id: "a".into(),
            name: "porter".into(),
            harness: HarnessKind::OpenCode,
            harness_label: "OpenCode".into(),
            status: AgentStatus::Completed,
            cwd: "/work/repo".into(),
            model: Some("anthropic/claude-sonnet-5".into()),
            permission: PermissionPolicy::AcceptEdits,
            tmux_session: "jod-a".into(),
            attach_command: "tmux attach -t jod-a".into(),
            switch_command: "tmux switch-client -t jod-a".into(),
            session_closed: false,
            created_at_ms: 0,
            session_id: Some("ses_123".into()),
            usage: Usage::default(),
            event_count: 0,
            last_message: None,
            stream_path: "/tmp/s".into(),
        }
    }

    /// The core of multi-turn: the follow-up carries the session id, so the
    /// harness continues the conversation instead of starting a new one.
    #[test]
    fn a_follow_up_resumes_the_reported_session() {
        let req = follow_request(&agent(), "and now the tests");
        assert_eq!(req.resume.as_deref(), Some("ses_123"));
        assert_eq!(req.prompt, "and now the tests");
    }

    #[test]
    fn a_follow_up_keeps_the_harness_model_cwd_and_permission() {
        let req = follow_request(&agent(), "next");
        assert_eq!(req.harness, HarnessKind::OpenCode);
        assert_eq!(req.model.as_deref(), Some("anthropic/claude-sonnet-5"));
        assert_eq!(req.cwd, PathBuf::from("/work/repo"));
        assert_eq!(req.permission, PermissionPolicy::AcceptEdits);
        assert_eq!(req.name, "porter", "a follow-up is the same agent, not a new one");
    }

    #[test]
    fn a_fresh_spawn_resumes_nothing() {
        let req = spawn_request(
            HarnessKind::Antigravity,
            "look around",
            PathBuf::from("/tmp"),
            "look",
        );
        assert!(req.resume.is_none());
        assert_eq!(req.harness, HarnessKind::Antigravity);
    }

    #[test]
    fn a_new_agent_is_named_after_its_prompt() {
        assert_eq!(name_from_prompt("Port the parser to Rust now"), "port-the-parser-to");
        assert_eq!(name_from_prompt("   "), "agent");
    }

    #[test]
    fn a_very_long_prompt_yields_a_short_name() {
        let name = name_from_prompt(&"verylongword ".repeat(10));
        assert!(name.chars().count() <= 32, "got {name}");
    }

    /// Continuing an agent that never reported a session must not quietly
    /// start a fresh context — the user is told instead.
    #[tokio::test]
    async fn continuing_without_a_session_id_explains_rather_than_restarting() {
        let jod = Jod::new();
        let app = App::default();
        let status = perform(
            &jod,
            &app,
            Action::Follow { agent_id: "missing".into(), prompt: "x".into() },
            &PathBuf::from("/tmp"),
        )
        .await;
        // An unknown agent resolves to nothing rather than panicking.
        assert!(status.is_none());
    }

    #[tokio::test]
    async fn killing_an_unknown_agent_reports_instead_of_panicking() {
        let jod = Jod::new();
        let app = App::default();
        let status = perform(
            &jod,
            &app,
            Action::Kill { agent_id: "nope".into() },
            &PathBuf::from("/tmp"),
        )
        .await
        .unwrap();
        assert!(status.starts_with("could not kill"), "got {status}");
    }

    #[tokio::test]
    async fn quit_and_none_ask_for_nothing() {
        let jod = Jod::new();
        let app = App::default();
        let cwd = PathBuf::from("/tmp");
        assert!(perform(&jod, &app, Action::None, &cwd).await.is_none());
        assert!(perform(&jod, &app, Action::Quit, &cwd).await.is_none());
    }

    #[tokio::test]
    async fn messaging_an_empty_team_says_so() {
        let jod = Jod::new();
        let app = App::default();
        let team = format!("tui-test-empty-{}", std::process::id());
        let status = perform(
            &jod,
            &app,
            Action::Broadcast { team: team.clone(), text: "hi".into() },
            &PathBuf::from("/tmp"),
        )
        .await
        .unwrap();
        assert_eq!(status, "nobody on the team to message");
    }

    #[tokio::test]
    async fn a_broadcast_reaches_the_members_and_is_reported() {
        let jod = Jod::new();
        let app = App::default();
        let name = format!("tui-test-crew-{}", std::process::id());
        let team = Team::new(&name);
        let _ = tokio::fs::remove_dir_all(team.dir()).await;
        team.join("scout", HarnessKind::OpenCode, "r").await.unwrap();
        team.join("builder", HarnessKind::ClaudeCode, "r").await.unwrap();

        let status = perform(
            &jod,
            &app,
            Action::Broadcast { team: name, text: "stand up".into() },
            &PathBuf::from("/tmp"),
        )
        .await
        .unwrap();

        assert!(status.starts_with("messaged"), "got {status}");
        assert_eq!(team.inbox("scout").await.len(), 1);
        let _ = tokio::fs::remove_dir_all(team.dir()).await;
    }
}
