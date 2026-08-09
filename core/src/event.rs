//! The one event vocabulary every harness is normalised into.
//!
//! Claude Code and OpenCode emit completely different JSONL. Everything above
//! this module — the desktop UI, a future iOS client, Jod's own orchestration —
//! only ever sees `AgentEvent`, so adding a third harness never reaches them.

use serde::{Deserialize, Serialize};

/// Token/cost accounting for one agent turn. Fields are optional because the
/// harnesses report different subsets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        *self == Usage::default()
    }
}

/// A single normalised thing that happened inside an agent run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The harness booted and told us which session/model it is using.
    Started {
        session_id: Option<String>,
        model: Option<String>,
    },
    /// Reasoning/thinking text, when the harness surfaces it.
    Thinking { text: String },
    /// Assistant prose addressed to the caller.
    Message { text: String },
    /// The agent invoked a tool.
    ToolCall {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
    /// A tool came back.
    ToolResult {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        is_error: bool,
    },
    /// The run ended. `text` is the final answer when the harness gives one.
    Finished {
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        is_error: bool,
        usage: Usage,
    },
    /// Something the harness said that we could not classify. Kept rather than
    /// dropped so a harness upgrade degrades to "shown verbatim", never to
    /// "silently swallowed".
    Raw { line: String },
    /// The runner itself failed (spawn error, unreadable stream, killed).
    Error { message: String },
}

/// An `AgentEvent` stamped with who emitted it and when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEnvelope {
    pub agent_id: String,
    /// Milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// Monotonic per-agent sequence number, so a late-joining UI can resume.
    pub seq: u64,
    #[serde(flatten)]
    pub event: AgentEvent,
}

/// Truncate long tool payloads so the UI stream stays readable and the event
/// log does not balloon with whole file contents.
pub(crate) fn summarize(value: &serde_json::Value, max: usize) -> String {
    let raw = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let trimmed = raw.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{head}… (+{} chars)", trimmed.chars().count() - max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_passes_short_strings_through() {
        let v = serde_json::json!("hello");
        assert_eq!(summarize(&v, 10), "hello");
    }

    #[test]
    fn summarize_truncates_and_reports_remainder() {
        let v = serde_json::json!("abcdefghij");
        assert_eq!(summarize(&v, 4), "abcd… (+6 chars)");
    }

    #[test]
    fn summarize_renders_non_strings_as_json() {
        let v = serde_json::json!({"a": 1});
        assert_eq!(summarize(&v, 100), "{\"a\":1}");
    }

    #[test]
    fn event_round_trips_through_json() {
        let e = AgentEvent::ToolCall {
            name: "Bash".into(),
            input: Some(serde_json::json!({"command": "ls"})),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&s).unwrap(), e);
    }
}
