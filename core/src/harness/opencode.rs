//! OpenCode adapter — `opencode run --format json <prompt>`.
//!
//! OpenCode streams *parts*. A part is re-emitted as it grows, and carries
//! `time.end` once it is complete, so we only surface completed parts and
//! never spam the UI with every token.

use std::collections::HashSet;

use serde_json::Value;

use super::{Accumulator, ArgPart, Harness, HarnessKind, PermissionPolicy, Resume, SpawnRequest};
use crate::event::{summarize, AgentEvent, Usage};

#[derive(Default)]
pub struct OpenCode {
    acc: Accumulator,
    announced_session: bool,
    /// Part ids already surfaced, so a re-emitted part is not shown twice.
    seen_parts: HashSet<String>,
}

impl Harness for OpenCode {
    fn kind(&self) -> HarnessKind {
        HarnessKind::OpenCode
    }

    fn args(&self, req: &SpawnRequest) -> Vec<ArgPart> {
        let mut args = vec![
            ArgPart::lit("run"),
            ArgPart::lit("--format"),
            ArgPart::lit("json"),
            // OpenCode resolves its project from --dir, not from the cwd of
            // the shell that launched it.
            ArgPart::lit("--dir"),
            ArgPart::lit(req.cwd.to_string_lossy().to_string()),
            // Ask for reasoning, always. Without this flag OpenCode emits no
            // `reasoning` parts at all, so `parse_line`'s handling of them
            // below — and the TUI's whole "show thinking" toggle — would be
            // switching between nothing and nothing.
            //
            // Requested here rather than made conditional on whether anyone is
            // currently looking: the transcript is stored, and a conversation
            // read back tomorrow should not be missing its reasoning because a
            // toggle was off yesterday. What to *display* is a question for the
            // screen; what to *record* is not.
            ArgPart::lit("--thinking"),
        ];
        if let Some(model) = &req.model {
            args.push(ArgPart::lit("--model"));
            args.push(ArgPart::lit(model));
        }
        match &req.resume {
            Resume::Fresh => {}
            Resume::Last => args.push(ArgPart::lit("--continue")),
            Resume::Session(id) => {
                args.push(ArgPart::lit("--session"));
                args.push(ArgPart::lit(id));
            }
        }
        // OpenCode has one auto-approve switch and nothing else — `opencode run
        // --help` offers `--auto` and no mode flag at all. So three of Jod's
        // four levels collapse to "leave it off": it cannot separate edits from
        // other tool calls, and it has no plan mode to ask for. Reported here
        // rather than faked, because a `--mode plan` OpenCode ignores would look
        // like a working plan mode right up until something got written.
        if req.permission == PermissionPolicy::Bypass {
            args.push(ArgPart::lit("--auto"));
        }
        // The message is positional and must come last.
        args.push(ArgPart::Prompt);
        args
    }

    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return vec![];
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return vec![AgentEvent::Raw {
                line: line.to_string(),
            }];
        };

        let mut out = vec![];
        if !self.announced_session {
            if let Some(session) = str_at(&v, "sessionID") {
                self.announced_session = true;
                out.push(AgentEvent::Started {
                    session_id: Some(session),
                    model: None,
                });
            }
        }

        let part = v.get("part");
        match v.get("type").and_then(Value::as_str) {
            Some("step_start") => {}
            Some("text") => {
                if let Some(text) = self.take_completed_part(part, "text") {
                    if !text.trim().is_empty() {
                        self.acc.note_text(&text);
                        out.push(AgentEvent::Message { text });
                    }
                }
            }
            Some("reasoning") => {
                if let Some(text) = self.take_completed_part(part, "text") {
                    if !text.trim().is_empty() {
                        out.push(AgentEvent::Thinking { text });
                    }
                }
            }
            // The envelope type is `tool_use`; `tool` is the inner part's type.
            // Both are accepted so a rename in either place stays handled.
            Some("tool_use") | Some("tool") => {
                if let Some(event) = self.parse_tool(part) {
                    out.push(event);
                }
            }
            Some("step_finish") => {
                if let Some(part) = part {
                    if str_at(part, "reason").as_deref() == Some("error") {
                        self.acc.errored = true;
                    }
                    self.acc.add_usage(&usage_from(part));
                }
            }
            Some("error") => {
                self.acc.errored = true;
                out.push(AgentEvent::Error {
                    message: v
                        .get("error")
                        .map(error_message)
                        .unwrap_or_else(|| line.to_string()),
                });
            }
            _ => out.push(AgentEvent::Raw {
                line: line.to_string(),
            }),
        }
        out
    }

    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent {
        self.acc.finish(exit_code)
    }
}

impl OpenCode {
    /// Return a part's field only once it is complete and not yet surfaced.
    fn take_completed_part(&mut self, part: Option<&Value>, field: &str) -> Option<String> {
        let part = part?;
        // A part with a `time` object but no `end` is still streaming.
        if let Some(time) = part.get("time") {
            time.get("end")?;
        }
        if let Some(id) = str_at(part, "id") {
            if !self.seen_parts.insert(id) {
                return None;
            }
        }
        str_at(part, field)
    }

    fn parse_tool(&mut self, part: Option<&Value>) -> Option<AgentEvent> {
        let part = part?;
        let name = str_at(part, "tool")
            .or_else(|| str_at(part, "name"))
            .unwrap_or_else(|| "tool".into());
        let state = part.get("state");
        let status = state.and_then(|s| str_at(s, "status")).unwrap_or_default();

        match status.as_str() {
            "completed" | "error" => {
                // Not latched onto the run: a failed tool is recoverable, and
                // the agent usually does recover. `step_finish` with
                // `reason: "error"` is the run-level signal. See the note in
                // the Claude adapter.
                let is_error = status == "error";
                // Distinguish the completed part from the earlier running part,
                // which shares the same part id.
                if let Some(id) = str_at(part, "id") {
                    if !self.seen_parts.insert(format!("{id}:done")) {
                        return None;
                    }
                }
                Some(AgentEvent::ToolResult {
                    name,
                    summary: state
                        .and_then(|s| s.get("output").or_else(|| s.get("error")))
                        .map(|o| summarize(o, 400)),
                    is_error,
                })
            }
            _ => {
                if let Some(id) = str_at(part, "id") {
                    if !self.seen_parts.insert(format!("{id}:call")) {
                        return None;
                    }
                }
                Some(AgentEvent::ToolCall {
                    name,
                    input: state.and_then(|s| s.get("input")).cloned(),
                })
            }
        }
    }
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// What an OpenCode error actually says, rather than the object it says it in.
///
/// Every error OpenCode defines — `ProviderAuthError`, `UnknownError`,
/// `MessageAbortedError`, `APIError` — is `{name, data: {message, ...}}`, and
/// the sentence a person needs is always `data.message`. Serialising the whole
/// object instead buries it: a 401 from a workspace with no payment method
/// arrived on screen as four hundred characters of escaped JSON with the
/// response headers, the response body and the request URL in front of "add a
/// payment method here". That is the difference between a billing problem you
/// fix in a minute and a harness that looks broken for no stated reason.
///
/// `name` is kept in front of it because the message alone does not say whether
/// the run was refused, aborted or cut short, and those are different problems.
/// The whole object is still the fallback: an error shape nobody anticipated is
/// better shown raw than swallowed.
fn error_message(error: &Value) -> String {
    let said = error
        .get("data")
        .and_then(|d| d.get("message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty());
    match (said, str_at(error, "name")) {
        (Some(said), Some(name)) => summarize(&Value::String(format!("{name}: {said}")), 400),
        (Some(said), None) => summarize(&Value::String(said.to_string()), 400),
        (None, _) => summarize(error, 400),
    }
}

fn usage_from(part: &Value) -> Usage {
    let t = part.get("tokens");
    let cache = t.and_then(|t| t.get("cache"));
    Usage {
        input_tokens: t.and_then(|t| t.get("input")).and_then(Value::as_u64),
        output_tokens: t.and_then(|t| t.get("output")).and_then(Value::as_u64),
        cache_read_tokens: cache.and_then(|c| c.get("read")).and_then(Value::as_u64),
        cache_write_tokens: cache.and_then(|c| c.get("write")).and_then(Value::as_u64),
        cost_usd: part.get("cost").and_then(Value::as_f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn req(permission: PermissionPolicy, model: Option<&str>) -> SpawnRequest {
        SpawnRequest {
            name: "t".into(),
            harness: HarnessKind::OpenCode,
            prompt: "hi".into(),
            system: None,
            cwd: PathBuf::from("/work"),
            model: model.map(str::to_string),
            permission,
            resume: Resume::Fresh,
            tools: None,
            ..SpawnRequest::default()
        }
    }

    #[test]
    fn the_prompt_is_the_last_argument() {
        let a = OpenCode::default().args(&req(PermissionPolicy::Ask, None));
        assert_eq!(a.last(), Some(&ArgPart::Prompt));
        assert!(a.contains(&ArgPart::lit("run")));
        assert!(a.contains(&ArgPart::lit("json")));
    }

    #[test]
    fn the_working_directory_is_passed_via_dir() {
        let a = OpenCode::default().args(&req(PermissionPolicy::Ask, None));
        let i = a.iter().position(|x| *x == ArgPart::lit("--dir")).unwrap();
        assert_eq!(a[i + 1], ArgPart::lit("/work"));
    }

    /// Reasoning has to be asked for. `parse_line` below turns OpenCode's
    /// `reasoning` parts into `AgentEvent::Thinking`, and without this flag
    /// OpenCode never sends any — so the parsing, the storage and the TUI's
    /// "show thinking" toggle would all have been correct and all been
    /// switching between nothing and nothing.
    #[test]
    fn reasoning_is_requested_so_there_is_something_to_show() {
        let a = OpenCode::default().args(&req(PermissionPolicy::Bypass, None));
        assert!(a.contains(&ArgPart::lit("--thinking")));
    }

    /// And in every mode, because what is *recorded* is not a display setting:
    /// a conversation read back tomorrow should not be missing its reasoning.
    #[test]
    fn reasoning_is_requested_whatever_the_permission_mode() {
        for policy in PermissionPolicy::ALL {
            assert!(
                OpenCode::default()
                    .args(&req(policy, None))
                    .contains(&ArgPart::lit("--thinking")),
                "{policy:?} would record no reasoning"
            );
        }
    }

    #[test]
    fn only_bypass_enables_auto_approval() {
        for policy in [
            PermissionPolicy::Plan,
            PermissionPolicy::Ask,
            PermissionPolicy::AcceptEdits,
        ] {
            let a = OpenCode::default().args(&req(policy, None));
            assert!(
                !a.contains(&ArgPart::lit("--auto")),
                "{policy:?} must not auto-approve"
            );
        }
        let a = OpenCode::default().args(&req(PermissionPolicy::Bypass, None));
        assert!(a.contains(&ArgPart::lit("--auto")));
    }

    #[test]
    fn the_first_line_announces_the_session_once() {
        let mut h = OpenCode::default();
        let first = h.parse_line(r#"{"type":"step_start","sessionID":"ses_1","part":{"id":"p0"}}"#);
        assert_eq!(
            first,
            vec![AgentEvent::Started {
                session_id: Some("ses_1".into()),
                model: None
            }]
        );
        let second =
            h.parse_line(r#"{"type":"step_start","sessionID":"ses_1","part":{"id":"p1"}}"#);
        assert!(second.is_empty(), "session must not be announced twice");
    }

    #[test]
    fn a_completed_text_part_becomes_a_message() {
        let mut h = OpenCode::default();
        h.parse_line(r#"{"type":"step_start","sessionID":"ses_1","part":{"id":"p0"}}"#);
        let out = h.parse_line(
            r#"{"type":"text","sessionID":"ses_1","part":{"id":"prt_1","type":"text",
                "text":"PONG","time":{"start":1,"end":2}}}"#,
        );
        assert_eq!(
            out,
            vec![AgentEvent::Message {
                text: "PONG".into()
            }]
        );
    }

    #[test]
    fn a_still_streaming_part_is_withheld_until_it_completes() {
        let mut h = OpenCode::default();
        let partial = h.parse_line(
            r#"{"type":"text","part":{"id":"prt_1","type":"text","text":"PO","time":{"start":1}}}"#,
        );
        assert!(partial.is_empty());
        let done = h.parse_line(
            r#"{"type":"text","part":{"id":"prt_1","type":"text","text":"PONG","time":{"start":1,"end":2}}}"#,
        );
        assert_eq!(
            done,
            vec![AgentEvent::Message {
                text: "PONG".into()
            }]
        );
    }

    #[test]
    fn a_repeated_completed_part_is_not_shown_twice() {
        let mut h = OpenCode::default();
        let line = r#"{"type":"text","part":{"id":"prt_1","type":"text","text":"PONG","time":{"start":1,"end":2}}}"#;
        assert_eq!(h.parse_line(line).len(), 1);
        assert!(h.parse_line(line).is_empty());
    }

    #[test]
    fn a_running_tool_then_its_completion_produce_a_call_and_a_result() {
        let mut h = OpenCode::default();
        let call = h.parse_line(
            r#"{"type":"tool","part":{"id":"prt_t","tool":"bash","state":{"status":"running","input":{"cmd":"ls"}}}}"#,
        );
        assert_eq!(
            call,
            vec![AgentEvent::ToolCall {
                name: "bash".into(),
                input: Some(serde_json::json!({"cmd": "ls"}))
            }]
        );
        let result = h.parse_line(
            r#"{"type":"tool","part":{"id":"prt_t","tool":"bash","state":{"status":"completed","output":"a.txt"}}}"#,
        );
        assert_eq!(
            result,
            vec![AgentEvent::ToolResult {
                name: "bash".into(),
                summary: Some("a.txt".into()),
                is_error: false
            }]
        );
    }

    /// Captured verbatim from `opencode run --format json`. A non-interactive
    /// run reports a tool once, already completed — there is no `running` step.
    #[test]
    fn a_real_tool_use_line_becomes_a_tool_result() {
        let mut h = OpenCode::default();
        let out = h.parse_line(
            r#"{"type":"tool_use","timestamp":1786201602758,"sessionID":"ses_1","part":{
                "type":"tool","tool":"bash","callID":"toolu_1",
                "state":{"status":"completed","input":{"command":"echo hello-from-tool"},
                         "output":"hello-from-tool\n","title":"echo hello-from-tool",
                         "time":{"start":1,"end":2}},
                "id":"prt_1","sessionID":"ses_1","messageID":"msg_1"}}"#,
        );
        assert_eq!(
            out,
            vec![
                AgentEvent::Started {
                    session_id: Some("ses_1".into()),
                    model: None
                },
                AgentEvent::ToolResult {
                    name: "bash".into(),
                    summary: Some("hello-from-tool".into()),
                    is_error: false
                }
            ]
        );
    }

    /// A tool may fail without the run failing — agents retry, and usually
    /// succeed. Only the run-level signal decides the run.
    #[test]
    fn a_failed_tool_does_not_fail_the_whole_run() {
        let mut h = OpenCode::default();
        let out = h.parse_line(
            r#"{"type":"tool","part":{"id":"p","tool":"bash","state":{"status":"error","error":"nope"}}}"#,
        );
        // The tool itself is still reported as failed.
        assert!(out
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolResult { is_error: true, .. })));
        match h.finalize(Some(0)) {
            AgentEvent::Finished { is_error, .. } => assert!(!is_error),
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn a_step_that_ends_in_error_does_fail_the_run() {
        let mut h = OpenCode::default();
        h.parse_line(r#"{"type":"step_finish","part":{"reason":"error"}}"#);
        match h.finalize(Some(0)) {
            AgentEvent::Finished { is_error, .. } => assert!(is_error),
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn step_finish_tokens_land_in_the_final_usage() {
        let mut h = OpenCode::default();
        let out = h.parse_line(
            r#"{"type":"step_finish","part":{"reason":"stop","cost":0,
                "tokens":{"total":9589,"input":7781,"output":16,"cache":{"write":0,"read":1792}}}}"#,
        );
        assert!(out.is_empty(), "step_finish is bookkeeping, not a UI event");
        match h.finalize(Some(0)) {
            AgentEvent::Finished {
                usage, is_error, ..
            } => {
                assert!(!is_error);
                assert_eq!(usage.input_tokens, Some(7781));
                assert_eq!(usage.output_tokens, Some(16));
                assert_eq!(usage.cache_read_tokens, Some(1792));
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn output_tokens_accumulate_across_steps() {
        let mut h = OpenCode::default();
        h.parse_line(
            r#"{"type":"step_finish","part":{"reason":"stop","tokens":{"input":100,"output":10}}}"#,
        );
        h.parse_line(
            r#"{"type":"step_finish","part":{"reason":"stop","tokens":{"input":140,"output":7}}}"#,
        );
        match h.finalize(Some(0)) {
            AgentEvent::Finished { usage, .. } => {
                assert_eq!(usage.output_tokens, Some(17));
                assert_eq!(usage.input_tokens, Some(140));
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    /// Captured verbatim from `opencode run --format json` against a workspace
    /// with no payment method. Every model in the picker failed this way, so
    /// changing model looked like it did nothing — and the one sentence that
    /// explained why sat behind the response headers, the response body and the
    /// request URL, past the point the transcript truncates.
    #[test]
    fn an_api_error_reads_as_its_message_not_as_its_json() {
        let mut h = OpenCode::default();
        let out = h.parse_line(
            r#"{"type":"error","timestamp":1786520295803,"sessionID":"ses_1","error":{
                "name":"APIError","data":{
                "message":"No payment method. Add a payment method here: https://opencode.ai/workspace/wrk_1/billing",
                "statusCode":401,"isRetryable":false,
                "responseHeaders":{"cf-ray":"a29dd1c78a8023ba-LAX","content-length":"175"},
                "responseBody":"{\"type\":\"error\"}","metadata":{"url":"https://opencode.ai/zen/v1/messages"}}}}"#,
        );
        let messages: Vec<_> = out
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Error { message } => Some(message.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            messages,
            vec![
                "APIError: No payment method. Add a payment method here: \
                 https://opencode.ai/workspace/wrk_1/billing"
            ]
        );
    }

    /// The other error shapes OpenCode defines carry the sentence in the same
    /// place, so one rule covers all of them.
    #[test]
    fn every_error_shape_puts_its_sentence_in_the_same_place() {
        let mut h = OpenCode::default();
        let out = h.parse_line(
            r#"{"type":"error","error":{"name":"ProviderAuthError",
                "data":{"providerID":"opencode","message":"not logged in"}}}"#,
        );
        assert_eq!(
            out,
            vec![AgentEvent::Error {
                message: "ProviderAuthError: not logged in".into()
            }]
        );
    }

    /// An error shape nobody anticipated is still shown. Reporting nothing
    /// because a field was missing is how a failed run becomes a silent one.
    #[test]
    fn an_error_with_no_message_is_still_reported_in_full() {
        let mut h = OpenCode::default();
        let out = h.parse_line(r#"{"type":"error","error":{"name":"Odd","data":{"code":7}}}"#);
        assert_eq!(
            out,
            vec![AgentEvent::Error {
                message: r#"{"data":{"code":7},"name":"Odd"}"#.into()
            }]
        );
    }

    #[test]
    fn unknown_event_types_are_kept_as_raw() {
        let mut h = OpenCode::default();
        let out = h.parse_line(r#"{"type":"something_new","part":{}}"#);
        assert!(matches!(out.as_slice(), [AgentEvent::Raw { .. }]));
    }

    #[test]
    fn non_json_output_is_kept_as_raw() {
        let mut h = OpenCode::default();
        assert_eq!(
            h.parse_line("plain warning"),
            vec![AgentEvent::Raw {
                line: "plain warning".into()
            }]
        );
    }
}
