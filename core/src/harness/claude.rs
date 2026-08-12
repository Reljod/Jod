//! Claude Code adapter — `claude -p <prompt> --output-format stream-json --verbose`.

use std::collections::HashMap;

use serde_json::Value;

use super::{Accumulator, ArgPart, Harness, HarnessKind, PermissionPolicy, Resume, SpawnRequest};
use crate::event::{summarize, AgentEvent, Usage};

/// Tools `PermissionPolicy::Ask` hands over without being asked.
///
/// Every one of them only *reads*, so granting them up front costs nothing: the
/// alternative under `-p` is a silent denial rather than a prompt.
///
/// **This list is a convenience, not the boundary.** It was believed to be the
/// boundary and it is not: `--allowedTools` grants without prompting and denies
/// nothing, so a run holding exactly these five once ran Bash and wrote a file.
/// What actually confines an `Ask` run is `--permission-mode plan`, applied
/// beside this list. See the comment at the call site.
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

    fn takes_system_prompt(&self) -> bool {
        true
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
        // `--append-system-prompt` rather than `--system-prompt`: the latter
        // replaces Claude Code's own, which is where its tools, its editing
        // conventions and its safety framing live. Jod adds a role; it does not
        // want to reconstruct a working agent from scratch.
        if let Some(system) = &req.system {
            args.push(ArgPart::lit("--append-system-prompt"));
            args.push(ArgPart::lit(system.clone()));
        }

        // Built across both branches below: the permission mode contributes the
        // read-only set, `req.tools` contributes Jod's own server.
        let mut allowed: Vec<String> = Vec::new();
        // The mode names below are the six this build actually accepts —
        // `acceptEdits`, `auto`, `bypassPermissions`, `manual`, `dontAsk`,
        // `plan` — read off `claude --help` rather than assumed.
        match req.permission {
            PermissionPolicy::Plan => {
                // A *mode*, not an allowlist, and that distinction is a
                // security fix rather than a preference. `--allowedTools`
                // *grants without prompting* — it is not a boundary: a run
                // given `Read,Grep,Glob,WebSearch,WebFetch` cheerfully ran Bash
                // and wrote a file, with `permission_denials: []` in its own
                // result. A name blocklist is no better; `--disallowedTools
                // Bash` blocked Bash and the agent reached the same shell
                // through another tool. Enumerating the tools that can execute
                // is a race you lose on the next release.
                //
                // `plan` closes the class rather than the names — verified:
                // reads and reasoning still work, and every write path is
                // refused, including a Bash heredoc or `printf >`.
                args.push(ArgPart::lit("--permission-mode"));
                args.push(ArgPart::lit("plan"));
                allowed.extend(READ_ONLY_TOOLS.iter().map(|t| t.to_string()));
            }
            // `manual` is the mode that actually means "put it to a person".
            // Under `-p` there is nobody to put it to, so this denies rather
            // than blocks — which is why it is a mode you choose and not one
            // you get by default. It used to be spelled `plan`, and that one
            // substitution is why every unattended run described its work
            // instead of doing it.
            PermissionPolicy::Ask => {
                args.push(ArgPart::lit("--permission-mode"));
                args.push(ArgPart::lit("manual"));
            }
            PermissionPolicy::AcceptEdits => {
                args.push(ArgPart::lit("--permission-mode"));
                args.push(ArgPart::lit("acceptEdits"));
            }
            PermissionPolicy::Bypass => args.push(ArgPart::lit("--dangerously-skip-permissions")),
        }

        // Jod's own tools have to be named here or they are denied, however
        // carefully they were granted. Found the hard way twice, and the second
        // time was this line's own fault: the grant used to live *inside* the
        // `Ask` arm, so the moment a run needed a mode other than plan it
        // silently lost every Jod tool. `acceptEdits` auto-approves file edits
        // and nothing else, so the orchestrator — whose only job is to call
        // these tools — got four consecutive
        // "requested permissions ... but you haven't granted it yet" and
        // delegated nothing.
        //
        // The grant belongs to `req.tools`, which is what actually decides
        // whether this run has Jod tools. The permission mode bounds what the
        // run may do to the machine; it has no opinion about Jod's own verbs.
        //
        // Server-wide rather than per tool, because which tools exist is
        // already decided by the access level the config carries. Listing them
        // again here would be a second copy of that decision, free to drift
        // from the first.
        if req.tools.is_some() {
            allowed.push(format!("mcp__{}", crate::mcp_config::SERVER_NAME));
        }
        // The browser, whatever `req.tools` says. Reading a page is not one of
        // Jod's verbs, so it is not bounded by the level that governs them —
        // see `mcp_config::config_for`. Granted here rather than left to the
        // permission mode because `--allowedTools` is what stops the run being
        // asked to approve a fetch that nobody is present to approve.
        if crate::mcp_config::browser_available() {
            allowed.push(format!("mcp__{}", crate::mcp_config::BROWSER_SERVER_NAME));
        }
        if !allowed.is_empty() {
            args.push(ArgPart::lit("--allowedTools"));
            args.push(ArgPart::lit(allowed.join(",")));
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
        //
        // `req.tools` no longer decides *whether* there is a config, only what
        // is in it: a run granted none of Jod's verbs still gets the browser,
        // because reading a web page is not one of them.
        if let Ok(Some(path)) = crate::mcp_config::config_for(req.tools, &crate::paths::jod_home()) {
            args.push(ArgPart::lit("--mcp-config"));
            args.push(ArgPart::lit(path.to_string_lossy()));
            args.push(ArgPart::lit("--strict-mcp-config"));
        }
        // A failure to write the config is deliberately not fatal. The run
        // still does its work with no Jod tools, which is worse than
        // intended and far better than refusing to start at all — and the
        // agent will say it has no tools rather than pretending otherwise.
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
            system: None,
            cwd: PathBuf::from("/tmp"),
            model: model.map(str::to_string),
            permission,
            resume: Resume::Fresh,
            tools: None,
            ..SpawnRequest::default()
        }
    }

    /// The argv as strings, with the prompt placeholder spelled out.
    fn flat(r: &SpawnRequest) -> Vec<String> {
        ClaudeCode::default()
            .args(r)
            .iter()
            .map(|a| match a {
                ArgPart::Literal(s) => s.clone(),
                ArgPart::Prompt => "<PROMPT>".into(),
            })
            .collect()
    }

    /// Every mode maps to a spelling this build accepts.
    ///
    /// The six are what `claude --help` prints — `acceptEdits`, `auto`,
    /// `bypassPermissions`, `manual`, `dontAsk`, `plan`. Pinned here because a
    /// mode name Claude Code does not recognise is not a compile error, not a
    /// spawn error, and not visible until an agent quietly does the wrong
    /// amount. An earlier attempt at this seam designed against
    /// `--permission-prompt-tool`, which this build does not have at all.
    #[test]
    fn each_mode_names_something_the_binary_accepts() {
        const ACCEPTED: [&str; 6] = [
            "acceptEdits",
            "auto",
            "bypassPermissions",
            "manual",
            "dontAsk",
            "plan",
        ];
        for mode in PermissionPolicy::ALL {
            let args = flat(&req(mode, None));
            let Some(at) = args.iter().position(|a| a == "--permission-mode") else {
                // Bypass uses the standalone flag instead of a mode name.
                assert!(
                    args.contains(&"--dangerously-skip-permissions".to_string()),
                    "{mode:?} asked the harness for nothing at all"
                );
                continue;
            };
            let named = &args[at + 1];
            assert!(
                ACCEPTED.contains(&named.as_str()),
                "{mode:?} passes --permission-mode {named}, which this build rejects"
            );
        }
    }

    /// The specific substitution that made every run a planning run.
    #[test]
    fn asking_is_manual_and_only_planning_is_plan() {
        let asked = flat(&req(PermissionPolicy::Ask, None));
        assert!(asked.contains(&"manual".to_string()));
        assert!(!asked.contains(&"plan".to_string()), "ask is not plan");

        let planned = flat(&req(PermissionPolicy::Plan, None));
        assert!(planned.contains(&"plan".to_string()));
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
        let args = flat(&r);

        // Located by the flag rather than by the read-only names that used to
        // sit beside it: only plan mode seeds those, so searching for them was
        // asking "is this plan mode" while claiming to ask "were the granted
        // tools allowed". The property holds in every mode; the locator has to.
        let at = args
            .iter()
            .position(|a| a == "--allowedTools")
            .expect("an allowlist");
        assert!(
            args[at + 1].contains("mcp__jod"),
            "granted tools are denied by the allowlist: {}",
            args[at + 1]
        );
    }

    /// And the converse: an agent granted nothing must not be handed the
    /// server name either, or the allowlist would advertise what does not exist.
    #[test]
    fn an_agent_granted_nothing_is_not_allowed_jods_tools() {
        // `Plan` rather than `Ask`: this reads the read-only allowlist, which
        // is now plan mode's, not ask mode's.
        let args = ClaudeCode::default().args(&req(PermissionPolicy::Plan, None));
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

    /// The test above passed while the feature was broken, because it only ever
    /// asked about `Ask`. The grant used to be written inside that one arm, so
    /// changing a run's permission mode silently revoked every Jod tool it had
    /// been given — which is how the orchestrator came to call four tools and be
    /// refused all four. A grant that depends on the permission mode is not a
    /// grant, so check every mode.
    #[test]
    fn granted_tools_survive_any_permission_mode() {
        for permission in [
            PermissionPolicy::Plan,
            PermissionPolicy::Ask,
            PermissionPolicy::AcceptEdits,
            PermissionPolicy::Bypass,
        ] {
            let mut r = req(permission, None);
            r.tools = Some(super::super::ToolAccess::Orchestrate);
            let args = ClaudeCode::default().args(&r);

            let allowed = args
                .iter()
                .filter_map(|a| match a {
                    ArgPart::Literal(s) => Some(s.clone()),
                    _ => None,
                })
                .find(|s| s.contains("mcp__jod"));
            assert!(
                allowed.is_some(),
                "{permission:?} revoked the granted tools: {args:?}"
            );
        }
    }

    /// An agent granted nothing gets none of *Jod's* verbs.
    ///
    /// This used to assert that no `--mcp-config` was written at all, which is
    /// no longer the same statement: a run granted nothing may still be handed
    /// the browser, because reading a web page is not one of Jod's verbs. The
    /// invariant worth keeping is the one about Jod's own tools, and it is now
    /// asserted directly rather than inferred from the flag's absence — which
    /// also makes it independent of whether the machine running the test
    /// happens to have the browser installed.
    #[test]
    fn an_agent_granted_nothing_reaches_none_of_jods_own_tools() {
        let args = ClaudeCode::default().args(&req(PermissionPolicy::Ask, None));
        let granted: Vec<String> = args
            .iter()
            .filter_map(|a| match a {
                ArgPart::Literal(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(
            !granted.iter().any(|s| s.contains("mcp__jod")),
            "a run granted nothing reached Jod's tools: {args:?}"
        );
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

    /// Each policy maps to its own flags, and no two share one.
    ///
    /// The property under test — distinct policies produce distinct argv — is
    /// the same one this test has always had; it now covers four modes rather
    /// than three. The `Ask` case is the interesting one: it used to assert
    /// `plan`, which is precisely the conflation that made every Jod run a
    /// planning run.
    #[test]
    fn permission_policies_map_to_distinct_flags() {
        let plan = flat(&req(PermissionPolicy::Plan, None));
        assert!(plan.contains(&"plan".to_string()));
        assert!(!plan.contains(&"manual".to_string()));
        assert!(!plan.contains(&"--dangerously-skip-permissions".to_string()));

        let ask = flat(&req(PermissionPolicy::Ask, None));
        assert!(ask.contains(&"manual".to_string()));
        assert!(!ask.contains(&"plan".to_string()));
        assert!(!ask.contains(&"--dangerously-skip-permissions".to_string()));

        let edits = flat(&req(PermissionPolicy::AcceptEdits, None));
        assert!(edits.contains(&"acceptEdits".to_string()));

        let bypass = flat(&req(PermissionPolicy::Bypass, None));
        assert!(bypass.contains(&"--dangerously-skip-permissions".to_string()));

        // And no two modes produce the same command line.
        let all: Vec<Vec<String>> = PermissionPolicy::ALL
            .iter()
            .map(|m| flat(&req(*m, None)))
            .collect();
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two modes are indistinguishable to the harness");
            }
        }
    }

    /// The regression guard for a false security claim. `Ask` used to be Jod's
    /// default,
    /// and it was documented as denying everything not on the allowlist while
    /// denying nothing at all — a run holding only the five read tools ran Bash
    /// and wrote a file, its own result reporting `permission_denials: []`.
    ///
    /// The mode is what confines it. A tool *blocklist* is not a substitute and
    /// must not be swapped back in: blocking `Bash` by name left the agent free
    /// to reach the same shell through another tool, and enumerating everything
    /// that can execute is a race lost on the next release.
    #[test]
    fn planning_confines_the_run_by_mode_rather_than_by_a_list_of_names() {
        let a = ClaudeCode::default().args(&req(PermissionPolicy::Plan, None));
        let i = a
            .iter()
            .position(|p| p == &ArgPart::lit("--permission-mode"))
            .expect("`Plan` must set a permission mode, or it grants everything");
        assert_eq!(
            a[i + 1],
            ArgPart::lit("plan"),
            "only plan mode refuses every write path, including a Bash heredoc"
        );
    }

    /// The modes that are meant to permit work must not inherit the confinement.
    #[test]
    fn a_run_allowed_to_work_is_not_put_in_plan_mode() {
        for policy in [
            PermissionPolicy::Ask,
            PermissionPolicy::AcceptEdits,
            PermissionPolicy::Bypass,
        ] {
            let a = ClaudeCode::default().args(&req(policy, None));
            let plan = a
                .iter()
                .position(|p| p == &ArgPart::lit("--permission-mode"))
                .map(|i| a[i + 1] == ArgPart::lit("plan"))
                .unwrap_or(false);
            assert!(!plan, "{policy:?} was confined to planning");
        }
    }

    #[test]
    fn planning_still_grants_the_tools_that_only_read() {
        let a = ClaudeCode::default().args(&req(PermissionPolicy::Plan, None));
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
