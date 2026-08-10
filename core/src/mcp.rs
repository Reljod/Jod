//! Jod as an MCP server — where a harness stops being a subprocess and starts
//! being Jod's mind.
//!
//! Jod has no model client and never will. What it has is *effects*:
//! delegating, scheduling, remembering, saying what is running. MCP is the
//! mechanism both Claude Code (`--mcp-config`) and OpenCode (`opencode mcp
//! add`) already speak for reaching effects, so exposing Jod's own verbs over
//! it gives one harness run that thinks *and* acts — with Jod supplying the
//! tools and the harness supplying every judgement. The charter holds exactly:
//! Jod owns the effects, the harness owns the reasoning.
//!
//! The wire format is JSON-RPC 2.0, one object per line, over stdin and stdout.
//! Three methods carry the whole surface — `initialize`, `tools/list`,
//! `tools/call` — which is why there is no SDK in this file: the dependency
//! would be bigger than the protocol it hides.
//!
//! Two rules hold everywhere below, and both exist because the caller is a
//! language model that may have just read something hostile:
//!
//! 1. **No tool can raise its own permissions.** [`Tool::delegate`]'s
//!    permission is capped at the ceiling this server was started with — the
//!    same rule, and the same ordering, `jod-api` applies to a remote caller in
//!    `api/src/config.rs` — and a child's [`ToolAccess`] is capped at the
//!    parent's.
//! 2. **Nothing here writes [`Origin::Owner`].** A fact an agent concluded is
//!    [`Origin::Agent`], whatever the agent says about where it got it. Only a
//!    person typing `jod remember` is the owner.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};

use crate::harness::ToolAccess;
use crate::schedule::{Goal, GoalState, Schedule, ScheduleState};
use crate::service::{default_cwd, AgentStatus, RunConversation};
use crate::store::{NewFact, Origin, Store, DEFAULT_SCOPE};
use crate::{HarnessKind, Jod, PermissionPolicy, Resume, SpawnRequest};

/// The revision of MCP this server answers with when the client asks for one it
/// does not recognise. Every version below negotiates identically for three
/// methods, so the list is about not refusing a client, not about behaviour.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

const SUPPORTED_PROTOCOLS: [&str; 3] = ["2024-11-05", "2025-03-26", PROTOCOL_VERSION];

/// How many stored runs to load before answering a question about runs.
///
/// The MCP server is a fresh process that has launched nothing, so without this
/// every listing would be empty and `stop_agent` would claim every id is
/// unknown. Matches what `jod ls` uses.
const REHYDRATE: usize = 200;

// JSON-RPC 2.0 error codes. Spelled out because a wrong one here reads to the
// client as a different failure than the one that happened.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

// ---- the tool catalogue ---------------------------------------------------

/// One tool, as advertised and as dispatched.
///
/// The name, the schema and the access it needs live in one value so that
/// `tools/list` and `tools/call` cannot describe different things. A tool that
/// is advertised but not dispatchable is the failure mode worth designing
/// against: the model spends a turn discovering it, and has no way to tell that
/// from Jod being broken.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// The least access that may call this.
    pub needs: ToolAccess,
    pub schema: Value,
}

fn obj(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

fn text(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn int(description: &str) -> Value {
    json!({ "type": "integer", "description": description })
}

fn num(description: &str) -> Value {
    json!({ "type": "number", "description": description })
}

fn one_of(description: &str, values: &[&str]) -> Value {
    json!({ "type": "string", "description": description, "enum": values })
}

const HARNESS_IDS: [&str; 3] = ["claude_code", "open_code", "agy"];
const PERMISSION_IDS: [&str; 3] = ["ask", "accept_edits", "bypass"];
const ACCESS_IDS: [&str; 3] = ["read_only", "delegate", "orchestrate"];

/// Every tool this server knows, whatever the caller may use.
///
/// Ordered as an agent would want to read them: what is running first, because
/// deciding to reuse a warm agent rather than start a cold one is the decision
/// that changes the most.
pub fn catalogue() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_agents",
            description:
                "Every agent Jod knows about, running or finished, each with its last message. \
                 Check this before delegating: continuing a warm agent that already has the \
                 context beats starting a cold one that has to rediscover it.",
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "running_only": { "type": "boolean", "description": "Only agents still working." },
                    "limit": int("How many to return. Default 20.")
                }),
                &[],
            ),
        },
        Tool {
            name: "delegate",
            description:
                "Start a new agent on a prompt and return its run id. Returns as soon as the \
                 agent is launched — use list_agents to see how it is getting on.",
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "prompt": text("What to ask the agent to do."),
                    "harness": one_of("Which harness runs it. Default claude_code.", &HARNESS_IDS),
                    "name": text("A short name, shown in listings. Defaults to the prompt's first words."),
                    "cwd": text("Working directory for the agent."),
                    "model": text("Model override, in the harness's own spelling."),
                    "permission": one_of(
                        "How much the agent may do unattended. Capped at this server's ceiling. Default ask.",
                        &PERMISSION_IDS,
                    ),
                    "tools": one_of(
                        "How much of Jod the new agent may reach. Capped at your own. Default read_only.",
                        &ACCESS_IDS,
                    )
                }),
                &["prompt"],
            ),
        },
        Tool {
            name: "continue_agent",
            description:
                "Send a follow-up to an agent that already ran, keeping its context. Only works \
                 once that run has reported a session id.",
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "run_id": text("The run to continue, as list_agents reports it."),
                    "prompt": text("The follow-up."),
                    "tools": one_of(
                        "How much of Jod the continued agent may reach. Capped at your own. Default read_only.",
                        &ACCESS_IDS,
                    )
                }),
                &["run_id", "prompt"],
            ),
        },
        Tool {
            name: "stop_agent",
            description: "Stop a running agent and everything it started.",
            needs: ToolAccess::Delegate,
            schema: obj(json!({ "run_id": text("The run to stop.") }), &["run_id"]),
        },
        Tool {
            name: "schedule_create",
            description:
                "Arm a prompt to run on a cron schedule, for ever, whether or not anyone is \
                 watching. Use a goal instead when the work has an end.",
            needs: ToolAccess::Orchestrate,
            schema: obj(
                json!({
                    "name": text("A short name, used to refer to it afterwards."),
                    "prompt": text("What to ask the agent to do each time."),
                    "cron": text("A cron expression: `0 2 * * *`, `@daily`, `*/15 * * * *`."),
                    "timezone": text("An IANA zone name — `Asia/Manila`, not `+08:00`. Default UTC."),
                    "harness": one_of("Which harness runs it. Default claude_code.", &HARNESS_IDS),
                    "cwd": text("Working directory for each run."),
                    "model": text("Model override."),
                    "misfire": one_of(
                        "What to do about instants missed while Jod was down. Default fire_once.",
                        &["fire_once", "skip", "fire_all"],
                    ),
                    "overlap": one_of(
                        "What to do when it comes due and the last run is still going. Default skip.",
                        &["skip", "replace", "allow"],
                    )
                }),
                &["name", "prompt", "cron"],
            ),
        },
        Tool {
            name: "schedule_list",
            description: "Every schedule, with its state and when it next fires.",
            needs: ToolAccess::ReadOnly,
            schema: obj(json!({}), &[]),
        },
        Tool {
            name: "schedule_pause",
            description: "Stop a schedule firing, without forgetting it.",
            needs: ToolAccess::Orchestrate,
            schema: obj(json!({ "name": text("The schedule to pause.") }), &["name"]),
        },
        Tool {
            name: "schedule_run_now",
            description:
                "Bring a schedule's next fire forward to now. Refuses a schedule that is not \
                 armed, so pausing one really does stop it.",
            needs: ToolAccess::Orchestrate,
            schema: obj(json!({ "name": text("The schedule to bring forward.") }), &["name"]),
        },
        Tool {
            name: "goal_create",
            description:
                "Set a standing objective, pursued on a cron until it is met, runs out of budget \
                 or iterations, or stops making progress.",
            needs: ToolAccess::Orchestrate,
            schema: obj(
                json!({
                    "name": text("A short name for the goal."),
                    "objective": text("What the goal is, in a sentence."),
                    "cron": text("How often to work on it. Default `0 * * * *`."),
                    "timezone": text("An IANA zone name. Default UTC."),
                    "harness": one_of("Which harness runs it. Default claude_code.", &HARNESS_IDS),
                    "cwd": text("Working directory for each iteration."),
                    "done_when": text("A shell command that decides `done`, consulted before anything is asked to judge progress."),
                    "max_iterations": int("Stop after this many iterations, whatever the state."),
                    "budget_usd": num("Stop once this much has been spent."),
                    "stall_after": int("How many iterations may finish without progress before it is called stalled. Default 6.")
                }),
                &["name", "objective"],
            ),
        },
        Tool {
            name: "goal_list",
            description: "Every goal, with its iteration count, spend and state.",
            needs: ToolAccess::ReadOnly,
            schema: obj(json!({}), &[]),
        },
        Tool {
            name: "remember",
            description:
                "Write something durable into Jod's memory, as a subject–predicate–object triple. \
                 Recorded as an agent's conclusion, never as the owner's own word.",
            needs: ToolAccess::Orchestrate,
            schema: obj(
                json!({
                    "subject": text("What the fact is about, e.g. `reljod`."),
                    "predicate": text("The relation, e.g. `prefers`."),
                    "object": text("The value, e.g. `linear for tasks`."),
                    "scope": text("Which domain this belongs to. Scopes are hard partitions. Default `default`."),
                    "source": text("Where this came from — a note path, a URL, a person.")
                }),
                &["subject", "predicate", "object"],
            ),
        },
        Tool {
            name: "recall",
            description:
                "Search what Jod remembers. Answers with currently-believed facts only, and never \
                 with material Jod read from outside.",
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "query": text("What to search for."),
                    "scope": text("Restrict to one domain. Omit to search every scope."),
                    "limit": int("How many facts to return. Default 10.")
                }),
                &["query"],
            ),
        },
        Tool {
            name: "related",
            description:
                "What Jod's memory connects to a thing, by walking the graph. Answers `what is X \
                 connected to`, which no list of facts about X can.",
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "subject": text("The entity to start from."),
                    "hops": int("How many hops out. Default 2, capped by the store."),
                    "scope": text("Which domain to walk. Default `default`.")
                }),
                &["subject"],
            ),
        },
        Tool {
            name: "conversations",
            description: "Conversations Jod owns, newest first.",
            needs: ToolAccess::ReadOnly,
            schema: obj(json!({ "limit": int("How many to return. Default 20.") }), &[]),
        },
        Tool {
            name: "conversation_search",
            description:
                "Search every conversation. Each hit comes with the messages around it and the \
                 conversation's opening and closing, so it reads without fetching the transcript.",
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "query": text("What to search for."),
                    "limit": int("How many hits to return. Default 5.")
                }),
                &["query"],
            ),
        },
    ]
}

/// Whether an agent holding `have` may call a tool needing `need`.
///
/// Expressed through [`ToolAccess`]'s own predicates rather than a second
/// ordering, so there is one definition of what each level means.
fn allows(have: ToolAccess, need: ToolAccess) -> bool {
    match need {
        ToolAccess::ReadOnly => true,
        ToolAccess::Delegate => have.may_delegate(),
        ToolAccess::Orchestrate => have.may_orchestrate(),
    }
}

/// Is `requested` within `ceiling`?
///
/// `Ask < AcceptEdits < Bypass`, ordered by how much can happen without anyone
/// being asked — the same rule `jod-api` applies to a remote caller in
/// `api/src/config.rs`. Restated here rather than shared because `jod-core`
/// cannot depend on `jod-api`; if one side's ordering changes, the other has to
/// change with it.
pub fn permits(ceiling: PermissionPolicy, requested: PermissionPolicy) -> bool {
    fn rank(p: PermissionPolicy) -> u8 {
        match p {
            PermissionPolicy::Ask => 0,
            PermissionPolicy::AcceptEdits => 1,
            PermissionPolicy::Bypass => 2,
        }
    }
    rank(requested) <= rank(ceiling)
}

pub fn parse_permission(s: &str) -> Option<PermissionPolicy> {
    match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "ask" => Some(PermissionPolicy::Ask),
        "accept_edits" => Some(PermissionPolicy::AcceptEdits),
        "bypass" => Some(PermissionPolicy::Bypass),
        _ => None,
    }
}

pub fn parse_access(s: &str) -> Option<ToolAccess> {
    match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "read_only" | "readonly" | "none" => Some(ToolAccess::ReadOnly),
        "delegate" => Some(ToolAccess::Delegate),
        "orchestrate" => Some(ToolAccess::Orchestrate),
        _ => None,
    }
}

/// A harness by its stored id, or by what a person would type at the CLI.
///
/// The aliases are not indulgence: a model that has read `jod run -H claude`
/// anywhere will write `claude`, and a refusal there costs a whole turn to
/// discover something Jod could simply have understood.
pub fn parse_harness(s: &str) -> Option<HarnessKind> {
    let s = s.trim().to_ascii_lowercase().replace('-', "_");
    HarnessKind::from_id(&s).or(match s.as_str() {
        "claude" | "claudecode" => Some(HarnessKind::ClaudeCode),
        "opencode" => Some(HarnessKind::OpenCode),
        _ => None,
    })
}

// ---- the server -----------------------------------------------------------

/// Why a tool call did not produce a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// No such tool. A protocol-level mistake, so it becomes a JSON-RPC error
    /// rather than a tool result: the model asked for something that was never
    /// advertised.
    Unknown(String),
    /// The tool exists but this agent was never shown it, because its
    /// [`ToolAccess`] does not reach that far.
    ///
    /// A JSON-RPC error rather than a tool result, and the distinction is
    /// deliberate: from the caller's side a tool it was never offered is
    /// indistinguishable from one that does not exist, so the two answer alike.
    /// Contrast [`ToolError::Refused`], which is "you may use this tool, but not
    /// like that" — a well-formed call the caller could make differently.
    Forbidden(String),
    /// The arguments do not describe a call that could be made.
    BadParams(String),
    /// The call was understood and Jod would not, or could not, carry it out.
    /// Comes back as a tool result marked `isError`, because it is an answer —
    /// the model should read it and choose differently.
    Refused(String),
}

/// Jod's tools, and the bounds the run that owns them was given.
pub struct Server {
    jod: Arc<Jod>,
    access: ToolAccess,
    max_permission: PermissionPolicy,
}

impl Server {
    /// A server with the least authority there is: read Jod, change nothing,
    /// and refuse any spawn above `ask`.
    ///
    /// Fail-closed on purpose. A server started without saying what it may do
    /// is one whose launcher forgot, and a forgotten line should not be the
    /// thing that hands a stranger's pull request the power to schedule work.
    pub fn new(jod: Arc<Jod>) -> Self {
        Server {
            jod,
            access: ToolAccess::ReadOnly,
            max_permission: PermissionPolicy::Ask,
        }
    }

    pub fn with_access(mut self, access: ToolAccess) -> Self {
        self.access = access;
        self
    }

    /// The most permissive policy `delegate` may ask for.
    pub fn with_max_permission(mut self, max: PermissionPolicy) -> Self {
        self.max_permission = max;
        self
    }

    pub fn access(&self) -> ToolAccess {
        self.access
    }

    /// The tools this server will advertise — exactly those it will dispatch.
    pub fn tools(&self) -> Vec<Tool> {
        catalogue()
            .into_iter()
            .filter(|t| allows(self.access, t.needs))
            .collect()
    }

    fn store(&self) -> Result<&Arc<Store>, ToolError> {
        self.jod
            .store()
            .ok_or_else(|| ToolError::Refused("this Jod has no database open".into()))
    }

    /// Carry out one tool call, or say why not.
    pub async fn call(&self, name: &str, args: &Value) -> Result<String, ToolError> {
        let Some(tool) = catalogue().into_iter().find(|t| t.name == name) else {
            return Err(ToolError::Unknown(name.to_string()));
        };
        // Checked here as well as in `tools()`, because `tools/list` is advice.
        // Nothing stops a model calling a name it read somewhere else, and the
        // list is not the control — this is.
        if !allows(self.access, tool.needs) {
            return Err(ToolError::Forbidden(format!(
                "`{name}` needs `{}` access and this agent has `{}`",
                tool.needs.as_str(),
                self.access.as_str()
            )));
        }
        match name {
            "list_agents" => self.list_agents(args).await,
            "delegate" => self.delegate(args).await,
            "continue_agent" => self.continue_agent(args).await,
            "stop_agent" => self.stop_agent(args).await,
            "schedule_create" => self.schedule_create(args),
            "schedule_list" => self.schedule_list(),
            "schedule_pause" => self.schedule_pause(args),
            "schedule_run_now" => self.schedule_run_now(args),
            "goal_create" => self.goal_create(args),
            "goal_list" => self.goal_list(),
            "remember" => self.remember(args),
            "recall" => self.recall(args),
            "related" => self.related(args),
            "conversations" => self.conversations(args),
            "conversation_search" => self.conversation_search(args),
            // Unreachable while the catalogue and this match agree, which is
            // what `every_advertised_tool_is_dispatchable` exists to hold.
            other => Err(ToolError::Unknown(other.to_string())),
        }
    }

    // ---- agents ---------------------------------------------------------

    async fn list_agents(&self, args: &Value) -> Result<String, ToolError> {
        // A fresh process knows nothing until it reads the database back.
        self.jod
            .rehydrate(REHYDRATE)
            .await
            .map_err(|e| ToolError::Refused(format!("could not read the runs: {e}")))?;
        let running_only = opt_bool(args, "running_only").unwrap_or(false);
        let limit = opt_usize(args, "limit")?.unwrap_or(20);

        let mut agents = self.jod.agents().await;
        // Running first, then newest — the order the "can I reuse one" question
        // is asked in.
        agents.sort_by(|a, b| {
            let live = |s| s == AgentStatus::Running;
            live(b.status)
                .cmp(&live(a.status))
                .then(b.created_at_ms.cmp(&a.created_at_ms))
        });
        let views: Vec<AgentView> = agents
            .iter()
            .filter(|a| !running_only || a.status == AgentStatus::Running)
            .take(limit)
            .map(|a| AgentView {
                run_id: &a.id,
                name: &a.name,
                harness: a.harness.id(),
                status: a.status,
                cwd: &a.cwd,
                model: a.model.as_deref(),
                session_id: a.session_id.as_deref(),
                created_at_ms: a.created_at_ms,
                cost_usd: a.usage.cost_usd,
                last_message: a.last_message.as_deref(),
            })
            .collect();
        as_json(&views)
    }

    async fn delegate(&self, args: &Value) -> Result<String, ToolError> {
        let prompt = required_str(args, "prompt")?;
        if prompt.trim().is_empty() {
            return Err(ToolError::BadParams("`prompt` is empty".into()));
        }
        let harness = match opt_str(args, "harness") {
            Some(h) => parse_harness(&h)
                .ok_or_else(|| ToolError::BadParams(format!("unknown harness `{h}`")))?,
            None => HarnessKind::ClaudeCode,
        };
        let permission = self.requested_permission(args)?;
        let tools = self.child_access(args)?;

        if !self.jod.supervisor_available() {
            return Err(ToolError::Refused(
                "`jod-run` is not installed on this machine, and it supervises every agent".into(),
            ));
        }

        let req = SpawnRequest {
            name: opt_str(args, "name").unwrap_or_else(|| default_name(&prompt)),
            harness,
            prompt,
            cwd: opt_str(args, "cwd").map(PathBuf::from).unwrap_or_else(default_cwd),
            model: opt_str(args, "model"),
            permission,
            resume: Resume::Fresh,
            tools: Some(tools),
        };
        let agent = self
            .jod
            .spawn_agent(req)
            .await
            .map_err(|e| ToolError::Refused(format!("could not start the agent: {e}")))?;
        as_json(&json!({
            "run_id": agent.id,
            "name": agent.name,
            "harness": agent.harness.id(),
            "watch": agent.watch_command,
        }))
    }

    async fn continue_agent(&self, args: &Value) -> Result<String, ToolError> {
        let run_id = required_str(args, "run_id")?;
        let prompt = required_str(args, "prompt")?;
        if prompt.trim().is_empty() {
            return Err(ToolError::BadParams("`prompt` is empty".into()));
        }
        let tools = self.child_access(args)?;

        self.jod
            .rehydrate(REHYDRATE)
            .await
            .map_err(|e| ToolError::Refused(format!("could not read the runs: {e}")))?;
        let agent = self
            .jod
            .agent(&run_id)
            .await
            .map_err(|_| ToolError::Refused(format!("no run `{run_id}` — list_agents has them")))?;
        let Some(session) = agent.session_id.clone() else {
            return Err(ToolError::Refused(format!(
                "run `{run_id}` never reported a session id, so there is no context to continue; \
                 delegate a fresh agent instead"
            )));
        };
        // Refused rather than quietly lowered. A follow-up that ran with less
        // authority than the turn before it would fail in ways the model reads
        // as the task being impossible.
        if !permits(self.max_permission, agent.permission) {
            return Err(ToolError::Refused(format!(
                "run `{run_id}` was launched above this server's permission ceiling; \
                 raise it locally to continue that run"
            )));
        }

        // Keep the follow-up in the same conversation the first turn opened, so
        // the transcript reads as one thread rather than as two runs that
        // happen to share a session id.
        let conversation = match self.store()?.conversation_for_run(&run_id) {
            Ok(Some(id)) => RunConversation::Existing(id),
            _ => RunConversation::New,
        };

        let req = SpawnRequest {
            name: agent.name.clone(),
            harness: agent.harness,
            prompt,
            cwd: PathBuf::from(&agent.cwd),
            model: agent.model.clone(),
            permission: agent.permission,
            resume: Resume::Session(session),
            tools: Some(tools),
        };
        let next = self
            .jod
            .spawn_agent_in(req, conversation)
            .await
            .map_err(|e| ToolError::Refused(format!("could not continue that agent: {e}")))?;
        as_json(&json!({
            "run_id": next.id,
            "continued": run_id,
            "watch": next.watch_command,
        }))
    }

    async fn stop_agent(&self, args: &Value) -> Result<String, ToolError> {
        let run_id = required_str(args, "run_id")?;
        self.jod
            .rehydrate(REHYDRATE)
            .await
            .map_err(|e| ToolError::Refused(format!("could not read the runs: {e}")))?;
        self.jod
            .kill_agent(&run_id)
            .await
            .map_err(|e| ToolError::Refused(format!("could not stop `{run_id}`: {e}")))?;
        Ok(format!("stopped {run_id}"))
    }

    /// The permission `delegate` may use, refusing anything above the ceiling.
    fn requested_permission(&self, args: &Value) -> Result<PermissionPolicy, ToolError> {
        let requested = match opt_str(args, "permission") {
            Some(p) => parse_permission(&p)
                .ok_or_else(|| ToolError::BadParams(format!("unknown permission `{p}`")))?,
            None => PermissionPolicy::Ask,
        };
        if !permits(self.max_permission, requested) {
            return Err(ToolError::Refused(format!(
                "permission `{}` exceeds this server's ceiling of `{}`; \
                 raise max_permission locally to allow it",
                permission_id(requested),
                permission_id(self.max_permission)
            )));
        }
        Ok(requested)
    }

    /// How much of Jod a spawned agent may reach.
    ///
    /// Defaults to the least, and can never exceed what this server itself
    /// holds. Both halves matter: without the cap an agent could mint a child
    /// with more authority than itself and then ask the child, and without the
    /// low default every delegation would hand out the power to delegate again.
    fn child_access(&self, args: &Value) -> Result<ToolAccess, ToolError> {
        let requested = match opt_str(args, "tools") {
            Some(t) => parse_access(&t)
                .ok_or_else(|| ToolError::BadParams(format!("unknown tool access `{t}`")))?,
            None => ToolAccess::ReadOnly,
        };
        if !allows(self.access, requested) {
            return Err(ToolError::Refused(format!(
                "`{}` tool access exceeds your own `{}`",
                requested.as_str(),
                self.access.as_str()
            )));
        }
        Ok(requested)
    }

    // ---- schedules and goals ---------------------------------------------

    fn schedule_create(&self, args: &Value) -> Result<String, ToolError> {
        let name = required_str(args, "name")?;
        let now = chrono::Utc::now().timestamp_millis();
        let s = Schedule {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.clone(),
            prompt: required_str(args, "prompt")?,
            harness: match opt_str(args, "harness") {
                Some(h) => parse_harness(&h)
                    .ok_or_else(|| ToolError::BadParams(format!("unknown harness `{h}`")))?,
                None => HarnessKind::ClaudeCode,
            }
            .id()
            .to_string(),
            cwd: opt_str(args, "cwd").unwrap_or_else(|| working_dir().display().to_string()),
            model: opt_str(args, "model"),
            cron: required_str(args, "cron")?,
            timezone: opt_str(args, "timezone").unwrap_or_else(|| "UTC".to_string()),
            state: ScheduleState::Armed,
            misfire: opt_str(args, "misfire")
                .unwrap_or_else(|| "fire_once".into())
                .parse()
                .map_err(|e| ToolError::BadParams(format!("{e}")))?,
            overlap: opt_str(args, "overlap")
                .unwrap_or_else(|| "skip".into())
                .parse()
                .map_err(|e| ToolError::BadParams(format!("{e}")))?,
            grace_ms: 300_000,
            jitter_ms: 0,
            next_fire_at_ms: None,
            last_fire_at_ms: None,
            consecutive_failures: 0,
            created_at_ms: now,
        };
        let store = self.store()?;
        store
            .add_schedule(&s)
            .map_err(|e| ToolError::Refused(format!("could not arm `{name}`: {e}")))?;
        // Read the row back for the fire time the store computed, rather than
        // reporting the `None` that was written.
        let next = store
            .schedule_named(&name)
            .ok()
            .flatten()
            .and_then(|s| s.next_fire_at_ms);
        as_json(&json!({ "name": name, "state": "armed", "next_fire_at_ms": next }))
    }

    fn schedule_list(&self) -> Result<String, ToolError> {
        let all = self
            .store()?
            .schedules()
            .map_err(|e| ToolError::Refused(format!("could not read the schedules: {e}")))?;
        as_json(&all)
    }

    fn schedule_pause(&self, args: &Value) -> Result<String, ToolError> {
        let name = required_str(args, "name")?;
        let changed = self
            .store()?
            .set_schedule_state(&name, ScheduleState::Paused)
            .map_err(|e| ToolError::Refused(format!("could not pause `{name}`: {e}")))?;
        if changed {
            Ok(format!("{name} paused"))
        } else {
            Err(ToolError::Refused(format!("no schedule `{name}`")))
        }
    }

    fn schedule_run_now(&self, args: &Value) -> Result<String, ToolError> {
        let name = required_str(args, "name")?;
        let now = chrono::Utc::now().timestamp_millis();
        let brought_forward = self
            .store()?
            .run_schedule_now(&name, now)
            .map_err(|e| ToolError::Refused(format!("could not bring `{name}` forward: {e}")))?;
        if brought_forward {
            Ok(format!("{name} is due now — the next tick will fire it"))
        } else {
            // A refusal, not a result. Forcing a paused schedule would defeat
            // the only reason anyone pauses one, and reporting it as success
            // would leave the model believing work is coming that never will.
            Err(ToolError::Refused(format!(
                "`{name}` is not armed, so it was not brought forward"
            )))
        }
    }

    fn goal_create(&self, args: &Value) -> Result<String, ToolError> {
        let name = required_str(args, "name")?;
        let now = chrono::Utc::now().timestamp_millis();
        let g = Goal {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.clone(),
            objective: required_str(args, "objective")?,
            done_when: opt_str(args, "done_when"),
            harness: match opt_str(args, "harness") {
                Some(h) => parse_harness(&h)
                    .ok_or_else(|| ToolError::BadParams(format!("unknown harness `{h}`")))?,
                None => HarnessKind::ClaudeCode,
            }
            .id()
            .to_string(),
            cwd: opt_str(args, "cwd").unwrap_or_else(|| working_dir().display().to_string()),
            model: opt_str(args, "model"),
            cron: opt_str(args, "cron").unwrap_or_else(|| "0 * * * *".to_string()),
            timezone: opt_str(args, "timezone").unwrap_or_else(|| "UTC".to_string()),
            state: GoalState::Running,
            iteration: 0,
            max_iterations: opt_i64(args, "max_iterations")?,
            budget_usd: opt_f64(args, "budget_usd")?,
            spent_usd: 0.0,
            stall_after: opt_i64(args, "stall_after")?.unwrap_or(6),
            no_progress: 0,
            next_fire_at_ms: None,
            created_at_ms: now,
        };
        self.store()?
            .add_goal(&g)
            .map_err(|e| ToolError::Refused(format!("could not set `{name}`: {e}")))?;
        as_json(&json!({ "name": name, "state": "running" }))
    }

    fn goal_list(&self) -> Result<String, ToolError> {
        let all = self
            .store()?
            .goals()
            .map_err(|e| ToolError::Refused(format!("could not read the goals: {e}")))?;
        as_json(&all)
    }

    // ---- memory ----------------------------------------------------------

    fn remember(&self, args: &Value) -> Result<String, ToolError> {
        let fact = NewFact {
            scope: opt_str(args, "scope").unwrap_or_else(|| DEFAULT_SCOPE.to_string()),
            subject: required_str(args, "subject")?,
            predicate: required_str(args, "predicate")?,
            object: required_str(args, "object")?,
            // Hard-coded, and there is deliberately no argument for it. An agent
            // saying "Reljod told me" is still an agent's report of what it
            // read; only a person typing `jod remember` is the owner.
            origin: Origin::Agent,
            source: opt_str(args, "source"),
            valid_from: None,
        };
        let id = self
            .store()?
            .remember(fact)
            .map_err(|e| ToolError::Refused(format!("could not remember that: {e}")))?;
        Ok(format!("remembered #{id} as an agent's conclusion"))
    }

    fn recall(&self, args: &Value) -> Result<String, ToolError> {
        let query = required_str(args, "query")?;
        let limit = opt_usize(args, "limit")?.unwrap_or(10);
        let scope = opt_str(args, "scope");
        // `recall_in` excludes `untrusted` facts, and that is the whole point of
        // reaching memory through this rather than through the raw table: the
        // caller is a model that may be acting on a page it just read.
        let facts = self
            .store()?
            .recall_in(scope.as_deref(), &query, limit)
            .map_err(|e| ToolError::Refused(format!("could not search memory: {e}")))?;
        as_json(&facts)
    }

    fn related(&self, args: &Value) -> Result<String, ToolError> {
        let subject = required_str(args, "subject")?;
        let hops = opt_i64(args, "hops")?.unwrap_or(2).clamp(1, u32::MAX as i64) as u32;
        let scope = opt_str(args, "scope").unwrap_or_else(|| DEFAULT_SCOPE.to_string());
        let now = chrono::Utc::now().timestamp_millis();
        let found = self
            .store()?
            .neighbourhood(&scope, &subject, hops, now)
            .map_err(|e| ToolError::Refused(format!("could not walk the graph: {e}")))?;
        as_json(&found)
    }

    // ---- conversations ----------------------------------------------------

    fn conversations(&self, args: &Value) -> Result<String, ToolError> {
        let limit = opt_usize(args, "limit")?.unwrap_or(20);
        let all = self
            .store()?
            .conversations(limit)
            .map_err(|e| ToolError::Refused(format!("could not read the conversations: {e}")))?;
        as_json(&all)
    }

    fn conversation_search(&self, args: &Value) -> Result<String, ToolError> {
        let query = required_str(args, "query")?;
        let limit = opt_usize(args, "limit")?.unwrap_or(5);
        let hits = self
            .store()?
            .search_messages(&query, limit)
            .map_err(|e| ToolError::Refused(format!("could not search: {e}")))?;
        as_json(&hits)
    }
}

/// One agent, trimmed to what the decision needs.
///
/// Not [`crate::AgentSummary`] itself: that carries pids, a process-alive probe
/// and a full usage block, and every field an agent reads is context it pays
/// for. What is left is what answers "is this the agent I should be talking to".
#[derive(Serialize)]
struct AgentView<'a> {
    run_id: &'a str,
    name: &'a str,
    harness: &'static str,
    status: AgentStatus,
    cwd: &'a str,
    model: Option<&'a str>,
    session_id: Option<&'a str>,
    created_at_ms: i64,
    cost_usd: Option<f64>,
    last_message: Option<&'a str>,
}

fn permission_id(p: PermissionPolicy) -> &'static str {
    match p {
        PermissionPolicy::Ask => "ask",
        PermissionPolicy::AcceptEdits => "accept_edits",
        PermissionPolicy::Bypass => "bypass",
    }
}

/// Where a schedule runs when nobody said. The server's own directory, which is
/// the daemon's — falling back to the home directory rather than failing, since
/// a deleted cwd should not stop a schedule being armed.
fn working_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| default_cwd())
}

/// A short, recognisable name from the prompt's first words — the same rule the
/// CLI applies when nobody passes `--name`.
fn default_name(prompt: &str) -> String {
    let name = prompt.split_whitespace().take(5).collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        "agent".to_string()
    } else if name.chars().count() > 48 {
        format!("{}…", name.chars().take(47).collect::<String>())
    } else {
        name
    }
}

fn as_json<T: Serialize>(value: &T) -> Result<String, ToolError> {
    serde_json::to_string_pretty(value)
        .map_err(|e| ToolError::Refused(format!("could not render the answer: {e}")))
}

fn required_str(args: &Value, key: &str) -> Result<String, ToolError> {
    match args.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(ToolError::BadParams(format!(
            "`{key}` must be a string, not {other}"
        ))),
        None => Err(ToolError::BadParams(format!("`{key}` is required"))),
    }
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn opt_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

fn opt_i64(args: &Value, key: &str) -> Result<Option<i64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| ToolError::BadParams(format!("`{key}` must be a whole number"))),
    }
}

fn opt_f64(args: &Value, key: &str) -> Result<Option<f64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .map(Some)
            .ok_or_else(|| ToolError::BadParams(format!("`{key}` must be a number"))),
    }
}

fn opt_usize(args: &Value, key: &str) -> Result<Option<usize>, ToolError> {
    Ok(opt_i64(args, key)?.map(|n| n.max(0) as usize))
}

// ---- the JSON-RPC surface -------------------------------------------------

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

/// Answer one request, or say nothing.
///
/// `None` means the message was a notification — no `id`, so by JSON-RPC there
/// is nothing to answer and answering anyway is a protocol violation. That
/// covers `notifications/initialized`, which every client sends and no server
/// needs to act on.
pub async fn handle(server: &Server, request: Value) -> Option<Value> {
    let Some(object) = request.as_object() else {
        return Some(error(Value::Null, INVALID_REQUEST, "request is not an object"));
    };
    // Presence, not truthiness: a notification omits `id` entirely, and an
    // explicit `"id": null` is still a request expecting an answer.
    let id = match object.get("id") {
        Some(id) => id.clone(),
        None => return None,
    };
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Some(error(id, INVALID_REQUEST, "request has no method"));
    };
    let params = object.get("params").cloned().unwrap_or(Value::Null);

    Some(match method {
        "initialize" => result(id, initialize(server, &params)),
        // Clients send this to check the connection is alive; an empty result
        // is the whole of the answer.
        "ping" => result(id, json!({})),
        "tools/list" => result(
            id,
            json!({ "tools": server.tools().iter().map(describe).collect::<Vec<_>>() }),
        ),
        "tools/call" => call_tool(server, id, &params).await,
        other => Some(error(id, METHOD_NOT_FOUND, format!("unknown method `{other}`")))?,
    })
}

fn describe(tool: &Tool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": tool.schema,
    })
}

/// Agree a protocol version and say what this server can do.
///
/// The client's own version is echoed back when it is one we know, which is how
/// MCP negotiation is meant to go; anything else gets ours, and the client
/// decides whether it can live with that.
fn initialize(server: &Server, params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = match asked {
        Some(v) if SUPPORTED_PROTOCOLS.contains(&v) => v,
        _ => PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "jod",
            "version": env!("CARGO_PKG_VERSION"),
            // Not decoration: this is what the harness shows a person who asks
            // where a tool came from, and "jod" alone does not say.
            "title": format!("Jod ({} access)", server.access.as_str()),
        },
    })
}

async fn call_tool(server: &Server, id: Value, params: &Value) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return error(id, INVALID_PARAMS, "tools/call needs a `name`");
    };
    let args = match params.get("arguments") {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return error(id, INVALID_PARAMS, "`arguments` must be an object"),
    };

    match server.call(name, &args).await {
        Ok(text) => result(id, content(&text, false)),
        Err(ToolError::Unknown(name)) => {
            error(id, INVALID_PARAMS, format!("unknown tool `{name}`"))
        }
        Err(ToolError::Forbidden(why)) | Err(ToolError::BadParams(why)) => {
            error(id, INVALID_PARAMS, why)
        }
        // A refusal is an answer the model should read and act on, so it goes
        // back as a tool result rather than as a protocol error — the same
        // reason a compiler prints an error instead of exiting silently.
        Err(ToolError::Refused(why)) => result(id, content(&why, true)),
    }
}

fn content(text: &str, is_error: bool) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error,
    })
}

/// Serve MCP over a pair of streams until the input ends.
///
/// Reads are blocking on purpose. A stdio MCP server has exactly one client and
/// nothing to do between its requests, so the honest shape is a loop that waits
/// on the next line; an async reader here would buy concurrency that the
/// protocol has no way to use.
///
/// A line that is not JSON is answered and stepped over rather than fatal. A
/// crashed MCP server takes the agent's tools away mid-task, with no message
/// and nothing in the transcript to explain why the next tool call failed.
pub async fn serve<R: BufRead, W: Write>(
    server: &Server,
    input: R,
    mut output: W,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(request) => handle(server, request).await,
            Err(e) => Some(error(Value::Null, PARSE_ERROR, format!("invalid JSON: {e}"))),
        };
        if let Some(answer) = answer {
            writeln!(output, "{answer}")?;
            // Flushed per response: the client is blocked waiting for this line,
            // so a buffered answer is a deadlock rather than an optimisation.
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn server(access: ToolAccess) -> Server {
        let store = Arc::new(Store::in_memory().unwrap());
        Server::new(Jod::with_store(store)).with_access(access)
    }

    fn request(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    async fn call(server: &Server, name: &str, args: Value) -> Value {
        handle(server, request(1, "tools/call", json!({ "name": name, "arguments": args })))
            .await
            .expect("a call is a request and must be answered")
    }

    /// The text a successful — or refused — tool call carries.
    fn said(answer: &Value) -> String {
        answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text content in {answer}"))
            .to_string()
    }

    fn is_error_result(answer: &Value) -> bool {
        answer["result"]["isError"] == json!(true)
    }

    fn error_code(answer: &Value) -> i64 {
        answer["error"]["code"]
            .as_i64()
            .unwrap_or_else(|| panic!("not an error response: {answer}"))
    }

    #[tokio::test]
    async fn initialize_answers_with_a_protocol_version_and_a_tools_capability() {
        let answer = handle(
            &server(ToolAccess::Orchestrate),
            request(1, "initialize", json!({ "protocolVersion": PROTOCOL_VERSION })),
        )
        .await
        .unwrap();
        assert_eq!(answer["jsonrpc"], "2.0");
        assert_eq!(answer["id"], 1);
        assert_eq!(answer["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(answer["result"]["serverInfo"]["name"], "jod");
        assert!(
            answer["result"]["capabilities"]["tools"].is_object(),
            "a server that does not advertise tools will never be asked for them"
        );
    }

    #[tokio::test]
    async fn initialize_agrees_to_an_older_protocol_the_client_asked_for() {
        let answer = handle(
            &server(ToolAccess::ReadOnly),
            request(1, "initialize", json!({ "protocolVersion": "2024-11-05" })),
        )
        .await
        .unwrap();
        assert_eq!(answer["result"]["protocolVersion"], "2024-11-05");
    }

    #[tokio::test]
    async fn initialize_offers_its_own_protocol_when_the_client_asks_for_one_it_does_not_know() {
        let answer = handle(
            &server(ToolAccess::ReadOnly),
            request(1, "initialize", json!({ "protocolVersion": "1999-01-01" })),
        )
        .await
        .unwrap();
        assert_eq!(answer["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn the_initialized_notification_is_ignored_rather_than_answered() {
        // It carries no id, so answering it at all would be the protocol error.
        let answer = handle(
            &server(ToolAccess::ReadOnly),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;
        assert!(answer.is_none());
    }

    #[tokio::test]
    async fn an_unknown_method_is_refused_rather_than_ignored() {
        let answer = handle(&server(ToolAccess::Orchestrate), request(7, "tools/subscribe", json!({})))
            .await
            .unwrap();
        assert_eq!(error_code(&answer), METHOD_NOT_FOUND);
        assert_eq!(answer["id"], 7, "an error must answer the request it came from");
    }

    #[tokio::test]
    async fn a_request_without_a_method_is_an_invalid_request() {
        let answer = handle(&server(ToolAccess::ReadOnly), json!({ "jsonrpc": "2.0", "id": 2 }))
            .await
            .unwrap();
        assert_eq!(error_code(&answer), INVALID_REQUEST);
    }

    #[tokio::test]
    async fn every_advertised_tool_is_dispatchable_at_every_access_level() {
        // The failure this guards is a tool listed but not wired: the model
        // spends a turn discovering it, and cannot tell that from Jod being
        // broken. Now that the list is a function of the level, it has to hold
        // per level — a tool advertised at one and dispatchable only at another
        // is the same bug wearing a disguise. Called with no arguments, so most
        // fail; the assertion is only that none is unknown or forbidden.
        for access in [
            ToolAccess::ReadOnly,
            ToolAccess::Delegate,
            ToolAccess::Orchestrate,
        ] {
            let server = server(access);
            for tool in server.tools() {
                match server.call(tool.name, &json!({})).await {
                    Err(ToolError::Unknown(name)) => {
                        panic!("{name} is advertised at {} but the dispatcher does not know it", access.as_str())
                    }
                    Err(ToolError::Forbidden(why)) => {
                        panic!("{} advertises a tool it then refuses: {why}", access.as_str())
                    }
                    _ => {}
                }
            }
        }
    }

    #[tokio::test]
    async fn tools_list_advertises_only_what_the_dispatcher_accepts() {
        let server = server(ToolAccess::Orchestrate);
        let answer = handle(&server, request(1, "tools/list", json!({}))).await.unwrap();
        let listed = answer["result"]["tools"].as_array().unwrap().clone();
        assert_eq!(listed.len(), catalogue().len());
        for tool in &listed {
            let name = tool["name"].as_str().unwrap();
            assert!(
                !matches!(server.call(name, &json!({})).await, Err(ToolError::Unknown(_))),
                "{name} is listed but not dispatchable"
            );
            assert_eq!(tool["inputSchema"]["type"], "object", "{name} has no object schema");
            assert!(
                !tool["description"].as_str().unwrap().is_empty(),
                "{name} has no description, so a model has to guess what it does"
            );
        }
    }

    #[test]
    fn no_two_tools_share_a_name() {
        let mut names: Vec<&str> = catalogue().iter().map(|t| t.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two tools with one name means one is unreachable");
    }

    #[test]
    fn every_required_argument_is_a_property_of_its_own_schema() {
        for tool in catalogue() {
            let properties = tool.schema["properties"].as_object().unwrap().clone();
            for required in tool.schema["required"].as_array().unwrap() {
                let key = required.as_str().unwrap();
                assert!(
                    properties.contains_key(key),
                    "{} requires `{key}` but never describes it",
                    tool.name
                );
            }
        }
    }

    /// What each level may see, spelled out rather than derived.
    ///
    /// Written by hand on purpose: a table computed from `catalogue()` would
    /// agree with any mistake made there, and the whole question is whether the
    /// line falls where the design says it does — reading is free and visible,
    /// delegating spends money now, scheduling spends it at 2am for ever.
    const READ_ONLY_TOOLS: [&str; 7] = [
        "list_agents",
        "schedule_list",
        "goal_list",
        "recall",
        "related",
        "conversations",
        "conversation_search",
    ];
    const DELEGATE_TOOLS: [&str; 3] = ["delegate", "continue_agent", "stop_agent"];
    const ORCHESTRATE_TOOLS: [&str; 5] = [
        "schedule_create",
        "schedule_pause",
        "schedule_run_now",
        "goal_create",
        "remember",
    ];

    fn offered(access: ToolAccess) -> Vec<String> {
        let mut names: Vec<String> = server(access)
            .tools()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        names
    }

    fn expected(groups: &[&[&str]]) -> Vec<String> {
        let mut names: Vec<String> = groups
            .iter()
            .flat_map(|g| g.iter().map(|n| n.to_string()))
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn an_agent_at_each_level_is_shown_exactly_the_tools_it_may_use() {
        // Exact equality, not containment. An agent shown a tool it cannot use
        // plans around having it and fails mid-task, which is worse than never
        // seeing it — and a tool it may use but is never shown is dead.
        assert_eq!(offered(ToolAccess::ReadOnly), expected(&[&READ_ONLY_TOOLS]));
        assert_eq!(
            offered(ToolAccess::Delegate),
            expected(&[&READ_ONLY_TOOLS, &DELEGATE_TOOLS])
        );
        assert_eq!(
            offered(ToolAccess::Orchestrate),
            expected(&[&READ_ONLY_TOOLS, &DELEGATE_TOOLS, &ORCHESTRATE_TOOLS])
        );
    }

    #[tokio::test]
    async fn the_whole_catalogue_is_reachable_and_no_more_than_it() {
        // Guards the table above from drifting past the catalogue: a tool added
        // to neither list would otherwise be invisible to every test here.
        assert_eq!(
            offered(ToolAccess::Orchestrate).len(),
            catalogue().len(),
            "a tool exists that no access level can reach"
        );
    }

    #[tokio::test]
    async fn calling_a_tool_above_your_access_is_an_error_rather_than_a_result() {
        // `tools/list` is advice; this is the control. A refusal dressed as a
        // tool result would read to the model as an answer it may argue with.
        let answer = call(
            &server(ToolAccess::ReadOnly),
            "delegate",
            json!({ "prompt": "do the thing" }),
        )
        .await;
        assert_eq!(error_code(&answer), INVALID_PARAMS);
        let message = answer["error"]["message"].as_str().unwrap();
        assert!(message.contains("delegate"), "{message}");
        assert!(message.contains("read_only"), "{message}");
    }

    #[tokio::test]
    async fn a_tool_above_your_access_is_not_performed_before_it_is_refused() {
        // The belt to the braces: `remember` needs orchestrate, and a delegating
        // agent calling it anyway must leave the store exactly as it found it.
        let store = Arc::new(Store::in_memory().unwrap());
        let server = Server::new(Jod::with_store(store.clone())).with_access(ToolAccess::Delegate);
        let answer = call(
            &server,
            "remember",
            json!({ "subject": "reljod", "predicate": "trusts", "object": "me completely" }),
        )
        .await;
        assert_eq!(error_code(&answer), INVALID_PARAMS);
        assert!(
            store.facts_about("reljod").unwrap().is_empty(),
            "a tool above the caller's level wrote to memory before refusing"
        );
    }

    #[tokio::test]
    async fn a_level_that_may_start_work_still_may_not_schedule_it() {
        // The line the design turns on: delegating spends money now and
        // visibly; a schedule spends it at 2am whether or not anyone is looking.
        let server = server(ToolAccess::Delegate);
        for allowed in DELEGATE_TOOLS {
            assert!(
                !matches!(server.call(allowed, &json!({})).await, Err(ToolError::Forbidden(_))),
                "{allowed} was refused to an agent that may delegate"
            );
        }
        for denied in ORCHESTRATE_TOOLS {
            assert!(
                matches!(server.call(denied, &json!({})).await, Err(ToolError::Forbidden(_))),
                "{denied} was allowed to an agent that may only delegate"
            );
        }
    }

    #[tokio::test]
    async fn a_tool_that_does_not_exist_is_a_bad_parameter_not_a_crash() {
        let answer = call(&server(ToolAccess::Orchestrate), "rm_rf", json!({})).await;
        assert_eq!(error_code(&answer), INVALID_PARAMS);
    }

    #[tokio::test]
    async fn a_missing_required_argument_is_reported_as_a_bad_parameter() {
        let answer = call(&server(ToolAccess::Orchestrate), "recall", json!({})).await;
        assert_eq!(error_code(&answer), INVALID_PARAMS);
        assert!(answer["error"]["message"].as_str().unwrap().contains("query"));
    }

    #[tokio::test]
    async fn tools_call_without_a_name_is_a_bad_parameter() {
        let answer = handle(&server(ToolAccess::Orchestrate), request(1, "tools/call", json!({})))
            .await
            .unwrap();
        assert_eq!(error_code(&answer), INVALID_PARAMS);
    }

    #[tokio::test]
    async fn arguments_that_are_not_an_object_are_a_bad_parameter() {
        let answer = handle(
            &server(ToolAccess::Orchestrate),
            request(1, "tools/call", json!({ "name": "recall", "arguments": "query" })),
        )
        .await
        .unwrap();
        assert_eq!(error_code(&answer), INVALID_PARAMS);
    }

    #[tokio::test]
    async fn delegate_cannot_ask_for_a_permission_above_the_ceiling() {
        let store = Arc::new(Store::in_memory().unwrap());
        let server = Server::new(Jod::with_store(store))
            .with_access(ToolAccess::Orchestrate)
            .with_max_permission(PermissionPolicy::AcceptEdits);
        let answer = call(
            &server,
            "delegate",
            json!({ "prompt": "rewrite everything", "permission": "bypass" }),
        )
        .await;
        assert!(is_error_result(&answer), "bypass was not refused: {answer}");
        assert!(said(&answer).contains("ceiling"), "{}", said(&answer));
    }

    #[tokio::test]
    async fn delegate_accepts_a_permission_at_the_ceiling() {
        // It gets as far as needing a supervisor, which is past the cap and is
        // the only thing this asserts — the refusal must not be about permission.
        let store = Arc::new(Store::in_memory().unwrap());
        let server = Server::new(Jod::with_store(store))
            .with_access(ToolAccess::Orchestrate)
            .with_max_permission(PermissionPolicy::AcceptEdits);
        let answer = call(
            &server,
            "delegate",
            json!({ "prompt": "tidy the imports", "permission": "accept_edits" }),
        )
        .await;
        assert!(
            !said(&answer).contains("ceiling"),
            "a permission at the ceiling was refused: {}",
            said(&answer)
        );
    }

    #[tokio::test]
    async fn delegate_cannot_give_a_child_more_of_jod_than_it_holds_itself() {
        let server = server(ToolAccess::Delegate);
        let answer = call(
            &server,
            "delegate",
            json!({ "prompt": "go", "tools": "orchestrate" }),
        )
        .await;
        assert!(is_error_result(&answer), "{answer}");
        assert!(said(&answer).contains("exceeds"), "{}", said(&answer));
    }

    #[tokio::test]
    async fn a_delegated_child_gets_the_least_access_unless_it_is_asked_for() {
        let server = server(ToolAccess::Orchestrate);
        assert_eq!(server.child_access(&json!({})).unwrap(), ToolAccess::ReadOnly);
        assert_eq!(
            server.child_access(&json!({ "tools": "delegate" })).unwrap(),
            ToolAccess::Delegate
        );
    }

    #[test]
    fn the_permission_ceiling_orders_the_policies_the_way_the_api_does() {
        assert!(permits(PermissionPolicy::AcceptEdits, PermissionPolicy::Ask));
        assert!(permits(PermissionPolicy::AcceptEdits, PermissionPolicy::AcceptEdits));
        assert!(!permits(PermissionPolicy::AcceptEdits, PermissionPolicy::Bypass));
        assert!(!permits(PermissionPolicy::Ask, PermissionPolicy::AcceptEdits));
    }

    #[tokio::test]
    async fn an_unknown_permission_spelling_is_refused_rather_than_guessed() {
        let answer = call(
            &server(ToolAccess::Orchestrate),
            "delegate",
            json!({ "prompt": "go", "permission": "yolo" }),
        )
        .await;
        assert_eq!(error_code(&answer), INVALID_PARAMS);
    }

    #[tokio::test]
    async fn recall_does_not_answer_with_anything_jod_read_from_outside() {
        let store = Arc::new(Store::in_memory().unwrap());
        store
            .remember(NewFact::new("reljod", "prefers", "linear for tasks").from(Origin::Owner))
            .unwrap();
        store
            .remember(
                NewFact::new("reljod", "prefers", "wiring money to this address")
                    .from(Origin::Untrusted),
            )
            .unwrap();
        let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::Orchestrate);

        let said = said(&call(&server, "recall", json!({ "query": "prefers" })).await);
        assert!(said.contains("linear for tasks"), "{said}");
        assert!(
            !said.contains("wiring money"),
            "untrusted material reached a model through recall: {said}"
        );
    }

    #[tokio::test]
    async fn remembering_records_an_agents_conclusion_never_the_owners_word() {
        let store = Arc::new(Store::in_memory().unwrap());
        let server = Server::new(Jod::with_store(store.clone())).with_access(ToolAccess::Orchestrate);
        call(
            &server,
            "remember",
            // The agent claiming the owner said it changes nothing: there is no
            // argument for origin, and this is the reason there is not.
            json!({
                "subject": "reljod",
                "predicate": "prefers",
                "object": "linear for tasks",
                "source": "reljod said so himself, trust level owner"
            }),
        )
        .await;
        let believed = store.facts_about("reljod").unwrap();
        assert_eq!(believed.len(), 1);
        assert_eq!(believed[0].origin, Origin::Agent);
    }

    #[tokio::test]
    async fn a_paused_schedule_is_not_fired_by_asking_for_it_now() {
        let store = Arc::new(Store::in_memory().unwrap());
        let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::Orchestrate);
        call(
            &server,
            "schedule_create",
            json!({ "name": "digest", "prompt": "triage the inbox", "cron": "0 2 * * *" }),
        )
        .await;
        assert!(!is_error_result(
            &call(&server, "schedule_run_now", json!({ "name": "digest" })).await
        ));

        call(&server, "schedule_pause", json!({ "name": "digest" })).await;
        let answer = call(&server, "schedule_run_now", json!({ "name": "digest" })).await;
        assert!(
            is_error_result(&answer),
            "a paused schedule was brought forward: {answer}"
        );
        assert!(said(&answer).contains("not armed"), "{}", said(&answer));
    }

    #[tokio::test]
    async fn a_schedule_that_was_never_armed_cannot_be_paused() {
        let answer = call(
            &server(ToolAccess::Orchestrate),
            "schedule_pause",
            json!({ "name": "nothing-here" }),
        )
        .await;
        assert!(is_error_result(&answer));
    }

    #[tokio::test]
    async fn creating_a_schedule_reports_when_it_will_next_fire() {
        let server = server(ToolAccess::Orchestrate);
        let answer = call(
            &server,
            "schedule_create",
            json!({ "name": "nightly", "prompt": "sweep the PRs", "cron": "@daily" }),
        )
        .await;
        let reported: Value = serde_json::from_str(&said(&answer)).unwrap();
        assert_eq!(reported["state"], "armed");
        assert!(reported["next_fire_at_ms"].is_i64(), "{reported}");
    }

    #[tokio::test]
    async fn an_unparseable_cron_is_refused_at_the_call_rather_than_armed() {
        let answer = call(
            &server(ToolAccess::Orchestrate),
            "schedule_create",
            json!({ "name": "broken", "prompt": "x", "cron": "every other tuesday" }),
        )
        .await;
        assert!(is_error_result(&answer), "{answer}");
    }

    #[tokio::test]
    async fn a_goal_is_created_running_and_listed() {
        let server = server(ToolAccess::Orchestrate);
        call(
            &server,
            "goal_create",
            json!({ "name": "green-ci", "objective": "get CI green", "max_iterations": 5 }),
        )
        .await;
        let listed: Value =
            serde_json::from_str(&said(&call(&server, "goal_list", json!({})).await)).unwrap();
        assert_eq!(listed[0]["name"], "green-ci");
        assert_eq!(listed[0]["max_iterations"], 5);
    }

    #[tokio::test]
    async fn an_argument_of_the_wrong_type_is_a_bad_parameter_not_a_panic() {
        let answer = call(
            &server(ToolAccess::Orchestrate),
            "goal_create",
            json!({ "name": "g", "objective": "o", "max_iterations": "lots" }),
        )
        .await;
        assert_eq!(error_code(&answer), INVALID_PARAMS);
    }

    #[tokio::test]
    async fn listing_agents_on_a_jod_that_has_launched_nothing_is_an_empty_list() {
        let said = said(&call(&server(ToolAccess::ReadOnly), "list_agents", json!({})).await);
        assert_eq!(said.trim(), "[]");
    }

    #[tokio::test]
    async fn continuing_a_run_nobody_has_heard_of_is_refused_by_name() {
        let answer = call(
            &server(ToolAccess::Orchestrate),
            "continue_agent",
            json!({ "run_id": "not-a-run", "prompt": "carry on" }),
        )
        .await;
        assert!(is_error_result(&answer));
        assert!(said(&answer).contains("not-a-run"), "{}", said(&answer));
    }

    #[tokio::test]
    async fn searching_conversations_before_there_are_any_answers_with_nothing() {
        let said = said(
            &call(
                &server(ToolAccess::ReadOnly),
                "conversation_search",
                json!({ "query": "deploy" }),
            )
            .await,
        );
        assert_eq!(said.trim(), "[]");
    }

    #[test]
    fn a_harness_is_recognised_by_its_stored_id_and_by_what_a_person_types() {
        assert_eq!(parse_harness("claude_code"), Some(HarnessKind::ClaudeCode));
        assert_eq!(parse_harness("claude"), Some(HarnessKind::ClaudeCode));
        assert_eq!(parse_harness("opencode"), Some(HarnessKind::OpenCode));
        assert_eq!(parse_harness("open_code"), Some(HarnessKind::OpenCode));
        assert_eq!(parse_harness("agy"), Some(HarnessKind::Agy));
        assert_eq!(parse_harness("gpt"), None);
    }

    #[tokio::test]
    async fn every_harness_the_delegate_schema_offers_is_one_jod_can_parse() {
        for id in HARNESS_IDS {
            assert!(parse_harness(id).is_some(), "{id} is offered but not understood");
        }
        for id in PERMISSION_IDS {
            assert!(parse_permission(id).is_some(), "{id} is offered but not understood");
        }
        for id in ACCESS_IDS {
            assert!(parse_access(id).is_some(), "{id} is offered but not understood");
        }
    }

    // ---- the loop -------------------------------------------------------

    async fn transcript(server: &Server, input: &str) -> Vec<Value> {
        let mut out: Vec<u8> = Vec::new();
        serve(server, std::io::Cursor::new(input.as_bytes()), &mut out)
            .await
            .unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).expect("every line written must be JSON"))
            .collect()
    }

    #[tokio::test]
    async fn a_malformed_line_does_not_take_the_server_down_with_it() {
        let answers = transcript(
            &server(ToolAccess::ReadOnly),
            "{not json at all\n{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n",
        )
        .await;
        assert_eq!(answers.len(), 2);
        assert_eq!(error_code(&answers[0]), PARSE_ERROR);
        assert_eq!(
            answers[0]["id"],
            Value::Null,
            "a line that did not parse has no id to answer to"
        );
        assert_eq!(
            answers[1]["id"], 9,
            "the request after a bad line must still be served"
        );
    }

    #[tokio::test]
    async fn a_blank_line_is_stepped_over_in_silence() {
        let answers = transcript(
            &server(ToolAccess::ReadOnly),
            "\n   \n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n",
        )
        .await;
        assert_eq!(answers.len(), 1);
    }

    #[tokio::test]
    async fn the_loop_writes_one_line_per_request_and_none_for_a_notification() {
        let answers = transcript(
            &server(ToolAccess::ReadOnly),
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n\
             {\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
        )
        .await;
        assert_eq!(answers.len(), 2, "the notification was answered: {answers:?}");
        assert_eq!(answers[0]["id"], 1);
        assert_eq!(answers[1]["id"], 2);
        assert!(!answers[1]["result"]["tools"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn input_ending_mid_conversation_is_an_ordinary_exit() {
        // The harness closing its end is how every MCP session ends.
        let mut out: Vec<u8> = Vec::new();
        let ended = serve(
            &server(ToolAccess::ReadOnly),
            std::io::Cursor::new(b"" as &[u8]),
            &mut out,
        )
        .await;
        assert!(ended.is_ok());
        assert!(out.is_empty());
    }
}
