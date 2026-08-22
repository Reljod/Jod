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

/// AGY's own default print timeout is 5 minutes, and it kills the run when it
/// expires. That is far too short for delegated work, and the failure looks
/// like a truncated answer rather than a timeout, so it is raised explicitly.
const PRINT_TIMEOUT: &str = "6h";

#[derive(Default)]
pub struct Agy {
    acc: Accumulator,
    /// The conversation we asked to resume, if any.
    ///
    /// AGY does not fail on an unknown conversation id — it silently starts a
    /// brand new conversation and reports success. Without this the caller
    /// would believe it was continuing a thread that had actually been lost.
    expected_session: std::cell::RefCell<Option<String>>,
    /// `step_index` → the prose accumulated for that step so far.
    ///
    /// AGY streams *fragments*: one step arrives ACTIVE carrying part of a
    /// sentence and again DONE carrying the rest. Surfacing each fragment as
    /// its own message splits words across lines ("I have creat" / "ed the
    /// file"), so they are joined and emitted once the step completes.
    partials: std::collections::HashMap<u64, String>,
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
            ArgPart::lit("--print-timeout"),
            ArgPart::lit(PRINT_TIMEOUT),
        ];
        // AGY resolves its workspace from its own settings, *not* from the cwd
        // of the shell that launched it. Without this it reports "there was no
        // active workspace set" and writes into
        // `~/.gemini/antigravity-cli/scratch` — while reporting success, so a
        // run that edited nothing in the repo looks like one that worked.
        args.push(ArgPart::lit("--add-dir"));
        args.push(ArgPart::lit(req.cwd.to_string_lossy().to_string()));
        // The same flag again, once per root. `agy --help` calls it repeatable
        // and it measurably is: a run given two of them listed exactly those
        // two as its workspace.
        //
        // That measurement also confirmed the grant above is not redundant.
        // Asked what its workspace contained, AGY named the two added
        // directories and *not* the shell's working directory — so a run whose
        // cwd went unpassed would have a workspace that simply omitted the
        // repository it was started in.
        //
        // Unlike Claude Code's, this flag is not variadic, so it is safe beside
        // the prompt. Granting still is not confining; see
        // `docs/harness-support.md`.
        for root in &req.roots {
            args.push(ArgPart::lit("--add-dir"));
            args.push(ArgPart::lit(root.to_string_lossy().to_string()));
        }
        if let Some(model) = &req.model {
            args.push(ArgPart::lit("--model"));
            args.push(ArgPart::lit(model));
        }
        // AGY spells reasoning effort the same way Claude Code does —
        // `--effort <level>` — but takes only `low`, `medium` and `high`, which
        // is why the value comes from `flag_value` rather than from `as_str`. A
        // level AGY has no word for produces no flag: passing `high` in place of
        // the `max` somebody asked for would be a setting that did something
        // other than what it says. `service::apply_role` refuses that
        // combination before it ever reaches here, and says so; this is the
        // backstop that keeps the wrong word out of the argv either way.
        //
        // AGY has a second channel for the same setting — a model name can
        // carry it, as in `gemini-3.6-flash-high` (`docs/harness-config.md`) —
        // so a role that sets both the model and the level has two sources of
        // truth and the model name is the one AGY reads last. Nothing here
        // tries to reconcile them; the flag is passed as asked.
        if let Some(level) = req.effort.and_then(|e| e.flag_value(HarnessKind::Agy)) {
            args.push(ArgPart::lit("--effort"));
            args.push(ArgPart::lit(level));
        }
        match &req.resume {
            Resume::Fresh => {}
            Resume::Last => args.push(ArgPart::lit("--continue")),
            Resume::Session(id) => {
                *self.expected_session.borrow_mut() = Some(id.clone());
                args.push(ArgPart::lit("--conversation"));
                args.push(ArgPart::lit(id));
            }
        }
        // `--mode` takes exactly `accept-edits` or `plan` in this build, read
        // off `agy --help`. There is no "ask" mode to name, which is fine:
        // AGY's own default *is* ask.
        match req.permission {
            PermissionPolicy::Plan => {
                args.push(ArgPart::lit("--mode"));
                args.push(ArgPart::lit("plan"));
            }
            // AGY's default is `request-review`, which in headless mode
            // auto-denies anything needing approval — exactly what `Ask` means
            // when nobody is at the other end. Adding no flag is the mapping.
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
            Some("init") => {
                let got = str_at(&v, "conversation_id")
                    .or_else(|| v.get("init").and_then(|i| str_at(i, "conversation_id")));
                let mut out = self.check_session(got.as_deref());
                out.push(AgentEvent::Started {
                    session_id: got,
                    model: v.get("init").and_then(|i| str_at(i, "model")),
                });
                out
            }
            Some("step_update") => match v.get("step_update") {
                Some(step) => self.parse_step(step),
                None => vec![],
            },
            Some("result") => {
                let mut out = vec![];
                if let Some(r) = v.get("result") {
                    let response = str_at(r, "response");
                    if let Some(text) = &response {
                        self.acc.note_text(text);
                    }
                    // Anything but SUCCESS is a failed run.
                    let status = str_at(r, "status").unwrap_or_default();
                    if !status.is_empty() && status != "SUCCESS" {
                        self.acc.errored = true;
                        if let Some(msg) = str_at(r, "error") {
                            out.push(AgentEvent::Error { message: msg });
                        }
                    }
                    // A successful run that produced nothing is AGY's headless
                    // permission denial: a tool needed approval, nothing could
                    // prompt for it, so it was auto-denied — and it still
                    // reports SUCCESS and exits 0. The only other signal is a
                    // human-readable line on stderr. Treating this as success
                    // would report "done" for work that never happened.
                    if status == "SUCCESS" && response.is_none() {
                        self.acc.errored = true;
                        out.push(AgentEvent::Error {
                            message: "AGY produced no output — a tool most likely needed a \
                                      permission that headless mode cannot prompt for and was \
                                      auto-denied. Re-run with a more permissive policy, or add \
                                      an allow-rule in ~/.gemini/antigravity-cli/settings.json."
                                .into(),
                        });
                    }
                    out.extend(self.check_session(str_at(r, "conversation_id").as_deref()));
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
                out
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
    /// Verify AGY resumed the conversation we asked for.
    ///
    /// An unknown id is not an error to AGY: it starts a fresh conversation,
    /// reports SUCCESS and exits 0. Comparing the id it reports against the one
    /// we requested is the only way to notice that the thread was lost — and a
    /// lost thread means the agent has silently forgotten everything.
    fn check_session(&mut self, got: Option<&str>) -> Vec<AgentEvent> {
        let expected = self.expected_session.borrow().clone();
        let (Some(want), Some(got)) = (expected, got) else {
            return vec![];
        };
        if want == got {
            return vec![];
        }
        // Only report it once, however many records carry the id.
        *self.expected_session.borrow_mut() = None;
        self.acc.errored = true;
        vec![AgentEvent::Error {
            message: format!(
                "AGY did not resume conversation {want} — it started {got} instead, \
                 so the earlier context is gone. AGY reports success for an unknown \
                 conversation id rather than failing."
            ),
        }]
    }

    fn parse_step(&mut self, step: &Value) -> Vec<AgentEvent> {
        let state = str_at(step, "state").unwrap_or_default();
        let step_type = str_at(step, "step_type").unwrap_or_default();

        // Per-step usage gives a live running total while the agent works. The
        // terminal `result` record overwrites it with the authoritative sum.
        if let Some(u) = step.get("usage").map(usage_from) {
            self.acc.add_usage(&u);
        }

        match step_type.as_str() {
            "agent_response" => {
                let index = step.get("step_index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(delta) = str_at(step, "text_delta") {
                    self.partials.entry(index).or_default().push_str(&delta);
                }
                // Only a completed step is whole enough to show.
                if state != "DONE" {
                    return vec![];
                }
                match self.partials.remove(&index) {
                    Some(text) if !text.trim().is_empty() => {
                        self.acc.note_text(&text);
                        vec![AgentEvent::Message { text }]
                    }
                    _ => vec![],
                }
            }
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
    use crate::harness::{Effort, Role};

    /// Regression, seen in a real run: AGY sends prose in fragments, one per
    /// `step_update`, so emitting each on sight split words across transcript
    /// lines — "I have creat" then "ed the file".
    #[test]
    fn prose_fragments_are_joined_into_one_message() {
        let mut h = Agy::default();
        assert!(
            h.parse_line(
                r#"{"event":"step_update","step_update":{"step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"I have creat"}}"#
            )
            .is_empty(),
            "a half-finished sentence must not be shown"
        );
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"ed the file."}}"#,
        );
        assert_eq!(
            events,
            vec![AgentEvent::Message { text: "I have created the file.".into() }]
        );
    }

    #[test]
    fn two_steps_accumulate_independently() {
        let mut h = Agy::default();
        h.parse_line(r#"{"event":"step_update","step_update":{"step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"first"}}"#);
        h.parse_line(r#"{"event":"step_update","step_update":{"step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"second"}}"#);
        let a = h.parse_line(r#"{"event":"step_update","step_update":{"step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"!"}}"#);
        let b = h.parse_line(r#"{"event":"step_update","step_update":{"step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"!"}}"#);
        assert_eq!(a, vec![AgentEvent::Message { text: "second!".into() }]);
        assert_eq!(b, vec![AgentEvent::Message { text: "first!".into() }]);
    }

    /// A step that completes in one update still produces its message.
    #[test]
    fn a_single_update_step_is_still_emitted() {
        let mut h = Agy::default();
        let events = h.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":3,"state":"DONE","step_type":"agent_response","text_delta":"all at once"}}"#,
        );
        assert_eq!(
            events,
            vec![AgentEvent::Message { text: "all at once".into() }]
        );
    }

    /// Regression, found by asking AGY to write a file: it ignores the cwd of
    /// the shell that launched it and falls back to its own scratch directory,
    /// reporting success while the repo is untouched.
    #[test]
    fn the_working_directory_is_added_to_the_workspace() {
        let mut r = req();
        r.cwd = std::path::PathBuf::from("/work/repo");
        let args = lits(&Agy::default().args(&r));
        let at = args
            .iter()
            .position(|a| a == "--add-dir")
            .expect("the cwd must be given to AGY explicitly");
        assert_eq!(args[at + 1], "/work/repo");
    }

    /// The cwd first, then one `--add-dir` per root.
    ///
    /// The order matters to nothing AGY does, but the *count* does: repeating
    /// the flag accumulates, measured against agy 1.1.12, which named exactly
    /// the two added directories as its workspace.
    #[test]
    fn every_root_is_added_to_the_workspace_beside_the_cwd() {
        let mut r = req();
        r.cwd = std::path::PathBuf::from("/work/repo");
        r.roots = vec![
            std::path::PathBuf::from("/work/one"),
            std::path::PathBuf::from("/work/two"),
        ];
        let args = lits(&Agy::default().args(&r));
        let granted: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "--add-dir")
            .map(|(i, _)| &args[i + 1])
            .collect();
        assert_eq!(
            granted,
            vec!["/work/repo", "/work/one", "/work/two"],
            "the cwd must survive alongside the roots"
        );
    }

    /// The cwd grant is not redundant with the roots. Asked what its workspace
    /// held, AGY listed the added directories and *not* the shell's working
    /// directory — so a build that dropped this in favour of roots alone would
    /// leave a run unable to see the repository it was started in.
    #[test]
    fn a_request_with_no_roots_still_grants_the_working_directory() {
        let mut r = req();
        r.cwd = std::path::PathBuf::from("/work/repo");
        let args = lits(&Agy::default().args(&r));
        let granted: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "--add-dir")
            .map(|(i, _)| &args[i + 1])
            .collect();
        assert_eq!(granted, vec!["/work/repo"]);
    }

    /// AGY expands `/name` from the prompt, so forwarding is passing the line
    /// through — no flag, no rewriting.
    ///
    /// It survives having framing in front of it, which is the case that
    /// matters here: AGY answers `false` to `takes_system_prompt`, so
    /// `runner.rs` prepends the system prompt to the message and the slash is
    /// no longer the first thing in it. Measured rather than assumed — a
    /// preamble followed by `/jodskill` still fired the skill — because the
    /// obvious guess is that a slash command has to lead the line, and had that
    /// been true every forwarded command under AGY would have been silently
    /// downgraded to prose.
    #[test]
    fn a_command_rides_in_the_prompt_untouched() {
        let mut r = req();
        r.prompt = "/planning now".into();
        let args = Agy::default().args(&r);
        assert!(args.contains(&ArgPart::Prompt), "the prompt is a placeholder");
        let flat = lits(&args);
        assert!(!flat.iter().any(|a| a == "--command"));
        assert!(
            !flat.iter().any(|a| a.contains("/planning")),
            "the prompt must not be inlined into argv"
        );
    }

    fn req() -> SpawnRequest {
        SpawnRequest {
            name: "t".into(),
            harness: HarnessKind::Agy,
            prompt: "hi".into(),
            system: None,
            cwd: std::path::PathBuf::from("/tmp"),
            model: None,
            permission: PermissionPolicy::Ask,
            resume: Resume::Fresh,
            tools: None,
            ..SpawnRequest::default()
        }
    }

    /// SPEC check 29. AGY spells effort the same way Claude Code does, which
    /// is the finding that made a single `thinking` column workable at all.
    #[test]
    fn an_effort_level_reaches_agy_as_its_own_flag() {
        let mut r = req();
        r.effort = Some(Effort::High);
        let args = lits(&Agy::default().args(&r));
        let at = args
            .iter()
            .position(|a| a == "--effort")
            .expect("a level that was asked for must reach the command line");
        assert_eq!(args[at + 1], "high");
    }

    /// AGY's `--effort` takes three words. Asked for one of the two it has
    /// never heard of, it is given no flag rather than the nearest level it
    /// does know — a run silently thinking at `high` when the role says `max`
    /// is a setting nobody could check.
    #[test]
    fn a_level_agy_has_no_word_for_produces_no_flag() {
        for level in [Effort::XHigh, Effort::Max] {
            let mut r = req();
            r.effort = Some(level);
            let args = lits(&Agy::default().args(&r));
            assert!(
                !args.iter().any(|a| a == "--effort"),
                "{level:?} reached AGY, which cannot spell it: {args:?}"
            );
        }
    }

    /// The other half of check 29: nothing asked for, nothing emitted, and the
    /// argv is the one this adapter produced before roles existed.
    #[test]
    fn no_effort_level_means_no_effort_flag_and_no_other_change() {
        let plain = req();
        let args = lits(&Agy::default().args(&plain));
        assert!(!args.iter().any(|a| a == "--effort"));

        let mut tagged = plain.clone();
        tagged.role = Some(Role::Engineer);
        assert_eq!(lits(&Agy::default().args(&tagged)), args);
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
            [AgentEvent::ToolResult {
                name,
                summary,
                is_error,
            }] => {
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
            .parse_line(
                r#"{"event":"result","result":{"status":"SUCCESS","response":"the answer"}}"#
            )
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
                assert_eq!(
                    usage.output_tokens,
                    Some(68),
                    "step usage was double-counted"
                );
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

    /// AGY's own default is 5 minutes and it kills the run when it expires,
    /// which looks like a truncated answer rather than a timeout.
    #[test]
    fn the_print_timeout_is_raised_off_agys_five_minute_default() {
        let args = lits(&Agy::default().args(&req()));
        let i = args.iter().position(|a| a == "--print-timeout").unwrap();
        assert_eq!(args[i + 1], PRINT_TIMEOUT);
        assert_ne!(
            PRINT_TIMEOUT, "5m",
            "5 minutes is the default we are avoiding"
        );
    }

    /// Regression: AGY auto-denies tools it cannot prompt for in headless mode,
    /// then reports SUCCESS with an empty response and exit code 0. Believing
    /// it means reporting "done" for work that never ran.
    #[test]
    fn a_successful_run_that_produced_nothing_is_treated_as_a_failure() {
        let mut h = Agy::default();
        let events = h.parse_line(
            r#"{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"","duration_seconds":2.5}}"#,
        );
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::Error { .. })),
            "the denial must be surfaced, got {events:?}"
        );
        match h.finalize(Some(0)) {
            AgentEvent::Finished { is_error, .. } => {
                assert!(
                    is_error,
                    "exit 0 must not make a denied run look successful"
                )
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn a_successful_run_with_real_output_is_not_flagged() {
        let mut h = Agy::default();
        let events = h.parse_line(
            r#"{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"the answer"}}"#,
        );
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));
        match h.finalize(Some(0)) {
            AgentEvent::Finished { is_error, .. } => assert!(!is_error),
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    /// Regression: an unknown conversation id makes AGY start a *new*
    /// conversation and report success, so the agent silently loses all prior
    /// context. Comparing ids is the only available signal.
    #[test]
    fn a_lost_conversation_is_detected_by_comparing_ids() {
        let mut h = Agy::default();
        let mut r = req();
        r.resume = Resume::Session("wanted-id".into());
        let _ = h.args(&r);

        let events =
            h.parse_line(r#"{"event":"init","conversation_id":"a-different-id","init":{}}"#);
        let message = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Error { message } => Some(message.clone()),
                _ => None,
            })
            .expect("a lost conversation must be reported");
        assert!(message.contains("wanted-id"), "got: {message}");
        match h.finalize(Some(0)) {
            AgentEvent::Finished { is_error, .. } => assert!(is_error),
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn resuming_the_conversation_we_asked_for_reports_nothing_unusual() {
        let mut h = Agy::default();
        let mut r = req();
        r.resume = Resume::Session("wanted-id".into());
        let _ = h.args(&r);

        let events = h.parse_line(r#"{"event":"init","conversation_id":"wanted-id","init":{}}"#);
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));
        assert!(matches!(events[0], AgentEvent::Started { .. }));
    }

    #[test]
    fn a_lost_conversation_is_reported_once_not_on_every_record() {
        let mut h = Agy::default();
        let mut r = req();
        r.resume = Resume::Session("wanted".into());
        let _ = h.args(&r);

        let first = h.parse_line(r#"{"event":"init","conversation_id":"other","init":{}}"#);
        let second = h.parse_line(
            r#"{"event":"result","result":{"conversation_id":"other","status":"SUCCESS","response":"hi"}}"#,
        );
        assert_eq!(
            first
                .iter()
                .filter(|e| matches!(e, AgentEvent::Error { .. }))
                .count(),
            1
        );
        assert_eq!(
            second
                .iter()
                .filter(|e| matches!(e, AgentEvent::Error { .. }))
                .count(),
            0
        );
    }

    /// A fresh conversation has nothing to compare against.
    #[test]
    fn a_fresh_run_never_reports_a_lost_conversation() {
        let mut h = Agy::default();
        let _ = h.args(&req());
        let events = h.parse_line(r#"{"event":"init","conversation_id":"brand-new","init":{}}"#);
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));
    }

    #[test]
    fn an_explicit_error_status_surfaces_agys_own_message() {
        let mut h = Agy::default();
        let events = h.parse_line(
            r#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"","error":"Error: empty prompt."}}"#,
        );
        let message = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Error { message } => Some(message.clone()),
                _ => None,
            })
            .expect("the error must be surfaced");
        assert!(message.contains("empty prompt"), "got: {message}");
    }

    #[test]
    fn no_cost_is_reported_because_agy_does_not_publish_one() {
        let u = usage_from(&serde_json::json!({"input_tokens":10,"output_tokens":2}));
        assert_eq!(u.cost_usd, None);
        assert_eq!(u.input_tokens, Some(10));
    }
}
