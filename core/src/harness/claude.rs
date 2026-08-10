//! Claude Code adapter — `claude -p <prompt> --output-format stream-json --verbose`.

use std::collections::HashMap;

use serde_json::Value;

use super::{Accumulator, ArgPart, Harness, HarnessKind, PermissionPolicy, Resume, SpawnRequest};
use crate::event::{summarize, AgentEvent, Usage};

/// Tools `PermissionPolicy::Ask` hands over without being asked.
///
/// Every one of them only *reads* — the filesystem or the web. Nothing here
/// can write a file, run a command or change anything outside the agent's own
/// answer, so granting them up front costs the caller no ground it could have
/// defended anyway: under `-p` the alternative is a silent denial, not a
/// prompt. Anything that mutates still needs `--permission accept-edits` or
/// `--permission bypass`.
const READ_ONLY_TOOLS: &[&str] = &["Read", "Grep", "Glob", "WebSearch", "WebFetch"];

#[derive(Default)]
pub struct ClaudeCode {
    acc: Accumulator,
    /// `tool_use_id` → tool name. Claude's tool *results* only carry the id, so
    /// we remember the name from the matching call to label the result.
    tool_names: HashMap<String, String>,
}

impl Harness for ClaudeCode {
    fn kind(&self) -> HarnessKind {
        HarnessKind::ClaudeCode
    }

    fn args(&self, req: &SpawnRequest) -> Vec<ArgPart> {
        let mut args = vec![
            ArgPart::lit("-p"),
            ArgPart::Prompt,
            ArgPart::lit("--output-format"),
            ArgPart::lit("stream-json"),
            // stream-json in headless mode requires --verbose.
            ArgPart::lit("--verbose"),
        ];
        if let Some(model) = &req.model {
            args.push(ArgPart::lit("--model"));
            args.push(ArgPart::lit(model));
        }
        match &req.resume {
            Resume::Fresh => {}
            Resume::Last => args.push(ArgPart::lit("--continue")),
            Resume::Session(id) => {
                args.push(ArgPart::lit("--resume"));
                args.push(ArgPart::lit(id));
            }
        }
        match req.permission {
            // Nobody is at the other end of `-p` to answer a prompt, so "ask"
            // is really "deny" — and a bare `claude -p` refused to so much as
            // search the web. Allow the tools that cannot change anything, and
            // keep denying the rest.
            PermissionPolicy::Ask => {
                args.push(ArgPart::lit("--allowedTools"));
                let mut allowed: Vec<String> =
                    READ_ONLY_TOOLS.iter().map(|t| t.to_string()).collect();
                // Jod's own tools have to be named here or they are denied,
                // however carefully they were granted. Found the hard way: the
                // `--mcp-config` flag reached the command line, the server
                // started, and the agent still reported "no jod tools" —
                // because this allowlist, one line above, did not mention them.
                //
                // Server-wide rather than per tool, because which tools exist
                // is already decided by the access level the config carries.
                // Listing them again here would be a second copy of that
                // decision, free to drift from the first.
                if req.tools.is_some() {
                    allowed.push(format!("mcp__{}", crate::mcp_config::SERVER_NAME));
                }
                args.push(ArgPart::lit(allowed.join(",")));
            }
            PermissionPolicy::AcceptEdits => {
                args.push(ArgPart::lit("--permission-mode"));
                args.push(ArgPart::lit("acceptEdits"));
            }
            PermissionPolicy::Bypass => args.push(ArgPart::lit("--dangerously-skip-permissions")),
        }

        // Jod's own tools, if this run was granted any. Without these two flags
        // `SpawnRequest::tools` is decoration — set, capped, tested, and
        // reaching no command line, which is precisely the failure this branch
        // keeps producing.
        //
        // `--strict-mcp-config` matters as much as the config itself: without
        // it Claude Code also loads whatever MCP servers the *user's* own
        // configuration names, so an agent Jod meant to hold read-only tools
        // could quietly inherit a filesystem server from `~/.claude.json`. The
        // grant has to be exactly what Jod granted.
        if let Some(access) = req.tools {
            if let Ok(path) = crate::mcp_config::config_for(access, &crate::paths::jod_home()) {
                args.push(ArgPart::lit("--mcp-config"));
                args.push(ArgPart::lit(path.to_string_lossy()));
                args.push(ArgPart::lit("--strict-mcp-config"));
            }
            // A failure to write the config is deliberately not fatal. The run
            // still does its work with no Jod tools, which is worse than
            // intended and far better than refusing to start at all — and the
            // agent will say it has no tools rather than pretending otherwise.
        }
        args
    }

    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return vec![];
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            // Claude prints human-readable warnings (e.g. workspace trust) to
            // the same stream. Surface them rather than dropping them.
            return vec![AgentEvent::Raw {
                line: strip_ansi(line),
            }];
        };

        match v.get("type").and_then(Value::as_str) {
            Some("system") => match v.get("subtype").and_then(Value::as_str) {
                Some("init") => vec![AgentEvent::Started {
                    session_id: str_at(&v, "session_id"),
                    model: str_at(&v, "model"),
                }],
                // hook_started / hook_response / thinking_tokens are bookkeeping.
                _ => vec![],
            },
            Some("assistant") => self.parse_assistant(&v),
            Some("user") => self.parse_tool_results(&v),
            Some("result") => {
                if let Some(text) = str_at(&v, "result") {
                    self.acc.note_text(&text);
                }
                if v.get("is_error").and_then(Value::as_bool) == Some(true) {
                    self.acc.errored = true;
                }
                self.acc.add_usage(&usage_from(&v));
                // The run is only *over* when the process exits; see finalize.
                vec![]
            }
            Some("rate_limit_event") => vec![],
            _ => vec![AgentEvent::Raw {
                line: strip_ansi(line),
            }],
        }
    }

    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent {
        self.acc.finish(exit_code)
    }
}

impl ClaudeCode {
    fn parse_assistant(&mut self, v: &Value) -> Vec<AgentEvent> {
        let mut out = vec![];
        for block in blocks(v) {
            match block.get("type").and_then(Value::as_str) {
                Some("thinking") => {
                    if let Some(t) = str_at(block, "thinking") {
                        out.push(AgentEvent::Thinking { text: t });
                    }
                }
                Some("text") => {
                    if let Some(t) = str_at(block, "text") {
                        if !t.trim().is_empty() {
                            self.acc.note_text(&t);
                            out.push(AgentEvent::Message { text: t });
                        }
                    }
                }
                Some("tool_use") => {
                    let name = str_at(block, "name").unwrap_or_else(|| "tool".into());
                    if let Some(id) = str_at(block, "id") {
                        self.tool_names.insert(id, name.clone());
                    }
                    out.push(AgentEvent::ToolCall {
                        name,
                        input: block.get("input").cloned(),
                    });
                }
                _ => {}
            }
        }
        out
    }

    fn parse_tool_results(&mut self, v: &Value) -> Vec<AgentEvent> {
        let mut out = vec![];
        for block in blocks(v) {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let name = str_at(block, "tool_use_id")
                .and_then(|id| self.tool_names.get(&id).cloned())
                .unwrap_or_else(|| "tool".into());
            let is_error = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            // Deliberately *not* latched onto the run. A tool that fails is
            // ordinary and usually recoverable — an agent that tries `python`,
            // is told there is no such command, and succeeds with `python3` has
            // not failed. Marking the run here reported `✗ failed` for work that
            // finished correctly. Whether the *run* failed is the harness's own
            // `result.is_error` and the exit code; this flag stays on the tool.
            out.push(AgentEvent::ToolResult {
                name,
                summary: block.get("content").map(|c| summarize(c, 400)),
                is_error,
            });
        }
        out
    }
}

fn blocks(v: &Value) -> impl Iterator<Item = &Value> {
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or(&[])
        .iter()
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn u64_at(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

fn usage_from(v: &Value) -> Usage {
    let u = v.get("usage");
    Usage {
        input_tokens: u.and_then(|u| u64_at(u, "input_tokens")),
        output_tokens: u.and_then(|u| u64_at(u, "output_tokens")),
        cache_read_tokens: u.and_then(|u| u64_at(u, "cache_read_input_tokens")),
        cache_write_tokens: u.and_then(|u| u64_at(u, "cache_creation_input_tokens")),
        cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
    }
}

/// Drop ANSI colour codes so warnings render cleanly in the UI.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // Consume the escape sequence up to its terminating letter.
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn req(permission: PermissionPolicy, model: Option<&str>) -> SpawnRequest {
        SpawnRequest {
            name: "t".into(),
            harness: HarnessKind::ClaudeCode,
            prompt: "hi".into(),
            cwd: PathBuf::from("/tmp"),
            model: model.map(str::to_string),
            permission,
            resume: Resume::Fresh,
            tools: None,
        }
    }

    /// The test that would have caught `tools` being decoration. It was set,
    /// capped and unit-tested for hours while reaching no command line at all —
    /// a component complete, tested, and wired to nothing.
    #[test]
    fn a_granted_tool_level_reaches_the_command_line() {
        let mut r = req(PermissionPolicy::Ask, None);
        r.tools = Some(super::super::ToolAccess::ReadOnly);
        let args = ClaudeCode::default().args(&r);

        assert!(
            args.contains(&ArgPart::lit("--mcp-config")),
            "no MCP config reached the harness: {args:?}"
        );
        // Without this, Claude Code also loads whatever servers the user's own
        // configuration names, so a read-only agent could inherit a filesystem
        // server from ~/.claude.json. The grant must be exactly Jod's.
        assert!(
            args.contains(&ArgPart::lit("--strict-mcp-config")),
            "the grant was not restricted to Jod's own servers: {args:?}"
        );
    }

    /// The bug a unit test could not have caught and a real run did: every flag
    /// was correct, the server started, and the agent still had no tools —
    /// because the permission allowlist one line above never mentioned them.
    #[test]
    fn granted_tools_are_also_allowed_by_the_permission_policy() {
        let mut r = req(PermissionPolicy::Ask, None);
        r.tools = Some(super::super::ToolAccess::ReadOnly);
        let args = ClaudeCode::default().args(&r);

        let allowed = args
            .iter()
            .filter_map(|a| match a {
                ArgPart::Literal(s) => Some(s.clone()),
                _ => None,
            })
            .find(|s| s.contains("Read,Grep"))
            .expect("an allowlist");
        assert!(
            allowed.contains("mcp__jod"),
            "granted tools are denied by the allowlist: {allowed}"
        );
    }

    /// And the converse: an agent granted nothing must not be handed the
    /// server name either, or the allowlist would advertise what does not exist.
    #[test]
    fn an_agent_granted_nothing_is_not_allowed_jods_tools() {
        let args = ClaudeCode::default().args(&req(PermissionPolicy::Ask, None));
        let allowed = args
            .iter()
            .filter_map(|a| match a {
                ArgPart::Literal(s) => Some(s.clone()),
                _ => None,
            })
            .find(|s| s.contains("Read,Grep"))
            .expect("an allowlist");
        assert!(!allowed.contains("mcp__jod"), "{allowed}");
    }

    #[test]
    fn an_agent_granted_nothing_is_handed_no_mcp_config() {
        let args = ClaudeCode::default().args(&req(PermissionPolicy::Ask, None));
        assert!(!args.contains(&ArgPart::lit("--mcp-config")));
        assert!(!args.contains(&ArgPart::lit("--strict-mcp-config")));
    }

    /// The level has to travel in the config the harness is handed, because the
    /// server has nothing else to go on — there is no handshake in which an
    /// agent could claim one.
    #[test]
    fn the_config_handed_over_names_the_level_that_was_granted() {
        for access in [
            super::super::ToolAccess::ReadOnly,
            super::super::ToolAccess::Delegate,
            super::super::ToolAccess::Orchestrate,
        ] {
            let mut r = req(PermissionPolicy::Ask, None);
            r.tools = Some(access);
            let args = ClaudeCode::default().args(&r);
            let path = args
                .iter()
                .filter_map(|a| match a {
                    ArgPart::Literal(s) => Some(s.clone()),
                    _ => None,
                })
                .find(|s| s.ends_with(".json"))
                .expect("a config path");
            assert!(path.contains(access.as_str()), "{path} is not {access:?}");
        }
    }

    #[test]
    fn args_always_request_streaming_json_with_verbose() {
        let a = ClaudeCode::default().args(&req(PermissionPolicy::Ask, None));
        assert!(a.contains(&ArgPart::lit("--output-format")));
        assert!(a.contains(&ArgPart::lit("stream-json")));
        assert!(a.contains(&ArgPart::lit("--verbose")));
        assert!(a.contains(&ArgPart::Prompt));
    }

    #[test]
    fn permission_policies_map_to_distinct_flags() {
        let ask = ClaudeCode::default().args(&req(PermissionPolicy::Ask, None));
        assert!(!ask.contains(&ArgPart::lit("--permission-mode")));
        assert!(!ask.contains(&ArgPart::lit("--dangerously-skip-permissions")));

        let edits = ClaudeCode::default().args(&req(PermissionPolicy::AcceptEdits, None));
        assert!(edits.contains(&ArgPart::lit("acceptEdits")));

        let bypass = ClaudeCode::default().args(&req(PermissionPolicy::Bypass, None));
        assert!(bypass.contains(&ArgPart::lit("--dangerously-skip-permissions")));
    }

    /// The default policy has to leave the agent able to *look things up*.
    /// Without this, `claude -p` denied WebSearch and a question as ordinary as
    /// the weather came back as "I need permission".
    #[test]
    fn asking_still_grants_the_tools_that_only_read() {
        let a = ClaudeCode::default().args(&req(PermissionPolicy::Ask, None));
        let i = a
            .iter()
            .position(|p| p == &ArgPart::lit("--allowedTools"))
            .expect("--allowedTools must be passed");
        let ArgPart::Literal(list) = &a[i + 1] else {
            panic!("--allowedTools must be followed by a list");
        };
        for tool in READ_ONLY_TOOLS {
            assert!(list.split(',').any(|t| t == *tool), "{tool} is not granted");
        }
    }

    /// The point of `Ask` is that it still refuses everything that can change
    /// something. If a mutating tool ever joins the grant list, this fails.
    #[test]
    fn asking_never_grants_a_tool_that_can_change_anything() {
        for tool in ["Bash", "Write", "Edit", "NotebookEdit", "Task"] {
            assert!(
                !READ_ONLY_TOOLS.contains(&tool),
                "{tool} can mutate and must not be granted without being asked"
            );
        }
    }

    #[test]
    fn model_is_forwarded_only_when_set() {
        let none = ClaudeCode::default().args(&req(PermissionPolicy::Ask, None));
        assert!(!none.contains(&ArgPart::lit("--model")));
        let some = ClaudeCode::default().args(&req(PermissionPolicy::Ask, Some("haiku")));
        assert!(some.contains(&ArgPart::lit("haiku")));
    }

    #[test]
    fn init_line_becomes_started() {
        let mut h = ClaudeCode::default();
        let out = h.parse_line(
            r#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-haiku-4-5"}"#,
        );
        assert_eq!(
            out,
            vec![AgentEvent::Started {
                session_id: Some("s1".into()),
                model: Some("claude-haiku-4-5".into())
            }]
        );
    }

    #[test]
    fn bookkeeping_system_lines_produce_nothing() {
        let mut h = ClaudeCode::default();
        assert!(h
            .parse_line(r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":5}"#)
            .is_empty());
        assert!(h
            .parse_line(r#"{"type":"rate_limit_event","rate_limit_info":{}}"#)
            .is_empty());
    }

    #[test]
    fn an_assistant_turn_yields_thinking_then_text() {
        let mut h = ClaudeCode::default();
        let out = h.parse_line(
            r#"{"type":"assistant","message":{"content":[
                {"type":"thinking","thinking":"pondering"},
                {"type":"text","text":"PONG"}]}}"#,
        );
        assert_eq!(
            out,
            vec![
                AgentEvent::Thinking {
                    text: "pondering".into()
                },
                AgentEvent::Message {
                    text: "PONG".into()
                },
            ]
        );
    }

    #[test]
    fn empty_text_blocks_are_dropped() {
        let mut h = ClaudeCode::default();
        let out = h.parse_line(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"  "}]}}"#,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_tool_result_is_labelled_from_its_matching_call() {
        let mut h = ClaudeCode::default();
        h.parse_line(
            r#"{"type":"assistant","message":{"content":[
                {"type":"tool_use","id":"tu_1","name":"Bash","input":{"command":"ls"}}]}}"#,
        );
        let out = h.parse_line(
            r#"{"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":"tu_1","content":"a.txt","is_error":false}]}}"#,
        );
        assert_eq!(
            out,
            vec![AgentEvent::ToolResult {
                name: "Bash".into(),
                summary: Some("a.txt".into()),
                is_error: false
            }]
        );
    }

    /// The case seen live: `python` is missing, the agent retries with
    /// `python3` and finishes correctly. The tool is marked failed; the run is
    /// not, because the harness's own result says the run succeeded.
    #[test]
    fn a_tool_that_fails_does_not_fail_a_run_that_recovered() {
        let mut h = ClaudeCode::default();
        h.parse_line(
            r#"{"type":"assistant","message":{"content":[
                {"type":"tool_use","id":"tu_1","name":"Bash","input":{"command":"python x.py"}}]}}"#,
        );
        let out = h.parse_line(
            r#"{"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":"tu_1","content":"command not found","is_error":true}]}}"#,
        );
        assert!(out
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolResult { is_error: true, .. })));

        h.parse_line(r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#);
        match h.finalize(Some(0)) {
            AgentEvent::Finished { is_error, .. } => {
                assert!(!is_error, "a recovered run must not be reported as failed")
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn an_unmatched_tool_result_still_reports_rather_than_vanishing() {
        let mut h = ClaudeCode::default();
        let out = h.parse_line(
            r#"{"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":"ghost","content":"x"}]}}"#,
        );
        assert_eq!(
            out,
            vec![AgentEvent::ToolResult {
                name: "tool".into(),
                summary: Some("x".into()),
                is_error: false
            }]
        );
    }

    #[test]
    fn the_result_line_feeds_finalize_instead_of_ending_the_run() {
        let mut h = ClaudeCode::default();
        let streamed = h.parse_line(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"PONG",
                "total_cost_usd":0.027,
                "usage":{"input_tokens":10,"output_tokens":44,"cache_read_input_tokens":17873,
                         "cache_creation_input_tokens":12487}}"#,
        );
        assert!(streamed.is_empty(), "result must not emit its own event");

        match h.finalize(Some(0)) {
            AgentEvent::Finished {
                text,
                is_error,
                usage,
                exit_code,
            } => {
                assert_eq!(text.as_deref(), Some("PONG"));
                assert!(!is_error);
                assert_eq!(exit_code, Some(0));
                assert_eq!(usage.output_tokens, Some(44));
                assert_eq!(usage.cost_usd, Some(0.027));
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn an_error_result_marks_the_run_failed() {
        let mut h = ClaudeCode::default();
        h.parse_line(r#"{"type":"result","is_error":true,"result":"boom"}"#);
        match h.finalize(Some(0)) {
            AgentEvent::Finished { is_error, .. } => assert!(is_error),
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn non_json_warnings_survive_as_raw_without_ansi_codes() {
        let mut h = ClaudeCode::default();
        let out = h.parse_line("\u{1b}[31mIgnoring 10 permissions.allow entries\u{1b}[0m");
        assert_eq!(
            out,
            vec![AgentEvent::Raw {
                line: "Ignoring 10 permissions.allow entries".into()
            }]
        );
    }

    #[test]
    fn blank_lines_are_ignored() {
        assert!(ClaudeCode::default().parse_line("   ").is_empty());
    }
}
