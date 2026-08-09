//! AGY adapter — `agy --print <prompt> --output-format stream-json`.
//!
//! AGY streams one JSON object per line under an `event` discriminator:
//!
//! ```text
//! {"event":"init","conversation_id":"…","init":{…}}
//! {"event":"step_update","step_update":{"step_type":"agent_response","text_delta":"…"}}
//! {"event":"step_update","step_update":{"step_type":"tool","state":"ACTIVE","tool_name":"list_dir"}}
//! {"event":"result","result":{"status":"SUCCESS","response":"…","usage":{…}}}
//! ```
//!
//! It also prints human-readable notices (permission denials, for one) onto the
//! same stream, which arrive here as [`AgentEvent::Raw`] rather than being lost.

use serde_json::Value;

use super::{Accumulator, ArgPart, Harness, HarnessKind, PermissionPolicy, Resume, SpawnRequest};
use crate::event::{summarize, AgentEvent, Usage};

#[derive(Default)]
pub struct Agy {
    acc: Accumulator,
}

impl Harness for Agy {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Agy
    }

    fn args(&self, req: &SpawnRequest) -> Vec<ArgPart> {
        let mut args = vec![
            ArgPart::lit("--print"),
            ArgPart::Prompt,
            ArgPart::lit("--output-format"),
            ArgPart::lit("stream-json"),
        ];
        if let Some(model) = &req.model {
            args.push(ArgPart::lit("--model"));
            args.push(ArgPart::lit(model));
        }
        match &req.resume {
            Resume::Fresh => {}
            Resume::Last => args.push(ArgPart::lit("--continue")),
            Resume::Session(id) => {
                args.push(ArgPart::lit("--conversation"));
                args.push(ArgPart::lit(id));
            }
        }
        match req.permission {
            // AGY's default is `request-review`, which in headless mode
            // auto-denies anything needing approval. That is the safe default.
            PermissionPolicy::Ask => {}
            PermissionPolicy::AcceptEdits => {
                args.push(ArgPart::lit("--mode"));
                args.push(ArgPart::lit("accept-edits"));
            }
            PermissionPolicy::Bypass => args.push(ArgPart::lit("--dangerously-skip-permissions")),
        }
        args
    }

    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return vec![];
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return vec![AgentEvent::Raw {
                line: strip_ansi(line),
            }];
        };

        match v.get("event").and_then(Value::as_str) {
            Some("init") => vec![AgentEvent::Started {
                session_id: str_at(&v, "conversation_id")
                    .or_else(|| v.get("init").and_then(|i| str_at(i, "conversation_id"))),
                model: v.get("init").and_then(|i| str_at(i, "model")),
            }],
            Some("step_update") => match v.get("step_update") {
                Some(step) => self.parse_step(step),
                None => vec![],
            },
            Some("result") => {
                if let Some(r) = v.get("result") {
                    if let Some(text) = str_at(r, "response") {
                        self.acc.note_text(&text);
                    }
                    // Anything but SUCCESS is a failed run.
                    if let Some(status) = str_at(r, "status") {
                        if status != "SUCCESS" {
                            self.acc.errored = true;
                        }
                    }
                    // The result's usage is the authoritative total — the
                    // per-step figures sum to it, so replace rather than add,
                    // which would double-count every step.
                    if let Some(total) = r.get("usage").map(usage_from) {
                        if !total.is_empty() {
                            self.acc.usage = total;
                        }
                    }
                }
                // The run is over when the process exits; see `finalize`.
                vec![]
            }
            _ => vec![AgentEvent::Raw {
                line: strip_ansi(line),
            }],
        }
    }

    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent {
        self.acc.finish(exit_code)
    }
}

impl Agy {
    fn parse_step(&mut self, step: &Value) -> Vec<AgentEvent> {
        let state = str_at(step, "state").unwrap_or_default();
        let step_type = str_at(step, "step_type").unwrap_or_default();

        // Per-step usage gives a live running total while the agent works. The
        // terminal `result` record overwrites it with the authoritative sum.
        if let Some(u) = step.get("usage").map(usage_from) {
            self.acc.add_usage(&u);
        }

        match step_type.as_str() {
            "agent_response" => match str_at(step, "text_delta") {
                Some(text) if !text.trim().is_empty() => {
                    self.acc.note_text(&text);
                    vec![AgentEvent::Message { text }]
                }
                _ => vec![],
            },
            "tool" => {
                let name = str_at(step, "tool_name")
                    .or_else(|| step.get("tool_info").and_then(|i| str_at(i, "name")))
                    .unwrap_or_else(|| "tool".into());
                let info = step.get("tool_info");
                match state.as_str() {
                    // A tool starts ACTIVE and comes back DONE or ERROR.
                    "ACTIVE" => vec![AgentEvent::ToolCall {
                        name,
                        input: info.and_then(|i| i.get("parameters").cloned()),
                    }],
                    "DONE" | "ERROR" => {
                        let error = info.and_then(|i| i.get("error"));
                        let is_error = state == "ERROR" || error.is_some();
                        let summary = error
                            .and_then(|e| str_at(e, "message"))
                            .or_else(|| info.and_then(|i| str_at(i, "result")))
                            .map(|s| summarize(&Value::String(s), 400));
                        vec![AgentEvent::ToolResult {
                            name,
                            summary,
                            is_error,
                        }]
                    }
                    _ => vec![],
                }
            }
            // user_input, checkpoint and unknown are bookkeeping, not output.
            _ => vec![],
        }
    }
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn u64_at(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

/// AGY reports no cost, only tokens. Leaving `cost_usd` unset is honest;
/// inventing a price from a rate card would not be.
fn usage_from(v: &Value) -> Usage {
    Usage {
        input_tokens: u64_at(v, "input_tokens"),
        output_tokens: u64_at(v, "output_tokens"),
        cache_read_tokens: u64_at(v, "cache_read_tokens"),
        cache_write_tokens: u64_at(v, "cache_write_tokens"),
        cost_usd: None,
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> SpawnRequest {
        SpawnRequest {
            name: "t".into(),
            harness: HarnessKind::Agy,
            prompt: "hi".into(),
            cwd: std::path::PathBuf::from("/tmp"),
            model: None,
            permission: PermissionPolicy::Ask,
            resume: Resume::Fresh,
        }
    }

    fn lits(args: &[ArgPart]) -> Vec<String> {
        args.iter()
            .map(|a| match a {
                ArgPart::Literal(s) => s.clone(),
                ArgPart::Prompt => "<PROMPT>".into(),
            })
            .collect()
    }

    #[test]
    fn the_prompt_is_a_placeholder_never_an_inlined_literal() {
        let args = Agy::default().args(&req());
        assert!(args.contains(&ArgPart::Prompt));
        assert!(lits(&args).contains(&"--print".to_string()));
    }

    #[test]
    fn streaming_json_is_always_requested_or_nothing_could_be_parsed() {
        let args = lits(&Agy::default().args(&req()));
        let i = args.iter().position(|a| a == "--output-format").unwrap();
        assert_eq!(args[i + 1], "stream-json");
    }

    #[test]
    fn the_default_permission_adds_no_bypass_flag() {
        let args = lits(&Agy::default().args(&req()));
        assert!(!args.iter().any(|a| a.contains("dangerously")));
    }

    #[test]
    fn bypass_is_the_only_policy_that_skips_permissions() {
        let mut r = req();
        r.permission = PermissionPolicy::Bypass;
        let args = lits(&Agy::default().args(&r));
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn accept_edits_maps_to_agys_own_mode_flag() {
        let mut r = req();
        r.permission = PermissionPolicy::AcceptEdits;
        let args = lits(&Agy::default().args(&r));
        let i = args.iter().position(|a| a == "--mode").unwrap();
        assert_eq!(args[i + 1], "accept-edits");
    }

    #[test]
    fn resuming_the_last_conversation_uses_continue() {
        let mut r = req();
        r.resume = Resume::Last;
        assert!(lits(&Agy::default().args(&r)).contains(&"--continue".to_string()));
    }

    #[test]
    fn resuming_a_named_conversation_passes_its_id() {
        let mut r = req();
        r.resume = Resume::Session("abc-123".into());
        let args = lits(&Agy::default().args(&r));
        let i = args.iter().position(|a| a == "--conversation").unwrap();
        assert_eq!(args[i + 1], "abc-123");
    }

    #[test]
    fn init_reports_the_conversation_id_as_the_session() {
        let mut h = Agy::default();
        let events = h.parse_line(
            r#"{"event":"init","conversation_id":"8a95","init":{"cwd":"/home","permission_mode":"request-review"}}"#,
        );
        match &events[..] {
            [AgentEvent::Started { session_id, .. }] => {
                assert_eq!(session_id.as_deref(), Some("8a95"))
            }
            other => panic!("expected Started, got {other:?}"),
        }
    }

    #[test]
    fn an_agent_response_delta_becomes_a_message() {
        let mut h = Agy::default();
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_type":"agent_response","state":"DONE","text_delta":"ok\n"}}"#,
        );
        match &events[..] {
            [AgentEvent::Message { text }] => assert_eq!(text, "ok\n"),
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_delta_produces_no_message() {
        let mut h = Agy::default();
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_type":"agent_response","state":"DONE","text_delta":"   "}}"#,
        );
        assert!(events.is_empty(), "got {events:?}");
    }

    #[test]
    fn an_active_tool_step_is_a_tool_call_with_its_parameters() {
        let mut h = Agy::default();
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_type":"tool","state":"ACTIVE","tool_name":"list_dir","tool_info":{"name":"list_dir","parameters":{"DirectoryPath":"/home"}}}}"#,
        );
        match &events[..] {
            [AgentEvent::ToolCall { name, input }] => {
                assert_eq!(name, "list_dir");
                assert_eq!(input.as_ref().unwrap()["DirectoryPath"], "/home");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    /// Regression: a denied permission arrives as an ERROR tool step and must
    /// be reported as a failed tool, not silently treated as success.
    #[test]
    fn a_denied_tool_comes_back_as_an_errored_tool_result() {
        let mut h = Agy::default();
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_type":"tool","state":"ERROR","tool_name":"list_dir","tool_info":{"name":"list_dir","error":{"type":"TOOL_ERROR","message":"User denied permission for read_file(/home)."}}}}"#,
        );
        match &events[..] {
            [AgentEvent::ToolResult { name, summary, is_error }] => {
                assert!(is_error);
                assert_eq!(name, "list_dir");
                assert!(summary.as_deref().unwrap().contains("denied"));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn the_result_record_supplies_the_final_answer() {
        let mut h = Agy::default();
        assert!(h
            .parse_line(r#"{"event":"result","result":{"status":"SUCCESS","response":"the answer"}}"#)
            .is_empty());
        match h.finalize(Some(0)) {
            AgentEvent::Finished { text, is_error, .. } => {
                assert_eq!(text.as_deref(), Some("the answer"));
                assert!(!is_error);
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn a_non_success_status_marks_the_run_failed_even_on_a_zero_exit() {
        let mut h = Agy::default();
        h.parse_line(r#"{"event":"result","result":{"status":"ERROR","response":"nope"}}"#);
        match h.finalize(Some(0)) {
            AgentEvent::Finished { is_error, .. } => assert!(is_error),
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    /// The per-step figures sum to the result's total, so adding both would
    /// report roughly double the tokens actually used.
    #[test]
    fn the_result_total_replaces_the_running_step_tally_rather_than_adding_to_it() {
        let mut h = Agy::default();
        h.parse_line(
            r#"{"event":"step_update","step_update":{"step_type":"agent_response","state":"DONE","usage":{"input_tokens":9337,"output_tokens":64}}}"#,
        );
        h.parse_line(
            r#"{"event":"step_update","step_update":{"step_type":"checkpoint","state":"DONE","usage":{"input_tokens":99,"output_tokens":4}}}"#,
        );
        h.parse_line(
            r#"{"event":"result","result":{"status":"SUCCESS","response":"ok","usage":{"input_tokens":9436,"output_tokens":68}}}"#,
        );
        match h.finalize(Some(0)) {
            AgentEvent::Finished { usage, .. } => {
                assert_eq!(usage.output_tokens, Some(68), "step usage was double-counted");
                assert_eq!(usage.input_tokens, Some(9436));
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    /// AGY prints permission notices as bare prose on the JSON stream.
    #[test]
    fn a_human_readable_notice_is_surfaced_rather_than_dropped() {
        let mut h = Agy::default();
        let events = h.parse_line("jetski: no output produced — a tool required permission");
        match &events[..] {
            [AgentEvent::Raw { line }] => assert!(line.contains("no output produced")),
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_event_kind_is_surfaced_not_swallowed() {
        let mut h = Agy::default();
        let events = h.parse_line(r#"{"event":"brand_new_thing","payload":1}"#);
        assert!(matches!(events[..], [AgentEvent::Raw { .. }]));
    }

    #[test]
    fn no_cost_is_reported_because_agy_does_not_publish_one() {
        let u = usage_from(&serde_json::json!({"input_tokens":10,"output_tokens":2}));
        assert_eq!(u.cost_usd, None);
        assert_eq!(u.input_tokens, Some(10));
    }
}
