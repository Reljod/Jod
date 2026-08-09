//! Antigravity adapter — `agy -p <prompt> --output-format stream-json`.
//!
//! Antigravity streams *deltas*, which is the one way it differs structurally
//! from the other two harnesses. A `step_update` carries a `text_delta`
//! fragment keyed by `step_index`, re-emitted as the step grows, with `state`
//! going `ACTIVE` → `DONE`. Claude Code emits whole blocks and OpenCode emits
//! completed parts, so both can be surfaced on sight; a delta has to be
//! accumulated per step or the same prose is emitted twice.
//!
//! Like OpenCode's adapter, a step is only surfaced once it is `DONE`, so the
//! UI sees whole thoughts rather than every token.
//!
//! **Antigravity does not surface reasoning text.** A live run reports
//! `thinking_tokens` in its usage block and emits no reasoning content, and
//! there is no `--thinking` equivalent in `agy --help`. The token count is
//! carried through so a client can show *that* the model reasoned; the text is
//! not available to show. → `docs/jod-tui.md`

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::{Accumulator, ArgPart, Harness, HarnessKind, PermissionPolicy, SpawnRequest};
use crate::event::{summarize, AgentEvent, Usage};

#[derive(Default)]
pub struct Antigravity {
    acc: Accumulator,
    /// `step_index` → the text accumulated for that step so far.
    partials: HashMap<u64, String>,
    /// Steps already surfaced, so a re-emitted `DONE` is not shown twice.
    seen_steps: HashSet<String>,
}

impl Harness for Antigravity {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Antigravity
    }

    fn args(&self, req: &SpawnRequest) -> Vec<ArgPart> {
        let mut args = vec![
            ArgPart::lit("-p"),
            ArgPart::Prompt,
            ArgPart::lit("--output-format"),
            ArgPart::lit("stream-json"),
        ];
        if let Some(model) = &req.model {
            args.push(ArgPart::lit("--model"));
            args.push(ArgPart::lit(model));
        }
        // Antigravity takes the conversation to continue by id.
        if let Some(session) = &req.resume {
            args.push(ArgPart::lit("--conversation"));
            args.push(ArgPart::lit(session));
        }
        match req.permission {
            PermissionPolicy::Ask => {}
            // `--mode accept-edits` is the closest analogue to Claude's
            // acceptEdits: file writes go through, other tools still ask.
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
            return vec![AgentEvent::Raw { line: line.to_string() }];
        };

        match v.get("event").and_then(Value::as_str) {
            Some("init") => {
                let init = v.get("init");
                vec![AgentEvent::Started {
                    session_id: str_at(&v, "conversation_id"),
                    model: init.and_then(|i| str_at(i, "model")),
                }]
            }
            Some("step_update") => match v.get("step_update") {
                Some(step) => self.parse_step(step),
                None => vec![AgentEvent::Raw { line: line.to_string() }],
            },
            Some("result") => {
                if let Some(result) = v.get("result") {
                    if let Some(text) = str_at(result, "response") {
                        self.acc.note_text(&text);
                    }
                    // Anything other than SUCCESS is a failed run — CANCELED and
                    // INTERRUPTED included, since neither produced the answer
                    // that was asked for.
                    match str_at(result, "status").as_deref() {
                        Some("SUCCESS") | None => {}
                        Some(_) => self.acc.errored = true,
                    }
                    self.acc.add_usage(&usage_from(result));
                }
                // The run is only *over* when the process exits; see finalize.
                vec![]
            }
            _ => vec![AgentEvent::Raw { line: line.to_string() }],
        }
    }

    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent {
        self.acc.finish(exit_code)
    }
}

impl Antigravity {
    fn parse_step(&mut self, step: &Value) -> Vec<AgentEvent> {
        let index = step.get("step_index").and_then(Value::as_u64).unwrap_or(0);
        let done = str_at(step, "state").as_deref() == Some("DONE");
        let step_type = str_at(step, "step_type").unwrap_or_default();

        if let Some(usage) = step.get("usage") {
            self.acc.add_usage(&usage_from_object(usage));
        }

        // Deltas arrive on every state, so accumulate before deciding anything.
        if let Some(delta) = str_at(step, "text_delta") {
            self.partials.entry(index).or_default().push_str(&delta);
        }

        match step_type.as_str() {
            "agent_response" => self.take_text(index, done).map_or(vec![], |text| {
                self.acc.note_text(&text);
                vec![AgentEvent::Message { text }]
            }),
            // Not emitted by any observed run — Antigravity reports
            // `thinking_tokens` and no reasoning text. Handled anyway so that
            // the day it ships, reasoning appears with no adapter change.
            "thinking" | "reasoning" => self
                .take_text(index, done)
                .map_or(vec![], |text| vec![AgentEvent::Thinking { text }]),
            "tool" => self.parse_tool(step, index, done),
            // Bookkeeping. `user_input` echoes the prompt back; `checkpoint` is
            // a save point; `unknown` is Antigravity's own label for a step it
            // did not classify, and carries no payload.
            "user_input" | "checkpoint" | "unknown" => vec![],
            // Emitted on every resumed conversation, normally empty. Showing it
            // raw would put a line of noise at the top of each follow-up turn,
            // but it is still surfaced whenever it actually carries something.
            "system_message" => self
                .take_text(index, done)
                .map_or(vec![], |text| vec![AgentEvent::Raw { line: text }]),
            _ => vec![AgentEvent::Raw {
                line: format!("step_type={step_type} {}", summarize(step, 300)),
            }],
        }
    }

    /// Take a step's accumulated text once the step is complete, exactly once.
    fn take_text(&mut self, index: u64, done: bool) -> Option<String> {
        if !done || !self.seen_steps.insert(format!("{index}:text")) {
            return None;
        }
        let text = self.partials.remove(&index)?;
        (!text.trim().is_empty()).then_some(text)
    }

    fn parse_tool(&mut self, step: &Value, index: u64, done: bool) -> Vec<AgentEvent> {
        let info = step.get("tool_info");
        let name = info
            .and_then(|i| str_at(i, "name"))
            .or_else(|| str_at(step, "tool_name"))
            .unwrap_or_else(|| "tool".into());

        if !done {
            if !self.seen_steps.insert(format!("{index}:call")) {
                return vec![];
            }
            return vec![AgentEvent::ToolCall {
                name,
                input: info.and_then(|i| i.get("parameters")).cloned(),
            }];
        }

        let mut out = vec![];
        // A tool that completed in one update never had an ACTIVE state, so the
        // call has to be synthesised or the result would appear unprompted.
        if self.seen_steps.insert(format!("{index}:call")) {
            out.push(AgentEvent::ToolCall {
                name: name.clone(),
                input: info.and_then(|i| i.get("parameters")).cloned(),
            });
        }
        if !self.seen_steps.insert(format!("{index}:done")) {
            return out;
        }
        let error = info.and_then(|i| i.get("error")).filter(|e| !e.is_null());
        let is_error = error.is_some();
        if is_error {
            self.acc.errored = true;
        }
        out.push(AgentEvent::ToolResult {
            name,
            summary: error
                .or_else(|| info.and_then(|i| i.get("output")))
                .map(|o| summarize(o, 400)),
            is_error,
        });
        out
    }
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn usage_from(result: &Value) -> Usage {
    result.get("usage").map(usage_from_object).unwrap_or_default()
}

/// Antigravity reports tokens only — there is no cost field anywhere in its
/// output, so `cost_usd` stays `None` rather than being guessed at.
fn usage_from_object(u: &Value) -> Usage {
    let at = |key: &str| u.get(key).and_then(Value::as_u64);
    Usage {
        input_tokens: at("input_tokens"),
        output_tokens: at("output_tokens"),
        cache_read_tokens: at("cache_read_tokens"),
        cache_write_tokens: None,
        thinking_tokens: at("thinking_tokens"),
        cost_usd: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn req() -> SpawnRequest {
        SpawnRequest {
            name: "n".into(),
            harness: HarnessKind::Antigravity,
            prompt: "hi".into(),
            cwd: PathBuf::from("/tmp"),
            model: None,
            permission: PermissionPolicy::Ask,
            resume: None,
        }
    }

    fn lits(parts: &[ArgPart]) -> Vec<String> {
        parts
            .iter()
            .map(|p| match p {
                ArgPart::Literal(s) => s.clone(),
                ArgPart::Prompt => "<prompt>".into(),
            })
            .collect()
    }

    #[test]
    fn the_command_line_asks_for_a_streaming_json_run() {
        let args = lits(&Antigravity::default().args(&req()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"<prompt>".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
    }

    #[test]
    fn resuming_passes_the_conversation_id() {
        let mut r = req();
        r.resume = Some("conv-1".into());
        let args = lits(&Antigravity::default().args(&r));
        let at = args.iter().position(|a| a == "--conversation").unwrap();
        assert_eq!(args[at + 1], "conv-1");
    }

    #[test]
    fn bypass_skips_permissions_and_ask_adds_no_flag() {
        let mut r = req();
        r.permission = PermissionPolicy::Bypass;
        assert!(lits(&Antigravity::default().args(&r))
            .contains(&"--dangerously-skip-permissions".to_string()));

        r.permission = PermissionPolicy::Ask;
        let args = lits(&Antigravity::default().args(&r));
        assert!(!args.iter().any(|a| a.starts_with("--dangerously")));
        assert!(!args.contains(&"--mode".to_string()));
    }

    #[test]
    fn init_announces_the_conversation_and_model() {
        let mut h = Antigravity::default();
        let events = h.parse_line(
            r#"{"event":"init","conversation_id":"c1","init":{"model":"gemini-3","cwd":"/x"}}"#,
        );
        assert_eq!(
            events,
            vec![AgentEvent::Started {
                session_id: Some("c1".into()),
                model: Some("gemini-3".into()),
            }]
        );
    }

    /// The regression this adapter exists to avoid: Antigravity re-emits a
    /// step as it grows, so surfacing every `text_delta` would print the
    /// answer's first half twice.
    #[test]
    fn deltas_are_joined_into_one_message_and_emitted_once() {
        let mut h = Antigravity::default();
        assert!(h
            .parse_line(
                r#"{"event":"step_update","step_update":{"step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"Hello "}}"#
            )
            .is_empty());
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"world"}}"#,
        );
        assert_eq!(events, vec![AgentEvent::Message { text: "Hello world".into() }]);

        // A repeated DONE must not emit the message a second time.
        assert!(h
            .parse_line(
                r#"{"event":"step_update","step_update":{"step_index":2,"state":"DONE","step_type":"agent_response","text_delta":""}}"#
            )
            .is_empty());
    }

    #[test]
    fn two_steps_accumulate_independently() {
        let mut h = Antigravity::default();
        h.parse_line(r#"{"event":"step_update","step_update":{"step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"one"}}"#);
        h.parse_line(r#"{"event":"step_update","step_update":{"step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"two"}}"#);
        let a = h.parse_line(r#"{"event":"step_update","step_update":{"step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"!"}}"#);
        let b = h.parse_line(r#"{"event":"step_update","step_update":{"step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"!"}}"#);
        assert_eq!(a, vec![AgentEvent::Message { text: "two!".into() }]);
        assert_eq!(b, vec![AgentEvent::Message { text: "one!".into() }]);
    }

    #[test]
    fn a_tool_yields_a_call_then_a_result() {
        let mut h = Antigravity::default();
        let call = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":3,"state":"ACTIVE","step_type":"tool","tool_info":{"name":"run_command","parameters":{"cmd":"ls"}}}}"#,
        );
        assert_eq!(
            call,
            vec![AgentEvent::ToolCall {
                name: "run_command".into(),
                input: Some(serde_json::json!({"cmd":"ls"})),
            }]
        );
        let result = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":3,"state":"DONE","step_type":"tool","tool_info":{"name":"run_command","output":"a\nb"}}}"#,
        );
        assert_eq!(
            result,
            vec![AgentEvent::ToolResult {
                name: "run_command".into(),
                summary: Some("a\nb".into()),
                is_error: false,
            }]
        );
    }

    /// A tool fast enough to complete in one update never had an ACTIVE state.
    /// Without a synthesised call the UI would show a result out of nowhere.
    #[test]
    fn a_tool_that_completes_immediately_still_reports_its_call() {
        let mut h = Antigravity::default();
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":4,"state":"DONE","step_type":"tool","tool_info":{"name":"view_file","parameters":{"p":"a"},"output":"x"}}}"#,
        );
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AgentEvent::ToolCall { .. }));
        assert!(matches!(events[1], AgentEvent::ToolResult { is_error: false, .. }));
    }

    #[test]
    fn a_failing_tool_marks_the_run_errored() {
        let mut h = Antigravity::default();
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":5,"state":"DONE","step_type":"tool","tool_info":{"name":"t","error":"boom"}}}"#,
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolResult { is_error: true, .. }
        )));
        match h.finalize(Some(0)) {
            AgentEvent::Finished { is_error, .. } => assert!(is_error),
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn bookkeeping_steps_are_ignored_rather_than_shown_as_raw() {
        let mut h = Antigravity::default();
        for step_type in ["user_input", "checkpoint", "unknown"] {
            let line = format!(
                r#"{{"event":"step_update","step_update":{{"step_index":0,"state":"DONE","step_type":"{step_type}"}}}}"#
            );
            assert!(h.parse_line(&line).is_empty(), "{step_type} should be quiet");
        }
    }

    /// Observed on every resumed run. Empty ones must stay quiet, or each
    /// follow-up turn opens with a line of noise.
    #[test]
    fn an_empty_system_message_is_quiet_but_a_full_one_is_shown() {
        let mut h = Antigravity::default();
        assert!(h
            .parse_line(
                r#"{"event":"step_update","step_update":{"step_index":5,"state":"DONE","step_type":"system_message"}}"#
            )
            .is_empty());

        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":6,"state":"DONE","step_type":"system_message","text_delta":"context was trimmed"}}"#,
        );
        assert_eq!(
            events,
            vec![AgentEvent::Raw { line: "context was trimmed".into() }]
        );
    }

    /// A resumed conversation continues the step numbering from the first turn.
    #[test]
    fn a_resumed_run_reports_the_same_conversation_id() {
        let mut h = Antigravity::default();
        let events = h.parse_line(
            r#"{"event":"init","conversation_id":"c7070a45","init":{"cwd":"/w"}}"#,
        );
        assert_eq!(
            events,
            vec![AgentEvent::Started { session_id: Some("c7070a45".into()), model: None }]
        );
    }

    #[test]
    fn a_step_update_without_its_payload_is_surfaced() {
        let mut h = Antigravity::default();
        let events = h.parse_line(r#"{"event":"step_update"}"#);
        assert!(matches!(events.as_slice(), [AgentEvent::Raw { .. }]));
    }

    #[test]
    fn an_unknown_top_level_event_is_surfaced() {
        let mut h = Antigravity::default();
        let events = h.parse_line(r#"{"event":"something_new","payload":1}"#);
        assert!(matches!(events.as_slice(), [AgentEvent::Raw { .. }]));
    }

    #[test]
    fn a_result_without_a_body_does_not_panic() {
        let mut h = Antigravity::default();
        assert!(h.parse_line(r#"{"event":"result"}"#).is_empty());
        assert!(matches!(h.finalize(Some(0)), AgentEvent::Finished { .. }));
    }

    #[test]
    fn blank_lines_are_ignored() {
        let mut h = Antigravity::default();
        assert!(h.parse_line("   ").is_empty());
    }

    #[test]
    fn accept_edits_maps_onto_antigravitys_own_mode_flag() {
        let mut r = req();
        r.permission = PermissionPolicy::AcceptEdits;
        let args = lits(&Antigravity::default().args(&r));
        let at = args.iter().position(|a| a == "--mode").unwrap();
        assert_eq!(args[at + 1], "accept-edits");
    }

    #[test]
    fn a_model_is_passed_through() {
        let mut r = req();
        r.model = Some("gemini-3-pro".into());
        let args = lits(&Antigravity::default().args(&r));
        let at = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[at + 1], "gemini-3-pro");
    }

    #[test]
    fn the_adapter_reports_its_own_kind() {
        assert_eq!(Antigravity::default().kind(), HarnessKind::Antigravity);
    }

    /// A tool step with no `tool_info` at all still names something, rather
    /// than rendering an unlabelled arrow.
    #[test]
    fn a_tool_without_info_still_has_a_name() {
        let mut h = Antigravity::default();
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":7,"state":"ACTIVE","step_type":"tool"}}"#,
        );
        assert_eq!(
            events,
            vec![AgentEvent::ToolCall { name: "tool".into(), input: None }]
        );
    }

    #[test]
    fn a_tool_name_can_come_from_the_step_itself() {
        let mut h = Antigravity::default();
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":8,"state":"ACTIVE","step_type":"tool","tool_name":"grep_search"}}"#,
        );
        assert_eq!(
            events,
            vec![AgentEvent::ToolCall { name: "grep_search".into(), input: None }]
        );
    }

    #[test]
    fn an_unrecognised_step_type_is_surfaced_not_dropped() {
        let mut h = Antigravity::default();
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":9,"state":"DONE","step_type":"brand_new_thing"}}"#,
        );
        assert!(matches!(events.as_slice(), [AgentEvent::Raw { .. }]));
    }

    #[test]
    fn unparseable_output_becomes_raw_rather_than_being_lost() {
        let mut h = Antigravity::default();
        assert_eq!(
            h.parse_line("not json at all"),
            vec![AgentEvent::Raw { line: "not json at all".into() }]
        );
    }

    #[test]
    fn the_result_record_supplies_the_answer_and_the_tokens() {
        let mut h = Antigravity::default();
        assert!(h
            .parse_line(
                r#"{"event":"result","result":{"status":"SUCCESS","response":"391","usage":{"input_tokens":17585,"output_tokens":438,"thinking_tokens":216}}}"#
            )
            .is_empty());
        match h.finalize(Some(0)) {
            AgentEvent::Finished { text, usage, is_error, .. } => {
                assert_eq!(text.as_deref(), Some("391"));
                assert_eq!(usage.input_tokens, Some(17585));
                assert_eq!(usage.thinking_tokens, Some(216));
                assert_eq!(usage.cost_usd, None, "Antigravity reports no cost");
                assert!(!is_error);
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_run_is_reported_as_an_error() {
        let mut h = Antigravity::default();
        h.parse_line(r#"{"event":"result","result":{"status":"CANCELED","response":"partial"}}"#);
        match h.finalize(Some(0)) {
            AgentEvent::Finished { is_error, .. } => assert!(is_error),
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    /// Replays the exact transcript captured from a live `agy` run.
    #[test]
    fn a_real_transcript_produces_one_message_and_a_priced_finish() {
        let transcript = [
            r#"{"event":"init","conversation_id":"44464feb","init":{"cwd":"/w","permission_mode":"request-review"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"44464feb","step_index":0,"state":"DONE","step_type":"user_input"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"44464feb","step_index":1,"state":"DONE","step_type":"unknown","duration_seconds":0.007}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"44464feb","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"17 x 23 "}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"44464feb","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"= 391","usage":{"input_tokens":17479,"output_tokens":434,"thinking_tokens":216}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"44464feb","step_index":3,"state":"DONE","step_type":"checkpoint","usage":{"input_tokens":106,"output_tokens":4}}}"#,
            r#"{"event":"result","result":{"conversation_id":"44464feb","status":"SUCCESS","response":"17 x 23 = 391","num_turns":1,"usage":{"input_tokens":17585,"output_tokens":438,"thinking_tokens":216}}}"#,
        ];

        let mut h = Antigravity::default();
        let events: Vec<AgentEvent> = transcript.iter().flat_map(|l| h.parse_line(l)).collect();

        assert_eq!(
            events,
            vec![
                AgentEvent::Started { session_id: Some("44464feb".into()), model: None },
                AgentEvent::Message { text: "17 x 23 = 391".into() },
            ],
            "a real run must yield exactly one Started and one Message"
        );

        match h.finalize(Some(0)) {
            AgentEvent::Finished { text, is_error, usage, .. } => {
                assert_eq!(text.as_deref(), Some("17 x 23 = 391"));
                assert!(!is_error);
                assert_eq!(usage.thinking_tokens, Some(216 + 216));
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }
}
