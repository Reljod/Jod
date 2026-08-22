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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};

use crate::cards::{Card, CardKind, Importance, NewCard, Source, Status};
use crate::delivery;
use crate::event::AgentEvent;
use crate::harness::{default_name, Role, ToolAccess};
use crate::orchestrator::Delegation;
use crate::schedule::{Goal, GoalState, Schedule, ScheduleState};
use crate::secrets;
use crate::service::{default_cwd, AgentStatus, RunConversation};
use crate::store::{NewFact, Origin, Store, DEFAULT_SCOPE};
use crate::team::{Caller, Kind, Post, Sent};
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

/// How long [`Tool::ask`] waits for a reply when nobody says otherwise.
///
/// Long enough for a peer to be woken by the next tick and take a turn — which
/// is the shortest honest answer to "how long does a colleague take" — and
/// short enough that a run blocked on a dead peer is stuck for two minutes
/// rather than for ever. **There is deliberately no way to wait without a
/// deadline**: A5 exists because an agent that can hang waiting for a peer can
/// hang for ever, and that is how a fleet deadlocks.
pub const ASK_DEADLINE_SECS: i64 = 120;

/// The longest wait a caller may ask for. A cap rather than a default, because
/// the argument is the model's and the bound is not.
pub const MAX_ASK_DEADLINE_SECS: i64 = 600;

/// How often a wait looks for its answer. Cheap — one indexed read of a local
/// SQLite file — so this is about how quickly a reply is noticed, not about
/// load.
const ASK_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// How long a **blocking** [`Tool::ask_question`] holds its run waiting for a
/// person, when the caller does not say.
///
/// Five minutes rather than `ask`'s two, because the two waits are on different
/// things: `ask` waits for a peer that Jod itself will wake within a tick, and
/// this waits for Reljod to look at the rail. Long enough to cover somebody
/// finishing a sentence and turning to the screen; short enough that a question
/// asked while nobody is at the desk costs one wait rather than a night of a
/// model context and a tmux session held open.
///
/// **There is deliberately no way to wait without a deadline.** The same rule
/// A5 states for the bus holds here for the same reason: an agent that can wait
/// for ever is an agent that hangs when the thing it waits for never comes, and
/// a human who has gone to bed is exactly that. When the deadline passes the
/// card stays open — giving up waiting is not withdrawing the question — and
/// the answer reaches the run later, through the ordinary delivery path.
pub const CARD_ANSWER_DEADLINE_SECS: i64 = 300;

/// The longest wait a caller may ask for. A cap rather than a default, because
/// the argument is the model's and the bound is not.
pub const MAX_CARD_WAIT_SECS: i64 = 1_800;

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

fn strings(description: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": description })
}

const HARNESS_IDS: [&str; 3] = ["claude_code", "open_code", "agy"];
const IMPORTANCE_IDS: [&str; 3] = ["low", "normal", "high"];
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
                "One page of the agents Jod knows about, running or finished, each with its \
                 last message. Running ones come first, then the newest. The reply says how \
                 many exist in `total` and how many the page left out in `hidden`; if `hidden` \
                 is above zero, ask again with a bigger `limit` to see the rest. Check this \
                 before delegating: continuing a warm agent that already has the context beats \
                 starting a cold one that has to rediscover it.\n\n\
                 **`reuse` is the answer to that in one sentence, and `idle` is the list of run \
                 ids behind it, newest first.** Read those rather than working availability out \
                 of `status`. Prefer a free agent for a new instruction even when the subject \
                 has changed: it already holds the checkout, and what it knows about the \
                 repository is worth more than the instruction matching what it was last \
                 doing.\n\n\
                 One exception, and it is the important one: an agent with `stalled_for_ms` \
                 set **cannot be continued**. It is wedged — still `running`, because it is, \
                 but it has produced nothing for that long and it will not answer you. Say so, \
                 start a fresh session beside it, and leave the stalled one for Reljod to stop. \
                 This is why `busy: false` is not the test for availability and `free` is: a \
                 stalled agent is not busy either. `busy` means what `status: running` used to \
                 — working, and not stuck.\n\n\
                 Each agent also says which `project` it is on and which `work` it belongs to. \
                 Group by those rather than by `cwd` — a session holding a worktree lease has \
                 the worktree as its cwd, not the checkout, so two agents on one repository \
                 look like two repositories.",
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "running_only": { "type": "boolean", "description": "Only agents still working." },
                    "project": text(
                        "Only agents on this project, by name. What a project manager asks \
                         with, so it reads its own repository instead of the whole fleet."
                    ),
                    "limit": int(
                        "How many agents to return. Default 20. Running agents are listed \
                         first, so a small limit drops finished ones before running ones."
                    )
                }),
                &[],
            ),
        },
        Tool {
            name: "interrupt_main",
            description:
                "Stop the turn a chat currently has in flight, so the message queued behind it \
                 is delivered now instead of when that turn ends.\n\n\
                 The assistant standing at the door is the only caller. It is handed one \
                 message Reljod typed into a chat that was already working, and this is what \
                 it calls when that message cannot wait — a stop, a correction, a \
                 contradiction of the instruction in flight.\n\n\
                 Stopping ends the run process and nothing else. The conversation and the \
                 harness session both survive, so the queued message arrives as the very next \
                 turn and the chat carries on from there. You do not deliver it yourself and \
                 you must not try to answer it.\n\n\
                 Whatever the stopped turn had not finished is lost. Hold instead of calling \
                 this whenever you are unsure.",
            // The floor, because the gate on this tool is *who is calling*
            // rather than how much of Jod they hold: a doorman is spawned with
            // the smallest toolbox anything gets, and needing more than that
            // would mean handing it verbs it has no business holding just to
            // reach the one it does.
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "run_id": text("The run to stop, as it was named in what you were handed."),
                    "reason": text(
                        "One sentence for Reljod, saying why his turn stopped. Written into \
                         that chat's own transcript, so write it for him."
                    )
                }),
                &["run_id", "reason"],
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
                    // No `permission`. The mode a child runs in is the
                    // operator's answer, not this model's — see
                    // [`Server::child_permission`], where the reasoning lives.
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
                 once that run has reported a session id. If stopping this agent also stopped \
                 agents working under it, they are started again too, each carrying on its own \
                 work.",
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
            description:
                "Stop a running agent, everything it forked, and every agent working under it. \
                 The agents it delegated to are stopped too, and so are theirs, all the way \
                 down: stopping a manager stops its workers. The main chat is the one \
                 exception — stopping that stops only the chat, and leaves the projects \
                 running. Continuing a stopped agent brings its workers back.",
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
            // **Not `orchestrate`, and the bundle it used to be in is what was
            // wrong rather than the level of any one caller.** It sat beside
            // `schedule_create` and `goal_create` because all three write
            // something that outlives the turn, but that is not the line
            // [`ToolAccess::may_orchestrate`] draws. That line is about
            // spending money unattended: a schedule spends it every night at
            // 2am whether or not anyone is watching, and a goal spends it until
            // something stops it. Writing a fact spends nothing, starts no
            // process and wakes nobody.
            //
            // What it does cost is the truth of what Jod believes, so the
            // question is who may write it, and `delegate` is already the
            // answer to that question everywhere else. Material from outside
            // never reaches this level at all — `Service::spawn_from_untrusted`
            // caps a run built from a payload, a page or a stranger's pull
            // request to `read_only`, which is the same reason `open_work` and
            // `delegate` give for sitting here. So `delegate` *is* the "started
            // by a person or by main" line, and a fact written by a run on that
            // line is a fact an agent Reljod started concluded. Which is
            // exactly what the description above says it is recorded as.
            //
            // The cost of having it a level too high was not theoretical: every
            // project manager runs at `delegate` and is told in its own brief
            // that memory is most of why a manager is worth having. It was
            // being told that and handed no verb to write one.
            needs: ToolAccess::Delegate,
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
        // ---- the rail -----------------------------------------------------
        //
        // The three tools D2 names, and the reason the rail is the same on all
        // three harnesses instead of a Claude Code feature reimplemented twice.
        //
        // All three sit at `read_only`, which looks wrong for something that
        // writes and is not. A card spends no money, starts no process and
        // costs no peer a turn — it is a sentence addressed to Reljod, and the
        // most confined agent on the box is precisely the one whose choices
        // most need to be visible enough to overrule. Gating emission behind
        // `delegate` would leave the rail empty for it.
        //
        // Note again what appears in no schema below: a conversation. A card
        // belongs to whoever raised it, and that comes from the run — see
        // [`Server::raiser`].
        Tool {
            name: "record_decision",
            description:
                "Say what you decided and what you decided against, so Reljod can overrule it \
                 with one keystroke instead of a conversation. Use it the moment you pick \
                 between real alternatives — a library, a schema, an approach — rather than \
                 saving it for a summary nobody reads until afterwards. Returns at once; \
                 nobody has to be looking.",
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "title": text("The choice, in a few words: `chat DB`, `auth for the webhook`."),
                    "chosen": text("What you went with."),
                    "options": strings(
                        "The alternatives you chose between, as you would offer them. Reljod \
                         switches to one of these by pressing its number, so they are the whole \
                         value of this tool — a decision with no alternatives is a note."
                    ),
                    "why": text("The reasoning, in a sentence or two."),
                    "importance": one_of(
                        "How much this matters if it is wrong. Default normal.",
                        &IMPORTANCE_IDS,
                    )
                }),
                &["title", "chosen"],
            ),
        },
        Tool {
            name: "ask_question",
            description:
                "Put a question to Reljod on the rail and carry on. Returns a card id \
                 immediately: the answer arrives in a later turn, so ask, then do whatever does \
                 not depend on it. Set `blocking` only when you genuinely cannot proceed — that \
                 waits, and even then it gives up after a while rather than hanging the run.",
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "question": text("What you need to know, as one line."),
                    "context": text("What you already know, and what turns on the answer."),
                    "options": strings("Answers you would accept, if the question has a shortlist. Answerable by number."),
                    "importance": one_of("How much it matters. Default normal.", &IMPORTANCE_IDS),
                    "blocking": {
                        "type": "boolean",
                        "description":
                            "You cannot proceed past this. Marks the card `blocked` and waits for \
                             an answer. Default false."
                    },
                    "wait_seconds": int(
                        "How long a blocking question waits. Default 300, capped at 1800. \
                         Ignored when `blocking` is false."
                    )
                }),
                &["question"],
            ),
        },
        Tool {
            name: "request_secret",
            description:
                "Ask for a credential by NAME. You are told a variable exists; you are never \
                 told a value, and this tool cannot carry one — do not paste a key into any \
                 argument. Reljod stores it outside every repository and Jod injects it into \
                 the environment of the *next* run, so it will not reach this one: if you need \
                 it now, you are blocked, and saying so is the correct ending.",
            // `read_only` for a credential request looks alarming and is not:
            // **asking is not getting.** A person answers, the value goes to a
            // file the model cannot read, and the agent is told a name. An
            // untrusted agent asking for a credential is a request Reljod can
            // simply decline — which is strictly better than the two things it
            // would otherwise do, invent one or fail without saying why.
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "name": text("The environment variable's name, e.g. `STRIPE_API_KEY`. A name, never a value."),
                    "hint": text("What it is for and where to get one, in Reljod's terms."),
                    "blocking": {
                        "type": "boolean",
                        "description":
                            "This run cannot finish without it. Default true, because a run that \
                             asks for a credential usually cannot."
                    }
                }),
                &["name", "hint"],
            ),
        },
        // ---- works and roots ----------------------------------------------
        Tool {
            name: "list_roots",
            description:
                "The directories this session may work in, and which of them it may write to. \
                 Read this before editing anything: a read-only root is Reljod's real checkout \
                 and changing it is the one thing you were told not to do.",
            needs: ToolAccess::ReadOnly,
            schema: obj(json!({}), &[]),
        },
        Tool {
            name: "project_list",
            description:
                "Every repository Reljod works on, most recently worked in first, with the \
                 other names he calls each one. This is what an instruction that names no \
                 project has to be resolved against — the roots belong to sessions that have \
                 already exited and the works have already closed, so this is the only record \
                 that outlives them. Each entry also says whether its directory is still on \
                 disk: an entry with `path_usable` false has been deleted, renamed or \
                 unmounted since it was catalogued, and opening work there will be reported \
                 as running and then fail with an error that names the harness binary rather \
                 than the project. Read `path_trouble` and say that instead of starting it.",
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "include_archived": {
                        "type": "boolean",
                        "description": "Include finished and abandoned projects. Default false."
                    }
                }),
                &[],
            ),
        },
        Tool {
            name: "project_current",
            description:
                "Which project this conversation is currently about, and how that was decided. \
                 Read it before assuming: when `how` is `sticky` nothing in the last instruction \
                 named a project and this one simply carried, which is exactly the case most \
                 likely to be wrong.",
            needs: ToolAccess::ReadOnly,
            schema: obj(json!({}), &[]),
        },
        Tool {
            name: "project_switch",
            description:
                "Point this conversation at a different project. Call it when you work out \
                 which repository an instruction meant and it is not the current one — \
                 including when Reljod's words were ambiguous and you resolved them. The \
                 reason is shown to him, so a switch he did not intend is one he can correct.",
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "project": text("The project's name, or any name he calls it."),
                    "reason": text("Why this instruction is about that project. Shown to Reljod.")
                }),
                &["project"],
            ),
        },
        Tool {
            name: "ask_manager",
            description:
                "Hand an instruction to the manager that owns a project. Use it for anything \
                 touching a repository — the manager decides whether to continue an agent \
                 already working on it or open new work, because it is the one conversation \
                 that has seen every instruction about that project.\n\n\
                 Returns as soon as the manager has it, like everything else here. Its answer \
                 arrives as a card on your rail rather than as this call's return value, so do \
                 not wait for it and do not poll. A project with no manager yet gets one, and \
                 the reply says so.\n\n\
                 The reply also names the project it resolved to. Say that in your answer: a \
                 routing decision nobody can see is one nobody can correct.",
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "project": text("The project's name, or any name Reljod calls it."),
                    "instruction": text(
                        "What to do, in his words. Pass it through rather than paraphrasing \
                         — the manager has the context to read it and you do not."
                    ),
                    "harness": one_of(
                        "Which harness runs the manager. Default claude_code.", &HARNESS_IDS
                    )
                }),
                &["project", "instruction"],
            ),
        },
        Tool {
            name: "project_add",
            description:
                "Put a repository in the catalog. Use it when Reljod mentions working somewhere \
                 that is not listed yet — an unlisted project cannot be inferred, so every \
                 later instruction about it has to name the path in full.",
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "path": text("The checkout's directory."),
                    "name": text("What he calls it. Defaults to the directory's name."),
                    "aliases": strings(
                        "Other things he says for it — \"the tetris thing\", \"my agent\". \
                         Lowercased, and matched against what he actually says."
                    ),
                    "notes": text("One line about it, carried into every main-chat turn. Keep it short.")
                }),
                &["path"],
            ),
        },
        Tool {
            name: "project_untrack",
            description:
                "Stop tracking a repository. Use it when Reljod says he is done with a project, \
                 or that it should not be on the fleet any more.\n\n\
                 It comes off the fleet along with its manager and every work under it, off the \
                 catalog `project_list` returns, and out of inference — so an instruction that \
                 names no project can no longer land there. Nothing is deleted: the works, the \
                 sessions and the transcripts all stay, an agent still running in there still \
                 shows up in `list_agents`, and naming the project outright still finds it. \
                 `jod project restore <name>` puts the whole thing back.\n\n\
                 This is a repository leaving Reljod's working set, which is his call and not \
                 yours. Ask before calling it unless he has just said so.",
            // Delegate, on the same line as `project_switch` and `project_add`
            // and for the reason given there: it changes what a *later*
            // instruction resolves to. An untracked project is not inferrable,
            // so the quiet failure is the next vague sentence about that
            // repository landing somewhere else entirely.
            needs: ToolAccess::Delegate,
            schema: obj(
                // Only the name. A `reason` field would have nowhere to go —
                // this answers the model, not Reljod, and the catalog has no
                // column for why an entry left it.
                json!({ "project": text("The project's name, or any name he calls it.") }),
                &["project"],
            ),
        },
        Tool {
            name: "claim_worktree",
            description:
                "Claim somewhere to write, before you change anything. Your roots start \
                 read-only — they are Reljod's real checkout — and this cuts a branch and a \
                 worktree of your own and makes that your one writable root, with the checkout \
                 still beside it so you can diff against what he is editing. A sibling already \
                 working on this repository is offered its worktree rather than a second branch \
                 being cut. Call it once, when you first need to write; not at the start out of \
                 habit. Read the `writable` field in the answer before you write: it says \
                 whether this session can really write in the worktree, and `no` or \
                 `unverified` means stop and say so rather than trying anyway.",
            // **D5's explicit step, and the reason it has to be a tool.**
            // "Detect the first write" has no harness-agnostic implementation —
            // every harness spells its pre-write hook differently and two of
            // the three barely have one — so the claim is something the agent
            // *does*, not something Jod notices. An agent that cannot call this
            // has been told to claim a worktree and given no way to obey, which
            // makes the instruction in its preamble unfollowable.
            //
            // `delegate` rather than `read_only`: this cuts a branch and
            // creates a directory. It is the one card-adjacent verb that
            // changes the world outside the database.
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "repo": text(
                        "The repository to cut a branch of, as an absolute path. Defaults to \
                         your first root — `list_roots` says what that is."
                    )
                }),
                &[],
            ),
        },
        Tool {
            name: "release_worktree",
            description:
                "Give back a worktree you claimed. It is removed only when it is clean and its \
                 branch is merged; otherwise it is kept and you are told why, because a \
                 directory costs nothing and somebody's uncommitted afternoon does not. Your \
                 writable root goes away either way, so claim again if you need to write more.",
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "lease_id": int(
                        "Which one. Omit when you hold exactly one, which is the ordinary case."
                    )
                }),
                &[],
            ),
        },
        Tool {
            name: "open_work",
            description:
                "Open a work — one intent, its own board, its own colour — and start the first \
                 session on it against a checkout it may read and not write. Returns as soon as \
                 that session is launched; it is titled in the background and it claims a \
                 worktree itself the moment it needs one. Use this rather than `delegate` when \
                 the instruction is about a repository and will take more than one session.",
            // **This tool is what makes E4.S4 reachable at all.** The
            // orchestrator acts through MCP tools rather than through a parsed
            // JSON `Decision`, so a routing outcome with no tool behind it
            // cannot be chosen by the model — it would be a `core` function
            // only Jod-side code could call, present in the codebase and never
            // invoked. Written down because "the orchestrator can already
            // delegate" is exactly the argument that would remove it.
            //
            // The line `delegate` sits on, and for the same reason: this starts
            // an agent, and the thing you least want an unattended run to hold
            // is the power to create more unattended runs. A webhook-triggered
            // agent must not be able to open works.
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "instruction": text("What the work is, in Reljod's own words. It becomes the first task on the board."),
                    "checkout": text(
                        "The repository this happens in, as an absolute path. Defaults to your \
                         own first root — `list_roots` says what that is."
                    ),
                    "harness": one_of("Which harness runs the first session. Default claude_code.", &HARNESS_IDS),
                    "model": text("Model override, in the harness's own spelling."),
                    // No `permission`, for the reason `delegate` has none —
                    // [`Server::child_permission`].
                    "tools": one_of(
                        "How much of Jod the first session may reach. Capped at your own. \
                         Default delegate, so it can talk to its siblings and start its own.",
                        &ACCESS_IDS,
                    ),
                    "placement": one_of(
                        "Where this engineer is allowed to write, and it is your call rather \
                         than its own. `explore` = read-only: no branch, no worktree, no pull \
                         request, right for anything that only looks. `worktree` = a branch and \
                         worktree of its own, cut before its session starts, for anything that \
                         writes. `share` = join the worktree another work already holds, named \
                         in `share_with`. `direct` = write in Reljod's real checkout, allowed \
                         only where there is no git remote, no other work on the project and \
                         nothing uncommitted — ask for it when any of those is false and you \
                         are refused with every failing reason at once.\n\n\
                         Leaving this out is not the same as `explore`. An unplaced session \
                         starts on the read-only checkout and calls `claim_worktree` for itself \
                         the moment it needs to write, which is how every session worked before \
                         placements existed; `explore` tells it plainly that it was opened to \
                         look and that needing to write is a thing to report.",
                        &crate::leases::PLACEMENT_IDS,
                    ),
                    "share_with": text(
                        "The id of the work whose worktree to join. Required when `placement` \
                         is `share`, and meaningless without it."
                    ),
                    "paths": strings(
                        "The files this engineer owns, as repository-relative path prefixes, \
                         and the only ones it may change. Recorded on the work's first task, \
                         which is what keeps two engineers sharing one worktree off each \
                         other's files. Leave it out for anything that only reads."
                    )
                }),
                &["instruction"],
            ),
        },
        // ---- the board ------------------------------------------------------
        //
        // A work has had a board since works existed, and until these three
        // there was no verb anywhere in this catalogue that put a task on one.
        // A manager told in its brief to break an instruction down had nowhere
        // to put the breakdown — the same trap `claim_lease` fell into before
        // `claim_worktree` named it, and it makes the instruction in a preamble
        // unfollowable rather than merely unhelpful.
        Tool {
            name: "plan_work",
            description:
                "Write a job's whole breakdown onto its board in one call: one task per \
                 engineer, and every task naming the files that engineer owns and nobody else \
                 touches.\n\n\
                 **Two tasks that claim the same file are refused**, by title and by path, \
                 before any of it is handed out. Two engineers editing one file in one worktree \
                 is a merge conflict neither of them can see coming, so plan around it rather \
                 than discovering it. A task that only reads — a look, a search, a review — \
                 owns no files at all, and an empty `paths` is the honest way to say so.\n\n\
                 Write the tasks in the order the work has to happen in. That is the board's \
                 order, and it is the order `stack_pull_requests` stacks the pull requests in, \
                 so a plan written in the order you happened to think of things produces a \
                 stack whose bases are wrong.\n\n\
                 The whole plan goes in one call, because a plan accumulated a task at a time \
                 cannot be checked for collisions before it is handed out. A refused plan \
                 writes nothing: the board is exactly as it was. Planning again later is fine, \
                 and the new tasks are checked against the ones still open as well as against \
                 each other.",
            // `delegate`, on the same line as `open_work` and for a related
            // reason. This does not start an agent itself, but it is the call
            // that decides what agents are started to do and where each of them
            // may write — and completing the last task on a board closes the
            // work. A run built from a webhook or a stranger's pull request is
            // capped at `read_only` by `Service::spawn_from_untrusted`, which
            // is precisely the material that should not be laying out who owns
            // which files in Reljod's repository.
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "work_id": text("The work to plan. `open_work` returns one; `work_board` reads one back."),
                    "tasks": {
                        "type": "array",
                        "description":
                            "The breakdown, in dependency order — the first task is the one \
                             everything else is built on.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": text("What this engineer is to do, in one line."),
                                "paths": strings(
                                    "The files this task owns, as repository-relative path \
                                     prefixes — `core/src/mcp.rs`, `cli/src/tui`. Leave it \
                                     empty for a task that only reads."
                                )
                            },
                            "required": ["title"]
                        }
                    }
                }),
                &["work_id", "tasks"],
            ),
        },
        Tool {
            name: "work_board",
            description:
                "Every task on a work's board, in the order it was planned, each with its \
                 owner, whether it is done, and the files it owns.\n\n\
                 This is how you find out whether a job is finished without asking an engineer. \
                 While one task is still open the job is not finished, whatever anybody's last \
                 message sounded like. When nothing is open, the work has closed itself and \
                 what it produced is yours to report.",
            // Reading a board costs nothing and hides nothing, which is the
            // line every other `read_only` tool here sits on.
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({ "work_id": text("The work whose board to read.") }),
                &["work_id"],
            ),
        },
        Tool {
            name: "complete_task",
            description:
                "Say your task is done, and report what you did. This marks it off the board \
                 and delivers your report into your manager's conversation, which starts your \
                 manager's next turn.\n\n\
                 **Your report reaches your manager and nobody above it.** Reljod is not \
                 reading this transcript, so what you write here is the whole of what he will \
                 be told you did — write it for somebody who has not seen your work. Say what \
                 you changed, what you deliberately left alone, and anything outside your own \
                 files that needs changing, because widening the plan is your manager's call \
                 and not yours.\n\n\
                 Call it once, when the task is genuinely finished. The answer says whether \
                 yours was the last open task, so your closing line is not a guess about \
                 whether anybody else is still working.",
            // `read_only` for something that writes, and it is the line
            // `record_decision` already sits on: this writes a row in Jod's own
            // database, starts no agent, spends nothing and cuts no branch.
            //
            // The level has to be this low for a second reason that is not
            // about the effect at all. An engineer is spawned at whatever
            // `open_work` was asked for, which includes `read_only` — an
            // engineer sent to read, search or review. An engineer that cannot
            // report is one whose work is invisible, so putting reporting
            // behind `delegate` would silence exactly the most confined
            // sessions on the box.
            //
            // **It is deliberately not a card.** Cards cascade upward through
            // the whole ancestor chain, so a routine "I finished" raised as one
            // arrives on main's rail three links up, which is the bug this
            // whole change exists to fix. `ask_question` and `request_secret`
            // still cascade and still reach Reljod, because a blocked engineer
            // must get through whether or not its manager is running again this
            // hour. The manager owns reporting, not escalation.
            needs: ToolAccess::ReadOnly,
            schema: obj(
                json!({
                    "task_id": text("The task you were given. Your brief names it; `work_board` lists them."),
                    "report": text(
                        "What you did, for a manager that did not watch you do it. This is \
                         relayed upward — it is not a note to yourself."
                    )
                }),
                &["task_id", "report"],
            ),
        },
        Tool {
            name: "stack_pull_requests",
            description:
                "Link a job's pull requests into a stack on GitHub, bottom to top.\n\n\
                 Three engineers on one job cut three branches from the same starting point, so \
                 you get three pull requests that each claim to change the same files from the \
                 same base, and whoever merges first breaks the other two. Stacking them says \
                 which sits on which, and each diff shrinks to the part its own engineer \
                 added.\n\n\
                 What comes back is the ordered list and the `gh stack link` command — Jod does \
                 not run it, because you are the one who can see whether the order still holds \
                 and which branches have already landed. The order is the order you planned the \
                 tasks in, so it is your own stated order rather than the order the engineers \
                 happened to finish in.\n\n\
                 Refused when the work has fewer than two pull requests, and the refusal says \
                 how many it found. One pull request is not a stack, and linking it rewrites \
                 its base branch in public for no gain.",
            // `delegate` rather than `read_only`, even though what comes back
            // is a command line rather than a call Jod makes. What decides the
            // level is the effect the tool exists to cause: linking rewrites
            // the base branch of every pull request named on that line, which
            // is a visible change to open work on somebody else's screen.
            // `claim_worktree` sits here for the same reason — it is the other
            // tool whose effect lands outside the database.
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({ "work_id": text("The work whose pull requests to stack.") }),
                &["work_id"],
            ),
        },
        // ---- the bus ------------------------------------------------------
        //
        // Reading is free; writing costs a peer a turn, which is money spent
        // now — the same line `delegate` sits on. Note what does *not* appear
        // in any schema below: who is sending. That comes from the run, and an
        // agent that could name its own sender could send as anyone.
        Tool {
            name: "roster",
            description:
                "Who you can reach from here, with each one's role, harness, whether it is idle, \
                 and how much mail it already has waiting. Read this before writing to a name: a \
                 message to a name nobody answers to is a message nobody reads.",
            needs: ToolAccess::ReadOnly,
            schema: obj(json!({}), &[]),
        },
        Tool {
            name: "read_messages",
            description:
                "Take everything waiting in your inbox. Each message comes back with its id and \
                 thread, so you can reply into the conversation it belongs to. Messages are \
                 handed over once — read them before asking a peer something they may already \
                 have answered.",
            needs: ToolAccess::ReadOnly,
            schema: obj(json!({}), &[]),
        },
        Tool {
            name: "send_message",
            description:
                "Send to one teammate by name, or to everyone here if you omit `to`. Returns as \
                 soon as it is on the bus — the recipient reads it on its next turn, which Jod \
                 starts for it. Questions, findings and handoffs belong here; ownership of code \
                 does not — that is a lease or a branch, never a message saying you are editing \
                 something.",
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "to": text("Who to send it to, as the roster spells it. Omit to tell everybody."),
                    "text": text("What to say.")
                }),
                &["text"],
            ),
        },
        Tool {
            name: "reply",
            description:
                "Answer a message you were sent, keeping it in the same thread. Prefer this to \
                 send_message when you are answering: a thread is what makes an exchange readable \
                 afterwards and what the depth bound counts.",
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "message_id": int("The message you are answering, as read_messages reported it."),
                    "text": text("Your answer.")
                }),
                &["message_id", "text"],
            ),
        },
        Tool {
            name: "ask",
            description:
                "Send a question and wait for the answer, up to a deadline. Returns the reply, or \
                 says plainly that none came — it never waits for ever, because the peer might be \
                 dead. Costs a turn of theirs and blocks yours, so use send_message when you do \
                 not need the answer to carry on.",
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "to": text("Who to ask, as the roster spells it."),
                    "text": text("The question."),
                    "timeout_seconds": int("How long to wait. Default 120, capped at 600.")
                }),
                &["to", "text"],
            ),
        },
        Tool {
            name: "handoff",
            description:
                "Give a task to somebody else: moves ownership on the board and tells them, in \
                 one call. Use this rather than asking them to pick it up, so who owns it never \
                 depends on both of you having read the same sentence.",
            needs: ToolAccess::Delegate,
            schema: obj(
                json!({
                    "to": text("Who is taking it over."),
                    "text": text("What they need to know to carry it on."),
                    "task_id": text("The task on the board to move. Omit to hand over something that is not a task.")
                }),
                &["to", "text"],
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
///
/// Visible to the rest of the crate because the preambles have to be checked
/// against it. A brief that names a tool the run's level filters out of the
/// catalogue is broken in exactly the way a brief that names a misspelled tool
/// is broken, and the only way for
/// [`crate::orchestrator`]'s test to know the difference between the two is to
/// ask the same question this function answers — see
/// `every_tool_the_preamble_tells_an_agent_to_call_is_one_that_exists`.
pub(crate) fn allows(have: ToolAccess, need: ToolAccess) -> bool {
    match need {
        ToolAccess::ReadOnly => true,
        ToolAccess::Delegate => have.may_delegate(),
        ToolAccess::Orchestrate => have.may_orchestrate(),
    }
}

/// Is `requested` within `ceiling`?
///
/// `Plan < Ask < AcceptEdits < Bypass`, ordered by how much can happen without
/// anyone being asked — the same rule `jod-api` applies to a remote caller in
/// `api/src/config.rs`. Restated here rather than shared because `jod-core`
/// cannot depend on `jod-api`; if one side's ordering changes, the other has to
/// change with it.
///
/// The rank is [`PermissionPolicy::ALL`]'s own index, so a mode added there and
/// forgotten here is impossible rather than merely unlikely.
pub fn permits(ceiling: PermissionPolicy, requested: PermissionPolicy) -> bool {
    fn rank(p: PermissionPolicy) -> usize {
        PermissionPolicy::ALL
            .iter()
            .position(|m| *m == p)
            .expect("PermissionPolicy::ALL is missing a variant")
    }
    rank(requested) <= rank(ceiling)
}

/// Why a run cannot be given a follow-up, when it cannot.
///
/// A run's status is the only record of whether its session ended at the end of
/// a sentence or in the middle of one. `killed` and `failed` both mean the
/// harness stopped part-way through: the transcript being resumed breaks off
/// wherever the process happened to be, and the model picks it up believing
/// that half-finished state is its own last turn. Worse, it looks like success
/// from outside — a new run appears, it is `running`, and nothing anywhere says
/// the thing it is continuing was stopped on purpose.
///
/// Refused at the tool boundary for the same reason the permission ceiling is
/// refused there: this is the last point at which the caller still has somewhere
/// useful to go, and `delegate` and `open_work` are that somewhere.
///
/// Every status is written out rather than caught by a wildcard, so a fifth one
/// added later has to be decided on here instead of quietly inheriting whichever
/// answer the wildcard gave.
fn refusal_to_continue(run_id: &str, status: AgentStatus) -> Option<String> {
    match status {
        // The ordinary target of a follow-up, and the run a second instruction
        // reaches mid-task. Both have a session that means what it says.
        AgentStatus::Completed | AgentStatus::Running => None,
        AgentStatus::Killed | AgentStatus::Failed => Some(format!(
            "run `{run_id}` did not finish cleanly: its status is `{}`. Continuing it \
             would resume a session that broke off part-way through a turn nobody \
             completed. Start a fresh agent with `delegate`, or with `open_work` if \
             this belongs to a piece of work you are already tracking.",
            status.as_str()
        )),
    }
}

pub fn parse_permission(s: &str) -> Option<PermissionPolicy> {
    match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        // `plan` and `auto` are what the harnesses call these two, and what a
        // person types; the stored spellings are accepted alongside.
        "plan" => Some(PermissionPolicy::Plan),
        "ask" | "manual" => Some(PermissionPolicy::Ask),
        "accept_edits" | "edits" => Some(PermissionPolicy::AcceptEdits),
        "bypass" | "auto" | "bypass_permissions" => Some(PermissionPolicy::Bypass),
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
    /// Which run this server speaks as, worked out by [`identify`] from the
    /// process group it is in.
    ///
    /// **This is sender identity, and it is why it lives on the server rather
    /// than in any tool's arguments.** A server that belongs to a run answers
    /// as that run's member and can answer as nothing else; one that belongs to
    /// no run — a session somebody opened by hand — cannot send at all, which
    /// is the honest refusal. There is deliberately no way to set it from a
    /// tool call.
    identity: Identity,
    /// How many times this run has looked at the fleet.
    ///
    /// **This lives on the server because, for a run Jod started, the server
    /// *is* the turn.** The harness spawns `jod mcp` as its own child and a
    /// run's process exits when its turn ends, so the server dies with it: one
    /// server, one run, one turn. That is exactly the span A4 wants to allow
    /// one look in. Keeping the count in the database instead would need a turn
    /// boundary nothing writes down, and keeping it per-run-for-ever would
    /// refuse the first `list_agents` of every turn after the first.
    ///
    /// The identity check in [`Server::refuse_a_second_look`] is what keeps
    /// that true. A session somebody opened by hand holds one server across
    /// many turns, and counting its looks would refuse the second one of the
    /// afternoon; it is also not the thing this rule exists for, which is a
    /// router burning an unattended turn on a poll loop.
    ///
    /// An atomic rather than a `Mutex<usize>` because [`Server::call`] takes
    /// `&self` and the only operation is a count.
    fleet_looks: AtomicUsize,
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
            identity: Identity::Unknown,
            fleet_looks: AtomicUsize::new(0),
        }
    }

    pub fn with_access(mut self, access: ToolAccess) -> Self {
        self.access = access;
        self
    }

    /// Take the identity [`identify`] worked out.
    ///
    /// Set by whatever launched the server — never by anything the model can
    /// reach. See [`Server::identity`].
    pub fn as_identity(mut self, identity: Identity) -> Self {
        self.identity = identity;
        self
    }

    /// Speak as one run. The shorthand tests use, and the honest name for what
    /// [`Server::as_identity`] does with an already-resolved run.
    pub fn for_run(self, run_id: impl Into<String>) -> Self {
        self.as_identity(Identity::Run(run_id.into()))
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn run(&self) -> Option<&str> {
        match &self.identity {
            Identity::Run(id) => Some(id),
            _ => None,
        }
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
            "interrupt_main" => self.interrupt_main(args).await,
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
            "record_decision" => self.record_decision(args),
            "ask_question" => self.ask_question(args).await,
            "request_secret" => self.request_secret(args),
            "list_roots" => self.list_roots(),
            "project_list" => self.project_list(args),
            "project_current" => self.project_current(),
            "project_switch" => self.project_switch(args),
            "project_add" => self.project_add(args),
            "project_untrack" => self.project_untrack(args),
            "ask_manager" => self.ask_manager(args).await,
            "claim_worktree" => self.claim_worktree(args),
            "release_worktree" => self.release_worktree(args),
            "open_work" => self.open_work(args).await,
            "plan_work" => self.plan_work(args),
            "work_board" => self.work_board(args),
            "complete_task" => self.complete_task(args),
            "stack_pull_requests" => self.stack_pull_requests(args),
            "roster" => self.roster(),
            "read_messages" => self.read_messages(),
            "send_message" => self.send_message(args),
            "reply" => self.reply(args),
            "ask" => self.ask(args).await,
            "handoff" => self.handoff(args),
            // Unreachable while the catalogue and this match agree, which is
            // what `every_advertised_tool_is_dispatchable` exists to hold.
            other => Err(ToolError::Unknown(other.to_string())),
        }
    }

    // ---- agents ---------------------------------------------------------

    /// One look at the fleet per turn, and the second call is refused.
    ///
    /// **A4, at the tool boundary rather than in a preamble.** The failure this
    /// exists for is written down in `tasks/01-routing.md` R4: a run that had
    /// handed work over sat calling `list_agents` again and again, waiting for
    /// a child it could not hurry, and spent forty-two seconds and thirty-nine
    /// cents mostly asleep before ending without the answer. Prose telling it
    /// not to is advice, and this is a rule the model talks itself past every
    /// time, because looking once more always feels like diligence.
    ///
    /// The first call is a legitimate decision — who is free, what is running.
    /// The second call inside one turn cannot be: nothing the caller is waiting
    /// for arrives by being looked at, and everything it is waiting for arrives
    /// on its own as a card or a message.
    ///
    /// Two bounds, and both are about where the reason stops.
    ///
    /// **Only the levels that can start work.** A read-only run cannot hand
    /// anything over, so it has nothing to be waiting for; polling the fleet
    /// from there is somebody's dashboard, not a router burning a turn.
    ///
    /// **Only a run Jod started.** That is what makes this server one turn —
    /// see [`Server::fleet_looks`]. A session opened by hand keeps one server
    /// across an afternoon, and refusing its second `list_agents` would be
    /// enforcing a per-turn budget against something that is not a turn.
    fn refuse_a_second_look(&self) -> Result<(), ToolError> {
        if !self.access.may_delegate() || self.run().is_none() {
            return Ok(());
        }
        if self.fleet_looks.fetch_add(1, Ordering::Relaxed) == 0 {
            return Ok(());
        }
        Err(ToolError::Refused(
            "you have already looked at the fleet this turn, and nothing has changed that a \
             second look would tell you. Whatever you are waiting for arrives on its own — a \
             card on the rail, or a message that starts a turn of yours — and not by being \
             watched for. Act on what the first call said, or return and say what you did."
                .into(),
        ))
    }

    async fn list_agents(&self, args: &Value) -> Result<String, ToolError> {
        self.refuse_a_second_look()?;
        let running_only = opt_bool(args, "running_only").unwrap_or(false);
        let limit = opt_usize(args, "limit")?.unwrap_or(20);
        // What a manager asks with, so it reads its own project rather than the
        // whole fleet. Matched against the project's name as `run_contexts`
        // resolved it, case-insensitively — a model that types `jod` for `Jod`
        // has not asked a different question.
        let only_project = opt_str(args, "project");

        // A fresh process knows nothing until it reads the database back, and
        // it has to read back at least as far as it has been asked to return —
        // the sum `jod ls` does at `cli/src/main.rs`. Reading a fixed few
        // hundred rows while the caller asked for a thousand is what made
        // "call again with a bigger limit" useless: an agent started before the
        // newest few hundred runs never entered memory, so no limit could
        // reach it.
        self.jod
            .rehydrate(REHYDRATE.max(limit))
            .await
            .map_err(|e| ToolError::Refused(format!("could not read the runs: {e}")))?;

        let mut agents = self.jod.agents().await;
        // Running first, then newest — the order the "can I reuse one" question
        // is asked in.
        agents.sort_by(|a, b| {
            let live = |s| s == AgentStatus::Running;
            live(b.status)
                .cmp(&live(a.status))
                .then(b.created_at_ms.cmp(&a.created_at_ms))
        });
        // Two reads for the whole answer, whatever the fleet's size. Both are
        // best-effort: a store that cannot answer them leaves the fields empty
        // rather than failing a call whose main job — listing the agents — it
        // has already done.
        let store = self.jod.store();
        let contexts = store
            .as_ref()
            .and_then(|s| s.run_contexts().ok())
            .unwrap_or_default();
        let stalled = store
            .as_ref()
            .and_then(|s| s.stalled_runs().ok())
            .unwrap_or_default();
        // The main chat's turns and every manager's. Both leave `completed`
        // rows with session ids and neither is an engineer, so without this the
        // roster's one recommendation is to hand the work back to whoever was
        // handing it out.
        let routers = store
            .as_ref()
            .and_then(|s| s.router_run_ids().ok())
            .unwrap_or_default();
        // Which runs are scratch, and which conversation each of them wrote
        // into. Both halves are needed: the flag keeps a scratch row out of the
        // engineer answer, and the conversation is what the reuse candidates
        // below are named by.
        let scratch_runs = store
            .as_ref()
            .and_then(|s| s.scratch_runs().ok())
            .unwrap_or_default();
        let now_ms = chrono::Utc::now().timestamp_millis();

        // The scratch sessions recent enough to be worth continuing, most
        // recently active first.
        //
        // A window of zero is reuse switched off, and it is a real setting
        // rather than a degenerate case — it is the way back to a fresh session
        // per instruction if reuse turns out badly. Asking the store for
        // candidates "since the beginning of time" would be the opposite
        // answer, so the window is checked before the query and not inside it.
        let reuse_window_minutes = store
            .as_ref()
            .and_then(|s| s.scratch_reuse_window_minutes().ok())
            .unwrap_or(0);
        let scratch_candidates: Vec<String> = match reuse_window_minutes > 0 {
            false => Vec::new(),
            true => store
                .as_ref()
                .and_then(|s| {
                    s.scratch_reuse_candidates(now_ms - reuse_window_minutes * 60_000)
                        .ok()
                })
                .unwrap_or_default(),
        };

        let keep = |a: &&crate::service::AgentSummary| {
            if running_only && a.status != AgentStatus::Running {
                return false;
            }
            match &only_project {
                None => true,
                Some(wanted) => contexts
                    .get(&a.id)
                    .and_then(|c| c.project.as_deref())
                    .is_some_and(|p| p.eq_ignore_ascii_case(wanted)),
            }
        };

        // Idle, in the only sense `continue_agent` will accept. A run's process
        // exits when its turn ends, so an engineer sitting there with nothing to
        // do is a `completed` row, not a `running` one — and it needs a session
        // id, because that is the thing being resumed.
        // A router is never free *to be given work*: continuing main's own
        // last turn, or a manager's, gives the instruction to the conversation
        // whose job is to give it to somebody else.
        let is_free = |a: &crate::service::AgentSummary| {
            a.status == AgentStatus::Completed
                && a.session_id.is_some()
                && !routers.contains(&a.id)
        };

        let matching = agents.iter().filter(keep).count();
        let views: Vec<AgentView> = agents
            .iter()
            .filter(keep)
            .take(limit)
            .map(|a| {
                let context = contexts.get(&a.id);
                let stalled_for_ms = stalled
                    .get(&a.id)
                    .filter(|_| a.status == AgentStatus::Running)
                    .map(|since| now_ms.saturating_sub(*since).max(0));
                AgentView {
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
                    project: context.and_then(|c| c.project.as_deref()),
                    work: context.and_then(|c| c.work.as_deref()),
                    stalled_for_ms,
                    busy: a.status == AgentStatus::Running && stalled_for_ms.is_none(),
                    free: is_free(a),
                    scratch: scratch_runs.contains_key(&a.id),
                }
            })
            .collect();

        // Computed over everything the filter matched rather than over the page,
        // because a free engineer that fell off the end of `limit` is still the
        // right answer and its run id is all `continue_agent` needs.
        //
        // **Scratch is excluded, and leaving it in is a real bug rather than a
        // tidiness point.** `is_free` matches a finished scratch session as
        // happily as an engineer — completed, with a session id, not a router —
        // so a lookup that finished five minutes ago would land here and be
        // advertised by the sentence below as an agent that "already holds this
        // checkout". It holds no checkout at all. Scratch has its own list and
        // its own sentence, a few lines down, saying something close to the
        // opposite.
        let idle: Vec<&str> = agents
            .iter()
            .filter(keep)
            .filter(|a| is_free(a) && !scratch_runs.contains_key(&a.id))
            .map(|a| a.id.as_str())
            .collect();

        // The scratch half, in the store's order — most recently active first,
        // which is the order the caller reads it in to find the session that
        // was talking about what it is about to ask.
        //
        // Named by conversation and answered by run, because that is how the
        // two sides are keyed: a scratch conversation may have been continued
        // more than once, so the run to continue is its newest, and the store
        // has already established that the newest one finished cleanly with a
        // session to resume.
        let scratch_idle: Vec<&str> = scratch_candidates
            .iter()
            .filter_map(|conversation| {
                agents
                    .iter()
                    .filter(keep)
                    .filter(|a| scratch_runs.get(&a.id) == Some(conversation))
                    .max_by_key(|a| a.created_at_ms)
                    .map(|a| a.id.as_str())
            })
            .collect();
        // Stalled counted separately from busy, because they lead to the same
        // tool call for opposite reasons and saying "busy" about a wedged run
        // would be telling the caller to wait for something that is not coming.
        let stalled_here = agents
            .iter()
            .filter(keep)
            .filter(|a| a.status == AgentStatus::Running && stalled.contains_key(&a.id))
            .count();
        let busy_here = agents
            .iter()
            .filter(keep)
            .filter(|a| a.status == AgentStatus::Running && !stalled.contains_key(&a.id))
            .count();
        let reuse = match (idle.first(), busy_here, stalled_here) {
            (Some(run_id), _, _) => format!(
                "`{run_id}` is free. Continue it with `continue_agent` — it already holds this \
                 checkout, so it starts where a new session would have to start over. Prefer it \
                 for any instruction here, including one on a different subject."
            ),
            (None, 0, 0) => "nothing to reuse — no agent here has a session to resume. Open a new \
                             one."
                .to_string(),
            (None, busy, 0) => format!(
                "nothing free — {busy} still working. Opening a second session beside them is the \
                 right call only if this genuinely has to run at the same time."
            ),
            (None, busy, wedged) => format!(
                "nothing free — {busy} working and {wedged} stalled. A stalled agent cannot be \
                 continued; leave it alone and open a new session beside it."
            ),
        };

        // **The scratch sentence says close to the opposite of the engineer
        // one, and that is the point of having two.**
        //
        // `reuse` above tells the caller to prefer a free agent "for any
        // instruction here, including one on a different subject", which is
        // right for an engineer: what it is worth reusing for is the warm
        // checkout, and every instruction in that repository benefits from it
        // whatever the subject. A scratch session has no checkout. The only
        // thing it carries that a fresh one would not is the subject it was
        // talking about, so reusing it across subjects buys nothing and
        // muddles what it knows. One sentence covering both cases would have
        // to be vague enough to be wrong about one of them.
        let scratch_reuse = scratch_idle.first().map(|run_id| {
            format!(
                "`{run_id}` is a scratch session that has finished recently — read its \
                 `last_message` to see what it was doing. Continue it with `continue_agent` \
                 **only if this instruction carries on that same subject**. It holds no \
                 checkout, so what it is worth reusing for is what it was already talking \
                 about, and nothing else. A different subject gets a new session with \
                 `delegate`, beside it. Never wait for a scratch session that is still \
                 running — one that is busy is not on this list, and the answer to it being \
                 busy is a new session, not a delay."
            )
        });

        // How many there were to choose from. The database is the authority on
        // that — this process only ever reads back the newest few hundred runs,
        // so counting what it holds would understate a busy box — but it can
        // only count rows, not the ones a filter kept. So a filtered call
        // reports what it matched, and an unfiltered one takes whichever of the
        // two numbers is larger. The same reasoning `jod ls` uses at
        // `cli/src/main.rs`, where it is `run_count()?.max(known)`.
        let total = match running_only || only_project.is_some() {
            true => matching,
            false => self.jod.run_count().unwrap_or(matching).max(matching),
        };
        let hidden = total.saturating_sub(views.len());
        as_json(&AgentPage {
            returned: views.len(),
            agents: views,
            total,
            hidden,
            idle,
            reuse,
            scratch_idle,
            scratch_reuse,
            // Spelled out as well as counted. The caller is a model deciding
            // whether it has seen every agent it might reuse, and a bare number
            // does not say what to do about it.
            note: (hidden > 0).then(|| {
                format!("{hidden} older hidden — call list_agents again with a bigger `limit`")
            }),
        })
    }

    async fn delegate(&self, args: &Value) -> Result<String, ToolError> {
        let req = self.delegate_request(args)?;

        if !self.jod.supervisor_available() {
            return Err(ToolError::Refused(
                "`jod-run` is not installed on this machine, and it supervises every agent".into(),
            ));
        }

        let tools = req.tools.unwrap_or(ToolAccess::ReadOnly);
        self.spawn_delegated(req, tools).await
    }

    /// Everything `delegate` decides before anything is spawned.
    ///
    /// Split out so the request can be inspected without a harness on the
    /// machine. What it carries is four rungs of precedence and three refusals,
    /// and the only way to check any of it through `delegate` itself would be
    /// to launch a real model and read back what it was launched with.
    fn delegate_request(&self, args: &Value) -> Result<SpawnRequest, ToolError> {
        // Main holds this one. It lost it for a release, along with
        // `ask_manager`, while an assistant made every routing decision; it has
        // the branch back, so it has the verb that serves it back.
        let prompt = required_str(args, "prompt")?;
        if prompt.trim().is_empty() {
            return Err(ToolError::BadParams("`prompt` is empty".into()));
        }
        let harness = match opt_str(args, "harness") {
            Some(h) => parse_harness(&h)
                .ok_or_else(|| ToolError::BadParams(format!("unknown harness `{h}`")))?,
            None => HarnessKind::ClaudeCode,
        };
        let permission = self.child_permission();
        let tools = self.child_access(args)?;
        let cwd = opt_str(args, "cwd").map(PathBuf::from).unwrap_or_else(default_cwd);

        // The route around `open_work`'s refusal, closed.
        //
        // The assistant keeps `delegate` for genuinely repo-less one-shots —
        // without it, "what's the weather in Manila" could not be handled
        // without opening a work. But `delegate` takes a `cwd`, so a model that
        // wants to help with something about a repository will point one at the
        // checkout rather than call `ask_manager`, and it will feel entirely
        // reasonable at the time. That is the rule routed around, silently.
        //
        // Main is refused `delegate` outright above, so the caller this catches
        // now is the assistant. Both are still tested, because the check that
        // matters is which conversations may not start work beside a manager,
        // and main is one of them whatever else refuses it first.
        //
        // Refused only when the directory is a *catalogued* project. That is
        // the same test `open_work`'s refusal rests on and it keeps the honest
        // case working: a scratch directory is not a repository Reljod is
        // managing, and a manager for it does not exist to be bypassed.
        if let Ok(store) = self.store() {
            if let Ok(Some(project)) = store.project_for_path(&cwd) {
                if let Ok(raiser) = self.raiser() {
                    if self.caller_is_main(&raiser) || self.caller_is_assistant(&raiser) {
                        return Err(ToolError::Refused(format!(
                            "`{}` is {}'s checkout, and a run started there is repository work \
                             however small the prompt looks. Call `ask_manager` with project \
                             `{}` instead. `delegate` is still yours for a one-shot that needs \
                             no repository at all.",
                            cwd.display(),
                            project.name,
                            project.name
                        )));
                    }
                }
            }
        }

        Ok(SpawnRequest {
            name: opt_str(args, "name").unwrap_or_else(|| default_name(&prompt)),
            harness,
            prompt,
            // A delegated agent gets its role from the prompt it was handed, so
            // there is almost nothing standing to tell it. The one exception is
            // who it answers to: a run that has an address for the orchestrator
            // and does not know it has one is a run that finishes silently, and
            // that was the whole of the missing return leg.
            //
            // Only when it can actually send. Telling a read-only run to report
            // back would be telling it to call a tool it has not been given.
            system: tools
                .may_delegate()
                .then(|| crate::orchestrator::delegated_preamble().to_string()),
            cwd,
            model: opt_str(args, "model"),
            permission,
            resume: Resume::Fresh,
            tools: Some(tools),
            // A one-shot errand is what the `scratch` role names, and this is
            // the spawn the roles panel most obviously targets: a lookup does
            // not need the model an engineer needs.
            //
            // Only when the call named no harness. An explicit argument
            // outranks the row, and `harness` above has already had the default
            // substituted into it, so this is the last point that still knows
            // the difference.
            role: opt_str(args, "harness").is_none().then_some(Role::Scratch),
            ..SpawnRequest::default()
        })
    }

    /// Start the run `delegate_request` described, and write down the three
    /// things that make it findable afterwards: who asked for it, that it is
    /// scratch, and the address it answers on.
    ///
    /// The half of `delegate` that cannot happen without a harness on the
    /// machine, kept apart from the half that can so the decisions above are
    /// testable on their own.
    async fn spawn_delegated(
        &self,
        req: SpawnRequest,
        tools: ToolAccess,
    ) -> Result<String, ToolError> {
        let agent = self
            .jod
            .spawn_agent(req)
            .await
            .map_err(|e| ToolError::Refused(format!("could not start the agent: {e}")))?;
        // Who asked for this, written down. `spawn_agent` binds
        // `RunConversation::New`, so without these two rows a delegated run is
        // a conversation nothing points at and a decision nothing records: the
        // orchestrator's own `jod main` listed the handoff *to* it and never
        // one of the agents it started.
        self.record_handoff("delegate", &agent.id, true);
        // What `delegate` starts is a scratch session, and this is where it is
        // marked as one — B1. Everything downstream keys on the column: the
        // sweep that archives a finished scratch row and deletes it later, and
        // the reuse candidates `list_agents` offers the assistant. Without this
        // there are no scratch rows at all, and both read as working while
        // finding nothing.
        //
        // Best-effort, like the handoff above and for the same reason: a
        // delegation that happened and was recorded badly is a smaller problem
        // than one refused over bookkeeping. The cost of it failing is a row
        // that never tidies itself away, which is visible and fixable.
        if let Ok(store) = self.store() {
            match store.conversation_for_run(&agent.id) {
                Ok(Some(conversation)) => {
                    if let Err(e) = store.mark_ephemeral(&conversation) {
                        eprintln!("[jod] could not mark {conversation} ephemeral: {e}");
                    }
                }
                // The run has not written into a conversation yet. Nothing to
                // mark; the sweep leaves an unmarked row alone, which is the
                // safe direction to be wrong in.
                Ok(None) => {}
                Err(e) => eprintln!(
                    "[jod] could not resolve the conversation for {}: {e}",
                    agent.id
                ),
            }
        }
        // And the way back. A delegated run belongs to no work and therefore to
        // no addressing scope, so until this existed every bus tool it called
        // answered `run ... is not a member of any team or work` — measured, in
        // a real run, on `roster`, `send_message` and `read_messages` alike.
        // Reljod's ask has a return leg in it: the run says what the answer is,
        // or that it has finished. This is the address it says it to.
        //
        // Best-effort, like the handoff above and for the same reason: a
        // delegation that happened and cannot report back is a smaller problem
        // than one refused over bookkeeping.
        let reports_back = match self.store() {
            Ok(store) => store
                .open_return_channel(&agent.id, &agent.name, agent.harness)
                .unwrap_or_else(|e| {
                    eprintln!("[jod] could not open a return channel for {}: {e}", agent.id);
                    None
                }),
            Err(_) => None,
        };
        as_json(&json!({
            "run_id": agent.id,
            "name": agent.name,
            "harness": agent.harness.id(),
            "watch": agent.watch_command,
            "reports_back_as": reports_back,
            "note": match (&reports_back, tools.may_delegate()) {
                (Some(name), true) => format!(
                    "running. It can reach you: `main` is on its roster and it is `{name}` on \
                     yours, so when it sends you the answer you will take a turn carrying it. \
                     Do not wait for it."
                ),
                (Some(_), false) => "running. It holds read-only tools, so it cannot send you \
                     anything — pass `tools: \"delegate\"` when you want the answer back. Do not \
                     wait for it."
                    .to_string(),
                (None, _) => "running. Do not wait for it.".to_string(),
            },
        }))
    }

    /// Write down that this session set something in motion.
    ///
    /// `link_child` hangs the new run's conversation under the caller's, which
    /// is right for `delegate` — it opened a fresh conversation — and wrong for
    /// `continue_agent`, whose target already sits wherever it sits.
    ///
    /// Best-effort in every part, and deliberately so: a delegation that
    /// happened and was recorded badly is a smaller problem than one refused
    /// over bookkeeping, and the caller may legitimately have no conversation
    /// at all — a `jod mcp` started by hand has no run behind it. Every failure
    /// here is a line on stderr and nothing more.
    fn record_handoff(&self, kind: &str, run_id: &str, link_child: bool) {
        let Ok(raiser) = self.raiser() else { return };
        let Ok(store) = self.store() else { return };

        if link_child {
            match store.conversation_for_run(run_id) {
                Ok(Some(child)) => {
                    if let Err(e) = store.set_conversation_parent(&child, &raiser.conversation_id) {
                        eprintln!(
                            "[jod] could not hang {child} under {}: {e}",
                            raiser.conversation_id
                        );
                    }
                }
                // The run has not written into a conversation yet. Nothing to
                // link, and the delegation row below still records the choice.
                Ok(None) => {}
                Err(e) => {
                    eprintln!("[jod] could not resolve the conversation for {run_id}: {e}")
                }
            }
        }

        if let Err(e) = store.record_delegation(&Delegation {
            id: 0,
            conversation_id: raiser.conversation_id.clone(),
            // No message id: this is a tool call inside a turn, not a turn of
            // its own. `hand_to_orchestrator` has one because the instruction
            // it records *is* the user's message.
            message_id: None,
            kind: kind.to_string(),
            run_id: Some(run_id.to_string()),
            schedule_name: None,
            goal_name: None,
            reason: String::new(),
            at_ms: chrono::Utc::now().timestamp_millis(),
        }) {
            eprintln!("[jod] could not record the {kind} of {run_id}: {e}");
        }
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
        // Asked before the session id, because how a run ended is a fact about
        // the run itself, while a missing session id is a fact about the
        // mechanism for resuming one. A killed run that also lost its session id
        // is better told it was killed.
        if let Some(refusal) = refusal_to_continue(&run_id, agent.status) {
            return Err(ToolError::Refused(refusal));
        }
        // A stalled run is refused here, not merely discouraged in a preamble.
        //
        // Reljod's decision was that a stalled session is *marked and surfaced,
        // never killed*, and that the router "treats it as not-continuable" —
        // say so, start a fresh session beside it, and leave the wedged one for
        // him to stop. Both preambles say it. Nothing enforced it, and the
        // precedent for what to do about that is in this same file: `open_work`
        // from the main chat is refused at the boundary because "prompt wording
        // is not enforcement".
        //
        // Observed by wedging a real engineer and giving its project another
        // instruction: the manager called `continue_agent` on the stalled run
        // and the tool allowed it. That does not resume the stuck process — it
        // starts a *second* one on the same session — so the wedged one is left
        // running and unnoticed, which is the state the mark exists to end.
        //
        // Best effort: a store that cannot answer leaves the call alone rather
        // than refusing work because a heartbeat could not be read.
        let stalled = self
            .store()
            .ok()
            .and_then(|s| s.stalled_runs().ok())
            .unwrap_or_default();
        if let Some(since) = stalled.get(&run_id) {
            let silent = chrono::Utc::now()
                .timestamp_millis()
                .saturating_sub(*since)
                .max(0);
            return Err(ToolError::Refused(format!(
                "run `{run_id}` is stalled — it has said nothing for {}, and it is still                  running, so continuing it would start a second agent on the same session                  and leave the wedged one going. Open a fresh session beside it with                  `open_work`, and leave this one for Reljod to stop.",
                crate::heartbeat::human_ms(silent)
            )));
        }
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
        let existing = self.store()?.conversation_for_run(&run_id).ok().flatten();
        let conversation = match &existing {
            Some(id) => RunConversation::Existing(id.clone()),
            None => RunConversation::New,
        };

        // **Which role this is depends on what is being continued.** A
        // follow-up to an engineer is an engineer; a follow-up to a scratch
        // session is scratch, and that is the whole of A6 — the assistant
        // reaches for this tool rather than `delegate` when a recent errand was
        // already on the subject. Tagging every continuation `engineer` would
        // put a lookup on the model an engineer needs, which is the opposite of
        // what the panel is for.
        //
        // No harness question here: this resumes a session, and `apply_role`
        // leaves a resumed request's harness alone, because a session id means
        // nothing to a program that did not mint it.
        let scratch = existing
            .as_ref()
            .and_then(|id| self.store().ok().and_then(|s| s.is_ephemeral(id).ok()))
            .unwrap_or(false);

        // A scratch conversation that had been archived is working again, so it
        // belongs back in the fleet. Archived means "finished and out of the
        // way", not "closed" — it archives itself once more when this run ends.
        // Unconditional and best-effort: clearing a null `archived_at_ms` is a
        // no-op, so there is nothing to be careful about and nothing worth
        // refusing a follow-up over.
        if let (Some(id), Ok(store)) = (&existing, self.store()) {
            if let Err(e) = store.unarchive_conversation(id) {
                eprintln!("[jod] could not bring {id} back to the fleet: {e}");
            }
        }

        let req = SpawnRequest {
            name: agent.name.clone(),
            harness: agent.harness,
            prompt,
            // Continuing a run, so its framing arrived with the first turn and
            // is already in the session being resumed.
            system: None,
            cwd: PathBuf::from(&agent.cwd),
            model: agent.model.clone(),
            permission: agent.permission,
            resume: Resume::Session(session),
            tools: Some(tools),
            role: Some(match scratch {
                true => Role::Scratch,
                false => Role::Engineer,
            }),
            ..SpawnRequest::default()
        };
        let next = self
            .jod
            .spawn_agent_in(req, conversation.clone())
            .await
            .map_err(|e| ToolError::Refused(format!("could not continue that agent: {e}")))?;
        // No link: the run being continued already sits wherever it sits, and
        // re-parenting it onto whoever happened to send this follow-up would
        // move a session in the tree for saying a second thing to it.
        self.record_handoff("continue", &next.id, false);

        // Bring back whatever stopping this conversation took down with it. A
        // manager resumed alone is a manager whose workers are gone, so the
        // resume has to reach as far as the stop did. Only runs the cascade
        // itself stopped come back; see `Jod::resume_cascade`.
        let brought_back = match &conversation {
            RunConversation::Existing(id) => self.jod.resume_cascade(id).await,
            _ => Vec::new(),
        };

        as_json(&json!({
            "run_id": next.id,
            "continued": run_id,
            "watch": next.watch_command,
            // Omitted when the resume took nothing else with it, so the
            // ordinary follow-up answer stays the shape it has always been.
            "resumed_with_it": (!brought_back.is_empty()).then(|| {
                brought_back
                    .iter()
                    .map(|r| json!({ "stopped": r.stopped, "now_running": r.resumed }))
                    .collect::<Vec<_>>()
            }),
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

    /// The mode a run started from here gets: the operator's, always.
    ///
    /// **Not an argument, and that is the whole of it.** `delegate` and
    /// `open_work` both used to take a `permission`, and the ceiling capped it,
    /// so the only thing a model could do with it was ask for *less*. That
    /// sounds harmless and is the last link in the chain that made the mode on
    /// the status bar a lie: a console in `auto` asked for the weather, the
    /// orchestrator volunteered `"permission": "accept_edits"` for the one-shot
    /// it started, and the one-shot stopped on a card asking to run `curl`.
    /// Nobody had chosen that. The model had no information the ceiling did not
    /// already carry — it was being careful with somebody else's decision.
    ///
    /// The ceiling arrives from the run that owns this server — see
    /// [`crate::mcp_config::server_args`] — so it *is* the operator's answer,
    /// already capped by whatever the parent holds. Inheriting it carries `auto`
    /// all the way down and still stops a server started deliberately low from
    /// handing out more than it has. `ask_manager` has worked this way from the
    /// start; these two now agree with it.
    fn child_permission(&self) -> PermissionPolicy {
        self.max_permission
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

    // ---- the rail ---------------------------------------------------------

    /// The run this server speaks as, or why it cannot say.
    ///
    /// Factored out of [`Server::caller`] because the rail asks the same
    /// question the bus does and must not answer it a second way. `doing` is
    /// the verb the refusal names — "sending", "raising a card" — so a model
    /// reading it is told what it was refused rather than which function
    /// refused it.
    fn identified_run(&self, doing: &str) -> Result<&str, ToolError> {
        match &self.identity {
            Identity::Run(id) => Ok(id.as_str()),
            Identity::Unknown => Err(ToolError::Refused(format!(
                "this session has no run behind it, so Jod cannot say who would be {doing}. \
                 That works from agents Jod started; a hand-started session can read but not \
                 write."
            ))),
            // Neither answer is preferred, on purpose. Two sources disagreeing
            // about who this is means something is wrong upstream, and picking
            // one would make a wrong sender permanent and silent.
            Identity::Disputed { group, claimed } => Err(ToolError::Refused(format!(
                "this server cannot say who it is: its process group belongs to {}, but its \
                 environment claims run `{claimed}`. Nothing will be written until they agree — \
                 a card or a message from the wrong sender is worse than none.",
                match group {
                    Some(id) => format!("run `{id}`"),
                    None => "no run at all".to_string(),
                }
            ))),
        }
    }

    /// Whose rail a card lands on, resolved from the run and from nothing else.
    ///
    /// **This is why no card tool takes a conversation.** A card is a sentence
    /// addressed to a person about *this* agent's work; an argument naming the
    /// conversation would let one run put words on another run's rail, and an
    /// answer would then be delivered to an agent that never asked anything.
    ///
    /// Deliberately laxer than [`Server::caller`] in exactly one respect, and
    /// no more: it does not require membership of a team or a work. The bus
    /// needs one because a message needs somebody to be addressed to; a card is
    /// addressed to Reljod, who is always there. A plain `jod run` that could
    /// not record a decision would leave the rail empty for the ordinary case.
    pub fn raiser(&self) -> Result<Raiser, ToolError> {
        let run_id = self.identified_run("raising a card")?;
        let store = self.store()?;
        let conversation_id = store
            .conversation_for_run(run_id)
            .map_err(|e| ToolError::Refused(format!("could not resolve who is calling: {e}")))?
            .ok_or_else(|| {
                ToolError::Refused(format!(
                    "run `{run_id}` has not written into a conversation, so there is no rail to \
                     raise this on. A run Jod started has one from its first turn; this one was \
                     started some other way."
                ))
            })?;
        Ok(Raiser {
            // A card carries its work so it keeps that work's colour after the
            // session that raised it is gone. Unresolvable is not a failure —
            // a conversation outside any work is the ordinary case.
            work_id: store.work_for_conversation(&conversation_id).ok().flatten(),
            conversation_id,
            run_id: run_id.to_string(),
        })
    }

    /// [`CardKind::Decision`] — the agent chose, and is saying so.
    ///
    /// Never blocking: the choice has already been made and the run has already
    /// carried on. What the card buys is the *undo*, and that is why `options`
    /// carries weight the prose does not — a decision offered with its
    /// alternatives is switched by pressing a number, and one without them is a
    /// note that provokes a conversation.
    fn record_decision(&self, args: &Value) -> Result<String, ToolError> {
        let raiser = self.raiser()?;
        let title = required_str(args, "title")?;
        let chosen = required_str(args, "chosen")?;
        let mut options = string_list(args, "options")?;
        // The chosen option belongs in the list even when the model left it
        // out, or the rail offers Reljod every alternative except the one that
        // is in force, and answering with a digit cannot restate it.
        if !options.iter().any(|o| o.trim() == chosen.trim()) {
            options.insert(0, chosen.clone());
        }
        let card = self
            .store()?
            .raise_card(NewCard {
                conversation_id: raiser.conversation_id,
                work_id: raiser.work_id,
                run_id: Some(raiser.run_id),
                kind: Some(CardKind::Decision),
                importance: importance(args)?,
                blocking: false,
                title: title.clone(),
                body: opt_str(args, "why").unwrap_or_default(),
                options,
                source: Some(Source::Mcp),
                // Keyed on the **choice and the subject**, not on the prose,
                // and the choice leads. Both halves are deliberate.
                //
                // The subject is in it because an agent that records the same
                // decision twice — a retried turn, a rewritten `why` — should
                // produce one row: `read_only` is a wide door and a full rail
                // is an unread rail.
                //
                // The choice is in it, and first, because a decision that was
                // *reconsidered* is not a repeat. Keyed on the subject alone,
                // "chat DB → postgres" would be swallowed by the earlier "chat
                // DB → sqlite" and the rail would show a choice that is no
                // longer in force, which is worse than either a duplicate or a
                // missing card. It leads so that the truncation `dedupe_key`
                // applies can only ever cost the subject's tail, never the part
                // that changes when an agent changes its mind.
                dedupe_key: Some(dedupe_key(
                    CardKind::Decision,
                    &format!("{chosen} for {title}"),
                )),
                chosen: Some(chosen),
                ..NewCard::default()
            })
            .map_err(|e| ToolError::Refused(format!("could not raise that: {e}")))?;
        as_json(&json!({
            "card_id": card.id,
            "note": "on the rail. Carry on — if Reljod switches it you will be told in a \
                     later turn.",
        }))
    }

    /// [`CardKind::Question`] — and the one tool here that may wait.
    ///
    /// Returns the card id at once unless the caller says it is blocked, per
    /// D2: emission never blocks the agent. A blocking question waits, bounded
    /// by [`CARD_ANSWER_DEADLINE_SECS`], and a wait that times out leaves the
    /// card open rather than withdrawing it.
    async fn ask_question(&self, args: &Value) -> Result<String, ToolError> {
        let raiser = self.raiser()?;
        let question = required_str(args, "question")?;
        let blocking = opt_bool(args, "blocking").unwrap_or(false);
        let seconds = opt_i64(args, "wait_seconds")?
            .unwrap_or(CARD_ANSWER_DEADLINE_SECS)
            .clamp(1, MAX_CARD_WAIT_SECS);
        let store = self.store()?;

        let card = store
            .raise_card(NewCard {
                conversation_id: raiser.conversation_id.clone(),
                work_id: raiser.work_id,
                run_id: Some(raiser.run_id.clone()),
                kind: Some(CardKind::Question),
                importance: importance(args)?,
                blocking,
                title: question.clone(),
                body: opt_str(args, "context").unwrap_or_default(),
                options: string_list(args, "options")?,
                source: Some(Source::Mcp),
                // The key both emission paths compute, so a harness that asks
                // Jod *and* prints its own question produces one card.
                dedupe_key: Some(dedupe_key(CardKind::Question, &question)),
                ..NewCard::default()
            })
            .map_err(|e| ToolError::Refused(format!("could not raise that: {e}")))?;

        if !blocking {
            return as_json(&json!({
                "card_id": card.id,
                "status": "open",
                "note": "asked. Jod is not waiting and neither should you — do whatever does \
                         not depend on the answer, and it will reach you in a later turn.",
            }));
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds as u64);
        loop {
            let now = store
                .card(card.id)
                .map_err(|e| ToolError::Refused(format!("could not watch for an answer: {e}")))?
                .ok_or_else(|| {
                    // Only reachable if the card's conversation was deleted
                    // under the run — worth saying plainly rather than looping
                    // until the deadline on a card that will never be answered.
                    ToolError::Refused(format!("card #{} is gone", card.id))
                })?;
            match now.status {
                Status::Open if std::time::Instant::now() >= deadline => {
                    return as_json(&json!({
                        "card_id": card.id,
                        "status": "open",
                        "waited_seconds": seconds,
                        "note": format!(
                            "nobody answered within {seconds}s. The question is still on the \
                             rail and the answer will reach you in a later turn — decide for \
                             yourself now, or stop and say you are blocked on it. Do not ask \
                             again; that is a second card about one question."
                        ),
                    }));
                }
                Status::Open => tokio::time::sleep(ASK_POLL).await,
                Status::Dismissed => {
                    return as_json(&json!({
                        "card_id": card.id,
                        "status": "dismissed",
                        "note": "read, and deliberately not answered. Decide for yourself.",
                    }));
                }
                Status::Answered => {
                    // Taken off the delivery queue here, for the same reason
                    // `ask` settles a reply it received: answering a card
                    // enqueues a synthetic turn, and a run that has just been
                    // handed the answer as a tool result would be told the same
                    // thing again later — which reads as a second instruction
                    // and gets the work done twice.
                    self.settle_card_delivery(&raiser.conversation_id, card.id, &raiser.run_id);
                    return as_json(&json!({
                        "card_id": card.id,
                        "status": "answered",
                        "chosen": now.chosen,
                        "answer": now.answer,
                    }));
                }
            }
        }
    }

    /// Mark a card's queued answer as already delivered, best effort.
    ///
    /// Best effort on purpose: the answer *is* in the caller's hands by the
    /// time this runs, and failing the tool call over the bookkeeping would
    /// turn a duplicate into a lost answer. The worst case is the milder bug —
    /// the agent hears it twice — and it is visible in the transcript.
    fn settle_card_delivery(&self, conversation_id: &str, card_id: i64, run_id: &str) {
        let Ok(store) = self.store() else { return };
        let Ok(queued) = store.pending_for(conversation_id) else {
            return;
        };
        let ids: Vec<i64> = queued
            .iter()
            .filter(|p| p.kind == delivery::Kind::CardAnswer && p.ref_id == card_id.to_string())
            .map(|p| p.id)
            .collect();
        if !ids.is_empty() {
            let _ = store.mark_deliveries_delivered(&ids, Some(run_id));
        }
    }

    /// [`CardKind::Secret`] — a name and a hint, and it cannot carry a value.
    ///
    /// Two properties hold here and both are load-bearing:
    ///
    /// 1. **There is no argument a value could arrive in**, and one that turns
    ///    up under an obvious name is refused rather than ignored. A credential
    ///    that reached this function would already be in the model's context
    ///    and in the transcript — D3 is about it never getting there, so the
    ///    only useful place to refuse is before it is stored, not after.
    /// 2. **It returns at once and never waits.** Waiting would be a lie:
    ///    injection happens at *spawn*, so the value cannot reach the run that
    ///    asked for it however long it sits there. Saying so is what turns a
    ///    missing credential into a blocked ending rather than an invented one.
    fn request_secret(&self, args: &Value) -> Result<String, ToolError> {
        for smuggled in ["value", "secret", "secret_value", "token"] {
            if args.get(smuggled).is_some() {
                return Err(ToolError::BadParams(format!(
                    "`{smuggled}` is not an argument of request_secret, and no argument of it \
                     carries a value. Ask for the credential by name; Reljod types the value \
                     into Jod, where you cannot read it."
                )));
            }
        }
        let raiser = self.raiser()?;
        let name = required_str(args, "name")?;
        if !secrets::is_valid_name(&name) {
            return Err(ToolError::BadParams(format!(
                "`{name}` is not a legal environment variable name: a letter or underscore, \
                 then letters, digits and underscores. A name a shell would drop makes a \
                 credential that is present behave exactly like one that is missing."
            )));
        }
        let hint = required_str(args, "hint")?;
        // The scope is not the agent's to choose. How widely a credential is
        // shared is the blast radius if it leaks, and it is decided by the
        // person typing the value; what the card records is where it *would*
        // go — the work when there is one, so a key given for one project is
        // not handed to every session on the box.
        let scope = match &raiser.work_id {
            Some(_) => secrets::Scope::Work,
            None => secrets::Scope::Conversation,
        };
        let card = self
            .store()?
            .raise_card(NewCard {
                conversation_id: raiser.conversation_id,
                work_id: raiser.work_id.clone(),
                run_id: Some(raiser.run_id),
                kind: Some(CardKind::Secret),
                importance: Some(Importance::High),
                blocking: opt_bool(args, "blocking").unwrap_or(true),
                title: format!("{name} needed"),
                body: hint,
                secret_name: Some(name.clone()),
                secret_scope: Some(scope.as_str().to_string()),
                source: Some(Source::Mcp),
                dedupe_key: Some(dedupe_key(CardKind::Secret, &name)),
                ..NewCard::default()
            })
            .map_err(|e| ToolError::Refused(format!("could not raise that: {e}")))?;
        as_json(&json!({
            "card_id": card.id,
            "secret": name,
            "scope": scope.as_str(),
            "note": format!(
                "asked for. `{name}` is injected into the environment of the next run, not \
                 this one, and you will never be shown its value. If you need it to finish, \
                 you are blocked: say so and stop. Do not invent a value, and do not work \
                 around it."
            ),
        }))
    }

    // ---- works and roots --------------------------------------------------

    /// Where this session may work, and which of it is writable.
    fn list_roots(&self) -> Result<String, ToolError> {
        let raiser = self.raiser()?;
        let roots = self
            .store()?
            .roots(&raiser.conversation_id)
            .map_err(|e| ToolError::Refused(format!("could not read your roots: {e}")))?;
        as_json(
            &roots
                .iter()
                .map(|r| {
                    json!({
                        "path": r.path.to_string_lossy(),
                        "writable": r.writable,
                        "origin": r.origin.as_str(),
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    /// The catalog, in the order that makes the first entry the best guess.
    ///
    /// Each entry says whether its directory is still there. The catalog is
    /// what an instruction naming no project gets resolved against, so an
    /// entry whose checkout has been deleted or renamed is a resolution target
    /// that cannot be worked in, and a model reading this list has no other
    /// way to find that out. It learns instead by opening work there, being
    /// told the work is running, and then reading a supervisor error about the
    /// harness binary — see [`crate::projects::Project::path_trouble`].
    fn project_list(&self, args: &Value) -> Result<String, ToolError> {
        let include_archived = opt_bool(args, "include_archived").unwrap_or(false);
        let projects = self
            .store()?
            .projects(include_archived)
            .map_err(|e| ToolError::Refused(format!("could not read the catalog: {e}")))?;
        as_json(
            &projects
                .iter()
                .map(|p| {
                    // Both halves are carried: the flag is what a model can
                    // branch on without reading prose, and the sentence is what
                    // it can repeat to Reljod instead of inventing its own
                    // explanation for why the project will not open.
                    let trouble = p.path_trouble();
                    json!({
                        "name": p.name,
                        "path": p.path.to_string_lossy(),
                        "also_called": p.spoken_forms(),
                        "state": p.state.as_str(),
                        "notes": p.notes,
                        "last_touched_ms": p.last_touched_ms,
                        "path_usable": trouble.is_none(),
                        "path_trouble": trouble,
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    /// What this conversation is about, and how sure that is.
    ///
    /// `how` is carried out to the model rather than kept internal, because a
    /// sticky answer and an inferred one deserve different confidence and the
    /// model cannot tell them apart from the project alone.
    fn project_current(&self) -> Result<String, ToolError> {
        let raiser = self.raiser()?;
        let store = self.store()?;
        let current = store
            .current_project(&raiser.conversation_id)
            .map_err(|e| ToolError::Refused(format!("could not read the current project: {e}")))?;
        let Some(project) = current else {
            return as_json(&json!({
                "project": Value::Null,
                "note": "this conversation is not about any project yet — ask, or call \
                         project_switch once you know which one",
            }));
        };
        let last = store
            .project_resolutions(&raiser.conversation_id, 1)
            .map_err(|e| ToolError::Refused(format!("could not read the resolution log: {e}")))?;
        as_json(&json!({
            "project": project.name,
            "path": project.path.to_string_lossy(),
            "notes": project.notes,
            "how": last.first().map(|r| r.how.as_str()).unwrap_or("human"),
            "reason": last.first().map(|r| r.reason.clone()).unwrap_or_default(),
        }))
    }

    /// The model's override of a sticky project.
    ///
    /// Named rather than given by id: the model is resolving something Reljod
    /// *said*, and making it carry an opaque id would mean it had to list the
    /// catalog first on every switch just to translate a word it already has.
    /// Whether the run calling this is the main chat.
    ///
    /// The MCP server already knows which run is calling — it resolves its own
    /// process group against `runs.pgid` — so the caller cannot argue about its
    /// identity, which is what makes a refusal here enforcement rather than a
    /// request. Prompt wording is not enforcement: it is advice a model may
    /// reasonably talk itself out of, and this rule is one it will be tempted
    /// to, because routing around it always feels helpful in the moment.
    ///
    /// Unresolvable identity means "not main". A run Jod did not start has no
    /// pinned conversation to be, and refusing everything Jod cannot identify
    /// would break `jod run` against its own MCP server.
    fn caller_is_main(&self, raiser: &Raiser) -> bool {
        let Ok(store) = self.store() else {
            return false;
        };
        match store.pinned_conversation() {
            Ok(Some(main)) => main == raiser.conversation_id,
            _ => false,
        }
    }

    /// Which conversation holds the project pointer this caller is really
    /// setting.
    ///
    /// Its own, for everybody except an assistant. An assistant's conversation
    /// is opened for one instruction and never read again, so a sticky pointer
    /// written there is discarded at the end of the turn that wrote it — and
    /// the pointer's entire purpose is that the *next* instruction inherits it.
    /// Main is where it has to land, and main is the assistant's parent.
    ///
    /// Falls back to the caller's own conversation when there is no parent,
    /// which is a database that has no main chat yet. Writing it somewhere is
    /// better than dropping it, and the row is harmless where it lands.
    fn sticky_conversation(&self, raiser: &Raiser) -> String {
        if !self.caller_is_assistant(raiser) {
            return raiser.conversation_id.clone();
        }
        self.store()
            .ok()
            .and_then(|s| s.parent_conversation(&raiser.conversation_id).ok().flatten())
            .unwrap_or_else(|| raiser.conversation_id.clone())
    }

    /// Whether the run calling this is an assistant.
    ///
    /// Read off `conversations.origin`, which [`Store::open_assistant_conversation`]
    /// wrote when the conversation was created — so, like
    /// [`Server::caller_is_main`], it is sender identity the caller cannot argue
    /// with. An unreadable origin means "not an assistant", for the same reason
    /// an unresolvable identity means "not main": failing closed here would
    /// refuse ordinary agents over a database read.
    fn caller_is_assistant(&self, raiser: &Raiser) -> bool {
        let Ok(store) = self.store() else {
            return false;
        };
        matches!(
            store.conversation_origin(&raiser.conversation_id),
            Ok(Some(origin)) if origin == crate::orchestrator::ASSISTANT_ORIGIN
        )
    }

    /// Stop the turn a conversation has in flight, on a doorman's say-so.
    ///
    /// **The assistant's one verb, and it is gated on who is calling rather
    /// than on how much of Jod they hold.** A doorman is spawned at
    /// [`ToolAccess::ReadOnly`] — the smallest toolbox anything gets — because
    /// its whole job is to read one message and answer one question. Putting
    /// this behind `Delegate` would have meant handing it the power to start
    /// agents in order to reach the power to stop one.
    ///
    /// So the check is [`Server::caller_is_assistant`], read off
    /// `conversations.origin`, which is sender identity the caller cannot argue
    /// with. Anything else is refused, including main itself: a chat that can
    /// stop its own turn is a chat that can stop itself mid-sentence, and the
    /// key for stopping a turn you are watching is Escape.
    ///
    /// **This delivers nothing.** Killing the run leaves the conversation and
    /// the harness session exactly where they were, so the queued message goes
    /// in as the next turn through the path that already exists —
    /// [`Store::plan_injection`] now says `Speak` about a conversation that is
    /// no longer busy. The doorman writing the message on itself would be a
    /// second delivery path, and the two would disagree the first time one of
    /// them changed.
    async fn interrupt_main(&self, args: &Value) -> Result<String, ToolError> {
        let run_id = required_str(args, "run_id")?;
        let reason = required_str(args, "reason")?;
        if reason.trim().is_empty() {
            return Err(ToolError::BadParams(
                "`reason` is what Reljod reads to find out why his turn stopped, and an \
                 empty one would stop it with nothing said"
                    .into(),
            ));
        }
        let raiser = self.raiser().map_err(|_| {
            ToolError::Refused(
                "Jod cannot tell which run is calling, so it cannot tell whether you are the \
                 assistant standing at a door. Only a doorman may stop a turn."
                    .into(),
            )
        })?;
        if !self.caller_is_assistant(&raiser) {
            return Err(ToolError::Refused(
                "`interrupt_main` belongs to the assistant reading a queued message, and \
                 nobody else. If you are watching a run that should stop, `stop_agent` is \
                 the verb; if you are a chat that should stop, that is Reljod's Escape key \
                 and not yours."
                    .into(),
            ));
        }

        let store = self.store()?;
        let target = store
            .conversation_for_run(&run_id)
            .map_err(|e| ToolError::Refused(format!("could not look up `{run_id}`: {e}")))?
            .ok_or_else(|| {
                ToolError::Refused(format!(
                    "no run `{run_id}`, so there is nothing to stop. Use the run id exactly \
                     as it was written in what you were handed."
                ))
            })?;

        self.jod
            .kill_agent(&run_id)
            .await
            .map_err(|e| ToolError::Refused(format!("could not stop `{run_id}`: {e}")))?;

        // Into the transcript Reljod is actually reading, not this one. A turn
        // that stops with no explanation reads as a crash, and the next thing
        // he does is ask what happened — which is a question this sentence has
        // already answered.
        if let Err(e) = store.append_message(
            &target,
            crate::conversation::NewMessage::new(
                crate::conversation::Role::System,
                format!("[stopped by Jod's assistant] {}", reason.trim()),
            ),
        ) {
            eprintln!("[jod] stopped `{run_id}` but could not say why in the chat: {e}");
        }

        as_json(&json!({
            "stopped": run_id,
            "note": "the turn is over and the conversation is intact. What Reljod typed is \
                     still queued and goes in as the next turn on its own — do not deliver \
                     it, do not answer it, and do not start anything. Say your one sentence \
                     and stop.",
        }))
    }

    /// Refuse a call that belongs to a project's manager rather than to main.
    ///
    /// **Main routes and answers; it does not open work.** Deciding how a
    /// repository's work is broken up, and how many engineers it is worth, is
    /// the manager's job and needs the repository in front of it — which main
    /// deliberately never has.
    ///
    /// This used to cover `ask_manager` and `delegate` as well, from the release
    /// where main handed every instruction to an assistant and answered nothing.
    /// That layer is gone: main decides again, so the two verbs it decides
    /// *with* cannot be refused to it. `open_work` is the one that stayed, and
    /// it is the one that was refused here before the assistant existed at all.
    ///
    /// This is enforcement rather than advice, and it has to be, because the
    /// rule is one a helpful model talks itself past.
    ///
    /// Keyed on identity and never on access level, which is the only thing
    /// that works: [`ToolAccess`] is a ladder, so lowering main below
    /// `Orchestrate` to take a verb away would take `schedule_create` and
    /// `goal_create` with it, and those stay with main on purpose.
    ///
    /// A caller Jod cannot identify is not main. A run Jod did not start has no
    /// pinned conversation to be, and refusing everything unidentifiable would
    /// break `jod run` against its own MCP server.
    fn refuse_routing_from_main(&self, tool: &str) -> Result<(), ToolError> {
        let Ok(raiser) = self.raiser() else {
            return Ok(());
        };
        if !self.caller_is_main(&raiser) {
            return Ok(());
        }
        Err(ToolError::Refused(format!(
            "`{tool}` is not the main chat's to call. Hand the instruction to the project's \
             manager with `ask_manager` instead: it owns the repository, it decides whether \
             this is new work or something an agent of its own is already doing, and it \
             raises a card that reaches your rail."
        )))
    }

    /// Route an instruction to the project's manager.
    ///
    /// The resolution is a plain string match over names, aliases and path
    /// basenames — the same one the router already runs before a main-chat turn
    /// starts. Naming a project is not a judgement call, so no model is asked
    /// to make one here; this tool is wiring.
    ///
    /// Refuses on a name that matches nothing or matches several, listing what
    /// it does know either way. The alternative — picking — points an
    /// instruction at a repository nobody chose, and it reads as perfectly
    /// ordinary in the manager that receives it.
    async fn ask_manager(&self, args: &Value) -> Result<String, ToolError> {
        // Main is the caller this tool was written for. It was refused here for
        // one release, while every instruction went through an assistant, and
        // lifting that refusal is most of what put the decision back.
        let wanted = required_str(args, "project")?;
        let instruction = required_str(args, "instruction")?;
        if instruction.trim().is_empty() {
            return Err(ToolError::BadParams(
                "`instruction` is what the manager is being asked to do, and an empty one \
                 would start a run with nothing to act on"
                    .into(),
            ));
        }
        // `None` rather than a default, because `hand_to_manager` has to know
        // whether anybody actually named a harness: an argument in the call
        // outranks the `manager` role's row, and a default that has already
        // been substituted is indistinguishable from a choice.
        let harness = match opt_str(args, "harness") {
            Some(h) => Some(
                parse_harness(&h)
                    .ok_or_else(|| ToolError::BadParams(format!("unknown harness `{h}`")))?,
            ),
            None => None,
        };
        let store = self.store()?;

        let project = named_project(
            store,
            &wanted,
            ", so there is no manager to ask",
            "Each has its own manager, so ask Reljod which one he means rather \
             than picking.",
        )?;

        if !self.jod.supervisor_available() {
            return Err(ToolError::Refused(
                "`jod-run` is not installed on this machine, and it supervises every agent".into(),
            ));
        }

        let managed = crate::orchestrator::hand_to_manager(
            &self.jod,
            &project.id,
            &instruction,
            harness,
            self.max_permission,
        )
        .await
        .map_err(|e| ToolError::Refused(format!("could not reach {}'s manager: {e}", project.name)))?;

        as_json(&json!({
            "run_id": managed.run_id,
            "conversation_id": managed.conversation_id,
            "project": managed.project,
            "started_fresh": managed.started_fresh,
            "note": if managed.started_fresh {
                format!(
                    "{} had no manager, so one was started. It will raise a card on your rail \
                     when it has decided what to do.",
                    managed.project
                )
            } else {
                format!(
                    "{}'s manager was resumed and has the instruction. It will raise a card on \
                     your rail when it has decided what to do.",
                    managed.project
                )
            },
        }))
    }

    fn project_switch(&self, args: &Value) -> Result<String, ToolError> {
        let raiser = self.raiser()?;
        let wanted = required_str(args, "project")?;
        let reason = opt_str(args, "reason").unwrap_or_default();
        let store = self.store()?;

        let project = named_project(
            store,
            &wanted,
            "",
            "Ask Reljod which one he means, or call project_switch again with \
             the exact name of one of them.",
        )?;

        // An assistant switches the *main chat's* project, not its own.
        //
        // The whole value of this tool is that the next instruction inherits
        // what this one resolved. An assistant's conversation is thrown away
        // when its turn ends, so a switch written there is a switch nobody
        // keeps: the next instruction opens a fresh assistant, which inherits
        // from main, which was never told. Main used to hold the routing
        // decision and therefore held this too; the decision moved down a layer
        // and the pointer it sets did not, because there is nowhere down here
        // for a pointer to live.
        let target = self.sticky_conversation(&raiser);

        // A switch away from an inferred project is Reljod's correction
        // arriving late, so the guess it replaces is marked as taken back.
        let previous = store
            .current_project(&target)
            .map_err(|e| ToolError::Refused(format!("could not read the current project: {e}")))?;
        if previous.as_ref().is_some_and(|p| p.id != project.id) {
            let _ = store.mark_resolution_corrected(&target);
        }

        store
            .set_current_project(
                &target,
                Some(&project.id),
                &reason,
                crate::projects::How::Human,
                &reason,
            )
            .map_err(|e| ToolError::Refused(format!("could not switch project: {e}")))?;
        // And on the caller's own row when that is a different one, so
        // `project_current` inside this same turn agrees with what was just
        // set. Best-effort: the pointer that outlives the turn is the one
        // above, and it has already been written.
        if target != raiser.conversation_id {
            let _ = store.set_current_project(
                &raiser.conversation_id,
                Some(&project.id),
                &reason,
                crate::projects::How::Human,
                &reason,
            );
        }

        as_json(&json!({
            "project": project.name,
            "path": project.path.to_string_lossy(),
            "switched_from": previous.map(|p| p.name),
        }))
    }

    fn project_add(&self, args: &Value) -> Result<String, ToolError> {
        let path = required_str(args, "path")?;
        let aliases: Vec<String> = args
            .get("aliases")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let mut new = crate::projects::NewProject::at(&path).with_aliases(aliases);
        if let Some(name) = opt_str(args, "name") {
            new = new.named(name);
        }
        if let Some(notes) = opt_str(args, "notes") {
            new = new.with_notes(notes);
        }

        let project = self
            .store()?
            .add_project(new)
            .map_err(|e| ToolError::Refused(format!("could not add the project: {e}")))?;
        as_json(&json!({
            "name": project.name,
            "path": project.path.to_string_lossy(),
            "also_called": project.spoken_forms(),
        }))
    }

    /// Take a repository out of the working set.
    ///
    /// The state it moves to is `archived`, which already meant exactly this
    /// everywhere except the fleet — see [`crate::projects::State`] and the
    /// filter in [`crate::tree::Store::forest_of`], which is the half that had
    /// to be written for the verb to be true.
    ///
    /// Untracking something already untracked answers rather than refusing.
    /// The state after the call is the state that was asked for, so a refusal
    /// would report a failure where nothing failed and invite a retry that
    /// cannot go better. It says which of the two happened, because a model
    /// relaying "done" for a no-op is how Reljod ends up believing he untracked
    /// the other checkout of the same name.
    fn project_untrack(&self, args: &Value) -> Result<String, ToolError> {
        let wanted = required_str(args, "project")?;
        let store = self.store()?;
        let project = named_project(
            store,
            &wanted,
            ", so there is nothing to untrack",
            "Untracking either would take a repository off the fleet, so ask \
             Reljod which one he means rather than picking.",
        )?;

        let already = project.state == crate::projects::State::Archived;
        if !already {
            store
                .set_project_state(&project.id, crate::projects::State::Archived)
                .map_err(|e| ToolError::Refused(format!("could not untrack the project: {e}")))?;
        }

        as_json(&json!({
            "name": project.name,
            "path": project.path.to_string_lossy(),
            "already_untracked": already,
            "said": if already {
                format!("{} was already untracked — nothing changed", project.name)
            } else {
                format!(
                    "{} is no longer tracked. It is off the fleet with its manager and its \
                     works, out of the catalog, and will not be inferred. Nothing was \
                     deleted — `jod project restore {}` puts it back.",
                    project.name, project.name
                )
            },
        }))
    }

    /// Claim somewhere to write — D5's explicit step.
    ///
    /// Everything hard about this is already in [`Store::claim_lease`]: the
    /// reuse-before-cutting rule, the race the partial index arbitrates, the
    /// root rebinding that leaves the checkout readable beside the worktree,
    /// and the card a non-git root raises instead of a crash. This is the seam
    /// that lets an agent reach it, and it is the *only* reason any of that
    /// runs outside a test.
    fn claim_worktree(&self, args: &Value) -> Result<String, ToolError> {
        let raiser = self.raiser()?;
        // A lease is per work *and* repository — that is what makes it
        // reusable by a sibling, and there is no sibling without a work.
        let Some(work_id) = raiser.work_id.clone() else {
            return Err(ToolError::Refused(
                "this session does not belong to a work, so there is nothing to key a lease to \
                 and no sibling to share one with. Work is opened with `open_work`; a session \
                 outside one writes wherever it was pointed and owns that decision."
                    .into(),
            ));
        };
        let store = self.store()?;
        let repo = match opt_str(args, "repo") {
            Some(path) => PathBuf::from(path),
            None => store
                .roots(&raiser.conversation_id)
                .map_err(|e| ToolError::Refused(format!("could not read your roots: {e}")))?
                .into_iter()
                // The first *read-only* root, not simply the first: after an
                // earlier claim the worktree may sort ahead of the checkout,
                // and cutting a branch of a worktree is not what anybody meant.
                .find(|r| !r.writable)
                .map(|r| r.path)
                .ok_or_else(|| {
                    ToolError::Refused(
                        "say which repository to claim — `repo` — because this session has no \
                         read-only root to infer one from"
                            .into(),
                    )
                })?,
        };

        match store
            .claim_lease(&work_id, &raiser.conversation_id, &repo)
            .map_err(|e| ToolError::Refused(format!("could not claim a worktree: {e}")))?
        {
            // Cut and reused are reported apart, not flattened into "here is a
            // path". A session that believes it cut a fresh branch when it was
            // handed a sibling's will commit over that sibling's work and
            // describe it as its own.
            claim @ (crate::leases::Claim::Cut(_) | crate::leases::Claim::Reused(_)) => {
                let reused = matches!(claim, crate::leases::Claim::Reused(_));
                let lease = claim.lease().expect("cut and reused both carry a lease");
                // Handing back a path and calling it writable is the claim that
                // cost a whole run. Checked before it is made.
                let can_write = can_write(&raiser.run_id, &lease.worktree_path);
                let warning = can_write.warning(&lease.worktree_path);
                let standing = if reused {
                    // Kept whatever the writability answer is. Sharing is a
                    // fact about a sibling, not about this session's sandbox,
                    // and a session that overwrites a colleague's branch does
                    // equal damage either way.
                    "this worktree was already claimed for this repository in this work, so \
                     you are sharing it. Somebody else is working here: read what is there \
                     before you change it, and say on the bus what you are taking."
                } else if warning.is_none() {
                    "cut for you. This is now your only writable root; the checkout is still \
                     beside it, read-only, so you can diff against what Reljod is editing."
                } else {
                    // The same sentence with the one clause this tool has just
                    // failed to establish taken out. That the checkout sits
                    // beside it, read-only, is still true and still useful;
                    // "this is now your only writable root" is exactly the
                    // claim that let a dead run look finished.
                    "cut for you, on a branch of your own. The checkout is still beside it, \
                     read-only, so you can diff against what Reljod is editing."
                };
                as_json(&json!({
                    "lease_id": lease.id,
                    "worktree": lease.worktree_path.to_string_lossy(),
                    "branch": lease.branch,
                    "base": lease.base_ref,
                    "reused": reused,
                    "writable": can_write.verdict(),
                    // The warning goes first when there is one. A session that
                    // reads only the opening clause of this field should read
                    // the thing that stops it wasting a run, not the thing that
                    // reassures it.
                    "note": match &warning {
                        Some(warning) => format!("{warning} {standing}"),
                        None => standing.to_string(),
                    },
                }))
            }
            crate::leases::Claim::NotGit { card_id, detail, .. } => {
                // An answer, not an error: the session is still running and
                // still useful, and a person now has to decide whether that
                // root was wrong or wants `git init`.
                as_json(&json!({
                    "claimed": false,
                    "card_id": card_id,
                    "why": detail,
                    "note": "raised on Reljod's rail. You have nowhere to write in that \
                             directory — do what you can read-only, and stop rather than \
                             writing into a root you were told not to change.",
                }))
            }
        }
    }

    /// Give a worktree back, keeping anything that would be lost by removing it.
    fn release_worktree(&self, args: &Value) -> Result<String, ToolError> {
        let raiser = self.raiser()?;
        let store = self.store()?;
        let lease_id = match opt_i64(args, "lease_id")? {
            Some(id) => id,
            None => {
                let Some(work_id) = raiser.work_id.clone() else {
                    return Err(ToolError::Refused(
                        "this session belongs to no work, so it holds no lease".into(),
                    ));
                };
                let mine: Vec<crate::leases::Lease> = store
                    .work_leases(&work_id)
                    .map_err(|e| ToolError::Refused(format!("could not read the leases: {e}")))?
                    .into_iter()
                    .filter(|l| {
                        l.state == crate::leases::State::Held
                            && l.conversation_id.as_deref() == Some(raiser.conversation_id.as_str())
                    })
                    .collect();
                match mine.as_slice() {
                    [only] => only.id,
                    [] => {
                        return Err(ToolError::Refused(
                            "you hold no worktree, so there is nothing to give back".into(),
                        ))
                    }
                    // Named rather than guessed: releasing the wrong one takes
                    // away the root the agent is actually writing in.
                    many => {
                        return Err(ToolError::Refused(format!(
                            "you hold {} worktrees — say which, by `lease_id`: {}",
                            many.len(),
                            many.iter()
                                .map(|l| format!("#{} on `{}`", l.id, l.branch))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )))
                    }
                }
            }
        };

        match store
            .release_lease(lease_id)
            .map_err(|e| ToolError::Refused(format!("could not release that: {e}")))?
        {
            crate::leases::Release::Removed { lease } => as_json(&json!({
                "removed": true,
                "branch": lease.branch,
                "worktree": lease.worktree_path.to_string_lossy(),
                "note": "clean and merged, so it is gone from disk. The branch remains.",
            })),
            crate::leases::Release::Kept {
                lease,
                condition,
                reason,
            } => as_json(&json!({
                "removed": false,
                "branch": lease.branch,
                "worktree": lease.worktree_path.to_string_lossy(),
                "dirty": condition.dirty,
                "merged": condition.merged,
                "why": reason,
                // Not a failure, and it must not read as one — an agent told
                // this is an error will try to force it.
                "note": "kept on disk on purpose: removing it would destroy work that is not \
                         recorded anywhere else. Commit or merge it and it can go later; \
                         `jod work leases` finds it in the meantime.",
            })),
        }
    }

    /// Open a work and put its first session on a checkout.
    ///
    /// **Returns as soon as the session is spawned.** That is the property the
    /// whole orchestrator design exists to protect: the main chat is what you
    /// reach for while something is already running, so a routing tool that
    /// waited for the work would make it useless at the one moment it matters.
    /// Nothing here reads the session's output, and the titler runs detached.
    ///
    /// The new session hangs under the *caller's* conversation, which is what
    /// makes the tree deeper than two levels and what makes the caller's rail
    /// show everything raised below it. Taken from the run, never from an
    /// argument — a caller that could name its own parent could graft a session
    /// onto a tree it has nothing to do with.
    async fn open_work(&self, args: &Value) -> Result<String, ToolError> {
        let raiser = self.raiser()?;
        self.refuse_routing_from_main("open_work")?;
        let Planned { opening, paths } = self.opening_for(&raiser, args)?;
        let placement = opening.placement.clone();

        if !self.jod.supervisor_available() {
            return Err(ToolError::Refused(
                "`jod-run` is not installed on this machine, and it supervises every agent".into(),
            ));
        }

        let opened = crate::orchestrator::open_work(&self.jod, opening)
            .await
            .map_err(|e| ToolError::Refused(format!("could not open that work: {e}")))?;
        // The parent link is already written — `Opening::under` carried it into
        // `attach_conversation`, which is the richer form of it — so this
        // records the decision only. Without it, the routing outcome the
        // orchestrator is now told to prefer would be the one outcome missing
        // from what `jod main` says the chat set in motion.
        self.record_handoff("open_work", &opened.agent.id, false);
        // The write side of `conversations.task_id`, and the reason it is here
        // rather than in the orchestrator is written on `Opening::assignment`:
        // this is the tool boundary's job. Two finished features read this
        // column and both of them were inert without it — see
        // [`spawn_onto_first_task`].
        let task_id =
            spawn_onto_first_task(self.store()?, &opened.work.id, &opened.conversation_id, &paths);
        as_json(&json!({
            "work_id": opened.work.id,
            "title": opened.work.title,
            "colour": opened.work.colour,
            "conversation_id": opened.conversation_id,
            "session": opened.name,
            "run_id": opened.agent.id,
            // Reported rather than kept quiet, because two things downstream
            // silently degrade when it is missing rather than failing: the
            // stack comes out in finish order and the collision guard waves
            // every share through. A `null` here is the only warning either of
            // those gives.
            "task_id": task_id,
            // Echoed back for the same reason. A placement that was accepted
            // and ignored answers exactly like one that was acted on, which is
            // how a manager comes to believe it placed an engineer read-only.
            "placement": placement.as_ref().map(|p| p.as_str()),
            "paths": paths,
            "worktree": opened.claim.as_ref()
                .and_then(|claim| claim.lease())
                .map(|lease| lease.worktree_path.to_string_lossy().to_string()),
            "note": match &placement {
                None => "opened and running. The checkout is a read-only root; the session \
                         claims a worktree itself if it needs to write. Its cards will arrive \
                         on your rail.",
                Some(crate::leases::Placement::Explore) =>
                    "opened and running, read-only. It holds no writable root and its brief \
                     says so — if it turns out to need one, it reports that rather than \
                     claiming it. No branch, so no pull request either.",
                Some(crate::leases::Placement::Worktree) =>
                    "opened and running, with a worktree already cut and writable. It will not \
                     call `claim_worktree`; it has one. Ask it for a draft pull request off \
                     that branch before it reports.",
                Some(crate::leases::Placement::Share { .. }) =>
                    "opened and running in the worktree the other work holds. Both engineers \
                     are writing in one directory, so what keeps them apart is the files each \
                     task owns and nothing else.",
                Some(crate::leases::Placement::Direct) =>
                    "opened and running in Reljod's real checkout. There is no branch between \
                     this session and his working tree.",
            },
        }))
    }

    /// Everything `open_work` decides before a process has to exist.
    ///
    /// Split out at exactly the seam [`crate::orchestrator::prepare_work`] is
    /// split at, and for the same reason: the spawn needs a supervisor and a
    /// harness binary, so a test that has to reach it cannot run, and the whole
    /// of the judgement here would go unchecked. Every argument the manager
    /// passes is read, validated and refused in this function; the caller below
    /// does the launching and the bookkeeping and no deciding.
    ///
    /// This is not a hypothetical worry. `obj()` puts no
    /// `additionalProperties: false` on any schema in this catalogue, so before
    /// the placement arguments existed a call carrying `placement: "explore"`
    /// was accepted, ignored, and answered with a success — a manager placing
    /// an engineer read-only was told it worked and got an ordinary one.
    fn opening_for(&self, raiser: &Raiser, args: &Value) -> Result<Planned, ToolError> {
        let instruction = required_str(args, "instruction")?;
        let harness = match opt_str(args, "harness") {
            Some(h) => parse_harness(&h)
                .ok_or_else(|| ToolError::BadParams(format!("unknown harness `{h}`")))?,
            None => HarnessKind::ClaudeCode,
        };
        // A work session that cannot talk to its siblings is not a member of
        // anything, so this defaults higher than `delegate`'s child does — and
        // is still capped at the caller's own, which is the half that matters.
        let tools = match opt_str(args, "tools") {
            Some(t) => parse_access(&t)
                .ok_or_else(|| ToolError::BadParams(format!("unknown tool access `{t}`")))?,
            None => ToolAccess::Delegate,
        };
        if !allows(self.access, tools) {
            return Err(ToolError::Refused(format!(
                "`{}` tool access exceeds your own `{}`",
                tools.as_str(),
                self.access.as_str()
            )));
        }

        let checkout = match opt_str(args, "checkout") {
            Some(path) => PathBuf::from(path),
            None => {
                let roots = self
                    .store()?
                    .roots(&raiser.conversation_id)
                    .map_err(|e| ToolError::Refused(format!("could not read your roots: {e}")))?;
                // Refused rather than defaulted to this process's directory: a
                // work opened in whatever directory the daemon happens to be
                // started in is a run editing something nobody meant.
                //
                // A project manager is the one caller that reliably has no
                // roots and still knows the answer. It is created against its
                // project and never adds a root of its own, so its every first
                // `open_work` was refused, and the manager spent a model turn
                // discovering a directory the store could have told it. The
                // project's own path is not a guess in the way the process
                // directory is: it is the repository this conversation exists
                // to run, written down when the manager was created.
                let project = self
                    .store()?
                    .current_project(&raiser.conversation_id)
                    .ok()
                    .flatten();
                match (roots.first(), project) {
                    (Some(root), _) => root.path.clone(),
                    (None, Some(project)) => project.path,
                    (None, None) => {
                        return Err(ToolError::Refused(
                            "say which directory this work happens in — `checkout` — because \
                             this session has no roots of its own to inherit one from, and no \
                             project to take one from either"
                                .into(),
                        ))
                    }
                }
            }
        };

        // Where the manager decided this engineer writes. **Absent is not
        // `explore`.** An absent placement is `None`, which is what every
        // caller that predates placements passes and what keeps their spawns
        // byte-identical: the checkout arrives read-only and the session calls
        // `claim_worktree` for itself. `explore` is a decision somebody made,
        // and the brief says so.
        let share_with = opt_str(args, "share_with");
        let placement = match opt_str(args, "placement") {
            Some(id) => Some(
                crate::leases::Placement::parse(&id, share_with.as_deref())
                    .map_err(|e| ToolError::BadParams(e.to_string()))?,
            ),
            // Refused rather than ignored. A `share_with` with no `placement`
            // is a manager that meant to share and will otherwise be told the
            // work opened, get an ordinary engineer with a branch of its own,
            // and have no way to tell from the answer.
            None if share_with.is_some() => {
                return Err(ToolError::BadParams(
                    "`share_with` names the work whose worktree to join, which only means \
                     something with `placement: \"share\"`. Pass both, or neither."
                        .into(),
                ))
            }
            None => None,
        };

        // The files this engineer owns, settled before anything is created so
        // a bad prefix refuses instead of opening a work and then failing.
        let mut paths = Vec::new();
        for raw in string_list(args, "paths")? {
            paths.push(
                crate::works::normalise_path(&raw)
                    .map_err(|e| ToolError::BadParams(e.to_string()))?,
            );
        }

        // **The gate on `direct`, and it lives here rather than in
        // `prepare_work` on purpose.** By the time a placement reaches the
        // orchestrator somebody has decided; this is the last point at which
        // the refusal can carry every failing condition at once and name what
        // to ask for instead. Run before the work exists, which is what makes
        // "no other work on this project" mean what it says — see
        // [`crate::leases::direct_is_allowed`].
        if matches!(placement, Some(crate::leases::Placement::Direct)) {
            let store = self.store()?;
            // An uncatalogued directory has no project id, and the empty
            // string counts no works — which is the honest answer for a
            // repository Jod has never been told about. The two git conditions
            // still decide it.
            let project = store.project_for_path(&checkout).ok().flatten();
            let verdict = crate::leases::direct_is_allowed(
                store,
                project.as_ref().map(|p| p.id.as_str()).unwrap_or(""),
                &checkout,
            )
            .map_err(|e| {
                ToolError::Refused(format!("could not check whether `direct` is allowed here: {e}"))
            })?;
            if !verdict.allowed {
                return Err(ToolError::Refused(format!(
                    "`direct` is not allowed in `{}`:\n{}\n\nOpen this work with \
                     `placement: \"worktree\"` instead. That cuts a branch and a worktree of \
                     this engineer's own, leaves Reljod's checkout beside it read-only, and is \
                     what every one of those conditions exists to send you to.",
                    checkout.display(),
                    verdict
                        .because
                        .iter()
                        .map(|reason| format!("- {reason}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )));
            }
        }

        // Inherited, not chosen here. This used to ask for `accept_edits`
        // outright and cap it, which reads as a safe default and is not one: a
        // main chat the operator had put in `auto` opened all of its background
        // work one level down, in a mode where headless Claude Code has nobody
        // to ask and refuses `git init`, `pnpm -v` and every other mutation.
        // The mode on the status bar never reached the process doing the work,
        // and the run reported the refusals as its own failures.
        //
        // The ceiling *is* the operator's answer, and there is no argument left
        // that can talk it down — see [`Server::child_permission`]. The override
        // this used to accept was how "a caller may ask for less without asking
        // anybody" turned into a model quietly choosing the mode.
        let permission = self.child_permission();
        let mut opening = crate::orchestrator::Opening::new(instruction, checkout)
            .on(harness)
            .with_permission(permission)
            .under(raiser.conversation_id.clone());
        opening.tools = tools;
        // The first session on a work is an engineer. Dropped when the call
        // named a harness, for the reason `delegate` drops it: an argument in
        // the tool call is the highest rung of the four.
        opening.role = opt_str(args, "harness").is_none().then_some(Role::Engineer);
        if let Some(placement) = placement {
            opening = opening.placed(placement);
        }
        if let Some(model) = opt_str(args, "model") {
            opening = opening.with_model(model);
        }
        Ok(Planned { opening, paths })
    }

    // ---- the board --------------------------------------------------------

    /// Write a manager's whole breakdown onto a work's board.
    ///
    /// Thin wiring over [`Store::plan_work`], and deliberately so. Every
    /// judgement this tool looks like it makes — whether two tasks collide,
    /// what a path prefix means, whether a plan half-written is worse than none
    /// — is settled in `works.rs` where it can be tested without a tool call.
    /// What is here is the shape of the arguments and the refusal getting back
    /// to the model unedited.
    ///
    /// The store's refusal is passed through verbatim rather than wrapped in
    /// "could not plan that". It already names both colliding titles and both
    /// paths, in the words a manager needs to write a different plan, and a
    /// prefix would only push the useful half further from the start of the
    /// line.
    fn plan_work(&self, args: &Value) -> Result<String, ToolError> {
        let work_id = required_str(args, "work_id")?;
        let Some(tasks) = args.get("tasks").and_then(Value::as_array) else {
            return Err(ToolError::BadParams(
                "`tasks` is the breakdown, as a list of `{title, paths}` objects. A call with \
                 none hands nothing out and leaves the board as it was."
                    .into(),
            ));
        };
        let mut plan = crate::works::Plan::default();
        for (index, task) in tasks.iter().enumerate() {
            let Some(title) = task.get("title").and_then(Value::as_str) else {
                return Err(ToolError::BadParams(format!(
                    "task {} has no `title`. A task nobody can name is one nobody can report \
                     finishing.",
                    index + 1
                )));
            };
            plan.tasks.push(crate::works::PlannedTask {
                title: title.to_string(),
                paths: string_list(task, "paths")?,
            });
        }

        let board = self
            .store()?
            .plan_work(&work_id, &plan)
            .map_err(|e| ToolError::Refused(e.to_string()))?;
        as_json(&json!({
            "work_id": work_id,
            "tasks": board.iter().map(task_row).collect::<Vec<_>>(),
            "note": "on the board, in the order you wrote them. Hand out the ones nothing is \
                     waiting on — `open_work` takes one engineer at a time — and keep the rest \
                     until what they depend on has landed.",
        }))
    }

    /// A work's board, which is the answer to "is it done yet" that does not
    /// cost an engineer a turn.
    ///
    /// An unknown work is refused rather than answered with an empty board. A
    /// manager that mistypes a work id and is handed `[]` reads it as a job
    /// with nothing left to do, which is the one wrong answer this tool can
    /// give that looks exactly like a right one.
    fn work_board(&self, args: &Value) -> Result<String, ToolError> {
        let work_id = required_str(args, "work_id")?;
        let store = self.store()?;
        if store
            .work(&work_id)
            .map_err(|e| ToolError::Refused(format!("could not read that work: {e}")))?
            .is_none()
        {
            return Err(ToolError::Refused(format!(
                "no work `{work_id}`. An empty board and a work that does not exist read alike \
                 and mean opposite things, so this is a refusal rather than an empty list."
            )));
        }
        let tasks = store
            .work_tasks(&work_id)
            .map_err(|e| ToolError::Refused(format!("could not read that board: {e}")))?;
        let open = tasks.iter().filter(|t| t.status != "done").count();
        as_json(&json!({
            "work_id": work_id,
            "tasks": tasks.iter().map(task_row).collect::<Vec<_>>(),
            "open": open,
            "done": tasks.len() - open,
            // Not simply `open == 0`: a board nobody has planned yet is empty
            // and is the opposite of finished.
            "finished": !tasks.is_empty() && open == 0,
        }))
    }

    /// An engineer says its one task is done, and its report goes to its
    /// manager.
    ///
    /// Three things happen, in this order, and the order matters. The task is
    /// marked done first, because that is what the caller asked for and it must
    /// not depend on there being anybody to tell. The report is then delivered
    /// into the manager's conversation. Last, the caller is told plainly
    /// whether it was the last open task, so an engineer's closing line is not
    /// a guess about whether its colleagues are still working.
    ///
    /// **The report is a delivery, not a card, and that is the whole of D4.4.**
    /// A card cascades upward through every ancestor — [`Store::cards_in`]
    /// walks the subtree — so an engineer's routine "I finished" raised as one
    /// lands on main's rail three links up, which is the noise Reljod asked to
    /// be rid of. Nothing about the cascade is changed here; what changed is
    /// that routine reporting stopped travelling as a card. A blocked
    /// engineer's `ask_question` still goes all the way to him, because a
    /// manager that will not run again for an hour cannot answer it.
    fn complete_task(&self, args: &Value) -> Result<String, ToolError> {
        let raiser = self.raiser()?;
        let task_id = required_str(args, "task_id")?;
        let report = required_str(args, "report")?;
        if report.trim().is_empty() {
            return Err(ToolError::BadParams(
                "`report` is the whole of what your manager will be told you did, so an empty \
                 one finishes the task and says nothing about it"
                    .into(),
            ));
        }
        let store = self.store()?;

        // Read before the task is marked done, because the title is what names
        // the report and the session name is what tells the manager who is
        // speaking. Both are best effort: a caller outside a work still gets to
        // finish its task and still gets its report delivered.
        let work_id = store
            .work_for_conversation(&raiser.conversation_id)
            .ok()
            .flatten();
        let title = work_id
            .as_deref()
            .and_then(|work| store.work_tasks(work).ok())
            .and_then(|tasks| tasks.into_iter().find(|t| t.id == task_id))
            .map(|t| t.title);
        let who = work_id
            .as_deref()
            .and_then(|work| store.work_sessions(work).ok())
            .and_then(|sessions| {
                sessions
                    .into_iter()
                    .find(|s| s.conversation_id == raiser.conversation_id)
            })
            .map(|s| s.name)
            .filter(|name| !name.is_empty());

        let closing = store
            .complete_work_task(&task_id)
            .map_err(|e| ToolError::Refused(e.to_string()))?;
        let last = closing.is_some();
        let still_open = work_id
            .as_deref()
            .and_then(|work| store.work_tasks(work).ok())
            .map(|tasks| tasks.iter().filter(|t| t.status != "done").count());

        let body = report_body(
            who.as_deref(),
            title.as_deref(),
            &task_id,
            &report,
            work_id.as_deref(),
            last,
            still_open,
        );
        let addressee = self.report_addressee(&raiser.conversation_id)?;
        let delivered = match &addressee {
            Some(to) => store
                .enqueue_delivery(&to.conversation_id, delivery::Kind::Mail, &task_id, &body)
                .map(|_| true)
                .map_err(|e| ToolError::Refused(format!("could not deliver your report: {e}")))?,
            None => false,
        };

        as_json(&json!({
            "task_id": task_id,
            "task": title,
            "last": last,
            "still_open": still_open,
            "reported_to": addressee.as_ref().map(|to| to.conversation_id.clone()),
            "delivered": delivered,
            "note": match (&addressee, last) {
                (Some(to), true) if to.is_manager =>
                    "done, and yours was the last open task on this board — the work is closed. \
                     Your report is with your manager, which decides what to tell Reljod. Stop \
                     here rather than looking for more to do.".to_string(),
                (Some(_), true) =>
                    "done, and yours was the last open task on this board — the work is closed. \
                     Your report went to the conversation that started you, because there is no \
                     manager above this session. Stop here.".to_string(),
                (Some(to), false) if to.is_manager =>
                    "done. Your report is with your manager. Other tasks on this board are still \
                     open, so the job is not finished — that is somebody else's task and not \
                     yours to pick up. Stop here.".to_string(),
                (Some(_), false) =>
                    "done. Your report went to the conversation that started you, because there \
                     is no manager above this session. Other tasks on this board are still open. \
                     Stop here.".to_string(),
                (None, _) =>
                    "done, and the task is off the board — but this session has no conversation \
                     above it, so there was nobody to deliver the report to. Say what you did in \
                     your own output as well, because nothing else carried it.".to_string(),
            },
        }))
    }

    /// Where an engineer's report goes: the nearest manager above it, or its
    /// parent when there is no manager.
    ///
    /// A manager is a conversation named by some project's
    /// `manager_conversation_id`, and "above" is the `parent_conversation_id`
    /// chain — the same edge the rail's cascade walks, in the other direction.
    /// The nearest one wins, so an engineer that opened work of its own reports
    /// to the manager that owns the repository rather than to main.
    ///
    /// **Falling back to the parent is required, not a convenience.** An
    /// engineer started by main directly, or by a test, has no manager over it,
    /// and a report with no addressee is the failure this whole change exists
    /// to remove. Only a conversation with no parent at all — main itself —
    /// ends up with nobody to tell.
    ///
    /// The walk is SQL here rather than a `Store` method because
    /// `conversations` has no ancestor query and the files that would host one
    /// belong to other people this week. It is the one statement in this module
    /// and it should move to `store.rs` beside
    /// [`Store::descendant_conversations`], which is its mirror image. The
    /// depth bound is what a recursive walk over a parent chain needs in order
    /// not to hang the process holding the store lock if a cycle is ever
    /// written — the same worry `descendant_conversations` answers with `UNION`
    /// rather than `UNION ALL`.
    fn report_addressee(&self, from: &str) -> Result<Option<Addressee>, ToolError> {
        let store = self.store()?;
        let conn = store.conn.lock().expect("store lock poisoned");
        let mut stmt = conn
            .prepare(
                "WITH RECURSIVE above(id, parent_id, depth) AS (
                   SELECT id, parent_conversation_id, 0
                     FROM conversations WHERE id = ?1
                   UNION ALL
                   SELECT c.id, c.parent_conversation_id, above.depth + 1
                     FROM conversations c JOIN above ON c.id = above.parent_id
                    WHERE above.depth < 64
                 )
                 SELECT above.id,
                        EXISTS (SELECT 1 FROM projects p
                                 WHERE p.manager_conversation_id = above.id)
                   FROM above
                  WHERE above.depth > 0
                  ORDER BY above.depth",
            )
            .map_err(|e| ToolError::Refused(format!("could not look for your manager: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![from], |r| {
                Ok(Addressee {
                    conversation_id: r.get(0)?,
                    is_manager: r.get::<_, i64>(1)? != 0,
                })
            })
            .map_err(|e| ToolError::Refused(format!("could not look for your manager: {e}")))?;
        let chain: Vec<Addressee> = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| ToolError::Refused(format!("could not look for your manager: {e}")))?;
        Ok(chain
            .iter()
            .find(|a| a.is_manager)
            .or_else(|| chain.first())
            .cloned())
    }

    /// Hand a manager the `gh stack link` line for its work's pull requests.
    ///
    /// Every hard part of this is in [`crate::prs`]: which pull request sits on
    /// which, how a shared worktree's single pull request is ranked, what the
    /// command line looks like, and what to say when there is nothing worth
    /// linking. This is wiring, and the refusal in particular comes from
    /// [`crate::prs::stack_refusal`] rather than being written again here —
    /// two sentences about the same rule are two sentences that can disagree.
    fn stack_pull_requests(&self, args: &Value) -> Result<String, ToolError> {
        let work_id = required_str(args, "work_id")?;
        let store = self.store()?;
        if store
            .work(&work_id)
            .map_err(|e| ToolError::Refused(format!("could not read that work: {e}")))?
            .is_none()
        {
            return Err(ToolError::Refused(format!("no work `{work_id}` to stack")));
        }
        match store
            .stack_for_work(&work_id)
            .map_err(|e| ToolError::Refused(format!("could not read the pull requests: {e}")))?
        {
            crate::prs::Stacking::TooFew { found } => {
                Err(ToolError::Refused(crate::prs::stack_refusal(found)))
            }
            crate::prs::Stacking::Ready(stack) => as_json(&json!({
                "work_id": work_id,
                "count": stack.prs.len(),
                // Bottom to top, which is the order the command takes and the
                // order the list has to be read in. Said in the field name as
                // well as in the instruction, because a list whose order
                // carries meaning and does not say so gets re-sorted.
                "bottom_to_top": stack.prs.iter().map(|pr| json!({
                    "number": pr.number,
                    "url": pr.url,
                    "branch": pr.branch,
                    "repo": pr.repo,
                    "title": pr.title,
                    "state": pr.state.as_str(),
                })).collect::<Vec<_>>(),
                "command": crate::prs::stack_link_command(&stack.prs),
                "instruction": crate::prs::stack_instruction(&stack.prs),
            })),
        }
    }

    // ---- the bus ----------------------------------------------------------

    /// Which member is calling, resolved from the run and from nothing else.
    ///
    /// Both refusals are deliberate and different. A server with no run behind
    /// it is a session somebody opened by hand: it may read Jod, but it cannot
    /// be anybody's teammate, and pretending otherwise would mean letting the
    /// caller say who it is. A run that belongs to no scope has nobody to talk
    /// to, which is a fact about the fleet rather than about this call.
    pub fn caller(&self) -> Result<Caller, ToolError> {
        let run_id = self.identified_run("sending")?;
        self.store()?
            .caller_for_run(run_id)
            .map_err(|e| ToolError::Refused(format!("could not resolve who is calling: {e}")))?
            .ok_or_else(|| {
                ToolError::Refused(format!(
                    "run `{run_id}` is not a member of any team or work, so there is nobody it \
                     could be writing to. Teams are joined with `jod team join`."
                ))
            })
    }

    fn roster(&self) -> Result<String, ToolError> {
        let caller = self.caller()?;
        let who = self
            .store()?
            .roster(caller.scope, &caller.team, &caller.name)
            .map_err(|e| ToolError::Refused(format!("could not read the roster: {e}")))?;
        as_json(&json!({
            "you": caller.name,
            "scope": caller.scope,
            "of": caller.team,
            "members": who,
        }))
    }

    fn read_messages(&self) -> Result<String, ToolError> {
        let caller = self.caller()?;
        let store = self.store()?;
        // The existing single-transaction drain, unchanged. It is the reason
        // the same instruction is never injected into two turns, and reusing it
        // rather than writing a second one is the whole point.
        let taken = store
            .drain_inbox(&caller.team, &caller.name)
            .map_err(|e| ToolError::Refused(format!("could not read your inbox: {e}")))?;
        let ids: Vec<i64> = taken.iter().map(|m| m.id).collect();
        store
            .mark_mail_delivered(&ids)
            .map_err(|e| ToolError::Refused(format!("could not mark your mail read: {e}")))?;
        // Read back for the thread each message belongs to, which is what a
        // reply needs and what the bare delivered message does not carry.
        let envelopes = store
            .envelopes(&ids)
            .map_err(|e| ToolError::Refused(format!("could not read your inbox: {e}")))?;
        as_json(&envelopes)
    }

    fn send_message(&self, args: &Value) -> Result<String, ToolError> {
        let caller = self.caller()?;
        let text = required_str(args, "text")?;
        if text.trim().is_empty() {
            return Err(ToolError::BadParams("`text` is empty".into()));
        }
        let to = opt_str(args, "to");
        let mut post = Post::new(caller.scope, &caller.team, &caller.name, &text);
        if let Some(to) = &to {
            post = post.to(to);
        }
        self.post(&post)
    }

    fn reply(&self, args: &Value) -> Result<String, ToolError> {
        let caller = self.caller()?;
        let text = required_str(args, "text")?;
        let message_id = opt_i64(args, "message_id")?
            .ok_or_else(|| ToolError::BadParams("`message_id` is required".into()))?;
        let store = self.store()?;
        let answering = store
            .envelope(message_id)
            .map_err(|e| ToolError::Refused(format!("could not read message #{message_id}: {e}")))?
            .ok_or_else(|| ToolError::Refused(format!("there is no message #{message_id}")))?;
        // Replies go back to whoever sent it. Taken from the message rather
        // than from an argument, so `reply` cannot be used to address a
        // stranger under cover of a thread.
        let to = answering.message.from.clone();
        self.post(
            &Post::new(caller.scope, &caller.team, &caller.name, &text)
                .to(&to)
                .replying_to(message_id),
        )
    }

    async fn ask(&self, args: &Value) -> Result<String, ToolError> {
        let caller = self.caller()?;
        let to = required_str(args, "to")?;
        let text = required_str(args, "text")?;
        // Bounded whatever the caller asks for. A5 exists because an agent that
        // can wait without a deadline is an agent that can hang for ever — the
        // peer it is waiting on may be dead, and nothing would ever say so.
        let seconds = opt_i64(args, "timeout_seconds")?
            .unwrap_or(ASK_DEADLINE_SECS)
            .clamp(1, MAX_ASK_DEADLINE_SECS);
        let store = self.store()?;

        let sent = store
            .post(&Post::new(caller.scope, &caller.team, &caller.name, &text).to(&to))
            .map_err(|e| ToolError::Refused(format!("could not send that: {e}")))?;
        let Sent::Queued { ids, thread_id, .. } = &sent else {
            return self.rendered(sent);
        };

        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(seconds as u64);
        loop {
            let answer = store
                .reply_to(ids)
                .map_err(|e| ToolError::Refused(format!("could not watch for a reply: {e}")))?;
            if let Some(answer) = answer {
                // Taken off the bus here, or it would be delivered again as a
                // synthetic turn later and the answer would arrive twice.
                store
                    .mark_mail_delivered(&[answer.message.id])
                    .map_err(|e| ToolError::Refused(format!("could not settle the reply: {e}")))?;
                return as_json(&json!({
                    "replied": true,
                    "from": answer.message.from,
                    "text": answer.message.text,
                    "message_id": answer.message.id,
                    "thread_id": answer.thread_id,
                }));
            }
            if std::time::Instant::now() >= deadline {
                // An answer, not an error: the asker is told plainly and
                // decides for itself what to do about the silence.
                return as_json(&json!({
                    "replied": false,
                    "waited_seconds": seconds,
                    "thread_id": thread_id,
                    "note": format!(
                        "no reply from `{to}` within {seconds}s. It may be busy, or holding no \
                         session to resume — the roster says which. Carry on without it, or ask \
                         again later; the question is on the bus either way."
                    ),
                }));
            }
            tokio::time::sleep(ASK_POLL).await;
        }
    }

    fn handoff(&self, args: &Value) -> Result<String, ToolError> {
        let caller = self.caller()?;
        let to = required_str(args, "to")?;
        let text = required_str(args, "text")?;
        let task_id = opt_str(args, "task_id");
        let store = self.store()?;

        // The board first, and the message second. Ownership is the claim, not
        // the telling — so if the message is refused by a bound, the task has
        // still moved and the record still says who holds it.
        let mut moved = None;
        if let Some(task) = &task_id {
            let ok = store
                .hand_over_task(task, &caller.name, &to)
                .map_err(|e| ToolError::Refused(format!("could not move `{task}`: {e}")))?;
            if !ok {
                return Err(ToolError::Refused(format!(
                    "`{task}` is not yours to hand over — it is either somebody else's or not on \
                     the board"
                )));
            }
            moved = Some(task.clone());
        }
        let body = match &moved {
            Some(task) => format!("handing `{task}` to you.\n\n{text}"),
            None => text.clone(),
        };
        let sent = store
            .post(
                &Post::new(caller.scope, &caller.team, &caller.name, &body)
                    .to(&to)
                    .of_kind(Kind::Handoff),
            )
            .map_err(|e| ToolError::Refused(format!("could not send the handoff: {e}")))?;
        match &sent {
            Sent::Queued { ids, thread_id, .. } => as_json(&json!({
                "handed_over": moved,
                "to": to,
                "message_id": ids.first(),
                "thread_id": thread_id,
            })),
            _ => {
                let rendered = self.rendered(sent);
                match moved {
                    // Said plainly, because the two halves ended differently
                    // and a caller told only about the message would believe
                    // the task did not move.
                    Some(task) => Err(ToolError::Refused(format!(
                        "`{task}` is now `{to}`'s on the board, but they were not told: {}",
                        rendered.err().map(|e| refusal_text(&e)).unwrap_or_default()
                    ))),
                    None => rendered,
                }
            }
        }
    }

    /// Put one message on the bus and answer for it.
    fn post(&self, post: &Post) -> Result<String, ToolError> {
        let sent = self
            .store()?
            .post(post)
            .map_err(|e| ToolError::Refused(format!("could not send that: {e}")))?;
        self.rendered(sent)
    }

    /// How every ending of a send reads to the agent that attempted it.
    ///
    /// A bound and an undeliverable address both come back as refusals rather
    /// than as errors, because they are answers: the model should read them and
    /// choose differently, which is exactly what it cannot do with a protocol
    /// error.
    fn rendered(&self, sent: Sent) -> Result<String, ToolError> {
        match sent {
            Sent::Queued {
                ids,
                thread_id,
                depth,
                recipients,
            } => as_json(&json!({
                "sent": true,
                "message_ids": ids,
                "thread_id": thread_id,
                "depth": depth,
                "to": recipients,
            })),
            Sent::Bounded {
                bound,
                limit,
                reached,
                thread_id,
                ..
            } => {
                // Logged as well as answered: a thread that stopped is a thing
                // a person should be able to find afterwards without reading
                // the transcript of either agent.
                //
                // TODO(E2 cards): raise this as a card — "these two have
                // exchanged N messages without closing a task; continue,
                // redirect, or stop?" — once the card store lands. The card is
                // the escalation surface the spec names; until it exists this
                // line and the paused thread state are how a human finds out.
                eprintln!(
                    "[jod/mcp] thread {thread_id} paused: {} bound of {limit} reached at {reached}",
                    bound.as_str()
                );
                Err(ToolError::Refused(format!(
                    "this thread has hit its {} bound of {limit} and is paused — the work and \
                     both sessions carry on, but this exchange needs a person to say whether to \
                     continue. Say what you have concluded so far rather than asking again.",
                    bound.as_str()
                )))
            }
            Sent::Undeliverable { detail, .. } => Err(ToolError::Refused(detail)),
        }
    }
}

// ---- whether a claimed worktree can actually be written in ----------------

/// What Jod is able to say about the caller's ability to write in a worktree.
///
/// Three answers rather than two, because "Jod could not find out" is a real
/// state and reporting it as either of the others is how this went wrong in the
/// first place. The tool used to imply the first answer unconditionally.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CanWrite {
    /// The run's own command line reaches this path, or the run is not confined
    /// by its command line at all.
    Yes,
    /// The run's command line is known and does not reach this path.
    No { granted: Vec<std::path::PathBuf> },
    /// The run is in plan mode, which refuses every write wherever it is aimed.
    /// Not the same failure, and it needs a different sentence: this one is the
    /// mode working, not Jod's bug.
    NoWritesAtAll,
    /// Jod could not read what the run was launched with.
    Unverified { why: String },
}

impl CanWrite {
    /// The machine-readable half, for a caller that wants to branch on it
    /// rather than read the prose.
    fn verdict(&self) -> &'static str {
        match self {
            CanWrite::Yes => "yes",
            CanWrite::No { .. } | CanWrite::NoWritesAtAll => "no",
            CanWrite::Unverified { .. } => "unverified",
        }
    }

    /// What to tell the session, when there is something to tell it.
    ///
    /// Longer than it looks like it needs to be, and each part is load-bearing.
    /// It names the worktree, because a session holding several paths cannot
    /// act on "somewhere is not writable". It says the session cannot write,
    /// rather than that a write failed, because the session has not tried yet
    /// and should not have to. And it says whose bug this is: two
    /// messages elsewhere in this codebase sent readers to hunt for a broken
    /// harness binary and a repository that was not a git repository, and both
    /// were wrong about where the problem lived. A session told only "cannot
    /// write" would reasonably start checking file modes in Reljod's own
    /// checkout and find nothing, because there is nothing there to find.
    fn warning(&self, worktree: &std::path::Path) -> Option<String> {
        let worktree = worktree.display();
        match self {
            CanWrite::Yes => None,
            CanWrite::No { granted } => {
                let granted: Vec<String> =
                    granted.iter().map(|d| d.display().to_string()).collect();
                Some(format!(
                    "you cannot write to {worktree}. This session was started with its writable \
                     directories fixed to {}, nothing can widen them while it is running, and the \
                     worktree above is outside them. This is a known limitation in Jod itself — \
                     finding O1 in `tasks/10-orchestration.md`, still open and waiting on a \
                     decision about how to close it — and it is not a permissions problem in this \
                     repository or on this machine, so do not go looking for one there. Do not \
                     route around it either. Say plainly that the work could not be written, and \
                     stop.",
                    granted.join(", ")
                ))
            }
            CanWrite::NoWritesAtAll => Some(format!(
                "you cannot write to {worktree}, or anywhere else. This session was started in \
                 plan mode, which refuses every write however it is attempted. That is the mode \
                 it was given rather than a fault in Jod or in this repository. Say what you \
                 would change and why, and leave the changing to a session that is allowed to \
                 make it."
            )),
            CanWrite::Unverified { why } => Some(format!(
                "Jod has not confirmed that you can write to {worktree}. It could not read the \
                 record of what this session was launched with ({why}), and a session's writable \
                 directories are fixed when it starts, so that path may be outside them. Try the \
                 write. If it is refused, that is finding O1 in `tasks/10-orchestration.md` — a \
                 known limitation in Jod itself, still open and waiting on a decision — and not a \
                 permissions problem in this repository or on this machine. Say so and stop \
                 rather than routing around it."
            )),
        }
    }
}

/// Whether the session calling `claim_worktree` can write in the worktree it
/// has just been handed.
///
/// **Not a probe, deliberately.** Writing a file into the worktree and removing
/// it again is the obvious implementation and it is worse than nothing here:
/// Jod created that directory itself, moments earlier, from this very process,
/// so the probe answers "can Jod write here" — which was never in doubt — and
/// answers it "yes" in precisely the case where the session cannot. Measured on
/// the run that prompted this: the harness was refused the worktree, and a probe
/// from a Jod-shaped process on the same directory succeeded immediately. A
/// check that cannot fail is worse than no check, because it is quoted as
/// evidence.
///
/// What it reads instead is `runs/<id>/spawn.json`, the record of the argument
/// list and working directory the process was actually launched with. That file
/// is written before the supervisor starts anything and is never edited, so it
/// says what the harness got rather than what Jod's tables say it should have
/// got — and the gap between those two is the whole of the bug being reported.
fn can_write(run_id: &str, worktree: &std::path::Path) -> CanWrite {
    let path = crate::paths::spawn_path(run_id);
    let plan: crate::runner::SpawnPlan = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(plan) => plan,
            Err(e) => {
                return CanWrite::Unverified {
                    why: format!("{} could not be read as a spawn plan: {e}", path.display()),
                }
            }
        },
        // The ordinary reason is a session Jod did not launch — someone ran the
        // harness by hand against Jod's MCP server. Such a session has no plan
        // and never will, and telling it "you cannot write" would be a
        // fabrication about a process Jod knows nothing about.
        Err(e) => {
            return CanWrite::Unverified {
                why: format!("{} could not be opened: {e}", path.display()),
            }
        }
    };
    let grant = crate::harness::grants::granted_at_launch(&plan.args, &plan.cwd);
    use crate::harness::grants::Confinement;
    match grant.confinement {
        Confinement::Unbounded => CanWrite::Yes,
        Confinement::Refused => CanWrite::NoWritesAtAll,
        Confinement::ToDirectories if grant.covers(worktree) => CanWrite::Yes,
        Confinement::ToDirectories => CanWrite::No {
            granted: grant.dirs,
        },
    }
}

// ---- the board's own small pieces ----------------------------------------

/// An `open_work` call, read and settled, with nothing started yet.
///
/// The paths ride beside the [`crate::orchestrator::Opening`] rather than on it
/// because they belong to a task that does not exist until the orchestrator
/// creates the work. See [`spawn_onto_first_task`], which is where they land.
struct Planned {
    opening: crate::orchestrator::Opening,
    paths: Vec<String>,
}

/// Record that this session was spawned onto its work's first task, and answer
/// with the task it was pointed at.
///
/// **This is the only production writer of `conversations.task_id`, and the
/// column had none at all until it existed.** That is worth spelling out
/// because nothing failed while it was missing. Two finished, tested, green
/// features read the column and both of them degrade *quietly* to a wrong
/// answer rather than to an error:
///
/// - [`Store::stack_for_work`] joins through it to rank a pull request by its
///   task's position in the plan. A null matches nothing, so every pull request
///   falls into the no-position bucket and the ordering drops through to
///   `detected_at_ms` — finish order, which is the exact bug the column was
///   added to fix. The stack comes out with its bases inverted and looks fine.
/// - `leases::share_lease` asks each session in a shared worktree what its open
///   task owns, through the same join, before letting a borrower in. A null
///   means "claims nothing", so the comparison has nothing to compare and every
///   share is waved through — at the moment file ownership matters most.
///
/// **The first task, not a chosen one.** `create_work_in` puts the work's
/// instruction on the board as its first task, so a work always has one and
/// the session opening that work is the session doing it. Once `open_work`
/// takes a placement and a task of its own, this is where that id goes instead;
/// until then, the first task is the true answer rather than a placeholder.
///
/// **And the files that task owns**, when the manager named any. `open_work`
/// creates the work, so the only task there is to hang them on is the one
/// `create_work_in` wrote, and it does not exist until the orchestrator has
/// run. They are recorded rather than merely reported because
/// `leases::share_lease` reads them back through `conversations.task_id` before
/// it lets a second engineer into a worktree — a task that owns nothing claims
/// nothing, and the collision guard has nothing to compare.
///
/// Both writes are one transaction. A session pointed at a task whose paths
/// did not land would be the same silent half-state this function exists to
/// remove.
///
/// Best effort, and it returns what it wrote so the caller can say. Failing the
/// tool call here would be worse than a null: the agent is already running by
/// the time this executes, so a refusal would tell a manager the work did not
/// open and invite it to open a second one.
fn spawn_onto_first_task(
    store: &Store,
    work_id: &str,
    conversation_id: &str,
    paths: &[String],
) -> Option<String> {
    let task = store.work_tasks(work_id).ok()?.into_iter().next()?;
    // The same encoding `works::paths_from_column` reads, which is the only
    // reader there is. Written here because that module's writer is private to
    // it and this file may not grow one; the round trip is held by a test
    // rather than by the two agreeing on paper.
    let stored = match paths.is_empty() {
        true => None,
        false => serde_json::to_string(paths).ok(),
    };
    store
        .write(|tx| {
            tx.execute(
                "UPDATE conversations SET task_id = ?2 WHERE id = ?1",
                rusqlite::params![conversation_id, task.id],
            )?;
            if stored.is_some() {
                tx.execute(
                    "UPDATE tasks SET paths = ?2 WHERE id = ?1",
                    rusqlite::params![task.id, stored],
                )?;
            }
            Ok(())
        })
        .ok()?;
    Some(task.id)
}

/// Where a completion report is delivered, and whether that is a manager.
///
/// The flag is carried rather than recomputed because the answer changes what
/// the engineer is told: reporting to a manager means somebody will decide what
/// Reljod hears, and reporting to a plain parent means nobody above has that
/// job.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Addressee {
    conversation_id: String,
    is_manager: bool,
}

/// One task, as both board tools render it.
///
/// Shared so `plan_work` and `work_board` cannot describe the same row two
/// ways. A manager that plans a board and then reads it back must see the same
/// fields under the same names, or it will believe the board changed between
/// the two calls.
fn task_row(task: &crate::team::TeamTask) -> Value {
    json!({
        "task_id": task.id,
        "title": task.title,
        "owner": task.owner,
        "status": task.status,
        "paths": task.paths,
    })
}

/// The turn a manager is handed when one of its engineers finishes.
///
/// Written here rather than left as the raw report, because a delivered turn
/// arrives in a session whose framing is several turns back — the argument
/// [`crate::delivery::render_injection`] already makes about mail. A manager
/// reading a bare paragraph of prose has to work out which engineer wrote it,
/// which task it was, and whether anything is still running; all three are
/// facts Jod already holds, so all three travel with the report.
///
/// It says how many tasks are still open rather than telling the manager what
/// to conclude from that. Deciding whether the job is finished is the manager's
/// job and `work_board` is where it is done properly — this only makes sure the
/// question is asked.
fn report_body(
    who: Option<&str>,
    title: Option<&str>,
    task_id: &str,
    report: &str,
    work_id: Option<&str>,
    last: bool,
    still_open: Option<usize>,
) -> String {
    let engineer = who.unwrap_or("An engineer");
    let task = match title {
        Some(title) => format!("`{title}`"),
        None => format!("task `{task_id}`"),
    };
    let standing = match (last, still_open) {
        (true, _) => {
            "That was the last open task on this board, so the work has closed itself. Nothing \
             else is running on it. You are the one who says the job is done — read the board, \
             then report what the whole job produced rather than relaying this message."
                .to_string()
        }
        (false, Some(1)) => {
            "One task is still open on this board, so the job is not finished yet.".to_string()
        }
        (false, Some(open)) => {
            format!("{open} tasks are still open on this board, so the job is not finished yet.")
        }
        (false, None) => "This task belongs to no board Jod could read back.".to_string(),
    };
    let board = match work_id {
        Some(work_id) => format!(" Call `work_board` with `{work_id}` before you answer."),
        None => String::new(),
    };
    format!("{engineer} has finished {task} and reports:\n\n{report}\n\n{standing}{board}")
}

/// Who this MCP server is entitled to speak as.
///
/// Three answers rather than two, because "we cannot tell" and "we are being
/// told two different things" are different situations and only one of them is
/// ordinary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// The process group belongs to no run at all. A session somebody opened by
    /// hand: it may read Jod and it is nobody's teammate.
    Unknown,
    /// Resolved from the process group this server is in.
    Run(String),
    /// The environment claims one run and the process group says another.
    /// Nothing is guessed — see [`identify`].
    Disputed {
        /// What the kernel says, which may be nothing.
        group: Option<String>,
        /// What the environment claimed.
        claimed: String,
    },
}

/// Work out which run this MCP server belongs to.
///
/// **Read this before simplifying it.** The obvious version of this function
/// takes the run id as an argument, and that version is wrong in a way that is
/// invisible until it matters: sender identity is the one thing an agent must
/// not be able to choose, and an argument — or a flag, or an environment
/// variable it can reach — is only as trustworthy as whatever set it. A model
/// that can write its own `from` can send as anyone on the team.
///
/// So the authority here is the **process group**, and nothing else is. The
/// supervisor `setsid`s itself into its own session and leads that group; the
/// harness runs in it, and so does every MCP server the harness starts. A
/// process cannot move itself into another session's group — that is a kernel
/// rule, not a convention — so the group id *is* the run, and no amount of
/// arguing changes which group a process is in.
///
/// [`crate::mcp_config::RUN_ID_ENV`] is **enrichment, never authority**. It is
/// pinned by whatever launched the run, before the model existed, and it is
/// useful for exactly one case: a group the store has no row for. Where both
/// answer and they **disagree**, this returns [`Identity::Disputed`] and every
/// tool that needs a sender refuses. Quietly preferring either one would turn a
/// misconfiguration — or an attempt at one — into a wrong answer that keeps
/// working, which is the failure mode worth spending a refusal on.
pub fn identify(store: &Store, claimed: Option<&str>) -> Identity {
    // SAFETY: `getpgrp` takes no arguments, touches no memory and cannot fail.
    let pgid = unsafe { libc::getpgrp() };
    let group = if pgid > 0 {
        store.run_by_pgid(pgid as u32).ok().flatten()
    } else {
        None
    };
    let claimed = claimed.map(str::trim).filter(|c| !c.is_empty());
    match (group, claimed) {
        (Some(group), None) => Identity::Run(group),
        (Some(group), Some(claimed)) if group == claimed => Identity::Run(group),
        (group, Some(claimed)) => Identity::Disputed {
            group,
            claimed: claimed.to_string(),
        },
        (None, None) => Identity::Unknown,
    }
}

// ---- the rail's own types and its second emission path --------------------

/// Whose rail a card lands on. See [`Server::raiser`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raiser {
    pub run_id: String,
    pub conversation_id: String,
    /// Denormalised onto every card raised, so a card keeps its work's colour
    /// after the session that raised it is gone.
    pub work_id: Option<String>,
}

/// The key the two emission paths must agree on, computed from what they both
/// have: the kind, and the words of the question.
///
/// A harness can emit one question twice — once by calling Jod's tool and once
/// by printing its own — and two rail cards for one question is worse than
/// none, because answering one leaves the other open for ever. Neither path can
/// see the other, so the only thing they can agree on is the text, and it has
/// to survive the differences between them: capitalisation, a trailing question
/// mark, the whitespace a JSON payload keeps and a prompt does not.
///
/// Capped, because an [`AgentEvent::ToolCall`] payload can be a whole plan and
/// a key that long would never match a second emission that reworded one line
/// near its end.
pub fn dedupe_key(kind: CardKind, subject: &str) -> String {
    let mut words = String::with_capacity(subject.len());
    for c in subject.chars() {
        if c.is_alphanumeric() {
            words.extend(c.to_lowercase());
        } else if !words.ends_with(' ') {
            words.push(' ');
        }
    }
    let words: String = words.trim().chars().take(120).collect();
    format!("{}:{words}", kind.as_str())
}

/// A card recognised in a harness's own output, before it has a conversation.
///
/// The passive half of D2. Jod's MCP server is the supported path and behaves
/// identically everywhere; this is what a run launched *without* it still
/// produces, so the rail is never simply empty because somebody started a
/// session by hand. It reports what the harness already said out loud — it
/// never invents a question that was not asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lifted {
    pub kind: CardKind,
    pub title: String,
    pub body: String,
    pub options: Vec<String>,
    pub blocking: bool,
    pub dedupe_key: String,
}

impl Lifted {
    /// The card this becomes once it knows whose it is.
    pub fn into_card(self, raiser: &Raiser) -> NewCard {
        NewCard {
            conversation_id: raiser.conversation_id.clone(),
            work_id: raiser.work_id.clone(),
            run_id: Some(raiser.run_id.clone()),
            kind: Some(self.kind),
            importance: Some(Importance::Normal),
            blocking: self.blocking,
            title: self.title,
            body: self.body,
            options: self.options,
            source: Some(Source::Lifted),
            dedupe_key: Some(self.dedupe_key),
            ..NewCard::default()
        }
    }
}

/// The harness tool calls that are really questions to a person.
///
/// Two, both Claude Code's, because those are the two that have been *measured*
/// — see `docs/harness-support.md` for the standard this repository holds
/// harness behaviour to. Adding a name here on the strength of a changelog
/// would produce cards for a payload nobody has seen, which is worse than the
/// gap: a wrong card is answered, and an absent one is noticed.
const ASK_USER_QUESTION: &str = "AskUserQuestion";
const EXIT_PLAN_MODE: &str = "ExitPlanMode";

/// Turn one event into the cards it is really asking for.
///
/// A list rather than an option because `AskUserQuestion` carries an array:
/// one call can ask three things, and three questions collapsed into one card
/// is a card that cannot be answered.
pub fn lift(event: &AgentEvent) -> Vec<Lifted> {
    let AgentEvent::ToolCall { name, input } = event else {
        return vec![];
    };
    let input = input.as_ref().unwrap_or(&Value::Null);
    match name.as_str() {
        ASK_USER_QUESTION => lift_questions(input),
        EXIT_PLAN_MODE => lift_plan(input),
        _ => vec![],
    }
}

/// Claude Code's `AskUserQuestion`, in either shape it has been seen in.
///
/// Deliberately tolerant. The payload is another program's private interface,
/// so the choice is between reading it loosely and dropping the card the moment
/// a field is renamed — and a dropped card is a question Reljod never sees. A
/// call with nothing question-shaped in it lifts nothing rather than raising a
/// card titled with a fragment of JSON.
fn lift_questions(input: &Value) -> Vec<Lifted> {
    let asked: Vec<&Value> = match input.get("questions").and_then(Value::as_array) {
        Some(list) => list.iter().collect(),
        None => vec![input],
    };
    asked
        .into_iter()
        .filter_map(|q| {
            let title = q
                .get("question")
                .or_else(|| q.get("header"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            Some(Lifted {
                kind: CardKind::Question,
                title: title.to_string(),
                body: q
                    .get("header")
                    .and_then(Value::as_str)
                    .filter(|h| *h != title)
                    .unwrap_or_default()
                    .to_string(),
                options: labels(q.get("options")),
                // The run is not stopped by this. In print mode — the only mode
                // Jod spawns a harness in — the harness answers its own
                // question and carries on, so marking it `blocked` would put a
                // coloured border and the auto-open on a run that is still
                // working perfectly well.
                blocking: false,
                dedupe_key: dedupe_key(CardKind::Question, title),
            })
        })
        .collect()
}

/// Claude Code's `ExitPlanMode`: the agent asking to start.
///
/// Blocking, and this one really is. Plan mode refuses every mutation, so a run
/// that has reached here does nothing further until somebody says go — which is
/// exactly the case E7.S2 hands to the rail, because print mode has no
/// interactive callback a permission prompt could hang on.
///
/// The options are answerable by digit and are delivered as the agent's *next*
/// turn rather than as this tool call's return: Jod cannot answer a call the
/// harness has already answered itself. That is why they read as instructions —
/// "go ahead" is a sentence the next turn can act on, whereas a bare "yes"
/// arriving with no anchor is an answer to nothing.
fn lift_plan(input: &Value) -> Vec<Lifted> {
    let plan = input
        .get("plan")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if plan.is_empty() {
        return vec![];
    }
    vec![Lifted {
        kind: CardKind::Question,
        title: "start on this plan?".into(),
        body: plan.to_string(),
        options: vec!["go ahead".into(), "stop, I want to change it".into()],
        blocking: true,
        dedupe_key: dedupe_key(CardKind::Question, plan),
    }]
}

/// Option labels out of either an array of strings or an array of objects.
fn labels(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|o| match o {
                    Value::String(s) => Some(s.clone()),
                    other => other
                        .get("label")
                        .or_else(|| other.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .filter(|s| !s.trim().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Lift one event onto the rail of whichever conversation the run belongs to.
///
/// The whole of what a caller watching an event stream has to do. It is
/// idempotent through `dedupe_key`, so replaying a run's events — which
/// `rehydrate` does on every fresh process — cannot produce a second copy of a
/// question, and neither can a harness that both calls Jod's tool and prints
/// its own.
///
/// A run with no conversation lifts nothing. That is not a failure: it is a run
/// nobody is watching a rail for.
pub fn lift_into_cards(
    store: &Store,
    run_id: &str,
    event: &AgentEvent,
) -> crate::Result<Vec<Card>> {
    let lifted = lift(event);
    if lifted.is_empty() {
        return Ok(vec![]);
    }
    let Some(conversation_id) = store.conversation_for_run(run_id)? else {
        return Ok(vec![]);
    };
    let raiser = Raiser {
        work_id: store.work_for_conversation(&conversation_id)?,
        conversation_id,
        run_id: run_id.to_string(),
    };
    lifted
        .into_iter()
        .map(|l| store.raise_card(l.into_card(&raiser)))
        .collect()
}

/// The words inside a refusal, for a caller that has to quote one.
fn refusal_text(e: &ToolError) -> String {
    match e {
        ToolError::Unknown(s)
        | ToolError::Forbidden(s)
        | ToolError::BadParams(s)
        | ToolError::Refused(s) => s.clone(),
    }
}

/// One page of `list_agents`, with the arithmetic that makes it readable as a
/// page rather than as the whole truth.
///
/// A bare array cannot say that it was cut, and the caller here is a model
/// deciding whether to reuse an agent or start a new one. Told twenty agents
/// and nothing else, it has no way to tell a quiet box from a busy one, and no
/// reason to ask again. `jod ls` has always printed the same thing for a person
/// — "{hidden} older hidden" — so this is that line, in fields.
#[derive(Serialize)]
struct AgentPage<'a> {
    agents: Vec<AgentView<'a>>,
    /// How many are on this page.
    returned: usize,
    /// How many there were to page through.
    total: usize,
    /// How many `limit` left out. Zero when the page is everything.
    hidden: usize,
    /// Run ids that can take a new instruction right now — see
    /// [`AgentView::free`]. Newest first, so the head is the one to prefer.
    ///
    /// Here because a manager's whole first decision is this list, and it was
    /// previously left to be derived from `status`, `stalled_for_ms` and
    /// `session_id` together. Two of those three are traps: `busy` is false for
    /// a *stalled* agent as well as for an idle one, and an agent with no
    /// session id cannot be continued however idle it looks. A caller
    /// recomputing that every time is a caller that will eventually get it
    /// wrong and open a cold session beside a perfectly good engineer.
    idle: Vec<&'a str>,
    /// The plain answer to "who should this instruction go to".
    ///
    /// A sentence rather than a field, because the answer is a choice between
    /// two different tool calls and the reason for the choice is the half that
    /// keeps being lost.
    reuse: String,
    /// Scratch sessions recent enough to pick up again, most recently active
    /// first. Kept apart from `idle` because the rule for reusing one is not
    /// the rule for reusing an engineer — see `scratch_reuse`.
    scratch_idle: Vec<&'a str>,
    /// The scratch half of "who should this instruction go to", when there is
    /// anything on that list. Absent when there is not, so the caller is not
    /// handed a sentence about an empty set.
    #[serde(skip_serializing_if = "Option::is_none")]
    scratch_reuse: Option<String>,
    /// What to do about `hidden`, when there is anything to do. Absent
    /// otherwise, so its presence alone is the signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
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
    /// Which repository this agent is working on.
    ///
    /// `cwd` used to be the only hint, and it is a bad one: a session holding a
    /// worktree lease has the *worktree* as its cwd, not the checkout, so a
    /// router grouping by directory put the same project's agents in two
    /// groups.
    project: Option<&'a str>,
    /// The work it belongs to, by title. `None` for a `delegate`d run, which
    /// belongs to no work on purpose.
    work: Option<&'a str>,
    /// How long it has been silent, when it has been marked stalled.
    ///
    /// The field that stops the router starting a second agent beside a wedged
    /// one without knowing it: `status` says `running` for a stalled run,
    /// because it *is* running.
    stalled_for_ms: Option<i64>,
    /// Running and not stalled — that is, actually getting on with something.
    ///
    /// Derived rather than left to the caller, because "running" and "busy"
    /// came apart the moment a stall could be marked, and every reader
    /// recomputing the difference is a reader that can get it wrong.
    busy: bool,
    /// Able to take a new instruction this moment.
    ///
    /// Not the negation of `busy`. A stalled agent is not busy and is not free
    /// either — `continue_agent` will reach a session that has stopped
    /// answering — and neither is one that never reported a session id, which
    /// `continue_agent` refuses outright. So this is the narrow thing it says
    /// it is: a run that finished its last turn cleanly and still has a session
    /// to resume. That is what an idle engineer looks like in this system,
    /// because a run's process exits when its turn ends.
    free: bool,
    /// A scratch session — a one-shot errand the assistant started, with no
    /// work and no checkout.
    ///
    /// Carried on the row because `free` alone reads the same for a scratch
    /// session and an engineer, and what to do about the two is different
    /// enough that the caller has to be able to tell them apart. `idle` and
    /// `reuse` are the engineer answer and exclude these; `scratch_idle` and
    /// `scratch_reuse` are theirs.
    scratch: bool,
}

/// Where a schedule runs when nobody said. The server's own directory, which is
/// the daemon's — falling back to the home directory rather than failing, since
/// a deleted cwd should not stop a schedule being armed.
fn working_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| default_cwd())
}

fn as_json<T: Serialize>(value: &T) -> Result<String, ToolError> {
    serde_json::to_string_pretty(value)
        .map_err(|e| ToolError::Refused(format!("could not render the answer: {e}")))
}

/// The one project a `project` argument names, or a refusal that says what to
/// do instead.
///
/// Three tools take a repository by name — `project_switch`, `ask_manager` and
/// `project_untrack` — and every one of them has to answer the same two
/// awkward cases. A name that matches nothing gets the catalog back, because
/// the usual cause is a name that is nearly right and a bare "not found" makes
/// the model guess again rather than pick from what is there. A name that
/// matches two checkouts is refused with both paths, because picking would act
/// on a repository nobody chose, and which one was meant is a question only
/// Reljod can answer.
///
/// The consequence of getting it wrong differs per tool, so each one supplies
/// its own last sentence: `absent_because` finishes "no project called `x`",
/// and `ambiguous_advice` finishes the list of candidates. Everything before
/// those is the same sentence three times over, which is the reason this is one
/// function.
///
/// It searches archived entries too, by way of [`Store::projects_by_name`].
/// Naming a project outright is an instruction, not a guess, and refusing to
/// find the one just named because it is untracked would be obtuse — it is also
/// the only way `project_untrack` could report "already untracked" rather than
/// "no such project".
fn named_project(
    store: &Store,
    wanted: &str,
    absent_because: &str,
    ambiguous_advice: &str,
) -> Result<crate::projects::Project, ToolError> {
    // A path first, because `projects.path` is UNIQUE and a name is not.
    //
    // Two checkouts with the same directory name are catalogued under one name
    // and neither can be addressed: the bare name matches both and is refused,
    // and there is nothing else to say. Observed by running it — the router
    // asked which `web` was meant, the answer came back, and every attempt to
    // act on it was refused, including two that named the full path. The path
    // is the one answer that cannot be ambiguous, so it is worth accepting from
    // a caller that has it.
    //
    // Tried before the name rather than after, so a project whose *name* is a
    // path cannot shadow the real checkout at that path.
    let by_path = wanted.starts_with('/').then(|| store.project_at_path(wanted));
    if let Some(Ok(Some(project))) = by_path {
        return Ok(project);
    }
    let found = store
        .projects_by_name(wanted)
        .map_err(|e| ToolError::Refused(format!("could not search the catalog: {e}")))?;
    match found.as_slice() {
        [only] => Ok(only.clone()),
        [] => {
            let known: Vec<String> = store
                .projects(false)
                .map_err(|e| ToolError::Refused(format!("could not read the catalog: {e}")))?
                .into_iter()
                .map(|p| p.name)
                .collect();
            Err(ToolError::Refused(format!(
                "no project called `{wanted}`{absent_because}. The catalog has: {}. \
                 Use project_add if this is somewhere new.",
                if known.is_empty() {
                    "(nothing yet)".to_string()
                } else {
                    known.join(", ")
                }
            )))
        }
        several => {
            let candidates = several
                .iter()
                .map(|p| format!("{} ({})", p.name, p.path.display()))
                .collect::<Vec<_>>()
                .join(", ");
            // The way out is named, because without it this refusal is a dead
            // end: the bare name is the only thing either project answers to,
            // so a caller told "pick one" has nothing to pick *with*. A path is
            // unique and both are printed just above.
            Err(ToolError::Refused(format!(
                "`{wanted}` is the name of {} projects — {candidates}. {ambiguous_advice} \
                 Once he has said which, pass that project's full path here instead of its \
                 name — a path names exactly one project and a shared name cannot.",
                several.len()
            )))
        }
    }
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

/// An array of strings, refusing anything that is not one.
///
/// A model that passes a single string where a list was asked for has offered
/// one option, and reading it as one costs nothing; anything else is refused
/// rather than coerced, because the alternative is a rail row labelled with a
/// fragment of JSON that nobody can answer.
fn string_list(args: &Value, key: &str) -> Result<Vec<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => Ok(s.trim().to_string()),
                other => Err(ToolError::BadParams(format!(
                    "`{key}` must be a list of strings, and one of them is {other}"
                ))),
            })
            .filter(|s| !matches!(s, Ok(s) if s.is_empty()))
            .collect(),
        Some(other) => Err(ToolError::BadParams(format!(
            "`{key}` must be a list of strings, not {other}"
        ))),
    }
}

/// How much the agent says a card matters, refusing a word nobody defined.
///
/// [`Importance::parse`] takes anything and answers `normal`, which is right
/// for a row read back from a database written by a newer build and wrong for
/// an argument: a model that writes `urgent` and is silently given `normal` has
/// been told nothing, and will write it again.
fn importance(args: &Value) -> Result<Option<Importance>, ToolError> {
    let Some(word) = opt_str(args, "importance") else {
        return Ok(None);
    };
    match word.trim().to_ascii_lowercase().as_str() {
        "low" => Ok(Some(Importance::Low)),
        "normal" => Ok(Some(Importance::Normal)),
        "high" => Ok(Some(Importance::High)),
        other => Err(ToolError::BadParams(format!(
            "`{other}` is not an importance — {}",
            IMPORTANCE_IDS.join(", ")
        ))),
    }
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
    const READ_ONLY_TOOLS: [&str; 18] = [
        "list_agents",
        "schedule_list",
        "goal_list",
        "recall",
        "related",
        "conversations",
        "conversation_search",
        // Reading your own inbox and looking at who is here costs nothing and
        // hides nothing.
        "roster",
        "read_messages",
        // Raising a card writes, and still belongs here. It spends no money,
        // starts no process and costs no peer a turn — it is a sentence
        // addressed to Reljod — and the most confined agent is the one whose
        // choices most need to be visible enough to overrule.
        "record_decision",
        "ask_question",
        "request_secret",
        // Knowing where you may write is the precondition for not writing
        // where you may not.
        "list_roots",
        // Reading the catalog is how an instruction that names no project gets
        // resolved at all. It reveals which repositories exist, which the most
        // confined agent already learns from its own roots.
        "project_list",
        "project_current",
        // Reading a board is reading. It is also the one question a manager
        // must be able to answer without spending an engineer's turn on it.
        "work_board",
        // Reporting writes a row and nothing else: no agent, no branch, no
        // money. It has to be here rather than a level up for a second reason —
        // an engineer is spawned at whatever `open_work` was asked for, and one
        // sent to read at `read_only` that cannot report is one whose work is
        // invisible.
        "complete_task",
        // Stopping a turn *is* consequential — it throws away work — so this
        // sitting at the floor wants explaining. The gate on it is identity
        // rather than access: only a run whose conversation origin says
        // `assistant` may call it, and a doorman is spawned with the smallest
        // toolbox anything gets. Putting it a level up would mean handing a
        // doorman the power to start agents in order to reach the power to
        // stop one, which is the wrong trade in both directions.
        "interrupt_main",
    ];
    // Writing to a peer spends a turn of theirs, which is money now — the same
    // line `delegate` sits on. What stops it running away is not the access
    // level but the bounds in `team`: depth, budget, and a deadline on a wait.
    const DELEGATE_TOOLS: [&str; 17] = [
        "delegate",
        // Resuming a manager starts an agent, so it sits on the same line as
        // every other tool that does. It is main's usual verb, and main runs at
        // `Orchestrate`, which is above this.
        "ask_manager",
        "continue_agent",
        "stop_agent",
        "send_message",
        "reply",
        "ask",
        "handoff",
        // Opening a work starts an agent, so it sits on `delegate`'s line: the
        // thing you least want an unattended run to hold is the power to create
        // more unattended runs.
        "open_work",
        // These two cut a branch and remove a directory. Every other tool on
        // the rail's side only writes rows.
        "claim_worktree",
        "release_worktree",
        // Both change what a *later* instruction resolves to, which is the
        // quiet kind of consequential: a run that mis-switches the project
        // sends the next thing Reljod says to the wrong repository.
        "project_switch",
        "project_add",
        // The third of the same kind, and the one with the largest quiet
        // consequence: an untracked project is not inferrable, so the next
        // vague sentence about that repository resolves somewhere else.
        "project_untrack",
        // Writing a fact spends nothing and wakes nobody, so it fails the test
        // the level above is for. What it does change is what Jod believes, and
        // `delegate` is where the design already puts "started by a person or
        // by main": nothing built from outside reaches this far, because
        // `Service::spawn_from_untrusted` caps it at read-only first.
        "remember",
        // Writing a plan starts no agent, and it still belongs here: it decides
        // what agents are started to do and which files each of them may write,
        // and completing the last task on the board it writes closes the work.
        "plan_work",
        // Linking a stack rewrites the base branch of every pull request it
        // names, which is a visible change to open work — the same line
        // `claim_worktree` sits on, and for the same reason.
        "stack_pull_requests",
    ];
    const ORCHESTRATE_TOOLS: [&str; 4] = [
        "schedule_create",
        "schedule_pause",
        "schedule_run_now",
        "goal_create",
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

    /// `stop_agent` stops a branch of the fleet, not one process, and the
    /// description is the only place a model finds that out.
    ///
    /// Both halves have to be there and they fail differently. Without the
    /// reach, a model that wants a whole project stopped calls this once per
    /// worker and races its own cascade. Without the exception, a model reading
    /// "and every agent working under it" has every reason to believe stopping
    /// the main chat stops the machine, and will avoid the call that is
    /// actually safe.
    #[test]
    fn stop_agent_says_it_stops_the_agents_underneath() {
        let stop = catalogue()
            .into_iter()
            .find(|t| t.name == "stop_agent")
            .expect("`stop_agent` is not in the catalogue");
        let said = stop.description.to_lowercase();
        assert!(
            said.contains("under it") || said.contains("underneath"),
            "`stop_agent` does not say it reaches the agents below the one \
             named, so a caller will stop them one at a time: {said}"
        );
        assert!(
            said.contains("delegate"),
            "`stop_agent` does not say that delegated agents are included, \
             which is the reach that surprises: {said}"
        );
        assert!(
            said.contains("main chat"),
            "`stop_agent` does not name the one conversation that does not \
             cascade, so its reach reads as unbounded: {said}"
        );
        assert!(
            !said.contains("keeps going") && !said.contains("keeps running"),
            "`stop_agent` still tells the caller a delegated run survives, \
             which stopped being true when the stop began to cascade: {said}"
        );
    }

    /// The resume has to advertise the same reach the stop does.
    ///
    /// A model that knows `stop_agent` takes a whole branch down, and does not
    /// know `continue_agent` brings it back, has one obvious way to restore a
    /// fleet: delegate every worker again by hand. That would start second
    /// copies alongside the ones this brings back, which is the exact failure
    /// `Store::claim_cascaded_stop` exists to prevent from the other direction.
    #[test]
    fn continue_agent_says_it_brings_the_stopped_workers_back() {
        let go_on = catalogue()
            .into_iter()
            .find(|t| t.name == "continue_agent")
            .expect("`continue_agent` is not in the catalogue");
        let said = go_on.description.to_lowercase();
        assert!(
            said.contains("started again") || said.contains("brought back"),
            "`continue_agent` does not say the workers come back with it: {said}"
        );
        assert!(
            said.contains("under it") || said.contains("underneath"),
            "`continue_agent` does not say which agents come back — the ones \
             that were working under this one: {said}"
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
        // The belt to the braces: `remember` needs delegate, and a read-only
        // agent calling it anyway must leave the store exactly as it found it.
        let store = Arc::new(Store::in_memory().unwrap());
        let server = Server::new(Jod::with_store(store.clone())).with_access(ToolAccess::ReadOnly);
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

    /// **Regression: the console said `auto` and the errand asked to run `curl`.**
    ///
    /// A one-shot delegated from a chat in `auto` came up in `accept_edits`,
    /// which auto-approves file edits and nothing else, so the first `Bash` call
    /// raised a card and the run sat there waiting. Nothing had been capped: the
    /// orchestrator passed `"permission": "accept_edits"` itself, being careful
    /// with a decision that was not its own.
    #[tokio::test]
    async fn a_delegated_child_runs_in_the_mode_the_operator_chose() {
        for ceiling in PermissionPolicy::ALL {
            let server = Server::new(Jod::with_store(Arc::new(Store::in_memory().unwrap())))
                .with_access(ToolAccess::Orchestrate)
                .with_max_permission(ceiling);
            assert_eq!(
                server.child_permission(),
                ceiling,
                "a run delegated under a {ceiling:?} console did not inherit it"
            );
        }
    }

    /// The class, closed rather than discouraged. A description saying "leaving
    /// it out is almost always right" is a request, and the model that started
    /// this had one in front of it.
    #[tokio::test]
    async fn no_tool_that_starts_a_run_lets_the_model_name_a_mode() {
        for tool in catalogue() {
            if !matches!(tool.name, "delegate" | "open_work") {
                continue;
            }
            assert!(
                tool.schema["properties"].get("permission").is_none(),
                "`{}` offers the model a permission to choose",
                tool.name
            );
        }
    }

    /// The catalog is what a model resolves an unnamed instruction against, so
    /// an entry whose checkout has been deleted is a resolution target it will
    /// pick and cannot work in. It used to be handed over looking exactly like
    /// a healthy one, and the model's next move — opening work there — is
    /// reported as running before it fails somewhere else entirely.
    #[tokio::test]
    async fn project_list_says_when_a_projects_checkout_has_gone_missing() {
        let dir = std::env::temp_dir().join(format!("jod-mcp-stale-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let store = Arc::new(Store::in_memory().unwrap());
        store
            .add_project(crate::projects::NewProject::at(&dir).named("ephemeral-proj"))
            .unwrap();
        let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::ReadOnly);

        let listed = call(&server, "project_list", json!({})).await;
        let healthy: Value = serde_json::from_str(&said(&listed)).unwrap();
        assert_eq!(
            healthy[0]["path_usable"],
            json!(true),
            "a checkout that is really there was reported as unusable: {healthy}"
        );

        std::fs::remove_dir_all(&dir).unwrap();

        let listed = call(&server, "project_list", json!({})).await;
        let stale: Value = serde_json::from_str(&said(&listed)).unwrap();
        assert_eq!(
            stale[0]["path_usable"],
            json!(false),
            "a deleted checkout is still offered as a healthy resolution target: {stale}"
        );
        let trouble = stale[0]["path_trouble"]
            .as_str()
            .unwrap_or_else(|| panic!("no explanation of what is wrong: {stale}"));
        assert!(
            trouble.contains(&dir.display().to_string()),
            "the explanation does not say which path is gone: {trouble}"
        );
        assert_eq!(
            stale[0]["name"], "ephemeral-proj",
            "the entry was removed rather than flagged: {stale}"
        );
    }

    /// Untracking is the whole loop: the tool changes the state, and the two
    /// surfaces that decide what Reljod sees agree that it did.
    ///
    /// Asserted through `project_list` and `forest` rather than by reading the
    /// column, because the column was already right before any of this — what
    /// was missing was a caller that could set it and a fleet that honoured it.
    #[tokio::test]
    async fn untracking_a_project_takes_it_off_the_catalog_and_off_the_fleet() {
        let dir = std::env::temp_dir().join(format!("jod-mcp-untrack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let store = Arc::new(Store::in_memory().unwrap());
        let project = store
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        store
            .create_work_in("port the parser", Some(&project.id))
            .unwrap();
        let server = Server::new(Jod::with_store(store.clone())).with_access(ToolAccess::Delegate);

        assert_eq!(store.projects(false).unwrap().len(), 1);
        assert!(!store.forest().unwrap().is_empty(), "nothing to untrack yet");

        let answer = call(&server, "project_untrack", json!({ "project": "tetris" })).await;
        assert!(!is_error_result(&answer), "{answer}");
        let said: Value = serde_json::from_str(&said(&answer)).unwrap();
        assert_eq!(said["already_untracked"], json!(false), "{said}");
        assert!(
            said["said"].as_str().unwrap().contains("jod project restore"),
            "the answer does not say how to put it back: {said}"
        );

        assert!(
            store.projects(false).unwrap().is_empty(),
            "still in the catalog after being untracked"
        );
        assert!(
            store.forest().unwrap().is_empty(),
            "still on the fleet after being untracked: {:?}",
            store.forest().unwrap()
        );
        assert_eq!(
            store.projects_by_name("tetris").unwrap().len(),
            1,
            "the row was deleted rather than untracked — naming it must still find it"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Untracking something already untracked is an answer, not a refusal. The
    /// state after the call is the state that was asked for, and reporting a
    /// failure would invite a retry that cannot go better — but it has to say
    /// which of the two happened, or a model relaying "done" for a no-op is how
    /// the wrong checkout ends up believed untracked.
    #[tokio::test]
    async fn untracking_an_already_untracked_project_says_so_rather_than_refusing() {
        let dir = std::env::temp_dir().join(format!("jod-mcp-untrack-twice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let store = Arc::new(Store::in_memory().unwrap());
        store
            .add_project(crate::projects::NewProject::at(&dir).named("tetris"))
            .unwrap();
        let server = Server::new(Jod::with_store(store.clone())).with_access(ToolAccess::Delegate);

        call(&server, "project_untrack", json!({ "project": "tetris" })).await;
        let answer = call(&server, "project_untrack", json!({ "project": "tetris" })).await;

        assert!(!is_error_result(&answer), "a no-op was reported as a failure: {answer}");
        let said: Value = serde_json::from_str(&said(&answer)).unwrap();
        assert_eq!(said["already_untracked"], json!(true), "{said}");
        assert!(
            said["said"].as_str().unwrap().contains("already"),
            "the answer reads as a fresh untrack: {said}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two checkouts answering to one name is the case where picking is worse
    /// than refusing: untracking either takes a repository off the fleet, and
    /// which one was meant is a question only Reljod can answer.
    #[tokio::test]
    async fn untracking_a_name_two_projects_answer_to_refuses_and_names_both() {
        let root = std::env::temp_dir().join(format!("jod-mcp-untrack-ambig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (one, two) = (root.join("a/proj"), root.join("b/proj"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();

        let store = Arc::new(Store::in_memory().unwrap());
        store.add_project(crate::projects::NewProject::at(&one).named("proj")).unwrap();
        store.add_project(crate::projects::NewProject::at(&two).named("proj")).unwrap();
        let server = Server::new(Jod::with_store(store.clone())).with_access(ToolAccess::Delegate);

        let answer = call(&server, "project_untrack", json!({ "project": "proj" })).await;
        assert!(is_error_result(&answer), "one of the two was picked: {answer}");
        let text = said(&answer);
        assert!(text.contains(&one.display().to_string()), "{text}");
        assert!(text.contains(&two.display().to_string()), "{text}");
        assert_eq!(
            store.projects(false).unwrap().len(),
            2,
            "a refused call still untracked something"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A read-only agent cannot take a repository off Reljod's fleet. The tool
    /// sits on `delegate` with `project_switch` and `project_add`, and the gate
    /// is the dispatcher rather than the advertised list.
    #[tokio::test]
    async fn untracking_is_refused_below_delegate() {
        let store = Arc::new(Store::in_memory().unwrap());
        let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::ReadOnly);

        // A forbidden call is a protocol error rather than a tool result, so
        // there is no `content` to read — the refusal is the whole answer.
        let answer = call(&server, "project_untrack", json!({ "project": "tetris" })).await;
        let refusal = answer["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("a read-only agent untracked a project: {answer}"));
        assert!(refusal.contains("delegate"), "{refusal}");
    }

    /// **Regression: `open_work` opened everything in `accept_edits`.**
    ///
    /// It asked for `accept_edits` outright and capped that, so the operator's
    /// own mode never reached the work. In `auto` that cost the whole feature:
    /// headless Claude Code in `accept_edits` has nobody to answer a permission
    /// prompt, so a background session refused `git init`, `pnpm -v` and every
    /// other mutation while the console still said `auto`.
    ///
    /// Asserted on the choice rather than through a spawn, because the spawn
    /// needs a supervisor this test has no business requiring.
    #[tokio::test]
    async fn open_work_inherits_the_operators_mode_rather_than_pinning_accept_edits() {
        for ceiling in PermissionPolicy::ALL {
            let server = Server::new(Jod::with_store(Arc::new(Store::in_memory().unwrap())))
                .with_access(ToolAccess::Orchestrate)
                .with_max_permission(ceiling);
            assert_eq!(
                server.child_permission(),
                ceiling,
                "a work opened under a {ceiling:?} console did not inherit it"
            );
        }
    }

    /// Inheriting is not a way to climb either, and now by construction: the
    /// value handed to a child is the ceiling itself, so there is nothing to
    /// cap and nothing that can exceed it.
    #[tokio::test]
    async fn a_child_never_holds_more_than_the_console_that_started_it() {
        for ceiling in PermissionPolicy::ALL {
            let server = Server::new(Jod::with_store(Arc::new(Store::in_memory().unwrap())))
                .with_access(ToolAccess::Orchestrate)
                .with_max_permission(ceiling);
            assert!(
                permits(ceiling, server.child_permission()),
                "a {ceiling:?} console handed out more than it holds"
            );
        }
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

    /// A server over a store already holding runs, so a listing can be asked
    /// about a box with a history rather than one that has launched nothing.
    ///
    /// Each entry of `spec` is a name prefix, how many runs to write under it,
    /// and the status they carry. Runs are written oldest first, in the order
    /// given. A running run is given this test process's own group id, because
    /// `rehydrate` demotes a run that claims to be running while its process
    /// group is gone — without a live group every seeded agent would come back
    /// `failed` and the ordering under test would never be exercised.
    fn server_with_runs(spec: &[(&str, usize, &str)]) -> Server {
        let store = Arc::new(Store::in_memory().unwrap());
        let alive_group = std::process::id();
        let mut created_at_ms = 0i64;
        for (prefix, count, status) in spec {
            for n in 0..*count {
                let id = format!("{prefix}-{n:03}");
                let summary = crate::service::AgentSummary {
                    id: id.clone(),
                    name: id.clone(),
                    harness: HarnessKind::ClaudeCode,
                    harness_label: "Claude Code".into(),
                    status: AgentStatus::parse(status).expect("a status the test spelled right"),
                    cwd: "/tmp".into(),
                    model: None,
                    permission: PermissionPolicy::default(),
                    pid: None,
                    pgid: None,
                    process_alive: false,
                    watch_command: String::new(),
                    created_at_ms,
                    session_id: None,
                    usage: Default::default(),
                    event_count: 0,
                    last_message: None,
                };
                store
                    .save_run(&crate::store::StoredRun {
                        id: id.clone(),
                        name: id,
                        harness: "claude_code".into(),
                        status: (*status).to_string(),
                        cwd: "/tmp".into(),
                        session_id: None,
                        pid: Some(alive_group),
                        pgid: Some(alive_group),
                        created_at_ms,
                        summary: serde_json::to_value(&summary).unwrap(),
                    })
                    .unwrap();
                created_at_ms += 1;
            }
        }
        Server::new(Jod::with_store(store)).with_access(ToolAccess::ReadOnly)
    }

    /// The parsed reply of one `list_agents` call.
    async fn listing(server: &Server, args: Value) -> Value {
        serde_json::from_str(&said(&call(server, "list_agents", args).await))
            .expect("list_agents answers with JSON")
    }

    /// The run ids in a listing, in the order they were returned.
    fn listed_ids(page: &Value) -> Vec<String> {
        page["agents"]
            .as_array()
            .expect("a listing carries an `agents` array")
            .iter()
            .map(|a| a["run_id"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn listing_agents_on_a_jod_that_has_launched_nothing_returns_no_agents() {
        let page = listing(&server(ToolAccess::ReadOnly), json!({})).await;
        assert_eq!(page["agents"], json!([]));
        assert_eq!(page["total"], 0);
        assert_eq!(page["hidden"], 0);
        assert!(
            page["note"].is_null(),
            "nothing was left out, so nothing should be said about it: {page}"
        );
    }

    #[tokio::test]
    async fn a_listing_says_how_many_agents_the_limit_left_out() {
        let server = server_with_runs(&[("done", 25, "completed")]);
        let page = listing(&server, json!({})).await;
        assert_eq!(page["returned"], 20, "the default limit still caps the page");
        assert_eq!(page["total"], 25, "every run on the box is counted");
        assert_eq!(page["hidden"], 5, "five runs did not fit and must be owned up to");
        let note = page["note"].as_str().unwrap_or_default();
        assert!(
            note.contains('5'),
            "a caller that only reads the note must still learn five were hidden: {note:?}"
        );
        assert!(
            note.contains("limit"),
            "the note has to name the way out, which is a bigger `limit`: {note:?}"
        );
    }

    /// The truncation does not bite where it looks like it should. Running
    /// agents sort ahead of finished ones, so the three oldest runs on a busy
    /// box still lead the page as long as they are the only ones running. What
    /// the cap actually drops is the oldest *finished* agents.
    #[tokio::test]
    async fn running_agents_keep_their_place_on_the_page_however_old_they_are() {
        let server = server_with_runs(&[("live", 3, "running"), ("done", 97, "completed")]);
        let page = listing(&server, json!({})).await;
        assert_eq!(
            listed_ids(&page)[..3],
            ["live-002", "live-001", "live-000"].map(String::from)[..3],
            "the oldest three runs are the running ones and must lead, newest of them first: \
             {page}"
        );
        assert_eq!(page["hidden"], 80, "the eighty oldest finished runs were dropped");
    }

    /// The case the cap genuinely hides a running agent in: more agents running
    /// at once than the limit returns.
    #[tokio::test]
    async fn a_running_agent_is_only_dropped_when_more_are_running_than_fit() {
        let server = server_with_runs(&[("live", 21, "running")]);
        let page = listing(&server, json!({})).await;
        let ids = listed_ids(&page);
        assert_eq!(ids.len(), 20);
        assert!(
            !ids.iter().any(|id| id == "live-000"),
            "the oldest of twenty-one running agents is the one that falls off: {ids:?}"
        );
        assert_eq!(page["hidden"], 1);
    }

    /// A bigger `limit` has to actually reach further back. The listing reads
    /// runs out of the database before it pages them, and reading back a fixed
    /// few hundred while the caller asked for more meant an older agent stayed
    /// invisible at every limit — the note would have pointed at a way out that
    /// did not work.
    #[tokio::test]
    async fn a_bigger_limit_reaches_agents_older_than_the_default_read_back() {
        let server = server_with_runs(&[("live", 3, "running"), ("done", 202, "completed")]);
        let page = listing(&server, json!({ "limit": 1000 })).await;
        let ids = listed_ids(&page);
        assert_eq!(ids.len(), 205, "asking for everything must return everything");
        assert!(
            ids.iter().any(|id| id == "live-000"),
            "the oldest running agent is exactly the one worth finding"
        );
        assert_eq!(page["hidden"], 0, "nothing was left out, so nothing is claimed to be");
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

    /// One stored run with everything `continue_agent` reads set by hand: how
    /// it ended, whether it recorded a session id, and what permission it was
    /// launched under. `server_with_runs` seeds no session id and leaves the
    /// permission at `bypass`, which sits above the default ceiling — and
    /// those are the two things that have to be held out of the way before the
    /// status question is reached at all.
    fn server_with_one_run(
        status: AgentStatus,
        session_id: Option<&str>,
        permission: PermissionPolicy,
    ) -> Server {
        let store = Arc::new(Store::in_memory().unwrap());
        let summary = crate::service::AgentSummary {
            id: "run-000".into(),
            name: "run-000".into(),
            harness: HarnessKind::ClaudeCode,
            harness_label: "Claude Code".into(),
            status,
            cwd: "/tmp".into(),
            model: None,
            permission,
            pid: None,
            pgid: None,
            process_alive: false,
            watch_command: String::new(),
            created_at_ms: 0,
            session_id: session_id.map(str::to_string),
            usage: Default::default(),
            event_count: 0,
            last_message: None,
        };
        store
            .save_run(&crate::store::StoredRun {
                id: "run-000".into(),
                name: "run-000".into(),
                harness: "claude_code".into(),
                status: status.as_str().to_string(),
                cwd: "/tmp".into(),
                session_id: session_id.map(str::to_string),
                pid: None,
                pgid: None,
                created_at_ms: 0,
                summary: serde_json::to_value(&summary).unwrap(),
            })
            .unwrap();
        Server::new(Jod::with_store(store)).with_access(ToolAccess::Orchestrate)
    }

    /// **The case O3 was filed on and could never reach.**
    ///
    /// The person who found this killed a run, confirmed its session id was
    /// still recorded, and asked the main chat to continue it. It was refused,
    /// but only because `jod run`'s default permission sat above that server's
    /// ceiling — the permission check answered first and the status was never
    /// consulted. This is the same case with the permission moved out of the
    /// way, which was the only thing standing between a dead session and a
    /// resume.
    #[tokio::test]
    async fn continuing_a_killed_run_is_refused_and_the_refusal_names_the_status() {
        let server = server_with_one_run(
            AgentStatus::Killed,
            Some("sess-abc"),
            // At the ceiling, not above it, so a refusal here can only be
            // about the status.
            PermissionPolicy::Ask,
        );
        let answer = call(
            &server,
            "continue_agent",
            json!({ "run_id": "run-000", "prompt": "carry on" }),
        )
        .await;
        assert!(is_error_result(&answer), "a killed run was resumed: {answer}");
        let said = said(&answer);
        assert!(
            said.contains("killed"),
            "the refusal does not say the run was killed: {said}"
        );
        assert!(
            !said.contains("ceiling"),
            "refused over the permission again, so the status is still unchecked: {said}"
        );
        assert!(
            said.contains("delegate"),
            "the refusal does not say how to start fresh instead: {said}"
        );
    }

    /// A run whose harness exited badly is the same problem wearing a different
    /// word, and it is the commoner one: `rehydrate` marks any run `failed`
    /// whose process group has gone, so most dead sessions arrive here as
    /// `failed` rather than as `killed`.
    #[tokio::test]
    async fn continuing_a_failed_run_is_refused_and_the_refusal_names_the_status() {
        let server =
            server_with_one_run(AgentStatus::Failed, Some("sess-abc"), PermissionPolicy::Ask);
        let answer = call(
            &server,
            "continue_agent",
            json!({ "run_id": "run-000", "prompt": "carry on" }),
        )
        .await;
        assert!(is_error_result(&answer), "a failed run was resumed: {answer}");
        assert!(
            said(&answer).contains("failed"),
            "the refusal does not say the run failed: {}",
            said(&answer)
        );
    }

    /// The other half, and the half that matters more: the gate must turn away
    /// the two statuses it is for and nothing else. A run that finished is the
    /// ordinary target of a follow-up, and a run still working is how a second
    /// instruction reaches an agent mid-task; refusing either would break the
    /// tool for the case it exists to serve.
    ///
    /// Asserted on the decision rather than through the tool, because letting a
    /// continue through means spawning a supervisor, which a unit test has no
    /// business doing.
    #[test]
    fn only_a_killed_or_failed_run_is_turned_away_by_the_status_gate() {
        for dead in [AgentStatus::Killed, AgentStatus::Failed] {
            let refusal = refusal_to_continue("run-000", dead)
                .unwrap_or_else(|| panic!("{dead:?} was let through"));
            assert!(
                refusal.contains(dead.as_str()),
                "a refusal that does not name the status: {refusal}"
            );
            assert!(
                refusal.contains("delegate") && refusal.contains("open_work"),
                "a refusal that does not say what to do instead: {refusal}"
            );
        }
        for alive in [AgentStatus::Running, AgentStatus::Completed] {
            assert_eq!(
                refusal_to_continue("run-000", alive),
                None,
                "{alive:?} is a run a follow-up should reach"
            );
        }
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

    // ---- the rail --------------------------------------------------------

    use crate::cards::{Delivery, Query};

    /// A run with a conversation behind it, which is what a card is raised
    /// against. Deliberately *not* a team member: the ordinary run that raises
    /// a card is nobody's teammate, and that is the case this fixture holds.
    fn working(access: ToolAccess) -> (Arc<Store>, Server, String) {
        let store = Arc::new(Store::in_memory().unwrap());
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
            .unwrap()
            .id;
        // The spawn path records the instruction as the conversation's first
        // user turn, keyed to the run — which is the join `raiser` reads.
        store
            .append_prompt(&conversation, "run-1", "port the parser")
            .unwrap();
        let server = Server::new(Jod::with_store(store.clone()))
            .with_access(access)
            .for_run("run-1");
        (store, server, conversation)
    }

    fn only_card(store: &Store, conversation: &str) -> crate::cards::Card {
        let mut all = store
            .cards(&Query {
                conversation_id: Some(conversation.to_string()),
                ..Query::default()
            })
            .unwrap();
        assert_eq!(all.len(), 1, "expected exactly one card, got {all:?}");
        all.remove(0)
    }

    #[tokio::test]
    async fn a_decision_arrives_with_the_alternatives_it_was_chosen_over() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        let answer = call(
            &server,
            "record_decision",
            json!({
                "title": "chat DB",
                "chosen": "sqlite",
                "options": ["sqlite", "postgres"],
                "why": "no server to run",
                "importance": "high"
            }),
        )
        .await;
        assert!(!is_error_result(&answer), "{answer}");

        let card = only_card(&store, &conversation);
        assert_eq!(card.kind, CardKind::Decision);
        assert_eq!(card.title, "chat DB");
        assert_eq!(card.chosen.as_deref(), Some("sqlite"));
        assert_eq!(card.options, vec!["sqlite", "postgres"]);
        assert_eq!(card.importance, Importance::High);
        assert_eq!(card.source, Source::Mcp);
        assert_eq!(card.run_id.as_deref(), Some("run-1"));
        assert!(
            !card.blocking,
            "a decision has already been taken, so nothing is waiting on it"
        );
    }

    /// A decision offered without the option that is in force cannot be
    /// restated by pressing a digit, which is the whole point of the row.
    #[tokio::test]
    async fn a_decision_whose_choice_is_missing_from_its_options_still_offers_it() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        call(
            &server,
            "record_decision",
            json!({ "title": "chat DB", "chosen": "sqlite", "options": ["postgres"] }),
        )
        .await;
        assert_eq!(
            only_card(&store, &conversation).options,
            vec!["sqlite", "postgres"]
        );
    }

    /// D2: emission never blocks the agent.
    #[tokio::test]
    async fn an_ordinary_question_returns_a_card_id_without_waiting_for_anybody() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        let started = std::time::Instant::now();
        let said: Value = serde_json::from_str(&said(
            &call(
                &server,
                "ask_question",
                json!({ "question": "which port?", "context": "the webhook receiver" }),
            )
            .await,
        ))
        .unwrap();

        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "an unblocking question waited for an answer"
        );
        assert_eq!(said["status"], "open");
        let card = only_card(&store, &conversation);
        assert_eq!(said["card_id"], card.id);
        assert_eq!(card.kind, CardKind::Question);
        assert!(!card.blocking);
    }

    #[tokio::test]
    async fn a_blocking_question_comes_back_with_the_answer_when_one_is_given() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        // Reljod, answering while the run waits.
        let answering = store.clone();
        let of = conversation.clone();
        tokio::spawn(async move {
            loop {
                let open = answering
                    .cards(&Query {
                        conversation_id: Some(of.clone()),
                        ..Query::default()
                    })
                    .unwrap();
                if let Some(card) = open.first() {
                    answering
                        .answer_card(card.id, Some("8443"), Some("same everywhere"))
                        .unwrap();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });

        let said: Value = serde_json::from_str(&said(
            &call(
                &server,
                "ask_question",
                json!({ "question": "which port?", "blocking": true, "wait_seconds": 10 }),
            )
            .await,
        ))
        .unwrap();
        assert_eq!(said["status"], "answered", "{said}");
        assert_eq!(said["chosen"], "8443");
        assert_eq!(said["answer"], "same everywhere");
        assert!(store.card(said["card_id"].as_i64().unwrap()).unwrap().is_some());
    }

    /// An answer handed back as a tool result and *also* delivered later reads
    /// to the agent as a second instruction, and the work gets done twice.
    #[tokio::test]
    async fn an_answer_taken_by_a_waiting_run_is_not_delivered_to_it_again() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        let answering = store.clone();
        let of = conversation.clone();
        tokio::spawn(async move {
            loop {
                let open = answering
                    .cards(&Query {
                        conversation_id: Some(of.clone()),
                        ..Query::default()
                    })
                    .unwrap();
                if let Some(card) = open.first() {
                    answering.answer_card(card.id, None, Some("8443")).unwrap();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        call(
            &server,
            "ask_question",
            json!({ "question": "which port?", "blocking": true, "wait_seconds": 10 }),
        )
        .await;

        assert!(
            store.pending_for(&conversation).unwrap().is_empty(),
            "the answer is queued for a turn as well as returned, so it arrives twice"
        );
        let card = only_card_of_status(&store, &conversation, Status::Answered);
        assert_eq!(card.delivery, Delivery::Delivered);
    }

    fn only_card_of_status(
        store: &Store,
        conversation: &str,
        status: Status,
    ) -> crate::cards::Card {
        let mut all = store
            .cards(&Query {
                conversation_id: Some(conversation.to_string()),
                status: Some(status),
                ..Query::default()
            })
            .unwrap();
        assert_eq!(all.len(), 1, "expected one card, got {all:?}");
        all.remove(0)
    }

    /// The property the deadline exists for: nobody is at the desk, and the run
    /// carries on rather than holding a session open all night.
    #[tokio::test]
    async fn a_blocking_question_gives_up_at_its_deadline_and_leaves_the_card_open() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        let started = std::time::Instant::now();
        let said: Value = serde_json::from_str(&said(
            &call(
                &server,
                "ask_question",
                json!({ "question": "which port?", "blocking": true, "wait_seconds": 1 }),
            )
            .await,
        ))
        .unwrap();
        assert_eq!(said["status"], "open", "{said}");
        assert!(said["note"].as_str().unwrap().contains("blocked"));
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        // Giving up waiting is not withdrawing the question.
        assert!(only_card(&store, &conversation).is_open());
    }

    #[tokio::test]
    async fn a_wait_can_never_be_asked_to_last_longer_than_the_card_cap() {
        // Asserted on the constants rather than by waiting half an hour.
        const { assert!(CARD_ANSWER_DEADLINE_SECS <= MAX_CARD_WAIT_SECS) };
        assert_eq!(
            (MAX_CARD_WAIT_SECS + 10_000).clamp(1, MAX_CARD_WAIT_SECS),
            MAX_CARD_WAIT_SECS
        );
    }

    #[tokio::test]
    async fn a_dismissed_question_tells_the_agent_to_decide_for_itself() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        let dismissing = store.clone();
        let of = conversation.clone();
        tokio::spawn(async move {
            loop {
                let open = dismissing
                    .cards(&Query {
                        conversation_id: Some(of.clone()),
                        ..Query::default()
                    })
                    .unwrap();
                if let Some(card) = open.first() {
                    dismissing.dismiss_card(card.id).unwrap();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        let said: Value = serde_json::from_str(&said(
            &call(
                &server,
                "ask_question",
                json!({ "question": "which port?", "blocking": true, "wait_seconds": 10 }),
            )
            .await,
        ))
        .unwrap();
        assert_eq!(said["status"], "dismissed", "{said}");
        assert!(store.pending_for(&conversation).unwrap().is_empty());
    }

    // ---- secrets ---------------------------------------------------------

    /// D3, at the one place a value could enter the model's world through a
    /// tool: there is no argument for one, and an obvious attempt is refused
    /// out loud rather than quietly dropped.
    #[tokio::test]
    async fn requesting_a_secret_refuses_every_argument_a_value_could_arrive_in() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        for smuggled in ["value", "secret", "secret_value", "token"] {
            let answer = call(
                &server,
                "request_secret",
                json!({ "name": "STRIPE_API_KEY", "hint": "the live key", smuggled: "sk-live-1234567890" }),
            )
            .await;
            assert_eq!(error_code(&answer), INVALID_PARAMS, "{smuggled}: {answer}");
        }
        assert!(
            store
                .cards(&Query {
                    conversation_id: Some(conversation),
                    ..Query::default()
                })
                .unwrap()
                .is_empty(),
            "a call carrying a value raised a card, so the value is now in the database"
        );
    }

    #[tokio::test]
    async fn a_secret_card_carries_a_name_and_a_scope_and_no_value() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        let said: Value = serde_json::from_str(&said(
            &call(
                &server,
                "request_secret",
                json!({ "name": "STRIPE_API_KEY", "hint": "the live key, from the dashboard" }),
            )
            .await,
        ))
        .unwrap();
        assert_eq!(said["secret"], "STRIPE_API_KEY");
        // Said to the model in as many words, because "a missing key is a
        // blocked ending" is the whole of E3.S5 and it has to arrive at the
        // moment the agent notices the key is missing.
        assert!(said["note"].as_str().unwrap().contains("blocked"), "{said}");

        let card = only_card(&store, &conversation);
        assert_eq!(card.kind, CardKind::Secret);
        assert_eq!(card.secret_name.as_deref(), Some("STRIPE_API_KEY"));
        assert_eq!(card.secret_scope.as_deref(), Some("conversation"));
        assert!(card.blocking);
        assert_eq!(card.importance, Importance::High);
    }

    /// A name a shell would drop makes a credential that is present behave
    /// exactly like one that is missing, so it is refused at the call.
    #[tokio::test]
    async fn a_secret_name_that_is_not_a_legal_variable_is_refused() {
        let (_, server, _) = working(ToolAccess::ReadOnly);
        let answer = call(
            &server,
            "request_secret",
            json!({ "name": "stripe-api-key", "hint": "the live key" }),
        )
        .await;
        assert_eq!(error_code(&answer), INVALID_PARAMS);
        assert!(answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("environment variable"));
    }

    // ---- who a card belongs to -------------------------------------------

    /// The rail's version of the property sender identity exists for: a card
    /// lands on the rail of the run that raised it, whatever the arguments say.
    #[tokio::test]
    async fn a_card_is_raised_against_the_calling_run_whatever_the_arguments_say() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        let elsewhere = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp/other", None)
            .unwrap()
            .id;
        call(
            &server,
            "record_decision",
            json!({
                "title": "chat DB",
                "chosen": "sqlite",
                // Every spelling an agent might try. None is read, and this is
                // the reason no card tool has an argument for it.
                "conversation_id": elsewhere,
                "conversation": elsewhere,
                "run_id": "run-somebody-else"
            }),
        )
        .await;

        assert_eq!(only_card(&store, &conversation).run_id.as_deref(), Some("run-1"));
        assert!(
            store
                .cards(&Query {
                    conversation_id: Some(elsewhere),
                    ..Query::default()
                })
                .unwrap()
                .is_empty(),
            "an agent named another conversation and Jod put a card on it"
        );
    }

    #[tokio::test]
    async fn a_session_with_no_run_behind_it_raises_nothing() {
        let store = Arc::new(Store::in_memory().unwrap());
        let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::Orchestrate);
        let answer = call(
            &server,
            "record_decision",
            json!({ "title": "chat DB", "chosen": "sqlite" }),
        )
        .await;
        assert!(is_error_result(&answer), "{answer}");
        assert!(said(&answer).contains("no run behind it"), "{}", said(&answer));
    }

    /// A run that is nobody's teammate is the ordinary case for a card, and the
    /// refusal `caller` gives the bus must not reach the rail.
    #[tokio::test]
    async fn a_run_on_no_team_can_still_say_what_it_decided() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        assert!(
            server.caller().is_err(),
            "this fixture is only meaningful while the run is nobody's teammate"
        );
        let answer = call(
            &server,
            "record_decision",
            json!({ "title": "chat DB", "chosen": "sqlite" }),
        )
        .await;
        assert!(!is_error_result(&answer), "{answer}");
        assert_eq!(only_card(&store, &conversation).title, "chat DB");
    }

    // ---- works and roots -------------------------------------------------

    #[tokio::test]
    async fn a_session_can_read_where_it_may_write_and_where_it_may_not() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        store
            .add_root(&conversation, crate::roots::NewRoot::reading("/tmp"))
            .unwrap();
        store
            .add_root(&conversation, crate::roots::NewRoot::lease("/tmp/worktree"))
            .unwrap();

        let seen: Value =
            serde_json::from_str(&said(&call(&server, "list_roots", json!({})).await)).unwrap();
        let rows = seen.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["writable"], false);
        assert_eq!(rows[1]["writable"], true);
        assert_eq!(rows[1]["origin"], "lease");
    }

    /// A work opened in whatever directory the daemon happens to have been
    /// started in is a run editing something nobody meant, so this is refused
    /// rather than defaulted.
    #[tokio::test]
    async fn opening_a_work_with_no_checkout_and_no_root_to_inherit_one_from_is_refused() {
        let (_, server, _) = working(ToolAccess::Delegate);
        let answer = call(
            &server,
            "open_work",
            json!({ "instruction": "port the parser" }),
        )
        .await;
        assert!(is_error_result(&answer), "{answer}");
        assert!(said(&answer).contains("checkout"), "{}", said(&answer));
    }

    #[tokio::test]
    async fn opening_a_work_cannot_give_its_session_more_of_jod_than_the_caller_holds() {
        let (_, server, _) = working(ToolAccess::Delegate);
        let answer = call(
            &server,
            "open_work",
            json!({
                "instruction": "port the parser",
                "checkout": "/tmp",
                "tools": "orchestrate"
            }),
        )
        .await;
        assert!(is_error_result(&answer), "{answer}");
        assert!(said(&answer).contains("exceeds"), "{}", said(&answer));
    }

    // ---- managers ---------------------------------------------------------

    mod managers {
        use super::*;
        use crate::projects::NewProject;

        /// A catalogued project on a real directory, plus a store.
        pub(super) fn with_project(dir: &str) -> (Arc<Store>, crate::projects::Project) {
            let store = Arc::new(Store::in_memory().unwrap());
            std::fs::create_dir_all(dir).unwrap();
            let project = store
                .add_project(NewProject::at(dir).named("tetris"))
                .unwrap();
            (store, project)
        }

        /// A run bound to a conversation, which is what `raiser` reads.
        pub(super) fn run_in(store: &Store, conversation: &str, run_id: &str) {
            store.append_prompt(conversation, run_id, "do the thing").unwrap();
        }

        /// A real detached process group, so a run recorded as `running` still
        /// reads as running after `Jod::rehydrate`.
        ///
        /// `rehydrate` probes the pgid and corrects a `running` row with no
        /// live group to `failed` — rightly, since that is a supervisor that
        /// died without saying so. A fixture with `pgid: None` therefore comes
        /// back `failed`, and every assertion about a *working* agent would be
        /// made against one that is not.
        fn a_living_group() -> u32 {
            use std::os::unix::process::CommandExt;
            let mut cmd = std::process::Command::new("/bin/sleep");
            cmd.arg("60")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // SAFETY: `setsid` is async-signal-safe, which is the only
            // requirement on code running between `fork` and `exec`.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            cmd.spawn()
                .expect("could not spawn a test process group")
                .id()
        }

        pub(super) fn kill_group(pgid: u32) {
            unsafe { libc::kill(-(pgid as i32), libc::SIGKILL) };
            let mut status: libc::c_int = 0;
            unsafe { libc::waitpid(pgid as i32, &mut status, 0) };
        }

        /// A running row that `Jod::rehydrate` can actually load back.
        ///
        /// The whole `AgentSummary` and not a stub: `rehydrate` deserialises
        /// `runs.summary` into one and *skips the row* when it cannot, so a
        /// thin fixture produces an empty fleet and a test that proves nothing.
        pub(super) fn running_run(store: &Store, run_id: &str, name: &str) -> u32 {
            let pgid = a_living_group();
            let summary = crate::service::AgentSummary {
                id: run_id.into(),
                name: name.into(),
                harness: HarnessKind::ClaudeCode,
                harness_label: "claude-code".into(),
                status: crate::service::AgentStatus::Running,
                cwd: "/tmp".into(),
                model: None,
                permission: PermissionPolicy::AcceptEdits,
                pid: Some(pgid),
                pgid: Some(pgid),
                process_alive: true,
                watch_command: crate::service::watch_command(run_id),
                created_at_ms: 0,
                session_id: None,
                usage: Default::default(),
                event_count: 0,
                last_message: None,
            };
            store
                .save_run(&crate::store::StoredRun {
                    id: run_id.into(),
                    name: name.into(),
                    harness: "claude-code".into(),
                    status: "running".into(),
                    cwd: "/tmp".into(),
                    session_id: None,
                    pid: Some(pgid),
                    pgid: Some(pgid),
                    created_at_ms: 0,
                    summary: serde_json::to_value(&summary).unwrap(),
                })
                .unwrap();
            pgid
        }

        /// An engineer that finished its last turn — which is what an idle one
        /// looks like here, because a run's process exits when its turn ends.
        ///
        /// `session` is a parameter rather than always present because the two
        /// cases are genuinely different: a completed run with a session id can
        /// be continued, and one without cannot, however free it looks.
        pub(super) fn finished_run(store: &Store, run_id: &str, name: &str, session: Option<&str>) {
            let summary = crate::service::AgentSummary {
                id: run_id.into(),
                name: name.into(),
                harness: HarnessKind::ClaudeCode,
                harness_label: "claude-code".into(),
                status: crate::service::AgentStatus::Completed,
                cwd: "/tmp".into(),
                model: None,
                permission: PermissionPolicy::AcceptEdits,
                pid: None,
                pgid: None,
                process_alive: false,
                watch_command: crate::service::watch_command(run_id),
                created_at_ms: 0,
                session_id: session.map(str::to_string),
                usage: Default::default(),
                event_count: 0,
                last_message: None,
            };
            store
                .save_run(&crate::store::StoredRun {
                    id: run_id.into(),
                    name: name.into(),
                    harness: "claude-code".into(),
                    status: "completed".into(),
                    cwd: "/tmp".into(),
                    session_id: session.map(str::to_string),
                    pid: None,
                    pgid: None,
                    created_at_ms: 0,
                    summary: serde_json::to_value(&summary).unwrap(),
                })
                .unwrap();
        }

        /// The whole of a manager's first decision, answered by the tool rather
        /// than derived by the model.
        ///
        /// A manager is resumed for each instruction and has to re-establish
        /// who is around every time, so the cheaper this is to read the more
        /// reliably it happens. Reljod's rule is availability, not subject: an
        /// engineer of this project that is free takes the next instruction
        /// whatever it is about, because it already holds the checkout.
        #[tokio::test]
        async fn an_idle_engineer_is_named_as_the_one_to_reuse() {
            let store = Arc::new(Store::in_memory().unwrap());
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            store.append_prompt(&c, "run-1", "build the game").unwrap();
            finished_run(&store, "run-1", "engineer", Some("sess-1"));

            let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::ReadOnly);
            let page: Value =
                serde_json::from_str(&said(&call(&server, "list_agents", json!({})).await))
                    .unwrap();

            assert_eq!(page["agents"][0]["free"], json!(true), "{page}");
            assert_eq!(page["idle"], json!(["run-1"]), "{page}");
            let reuse = page["reuse"].as_str().unwrap();
            assert!(reuse.contains("run-1"), "{page}");
            assert!(
                reuse.contains("continue_agent"),
                "the answer has to name the tool call, not just the run: {page}"
            );
        }

        /// The trap this field exists to close. `busy` is false for a stalled
        /// agent *and* for an idle one, so a manager deriving availability from
        /// it would try to continue a session that has stopped answering.
        #[tokio::test]
        async fn a_stalled_agent_is_not_free_however_unbusy_it_looks() {
            let store = Arc::new(Store::in_memory().unwrap());
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            store.append_prompt(&c, "run-1", "build the game").unwrap();
            let pgid = running_run(&store, "run-1", "engineer");
            let now = chrono::Utc::now().timestamp_millis();
            store
                .watch_run(&crate::heartbeat::Heartbeat::starting(
                    "run-1",
                    crate::heartbeat::Watching::Run,
                    now,
                ))
                .unwrap();
            store
                .record_beat(&crate::heartbeat::Beat {
                    run_id: "run-1".into(),
                    last_seq: -1,
                    last_progress_ms: now - 60_000,
                    last_beat_ms: now,
                    stalled_since_ms: Some(now - 60_000),
                })
                .unwrap();

            let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::ReadOnly);
            let page: Value =
                serde_json::from_str(&said(&call(&server, "list_agents", json!({})).await))
                    .unwrap();
            kill_group(pgid);

            assert_eq!(page["agents"][0]["busy"], json!(false), "{page}");
            assert_eq!(
                page["agents"][0]["free"],
                json!(false),
                "not busy is not the same as free: {page}"
            );
            assert_eq!(page["idle"], json!([]), "{page}");
            assert!(
                page["reuse"].as_str().unwrap().contains("stalled"),
                "a wedged agent has to be named as wedged, not counted as busy: {page}"
            );
        }

        /// `continue_agent` refuses a run that never reported a session id, so
        /// offering one for reuse would be routing an instruction into a
        /// refusal.
        #[tokio::test]
        async fn an_agent_with_no_session_to_resume_is_not_offered_for_reuse() {
            let store = Arc::new(Store::in_memory().unwrap());
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            store.append_prompt(&c, "run-1", "build the game").unwrap();
            finished_run(&store, "run-1", "engineer", None);

            let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::ReadOnly);
            let page: Value =
                serde_json::from_str(&said(&call(&server, "list_agents", json!({})).await))
                    .unwrap();

            assert_eq!(page["agents"][0]["free"], json!(false), "{page}");
            assert_eq!(page["idle"], json!([]), "{page}");
        }

        /// And the case that genuinely does call for a second session.
        #[tokio::test]
        async fn a_project_whose_only_engineer_is_working_says_nothing_is_free() {
            let store = Arc::new(Store::in_memory().unwrap());
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            store.append_prompt(&c, "run-1", "build the game").unwrap();
            let pgid = running_run(&store, "run-1", "engineer");

            let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::ReadOnly);
            let page: Value =
                serde_json::from_str(&said(&call(&server, "list_agents", json!({})).await))
                    .unwrap();
            kill_group(pgid);

            assert_eq!(page["agents"][0]["busy"], json!(true), "{page}");
            assert_eq!(page["agents"][0]["free"], json!(false), "{page}");
            assert_eq!(page["idle"], json!([]), "{page}");
            let reuse = page["reuse"].as_str().unwrap();
            assert!(reuse.contains("nothing free"), "{page}");
            assert!(
                !reuse.contains("stalled"),
                "a healthy busy agent must not be reported as wedged: {page}"
            );
        }

        /// Reuse is decided on availability, not on subject. The brief used to
        /// say to continue "an agent already doing this", which a manager reads
        /// as a topical test — and so opens a cold session the moment an
        /// instruction changes subject, with an idle engineer sitting beside it
        /// holding the whole repository in its head.
        #[test]
        fn a_managers_brief_sends_a_new_instruction_to_whoever_is_free() {
            // The cap is a separate sentence and this test is about none of
            // it, so any non-zero one will do — `orchestrator` covers both arms.
            let said = crate::orchestrator::manager_preamble("tetris", 3);

            assert!(
                said.contains("free"),
                "the brief has to name availability as the test: {said}"
            );
            assert!(
                said.contains("`idle`") && said.contains("`reuse`"),
                "and point at the fields that answer it: {said}"
            );
            assert!(
                !said.contains("An agent already doing this"),
                "the topical rule is the thing being replaced: {said}"
            );
            let continues = said.find("`continue_agent`").expect("continue_agent named");
            let opens = said.find("`open_work`").expect("open_work named");
            assert!(
                continues < opens,
                "reuse has to be offered before opening something new, because the \
                 first tool a model reads is the one it reaches for: {said}"
            );
        }

        /// Check 12. A name that matches nothing refuses and lists what is
        /// known, rather than picking or saying a bare "not found" that makes
        /// the model guess again.
        #[tokio::test]
        async fn asking_an_unknown_projects_manager_refuses_and_names_what_is_known() {
            let dir = format!("/tmp/jod-mgr-unknown-{}", std::process::id());
            let (store, _) = with_project(&dir);
            let conversation = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            run_in(&store, &conversation, "run-1");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Orchestrate)
                .for_run("run-1");

            let answer = call(
                &server,
                "ask_manager",
                json!({ "project": "pacman", "instruction": "fix the tests" }),
            )
            .await;

            assert!(is_error_result(&answer), "{answer}");
            let said = said(&answer);
            assert!(said.contains("pacman"), "{said}");
            assert!(
                said.contains("tetris"),
                "the refusal has to say what the catalog does have: {said}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// Two projects answering to one name have two managers, so picking one
        /// would route the instruction into a repository nobody chose — where
        /// it reads as perfectly ordinary.
        #[tokio::test]
        async fn asking_an_ambiguous_projects_manager_refuses_and_names_both() {
            let base = format!("/tmp/jod-mgr-ambig-{}", std::process::id());
            let (a, b) = (format!("{base}/one/shared"), format!("{base}/two/shared"));
            std::fs::create_dir_all(&a).unwrap();
            std::fs::create_dir_all(&b).unwrap();
            let store = Arc::new(Store::in_memory().unwrap());
            store.add_project(NewProject::at(&a)).unwrap();
            store.add_project(NewProject::at(&b)).unwrap();
            let conversation = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            run_in(&store, &conversation, "run-1");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Orchestrate)
                .for_run("run-1");

            let answer = call(
                &server,
                "ask_manager",
                json!({ "project": "shared", "instruction": "fix the tests" }),
            )
            .await;

            assert!(is_error_result(&answer), "{answer}");
            let said = said(&answer);
            assert!(said.contains(&a), "{said}");
            assert!(said.contains(&b), "{said}");
            std::fs::remove_dir_all(&base).ok();
        }

        /// Refused at the tool boundary, not by prompt wording — the server
        /// resolves the calling run itself, so the caller cannot argue about
        /// its identity. And the refusal names the verb to use instead: a rule
        /// that only says no leaves the model guessing at what yes is.
        ///
        /// The verb it names has changed. It was `ask_manager` while main still
        /// routed; main routes nothing now, so what it is sent to is
        /// `ask_assistant` and the assistant decides whether a manager is what
        /// this needs.
        #[tokio::test]
        async fn open_work_from_the_main_chat_is_refused_and_names_ask_manager() {
            let store = Arc::new(Store::in_memory().unwrap());
            let main = store
                .main_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            run_in(&store, &main, "run-main");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Orchestrate)
                .for_run("run-main");

            let answer = call(
                &server,
                "open_work",
                json!({ "instruction": "port the parser", "checkout": "/tmp" }),
            )
            .await;

            assert!(is_error_result(&answer), "{answer}");
            let said = said(&answer);
            assert!(
                said.contains("ask_manager"),
                "the refusal has to say what yes looks like, or main spends a turn \
                 guessing: {said}"
            );
            assert!(!said.contains("ask_assistant"), "a verb that no longer exists: {said}");
            assert!(said.contains("not the main chat's to call"), "{said}");
        }

        /// The other half of check 13, and the reason it is a separate test: a
        /// refusal that fired for everybody would pass the test above while
        /// breaking every agent in the fleet.
        #[tokio::test]
        async fn open_work_from_a_session_that_is_not_main_is_not_refused_for_being_main() {
            let store = Arc::new(Store::in_memory().unwrap());
            store
                .main_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            let other = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            run_in(&store, &other, "run-2");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Orchestrate)
                .for_run("run-2");

            let answer = call(
                &server,
                "open_work",
                json!({ "instruction": "port the parser", "checkout": "/tmp" }),
            )
            .await;

            // It may still fail for an unrelated reason on a box with no
            // supervisor, which is fine and is why this asserts on the *reason*
            // rather than on success.
            let said = said(&answer);
            assert!(
                !said.contains("not the main chat's to call"),
                "a manager or an engineer must still be able to open work: {said}"
            );
        }

        /// A stalled run is refused by the tool, not merely discouraged.
        ///
        /// Reljod's decision was that a stalled session is marked and surfaced,
        /// never killed, and that the router treats it as not-continuable: say
        /// so, start a fresh session beside it, and leave the wedged one for
        /// him to stop. Both preambles say it, and nothing enforced it.
        ///
        /// Found by wedging a real engineer and giving its project another
        /// instruction: the manager called `continue_agent` on the stalled run
        /// and the tool allowed it. That does not resume the stuck process — it
        /// starts a second one on the same session — so the wedged one is left
        /// running and unnoticed, which is the state the mark exists to end.
        #[tokio::test]
        async fn continuing_a_stalled_run_is_refused_and_names_what_to_do_instead() {
            let store = Arc::new(Store::in_memory().unwrap());
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            run_in(&store, &c, "run-wedged");
            finished_run(&store, "run-wedged", "engineer", Some("a-session"));

            let server = Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::Delegate)
                .for_run("run-caller");

            // Healthy first, so the refusal below is a change and not a
            // constant. It may still fail for want of a supervisor, so this
            // asserts on the *reason*.
            let healthy = said(
                &call(
                    &server,
                    "continue_agent",
                    json!({ "run_id": "run-wedged", "prompt": "carry on" }),
                )
                .await,
            );
            assert!(
                !healthy.contains("is stalled"),
                "nothing is stalled yet: {healthy}"
            );

            let now = chrono::Utc::now().timestamp_millis();
            let mut hb = crate::heartbeat::Heartbeat::starting(
                "run-wedged",
                crate::heartbeat::Watching::Run,
                now,
            );
            hb.stalled_since_ms = Some(now - 35 * 60 * 1000);
            store.watch_run(&hb).unwrap();

            let said = said(
                &call(
                    &server,
                    "continue_agent",
                    json!({ "run_id": "run-wedged", "prompt": "carry on" }),
                )
                .await,
            );
            assert!(said.contains("is stalled"), "the tool refuses it: {said}");
            assert!(
                said.contains("open_work"),
                "and names the way forward: {said}"
            );
            assert!(
                said.contains("Reljod"),
                "and leaves the wedged one to him, per the decision: {said}"
            );
        }

        /// The roster must not offer a router as the agent to continue.
        ///
        /// Main and a manager both spend a turn deciding who does the work and
        /// then exit, leaving `completed` rows with session ids — which is
        /// exactly what "free" meant. So `list_agents` answered a manager
        /// looking for an engineer with main's own last turn, saying it
        /// "already holds this checkout" and to "prefer it for any instruction
        /// here". Seen repeatedly in one session, and declined every time only
        /// because the model reasoned past its own tool.
        #[tokio::test]
        async fn the_roster_does_not_offer_main_or_a_manager_as_an_engineer() {
            let dir = format!("/tmp/jod-roster-{}", std::process::id());
            let (store, project) = with_project(&dir);
            let main = store
                .main_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            let (manager, _) = store
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();

            // A finished turn each, with a session to resume — the shape that
            // used to read as an idle engineer.
            for (conversation, run) in [(&main, "run-main"), (&manager, "run-manager")] {
                run_in(&store, conversation, run);
                finished_run(&store, run, run, Some(&format!("{run}-session")));
            }

            let routers = store.router_run_ids().unwrap();
            assert!(routers.contains("run-main"), "main's own turn is a router");
            assert!(routers.contains("run-manager"), "so is a manager's");

            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Delegate)
                .for_run("run-manager");
            let answer = call(&server, "list_agents", json!({ "limit": 20 })).await;
            let said = said(&answer);

            assert!(
                !said.contains("`run-main` is free"),
                "main is not an engineer to hand work to: {said}"
            );
            assert!(
                !said.contains("`run-manager` is free"),
                "and neither is a manager: {said}"
            );
            assert!(
                said.contains("nothing to reuse") || said.contains("nothing free"),
                "with no engineer at all, the honest answer is that there is none: {said}"
            );

            std::fs::remove_dir_all(&dir).ok();
        }

        /// Two checkouts whose directories share a name are still each
        /// reachable, because a path is.
        ///
        /// Found by running it. Two repositories both called `web` were
        /// catalogued; the router correctly refused to guess and asked which
        /// one was meant; the answer came back — and then every way of acting
        /// on that answer was refused, including two attempts that named the
        /// full path. The catalog printed `web, web, …` and neither `web` could
        /// be addressed at all. A question nobody can answer is worse than one
        /// nobody asks.
        #[tokio::test]
        async fn a_project_whose_name_is_shared_is_still_reachable_by_its_path() {
            let base = format!("/tmp/jod-two-webs-{}", std::process::id());
            let one = format!("{base}/one/web");
            let two = format!("{base}/two/web");
            let store = Arc::new(Store::in_memory().unwrap());
            std::fs::create_dir_all(&one).unwrap();
            std::fs::create_dir_all(&two).unwrap();
            let first = store.add_project(NewProject::at(&one)).unwrap();
            let second = store.add_project(NewProject::at(&two)).unwrap();
            assert_eq!(first.name, second.name, "the premise: one name, two rows");

            // The bare name is refused, and now says what to do about it.
            let refusal = named_project(&store, &first.name, "", "Ask Reljod which.")
                .expect_err("a shared name cannot resolve");
            let ToolError::Refused(said) = refusal else {
                panic!("a shared name is refused, not failed");
            };
            assert!(
                said.contains("full path"),
                "the refusal has to name the way out: {said}"
            );

            // And the path reaches exactly the one asked for, both ways round.
            let got = named_project(&store, &one, "", "").expect("the first web, by path");
            assert_eq!(got.id, first.id);
            let got = named_project(&store, &two, "", "").expect("the second web, by path");
            assert_eq!(got.id, second.id);

            // A path nothing is catalogued at still falls through to the name
            // branch rather than resolving to something near it.
            assert!(
                named_project(&store, &format!("{base}/three/web"), "", "").is_err(),
                "an uncatalogued path is not a project",
            );

            std::fs::remove_dir_all(&base).ok();
        }

        /// A manager has no roots, and does not need to be told its own
        /// repository.
        ///
        /// Observed by running one: alpha's manager called `open_work` with no
        /// `checkout`, was refused with "this session has no roots of its own
        /// to inherit one from", and spent a second model turn supplying the
        /// path the store already had. A manager is created against its project
        /// and never adds a root, so that happened on every manager's first
        /// piece of work.
        #[tokio::test]
        async fn a_manager_opening_work_takes_the_checkout_from_its_project() {
            let dir = format!("/tmp/jod-mgr-checkout-{}", std::process::id());
            let (store, project) = with_project(&dir);
            store
                .main_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            let (manager, _) = store
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();
            assert!(
                store.roots(&manager).unwrap().is_empty(),
                "the premise: a manager has no roots of its own",
            );
            run_in(&store, &manager, "run-manager");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Delegate)
                .for_run("run-manager");

            // No `checkout` argument, which is the call that used to be refused.
            let answer = call(&server, "open_work", json!({ "instruction": "port the parser" })).await;

            // As above, this may still fail on a box with no supervisor, so it
            // asserts on the reason rather than on success.
            let said = said(&answer);
            assert!(
                !said.contains("no roots of its own"),
                "a manager knows its own repository: {said}"
            );

            std::fs::remove_dir_all(&dir).ok();
        }

        /// The route around the rule, closed, one layer down from where it used
        /// to be.
        ///
        /// A model that wants to help with something about a repository and has
        /// `delegate` in its hand will reach for it with the checkout as `cwd`
        /// rather than call `ask_manager`, and it feels entirely reasonable at
        /// the time. That caller was main; it is the assistant now, because
        /// main holds no `delegate` at all. The hole is the same hole and it
        /// moved with the verb.
        #[tokio::test]
        async fn delegate_at_a_known_projects_checkout_is_refused_from_the_assistant() {
            let dir = format!("/tmp/jod-asst-delegate-{}", std::process::id());
            let (store, _) = with_project(&dir);
            store
                .main_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            let assistant = store
                .open_assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            run_in(&store, &assistant, "run-assistant");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Delegate)
                .for_run("run-assistant");

            let answer = call(
                &server,
                "delegate",
                json!({ "prompt": "count the tests", "cwd": dir.clone() }),
            )
            .await;

            assert!(is_error_result(&answer), "{answer}");
            let said = said(&answer);
            assert!(said.contains("ask_manager"), "{said}");
            assert!(said.contains("tetris"), "{said}");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// And the honest case still works. Refusing every `delegate` from the
        /// assistant would leave it unable to answer "what's the weather in
        /// Manila" without opening a work, which is the thing this whole
        /// design is trying to stop.
        #[tokio::test]
        async fn delegate_somewhere_that_is_not_a_project_is_still_the_assistants_to_call() {
            let store = Arc::new(Store::in_memory().unwrap());
            store
                .main_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            let assistant = store
                .open_assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            run_in(&store, &assistant, "run-assistant");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Delegate)
                .for_run("run-assistant");

            let answer = call(
                &server,
                "delegate",
                json!({ "prompt": "what is the weather in Manila", "cwd": "/tmp" }),
            )
            .await;

            let said = said(&answer);
            assert!(
                !said.contains("ask_manager"),
                "a repo-less one-shot must not be routed into a manager: {said}"
            );
            assert!(
                !said.contains("not the main chat's to call"),
                "an assistant is not main, and the refusal that stops main must not \
                 catch it: {said}"
            );
        }

        /// Check 8. What a manager asks with, so it reads its own repository
        /// rather than the whole fleet.
        #[tokio::test]
        async fn list_agents_filtered_by_project_returns_only_that_projects_agents() {
            let base = format!("/tmp/jod-mgr-filter-{}", std::process::id());
            let (a, b) = (format!("{base}/tetris"), format!("{base}/pacman"));
            std::fs::create_dir_all(&a).unwrap();
            std::fs::create_dir_all(&b).unwrap();
            let store = Arc::new(Store::in_memory().unwrap());
            let tetris = store.add_project(NewProject::at(&a)).unwrap();
            let pacman = store.add_project(NewProject::at(&b)).unwrap();

            let mut groups = Vec::new();
            for (run, project) in [("run-t", &tetris), ("run-p", &pacman)] {
                let c = store
                    .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                    .unwrap()
                    .id;
                store.append_prompt(&c, run, "work").unwrap();
                store
                    .set_current_project(
                        &c,
                        Some(&project.id),
                        "work",
                        crate::projects::How::Human,
                        "test",
                    )
                    .unwrap();
                groups.push(running_run(&store, run, run));
            }

            let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::ReadOnly);
            let answer = call(&server, "list_agents", json!({ "project": "tetris" })).await;
            let page: Value = serde_json::from_str(&said(&answer)).unwrap();

            let ids: Vec<&str> = page["agents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["run_id"].as_str().unwrap())
                .collect();
            for pgid in groups {
                kill_group(pgid);
            }
            std::fs::remove_dir_all(&base).ok();

            assert_eq!(ids, vec!["run-t"], "{page}");
            assert_eq!(page["agents"][0]["project"], "tetris");
        }

        /// Check 7. The four fields the router needs, and the one that stops it
        /// starting a second agent beside a wedged one without knowing it.
        #[tokio::test]
        async fn list_agents_says_which_project_and_work_an_agent_is_on_and_whether_it_is_stuck() {
            let dir = format!("/tmp/jod-mgr-view-{}", std::process::id());
            let (store, project) = with_project(&dir);
            let work = store
                .create_work_in("port the parser", Some(&project.id))
                .unwrap();
            let c = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            store.attach_conversation(&c, &work.id, None, crate::works::Origin::Orchestrator)
                .unwrap();
            store.append_prompt(&c, "run-1", "work").unwrap();
            store
                .set_current_project(
                    &c,
                    Some(&project.id),
                    "work",
                    crate::projects::How::Human,
                    "test",
                )
                .unwrap();
            let pgid = running_run(&store, "run-1", "engineer");

            let server =
                Server::new(Jod::with_store(store.clone())).with_access(ToolAccess::ReadOnly);

            // Healthy first.
            let page: Value =
                serde_json::from_str(&said(&call(&server, "list_agents", json!({})).await))
                    .unwrap();
            let agent = &page["agents"][0];
            assert_eq!(agent["project"], "tetris", "{page}");
            assert_eq!(agent["work"], work.title, "{page}");
            assert_eq!(agent["stalled_for_ms"], Value::Null, "{page}");
            assert_eq!(agent["busy"], json!(true), "{page}");

            // Now mark it stalled, the way the sweep would.
            let now = chrono::Utc::now().timestamp_millis();
            store
                .watch_run(&crate::heartbeat::Heartbeat::starting(
                    "run-1",
                    crate::heartbeat::Watching::Run,
                    now,
                ))
                .unwrap();
            store
                .record_beat(&crate::heartbeat::Beat {
                    run_id: "run-1".into(),
                    last_seq: -1,
                    last_progress_ms: now - 60_000,
                    last_beat_ms: now,
                    stalled_since_ms: Some(now - 60_000),
                })
                .unwrap();

            let page: Value =
                serde_json::from_str(&said(&call(&server, "list_agents", json!({})).await))
                    .unwrap();
            let agent = &page["agents"][0];
            assert!(
                agent["stalled_for_ms"].as_i64().unwrap() >= 60_000,
                "{page}"
            );
            assert_eq!(
                agent["busy"],
                json!(false),
                "a stalled agent is still `running`, which is exactly why `busy` \
                 has to say otherwise: {page}"
            );
            assert_eq!(
                agent["status"], "running",
                "and `status` must keep telling the truth: {page}"
            );
            kill_group(pgid);
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    // ---- the assistant layer ----------------------------------------------
    //
    // Main hands every instruction to an assistant and comes straight back, and
    // the assistant is what decides where it goes. These tests are the two
    // halves of that: the refusals that stop main deciding anything, and the
    // reuse answer `list_agents` gives an assistant about the scratch sessions
    // underneath it.

    mod assistant {
        use super::managers::{finished_run, kill_group, run_in, running_run};
        use super::*;
        use crate::store::SCRATCH_REUSE_WINDOW_MINUTES_KEY;

        /// A store with a main chat and one run inside it, which is what
        /// `caller_is_main` reads.
        fn main_chat() -> (Arc<Store>, String) {
            let store = Arc::new(Store::in_memory().unwrap());
            let main = store
                .main_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            run_in(&store, &main, "run-main");
            (store, main)
        }

        fn main_server(store: &Arc<Store>) -> Server {
            Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::Orchestrate)
                .for_run("run-main")
        }

        /// A scratch conversation hanging under main, with one run in it.
        ///
        /// Hung under main on purpose: `scratch_reuse_candidates` walks down
        /// from the pinned conversation, so a scratch row that belongs to
        /// nobody is not a candidate however recent it is.
        fn scratch_under(store: &Store, parent: &str, run_id: &str) -> String {
            let conversation = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            store.mark_ephemeral(&conversation).unwrap();
            store.set_conversation_parent(&conversation, parent).unwrap();
            run_in(store, &conversation, run_id);
            conversation
        }

        /// Push a conversation's last activity into the past, so a window can
        /// be shown to exclude it without a test that waits an hour.
        fn last_active_at(store: &Store, conversation: &str, at_ms: i64) {
            store
                .write(|tx| {
                    tx.execute(
                        "UPDATE conversations SET updated_at_ms = ?2 WHERE id = ?1",
                        rusqlite::params![conversation, at_ms],
                    )?;
                    Ok(())
                })
                .unwrap();
        }

        fn page(answer: &Value) -> Value {
            serde_json::from_str(&said(answer)).expect("list_agents answers with JSON")
        }

        /// Every `delegate` is tagged `scratch`, which is what makes the roles
        /// panel do anything at all.
        ///
        /// The machinery Epic C built reads [`SpawnRequest::role`], and a
        /// request that carries none reads no row and changes nothing. So a
        /// person could set `scratch` to a cheap model, watch a lookup start,
        /// and see it launch on the frontier model anyway — a settings screen
        /// wired to nothing, which is worse than one that is not there.
        ///
        /// Asserted on the request rather than on a launched process: what
        /// `delegate` decides is now a function of its own, so this needs no
        /// harness on the machine and no model call.
        #[tokio::test]
        async fn a_delegated_run_is_tagged_as_scratch() {
            let store = Arc::new(Store::in_memory().unwrap());
            let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::Delegate);

            let req = server
                .delegate_request(&json!({ "prompt": "what is the weather in Manila" }))
                .expect("an ordinary delegate");

            assert_eq!(req.role, Some(Role::Scratch));
        }

        /// And an explicit harness argument outranks the row.
        ///
        /// `SpawnRequest::harness` is a `HarnessKind` rather than an `Option`,
        /// so "the caller asked for Claude Code" and "the caller asked for
        /// nothing" arrive at `apply_role` as the same value — and the roles
        /// table sits exactly between them. The call site is the last place
        /// that still knows the difference, so dropping the tag here is how the
        /// top rung of the precedence stays the top rung.
        #[tokio::test]
        async fn naming_a_harness_on_delegate_outranks_the_scratch_role() {
            let store = Arc::new(Store::in_memory().unwrap());
            let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::Delegate);

            let req = server
                .delegate_request(&json!({ "prompt": "look it up", "harness": "open_code" }))
                .expect("an explicit harness");

            assert_eq!(req.harness, HarnessKind::OpenCode);
            assert_eq!(
                req.role, None,
                "the `scratch` row would be free to change the harness back"
            );
        }

        /// Check 25. An empty `roles` table changes no spawn.
        ///
        /// The promise the whole of Epic C rests on: a machine whose owner
        /// never opens the panel must behave exactly as it did before the panel
        /// existed. Asserted by running the real resolution over a real store
        /// with no rows in it and comparing the request with itself.
        #[tokio::test]
        async fn an_empty_roles_table_changes_nothing_about_a_delegated_spawn() {
            let store = Arc::new(Store::in_memory().unwrap());
            let server = Server::new(Jod::with_store(store.clone())).with_access(ToolAccess::Delegate);

            let before = server
                .delegate_request(&json!({ "prompt": "what is the weather in Manila" }))
                .expect("an ordinary delegate");
            assert!(
                store.role_list().unwrap().is_empty(),
                "the premise: nobody has configured anything"
            );

            let mut after = before.clone();
            crate::service::apply_role(&store, &mut after);

            assert_eq!(after.harness, before.harness);
            assert_eq!(after.model, before.model);
            assert_eq!(after.effort, before.effort);
            assert_eq!(after.permission, before.permission);
        }

        /// The assistant is the one layer with a built-in, and this is it.
        ///
        /// Nothing is configured — `roles` is empty — and the spawn still comes
        /// out on AGY's cheap model, because reading one message and answering
        /// "can this wait" is not work a frontier model does better and it
        /// happens every time Reljod types into a busy chat.
        #[tokio::test]
        async fn the_assistant_runs_on_its_built_in_when_nothing_is_configured() {
            let store = Store::in_memory().unwrap();
            let mut req = SpawnRequest {
                role: Some(crate::harness::Role::Assistant),
                ..SpawnRequest::default()
            };
            assert!(store.role_list().unwrap().is_empty(), "the premise");

            crate::service::apply_role(&store, &mut req);

            assert_eq!(req.harness, HarnessKind::Agy);
            assert_eq!(req.model.as_deref(), Some("gpt-oss-120b-medium"));
        }

        /// And no other layer gets one, because no other layer has work small
        /// enough for the model to stop mattering.
        #[tokio::test]
        async fn no_layer_but_the_assistant_has_a_built_in() {
            let store = Store::in_memory().unwrap();
            for role in crate::harness::Role::ALL {
                if role == crate::harness::Role::Assistant {
                    continue;
                }
                let before = SpawnRequest {
                    role: Some(role),
                    ..SpawnRequest::default()
                };
                let mut after = before.clone();
                crate::service::apply_role(&store, &mut after);
                assert_eq!(after.harness, before.harness, "{role:?}");
                assert_eq!(after.model, before.model, "{role:?}");
            }
        }

        /// **The built-in is a pair, and moving the harness leaves the model
        /// behind.** A model name belongs to exactly one harness.
        ///
        /// Found by running it rather than by reading it: with the two halves
        /// applied independently, a row moving the assistant to Claude Code
        /// still handed it `--model gpt-oss-120b-medium`, and the run came back
        /// "There's an issue with the selected model" — a whole spawn spent on
        /// a sentence, and nothing in the suite would have read it.
        #[tokio::test]
        async fn a_row_that_moves_the_assistant_does_not_take_the_built_in_model_with_it() {
            let store = Store::in_memory().unwrap();
            store
                .role_set("assistant", crate::store::RoleField::Harness, Some("claude_code"))
                .unwrap();
            let mut req = SpawnRequest {
                role: Some(crate::harness::Role::Assistant),
                ..SpawnRequest::default()
            };

            crate::service::apply_role(&store, &mut req);

            assert_eq!(req.harness, HarnessKind::ClaudeCode, "the row wins");
            assert_eq!(
                req.model, None,
                "and the model is left to Claude Code, which is the only thing \
                 that knows its own names"
            );
        }

        /// The row still wins outright when it names both.
        #[tokio::test]
        async fn a_row_naming_both_outranks_the_built_in_entirely() {
            let store = Store::in_memory().unwrap();
            store
                .role_set("assistant", crate::store::RoleField::Harness, Some("claude_code"))
                .unwrap();
            store
                .role_set("assistant", crate::store::RoleField::Model, Some("haiku"))
                .unwrap();
            let mut req = SpawnRequest {
                role: Some(crate::harness::Role::Assistant),
                ..SpawnRequest::default()
            };

            crate::service::apply_role(&store, &mut req);

            assert_eq!(req.harness, HarnessKind::ClaudeCode);
            assert_eq!(req.model.as_deref(), Some("haiku"));
        }

        /// Main's own verb for a repository is back, and this is the test that
        /// used to say it was gone.
        ///
        /// It is inverted rather than deleted, because "main may reach a
        /// manager" is the whole of what lifting the refusal means, and a rule
        /// removed with no test in its place is a rule that quietly comes back.
        ///
        /// It asks for a project that does not exist, so the answer comes back
        /// without starting a manager. What is asserted is *which* refusal
        /// arrives: "no project called that" is proof the call reached the
        /// lookup, which is as far as it can get without a repository on disk.
        #[tokio::test]
        async fn ask_manager_from_mains_run_reaches_the_project_lookup() {
            let (store, _) = main_chat();
            let server = main_server(&store);

            let answer = call(
                &server,
                "ask_manager",
                json!({ "project": "tetris", "instruction": "fix the tests" }),
            )
            .await;

            let said = said(&answer);
            assert!(
                !said.contains("not the main chat's to call"),
                "main is still refused the verb it routes with: {said}"
            );
            assert!(
                said.contains("no project"),
                "the call has to reach the project lookup, which is the furthest it \
                 goes without a repository on disk: {said}"
            );
        }

        /// The same, through the other verb. Two tests rather than one because
        /// a refusal left in `delegate` alone would pass the test above while
        /// leaving main unable to start a one-shot.
        #[tokio::test]
        async fn delegate_from_mains_run_is_not_refused_for_being_main() {
            let (store, _) = main_chat();
            let server = main_server(&store);

            let answer = call(
                &server,
                "delegate",
                json!({ "prompt": "what is the weather in Manila", "cwd": "/tmp" }),
            )
            .await;

            let said = said(&answer);
            assert!(
                !said.contains("not the main chat's to call"),
                "main is still refused the verb it runs one-shots with: {said}"
            );
        }

        /// Check 6. And the layer below is not caught by it.
        ///
        /// The half that matters: a refusal keyed on something broader than
        /// identity — the access level, say — would pass both tests above and
        /// leave the assistant unable to do the only job it has.
        ///
        /// It asks for a project that does not exist, so the answer comes back
        /// without starting a manager. What is being asserted is which refusal
        /// arrives, and "no project called that" is proof the identity rule let
        /// the call through to the lookup.
        #[tokio::test]
        async fn ask_manager_from_an_assistants_run_is_not_refused_for_being_main() {
            let (store, _) = main_chat();
            let assistant = store
                .open_assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            run_in(&store, &assistant, "run-assistant");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Delegate)
                .for_run("run-assistant");

            let said = said(
                &call(
                    &server,
                    "ask_manager",
                    json!({ "project": "nothing-is-called-this", "instruction": "fix the tests" }),
                )
                .await,
            );

            assert!(
                !said.contains("not the main chat's to call"),
                "the assistant is refused the one verb it exists to call: {said}"
            );
            assert!(
                said.contains("no project"),
                "the call reached the project lookup, which is what proves it was \
                 not stopped at the identity gate: {said}"
            );
        }

        /// Stopping a turn belongs to the assistant standing at a door, and to
        /// nobody else.
        ///
        /// **The gate is identity, not access.** A doorman runs at
        /// `ToolAccess::ReadOnly`, so every read-only agent in the fleet can
        /// *see* this tool — which is the right trade, because putting it
        /// behind `Delegate` would mean handing a doorman the power to start
        /// agents in order to reach the power to stop one. What keeps it safe
        /// is that the caller's conversation origin has to say `assistant`, and
        /// that is written by `open_assistant_conversation` rather than by
        /// anything the model can pass.
        #[tokio::test]
        async fn interrupt_main_from_anything_but_an_assistant_is_refused() {
            let (store, _) = main_chat();
            let other = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            run_in(&store, &other, "run-other");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::ReadOnly)
                .for_run("run-other");

            let answer = call(
                &server,
                "interrupt_main",
                json!({ "run_id": "run-main", "reason": "he changed his mind" }),
            )
            .await;

            assert!(is_error_result(&answer), "{answer}");
            let said = said(&answer);
            assert!(said.contains("belongs to the assistant"), "{said}");
            // A refusal that only says no leaves the model guessing at what yes
            // is, and the guess here would be to try again.
            assert!(said.contains("`stop_agent`"), "{said}");
        }

        /// And main may not stop itself with it either.
        ///
        /// A chat that can end its own turn is a chat that can end itself
        /// mid-sentence, and the key for stopping a turn you are watching is
        /// Escape — which costs no model call at all.
        #[tokio::test]
        async fn interrupt_main_from_the_main_chat_is_refused() {
            let (store, _) = main_chat();
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Orchestrate)
                .for_run("run-main");

            let answer = call(
                &server,
                "interrupt_main",
                json!({ "run_id": "run-main", "reason": "stopping myself" }),
            )
            .await;

            assert!(is_error_result(&answer), "{answer}");
            assert!(said(&answer).contains("Escape key"), "{}", said(&answer));
        }

        /// An assistant gets past the identity gate, and is stopped by the run
        /// id instead.
        ///
        /// The half that matters: a gate keyed on something broader than
        /// identity would pass both tests above and leave a doorman unable to
        /// do the only thing it exists for. Naming a run that does not exist is
        /// as far as this can go without a supervisor, and *which* refusal
        /// comes back is the proof.
        #[tokio::test]
        async fn interrupt_main_from_an_assistants_run_reaches_the_run_lookup() {
            let (store, _) = main_chat();
            let assistant = store
                .open_assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            run_in(&store, &assistant, "run-assistant");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::ReadOnly)
                .for_run("run-assistant");

            let answer = call(
                &server,
                "interrupt_main",
                json!({ "run_id": "run-that-never-was", "reason": "he changed his mind" }),
            )
            .await;

            let said = said(&answer);
            assert!(
                !said.contains("belongs to the assistant"),
                "the doorman is refused the one verb it exists to call: {said}"
            );
            assert!(
                said.contains("no run `run-that-never-was`"),
                "the call has to reach the run lookup, which is what proves it was \
                 not stopped at the identity gate: {said}"
            );
        }

        /// A stop with nothing said is a turn that ends and reads as a crash.
        #[tokio::test]
        async fn interrupt_main_refuses_to_stop_a_turn_without_saying_why() {
            let (store, _) = main_chat();
            let assistant = store
                .open_assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            run_in(&store, &assistant, "run-assistant");
            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::ReadOnly)
                .for_run("run-assistant");

            let answer = call(
                &server,
                "interrupt_main",
                json!({ "run_id": "run-main", "reason": "   " }),
            )
            .await;

            // A bad argument rather than a refusal, so it comes back as a
            // protocol error and not as a tool result.
            assert_eq!(error_code(&answer), -32602, "{answer}");
            assert!(
                answer["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("why his turn stopped")),
                "{answer}"
            );
        }

        /// An assistant's `project_switch` reaches the main chat.
        ///
        /// Its own conversation is opened for one instruction and thrown away,
        /// so a sticky pointer written there is discarded by the end of the
        /// turn that wrote it — and the whole value of the pointer is that the
        /// *next* instruction inherits what this one resolved. Main used to
        /// hold the routing decision and set this itself; the decision moved
        /// down a layer and there is nowhere down here for a pointer to live.
        ///
        /// Without this, an assistant that correctly works out Reljod meant a
        /// different repository is right once and forgotten, and the next
        /// dictated sentence inherits the stale answer. That failure is
        /// invisible: the instruction lands in another repository's manager and
        /// reads as perfectly ordinary there.
        #[tokio::test]
        async fn an_assistants_project_switch_lands_on_the_main_chat() {
            let dir = format!("/tmp/jod-asst-switch-{}", std::process::id());
            let (store, project) = super::managers::with_project(&dir);
            let main = store
                .main_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            let assistant = store
                .open_assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
                .unwrap();
            run_in(&store, &assistant, "run-assistant");
            let server = Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::Delegate)
                .for_run("run-assistant");

            let answer = call(
                &server,
                "project_switch",
                json!({ "project": "tetris", "reason": "he named the tetris thing" }),
            )
            .await;
            assert!(!is_error_result(&answer), "{answer}");

            assert_eq!(
                store.current_project(&main).unwrap().map(|p| p.id),
                Some(project.id.clone()),
                "the main chat never learned what the assistant resolved, so the next \
                 instruction inherits the stale project"
            );
            // And the assistant's own row agrees, so `project_current` inside
            // this same turn does not contradict the call that just succeeded.
            assert_eq!(
                store.current_project(&assistant).unwrap().map(|p| p.id),
                Some(project.id)
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// Check 7. One look at the fleet per turn.
        ///
        /// The first call is a decision — who is free, what is running. The
        /// second cannot be: nothing the caller is waiting for arrives by being
        /// looked at. `tasks/01-routing.md` R4 is what this exists for, where a
        /// run polled `list_agents` between `sleep`s for forty-two seconds and
        /// ended without the answer.
        #[tokio::test]
        async fn a_second_list_agents_in_one_turn_is_refused_and_the_first_is_not() {
            let (store, _) = main_chat();
            let server = main_server(&store);

            let first = call(&server, "list_agents", json!({})).await;
            assert!(
                !is_error_result(&first),
                "the first look is legitimate: {first}"
            );

            let second = call(&server, "list_agents", json!({})).await;
            assert!(is_error_result(&second), "{second}");
            let said = said(&second);
            assert!(said.contains("already looked at the fleet this turn"), "{said}");
            assert!(
                said.contains("arrives on its own"),
                "the refusal has to say why waiting is pointless, or it reads as \
                 an arbitrary quota: {said}"
            );
        }

        /// And the rule stops where its reason stops.
        ///
        /// A read-only run cannot start anything, so it is not a router burning
        /// a turn on a poll loop — it is somebody's dashboard, and refusing it
        /// a second read would be a rule with no failure behind it.
        #[tokio::test]
        async fn a_read_only_run_may_look_at_the_fleet_as_often_as_it_likes() {
            let store = Arc::new(Store::in_memory().unwrap());
            let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::ReadOnly);
            for _ in 0..3 {
                let answer = call(&server, "list_agents", json!({})).await;
                assert!(!is_error_result(&answer), "{answer}");
            }
        }

        /// And a session somebody opened by hand is not a turn.
        ///
        /// It holds one server for as long as the person keeps it open, so a
        /// per-turn budget counted here would refuse the second `list_agents`
        /// of the afternoon. It is also not what the rule is for: nothing is
        /// waiting on it and nobody is paying for a turn it spends looking.
        #[tokio::test]
        async fn a_session_that_is_not_a_run_may_look_at_the_fleet_more_than_once() {
            let store = Arc::new(Store::in_memory().unwrap());
            // No `for_run`: this is `jod mcp` started by hand.
            let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::Orchestrate);
            for _ in 0..3 {
                let answer = call(&server, "list_agents", json!({})).await;
                assert!(!is_error_result(&answer), "{answer}");
            }
        }

        /// Check 10. A finished scratch session inside the window is offered,
        /// and the same session outside it is not.
        #[tokio::test]
        async fn a_completed_scratch_session_is_offered_inside_the_window_and_not_outside_it() {
            let (store, main) = main_chat();
            store
                .set_setting(SCRATCH_REUSE_WINDOW_MINUTES_KEY, "60")
                .unwrap();
            let scratch = scratch_under(&store, &main, "run-scratch");
            finished_run(&store, "run-scratch", "lookup", Some("scratch-session"));

            let server = Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::Delegate)
                .for_run("run-assistant");
            let inside = page(&call(&server, "list_agents", json!({})).await);
            assert_eq!(
                inside["scratch_idle"],
                json!(["run-scratch"]),
                "a scratch session that finished a moment ago is the one to continue: {inside}"
            );

            // The same row, last active two hours ago. Nothing about it has
            // changed except the clock.
            let two_hours_ago = chrono::Utc::now().timestamp_millis() - 2 * 60 * 60 * 1000;
            last_active_at(&store, &scratch, two_hours_ago);

            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Delegate)
                .for_run("run-assistant");
            let outside = page(&call(&server, "list_agents", json!({})).await);
            assert_eq!(
                outside["scratch_idle"],
                json!([]),
                "an hour-old window still offered a two-hour-old session: {outside}"
            );
            assert!(
                outside["scratch_reuse"].is_null(),
                "and the sentence about it should be gone with it: {outside}"
            );
        }

        /// Check 11. A *running* scratch session is never offered.
        ///
        /// The regression guard on rebuilding the block one layer down. If the
        /// only session on the right subject is busy, the answer is a new one
        /// beside it; reuse that waits for a session to free up is the exact
        /// thing this whole design removes.
        #[tokio::test]
        async fn a_running_scratch_session_is_never_offered_for_reuse() {
            let (store, main) = main_chat();
            store
                .set_setting(SCRATCH_REUSE_WINDOW_MINUTES_KEY, "60")
                .unwrap();
            scratch_under(&store, &main, "run-scratch");
            let pgid = running_run(&store, "run-scratch", "lookup");

            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Delegate)
                .for_run("run-assistant");
            let page = page(&call(&server, "list_agents", json!({})).await);

            assert_eq!(
                page["scratch_idle"],
                json!([]),
                "a busy scratch session was offered as something to continue, which \
                 is the block rebuilt one layer down: {page}"
            );
            assert!(page["scratch_reuse"].is_null(), "{page}");
            kill_group(pgid);
        }

        /// Check 12. The cross-talk guard.
        ///
        /// `is_free` matches a finished scratch session exactly as happily as
        /// an engineer — completed, with a session id, not a router — so
        /// without the exclusion a lookup that finished five minutes ago lands
        /// in `idle` and the engineer sentence tells the caller it "already
        /// holds this checkout". It holds no checkout at all.
        #[tokio::test]
        async fn a_completed_scratch_session_is_absent_from_the_engineer_answer() {
            let (store, main) = main_chat();
            let scratch = scratch_under(&store, &main, "run-scratch");
            finished_run(&store, "run-scratch", "lookup", Some("scratch-session"));

            let server = Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::Delegate)
                .for_run("run-assistant");
            let page = page(&call(&server, "list_agents", json!({})).await);

            assert_eq!(
                page["idle"],
                json!([]),
                "a scratch row reached the engineer idle list: {page}"
            );
            let reuse = page["reuse"].as_str().expect("a reuse sentence is always written");
            assert!(
                !reuse.contains("run-scratch"),
                "and the engineer sentence offered it: {reuse}"
            );
            assert!(
                reuse.contains("nothing to reuse"),
                "with no engineer at all the honest answer is that there is none: {reuse}"
            );
            // The row is still on the page, and it says what it is.
            let row = page["agents"]
                .as_array()
                .expect("agents is a list")
                .iter()
                .find(|a| a["run_id"] == "run-scratch")
                .expect("the scratch row is still listed");
            assert_eq!(row["scratch"], json!(true), "{row}");
            assert_eq!(
                store.conversation_origin(&scratch).unwrap().as_deref(),
                Some("human"),
                "a `delegate`d scratch conversation is not an assistant's own"
            );
        }

        /// Check 13. Reuse switched off.
        ///
        /// The way back to a fresh session per instruction if reuse turns out
        /// badly, so it has to be a real setting rather than a degenerate one —
        /// zero minutes must offer nothing, not everything since the epoch.
        #[tokio::test]
        async fn a_reuse_window_of_zero_offers_nothing() {
            let (store, main) = main_chat();
            store
                .set_setting(SCRATCH_REUSE_WINDOW_MINUTES_KEY, "0")
                .unwrap();
            scratch_under(&store, &main, "run-scratch");
            finished_run(&store, "run-scratch", "lookup", Some("scratch-session"));

            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Delegate)
                .for_run("run-assistant");
            let page = page(&call(&server, "list_agents", json!({})).await);

            assert_eq!(page["scratch_idle"], json!([]), "{page}");
            assert!(page["scratch_reuse"].is_null(), "{page}");
        }

        /// Check 14. The two sentences say opposite things, and they have to.
        ///
        /// An engineer is worth reusing for its warm checkout, which any
        /// instruction in that repository benefits from whatever the subject. A
        /// scratch session has no checkout: the only thing it carries is the
        /// subject it was talking about, so reusing it across subjects buys
        /// nothing and muddles what it knows. One sentence covering both would
        /// have to be vague enough to be wrong about one of them.
        #[tokio::test]
        async fn the_scratch_sentence_says_same_subject_where_the_engineer_one_says_any() {
            let (store, main) = main_chat();
            store
                .set_setting(SCRATCH_REUSE_WINDOW_MINUTES_KEY, "60")
                .unwrap();
            scratch_under(&store, &main, "run-scratch");
            finished_run(&store, "run-scratch", "lookup", Some("scratch-session"));

            // An engineer beside it: an ordinary conversation with a finished
            // run and a session to resume.
            let engineer = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp", None)
                .unwrap()
                .id;
            run_in(&store, &engineer, "run-engineer");
            finished_run(&store, "run-engineer", "engineer", Some("engineer-session"));

            let server = Server::new(Jod::with_store(store))
                .with_access(ToolAccess::Delegate)
                .for_run("run-assistant");
            let page = page(&call(&server, "list_agents", json!({})).await);

            assert_eq!(page["idle"], json!(["run-engineer"]), "{page}");
            assert_eq!(page["scratch_idle"], json!(["run-scratch"]), "{page}");

            let engineer_says = page["reuse"].as_str().expect("always written");
            let scratch_says = page["scratch_reuse"].as_str().expect("there is a candidate");
            assert!(
                engineer_says.contains("including one on a different subject"),
                "the engineer sentence stopped saying any subject will do: {engineer_says}"
            );
            assert!(
                scratch_says.contains("only if this instruction carries on that same subject"),
                "the scratch sentence does not say same-subject-only: {scratch_says}"
            );
            assert!(
                scratch_says.contains("Never wait"),
                "and it has to carry the rule that a busy one is not worth waiting \
                 for, since that is the one the caller is most tempted by: {scratch_says}"
            );
        }
    }

    // ---- claiming somewhere to write -------------------------------------

    /// A session on a real git repository, and the two things that make it
    /// safe to drive.
    ///
    /// These tests are synchronous on purpose. They hold `ENV_LOCK` — a
    /// worktree is cut under the process-wide `JOD_HOME`, so two of them at
    /// once would each get the other's — and a `std::sync::MutexGuard` must not
    /// be held across an `.await`. Owning a runtime and stepping into it keeps
    /// every await inside `block_on`, so the lock never crosses a suspension
    /// point.
    struct OnARepo {
        _guard: std::sync::MutexGuard<'static, ()>,
        runtime: tokio::runtime::Runtime,
        store: Arc<Store>,
        server: Server,
        repo: PathBuf,
        conversation: String,
    }

    impl OnARepo {
        fn call(&self, name: &str, args: Value) -> Value {
            self.runtime.block_on(call(&self.server, name, args))
        }
    }

    /// A current-thread runtime, entered so that anything constructed under it
    /// — `Jod::with_store` spawns a task — has a reactor to attach to.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
    }

    /// A run in a work, on a real git repository, answering as its own session.
    ///
    /// Deliberately built through the same calls the orchestrator makes, so
    /// this exercises the arrangement a real session is actually in rather than
    /// one assembled to make the test pass.
    fn on_a_repo(name: &str) -> OnARepo {
        let (guard, scratch) = crate::leases::scratch(name);
        let repo = crate::leases::fixture_repo(&scratch.join("repo"));
        let runtime = runtime();
        let entered = runtime.enter();
        let store = Arc::new(Store::in_memory().unwrap());
        let work = store.create_work("tidy the parser").unwrap();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, &repo.to_string_lossy(), None)
            .unwrap();
        store
            .attach_conversation(
                &conversation.id,
                &work.id,
                None,
                crate::works::Origin::Orchestrator,
            )
            .unwrap();
        store
            .add_root(&conversation.id, crate::roots::NewRoot::reading(&repo))
            .unwrap();
        store
            .append_prompt(&conversation.id, "run-1", "tidy the parser")
            .unwrap();
        let server = Server::new(Jod::with_store(store.clone()))
            .with_access(ToolAccess::Delegate)
            .for_run("run-1");
        // The guard borrows the runtime, so it goes before the runtime moves
        // into the value. The task `Jod::with_store` spawned stays on the
        // runtime and runs whenever `block_on` drives it.
        drop(entered);
        OnARepo {
            _guard: guard,
            runtime,
            store,
            server,
            repo,
            conversation: conversation.id,
        }
    }

    /// Write the record of what a run was launched with, the way the runner
    /// does before the supervisor starts anything.
    ///
    /// `granted` is what reaches `--add-dir`. The interesting value is the
    /// checkout on its own, because that is what `prepare_work` really hands a
    /// work session — no worktree exists yet at that point, so none can be
    /// granted.
    fn spawn_plan_granting(run_id: &str, cwd: &std::path::Path, granted: &[&std::path::Path]) {
        let mut args = vec![
            "-p".to_string(),
            "do the thing".to_string(),
            "--permission-mode".to_string(),
            "acceptEdits".to_string(),
        ];
        for dir in granted {
            args.push("--add-dir".to_string());
            args.push(dir.to_string_lossy().to_string());
        }
        let plan = crate::runner::SpawnPlan {
            run_id: run_id.to_string(),
            harness: HarnessKind::ClaudeCode,
            db_path: PathBuf::from("/dev/null"),
            program: PathBuf::from("claude"),
            args,
            cwd: cwd.to_path_buf(),
            env: Vec::new(),
            secrets: Vec::new(),
        };
        let dir = crate::paths::run_dir(run_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            crate::paths::spawn_path(run_id),
            serde_json::to_vec_pretty(&plan).unwrap(),
        )
        .unwrap();
    }

    /// **The check for this change.** A session that cannot write to the
    /// worktree it just claimed is told so, by name, in the answer it acts on.
    ///
    /// This is not a test that writing works — whether a work session can ever
    /// write to a claimed worktree is finding O1, it is still open, and closing
    /// it is a decision rather than a patch. This is a test that the failure is
    /// *visible*. The run that prompted it claimed a worktree, wrote nothing,
    /// committed nothing and finished `done` with no error recorded anywhere;
    /// the only way anyone found out was by opening the worktree afterwards and
    /// seeing one `README.md` and one `init` commit.
    ///
    /// The arrangement below is the real one, not one built to fail: the
    /// session is launched with the checkout granted and nothing else, which is
    /// exactly what `prepare_work` does, and the worktree is then cut somewhere
    /// else entirely under `JOD_HOME`.
    #[test]
    fn a_session_that_cannot_write_to_its_claimed_worktree_is_told_so() {
        let on = on_a_repo("mcp-unwritable");
        // What the work session really gets: its checkout, read-only, and no
        // worktree because none has been cut yet.
        spawn_plan_granting("run-1", &on.repo, &[&on.repo]);

        let answer = on.call("claim_worktree", json!({}));
        assert!(!is_error_result(&answer), "{}", said(&answer));
        let claimed: Value = serde_json::from_str(&said(&answer)).unwrap();
        let worktree = claimed["worktree"].as_str().unwrap().to_string();

        assert_eq!(
            claimed["writable"], "no",
            "the tool handed back a worktree outside everything this run was launched with, and \
             reported nothing about it: {claimed}"
        );
        let note = claimed["note"].as_str().unwrap_or_default();
        assert!(
            note.contains(&worktree),
            "the warning must name the worktree, or a session holding several paths cannot act \
             on it: {note}"
        );
        assert!(
            note.contains("cannot write"),
            "the warning must say the session cannot write, not merely that something is odd: \
             {note}"
        );
        // The part that stops the reader debugging the wrong machine. Two other
        // messages in this codebase sent people hunting for a broken harness
        // binary and a repository that was not a git repository, and both were
        // pointing at the wrong thing.
        assert!(
            note.contains("O1") && note.contains("tasks/10-orchestration.md"),
            "the warning must name the open finding, so the reader can see this is known: {note}"
        );
        assert!(
            note.contains("not a permissions problem"),
            "the warning must say this is not the reader's own repository or machine at fault: \
             {note}"
        );
    }

    /// The other half, and the one that stops this from being a check that
    /// always fires. A session whose grant does reach the worktree is told
    /// nothing alarming — otherwise the warning becomes noise and gets ignored
    /// exactly when it is true.
    #[test]
    fn a_session_whose_grant_reaches_the_worktree_is_not_warned() {
        let on = on_a_repo("mcp-writable");
        // The directory worktrees are cut under, granted up front. This is the
        // shape one of the two candidate fixes for O1 would produce, and it is
        // the only arrangement in which the tool may honestly say "writable".
        let worktrees = crate::leases::worktrees_dir();
        std::fs::create_dir_all(&worktrees).unwrap();
        spawn_plan_granting("run-1", &on.repo, &[&on.repo, &worktrees]);

        let claimed: Value =
            serde_json::from_str(&said(&on.call("claim_worktree", json!({})))).unwrap();
        assert_eq!(claimed["writable"], "yes", "{claimed}");
        assert!(
            claimed["note"].as_str().unwrap().contains("cut for you"),
            "{claimed}"
        );
    }

    /// **The test the lead asked for, and the one that matters.** `claim_lease`
    /// was written, tested and had no caller outside its own tests, which made
    /// D5 — read-only checkout, claim before writing — absent from the running
    /// system while every unit test stayed green. This goes through `call()`,
    /// so removing the tool from the catalogue fails it.
    #[test]
    fn claiming_a_worktree_is_reachable_through_the_tool_and_rebinds_the_roots() {
        let on = on_a_repo("mcp-claim");
        let (store, repo, conversation) = (&on.store, &on.repo, &on.conversation);
        let answer = on.call("claim_worktree", json!({}));
        assert!(!is_error_result(&answer), "{}", said(&answer));
        let claimed: Value = serde_json::from_str(&said(&answer)).unwrap();

        assert_eq!(claimed["reused"], false);
        assert!(claimed["branch"].as_str().unwrap().starts_with("jod/"));
        let worktree = PathBuf::from(claimed["worktree"].as_str().unwrap());
        assert!(worktree.is_dir(), "the tool reported a worktree it did not cut");

        // D5's actual promise: the worktree is the one writable root and the
        // real checkout is still there, readable, so the session can diff
        // against what Reljod is editing.
        let roots = store.roots(conversation).unwrap();
        let writable: Vec<&crate::roots::Root> = roots.iter().filter(|r| r.writable).collect();
        assert_eq!(writable.len(), 1, "{roots:?}");
        assert_eq!(writable[0].path, worktree);
        let checkout = roots
            .iter()
            .find(|r| &r.path == repo)
            .expect("the checkout must stay in the session's roots");
        assert!(!checkout.writable, "the real checkout became writable");
    }

    /// A second session on the same repository in the same work is *offered*
    /// the existing worktree. Reported as reuse rather than hidden: a session
    /// that believes it cut a fresh branch will commit over a sibling's work
    /// and describe it as its own.
    #[test]
    fn a_sibling_is_offered_the_worktree_rather_than_a_second_branch() {
        let on = on_a_repo("mcp-reuse");
        let (store, repo) = (&on.store, &on.repo);
        let first: Value =
            serde_json::from_str(&said(&on.call("claim_worktree", json!({})))).unwrap();

        // A sibling: another conversation in the same work, another run.
        let work_id = store
            .works(crate::works::Filter::All)
            .unwrap()
            .remove(0)
            .id;
        let sibling = store
            .new_conversation(HarnessKind::ClaudeCode, &repo.to_string_lossy(), None)
            .unwrap();
        store
            .attach_conversation(&sibling.id, &work_id, None, crate::works::Origin::Agent)
            .unwrap();
        store
            .add_root(&sibling.id, crate::roots::NewRoot::reading(repo))
            .unwrap();
        store.append_prompt(&sibling.id, "run-2", "and the tests").unwrap();
        let theirs = {
            // `Jod::with_store` spawns, so it is built inside the runtime this
            // fixture owns rather than on a bare thread.
            let _entered = on.runtime.enter();
            Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::Delegate)
                .for_run("run-2")
        };

        let second: Value = serde_json::from_str(&said(
            &on.runtime.block_on(call(&theirs, "claim_worktree", json!({}))),
        ))
        .unwrap();
        assert_eq!(second["reused"], true, "{second}");
        assert_eq!(second["lease_id"], first["lease_id"]);
        assert_eq!(second["branch"], first["branch"]);
        assert!(
            second["note"].as_str().unwrap().contains("sharing"),
            "reuse has to be said out loud: {second}"
        );
    }

    /// A non-git root raises a card and leaves the session running. It is not
    /// an error the agent should retry, and it is certainly not a crash.
    #[test]
    fn claiming_somewhere_that_is_not_a_repository_raises_a_card() {
        let (_guard, scratch) = crate::leases::scratch("mcp-not-git");
        let plain = scratch.join("not-a-repo");
        std::fs::create_dir_all(&plain).unwrap();
        let runtime = runtime();
        let entered = runtime.enter();
        let store = Arc::new(Store::in_memory().unwrap());
        let work = store.create_work("tidy the parser").unwrap();
        let conversation = store
            .new_conversation(HarnessKind::ClaudeCode, &plain.to_string_lossy(), None)
            .unwrap();
        store
            .attach_conversation(
                &conversation.id,
                &work.id,
                None,
                crate::works::Origin::Orchestrator,
            )
            .unwrap();
        store
            .add_root(&conversation.id, crate::roots::NewRoot::reading(&plain))
            .unwrap();
        store
            .append_prompt(&conversation.id, "run-1", "tidy the parser")
            .unwrap();
        let server = Server::new(Jod::with_store(store.clone()))
            .with_access(ToolAccess::Delegate)
            .for_run("run-1");

        drop(entered);
        let answer = runtime.block_on(call(&server, "claim_worktree", json!({})));
        assert!(!is_error_result(&answer), "a card is an answer, not an error");
        let said: Value = serde_json::from_str(&said(&answer)).unwrap();
        assert_eq!(said["claimed"], false);
        let card = store
            .card(said["card_id"].as_i64().unwrap())
            .unwrap()
            .expect("the refusal must be on the rail, not only in the answer");
        assert!(card.blocking);
        assert!(store.work_leases(&work.id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_session_outside_a_work_is_told_why_it_cannot_claim() {
        // A lease is per work *and* repository — that is what makes it
        // shareable — so there is nothing to key one to here.
        let (_, server, _) = working(ToolAccess::Delegate);
        let answer = call(&server, "claim_worktree", json!({})).await;
        assert!(is_error_result(&answer), "{answer}");
        assert!(said(&answer).contains("open_work"), "{}", said(&answer));
    }

    /// Releasing keeps anything that would be lost, and says why. Not a
    /// failure — an agent told this is an error will try to force it.
    #[test]
    fn releasing_a_worktree_with_uncommitted_work_keeps_it_and_says_why() {
        let on = on_a_repo("mcp-release-dirty");
        let claimed: Value =
            serde_json::from_str(&said(&on.call("claim_worktree", json!({})))).unwrap();
        let worktree = PathBuf::from(claimed["worktree"].as_str().unwrap());
        std::fs::write(worktree.join("half-done.rs"), "fn main() {}\n").unwrap();

        let released: Value =
            serde_json::from_str(&said(&on.call("release_worktree", json!({})))).unwrap();
        assert_eq!(released["removed"], false, "{released}");
        assert_eq!(released["dirty"], true);
        assert!(worktree.is_dir(), "uncommitted work was destroyed");
        assert!(released["note"].as_str().unwrap().contains("on purpose"));
    }

    #[test]
    fn releasing_a_clean_merged_worktree_removes_it() {
        let on = on_a_repo("mcp-release-clean");
        let claimed: Value =
            serde_json::from_str(&said(&on.call("claim_worktree", json!({})))).unwrap();
        let worktree = PathBuf::from(claimed["worktree"].as_str().unwrap());

        let released: Value =
            serde_json::from_str(&said(&on.call("release_worktree", json!({})))).unwrap();
        assert_eq!(released["removed"], true, "{released}");
        assert!(!worktree.exists());
    }

    #[test]
    fn releasing_when_you_hold_nothing_says_so_rather_than_failing_obscurely() {
        let on = on_a_repo("mcp-release-none");
        let answer = on.call("release_worktree", json!({}));
        assert!(is_error_result(&answer), "{answer}");
        assert!(said(&answer).contains("hold no worktree"), "{}", said(&answer));
    }

    /// A work with nothing on its board can never be complete, so an
    /// instruction that says nothing is refused before anything is started.
    #[tokio::test]
    async fn opening_a_work_with_no_instruction_starts_nothing() {
        let (store, server, _) = working(ToolAccess::Delegate);
        let answer = call(
            &server,
            "open_work",
            json!({ "instruction": "   ", "checkout": "/tmp" }),
        )
        .await;
        assert!(is_error_result(&answer), "{answer}");
        assert!(
            store.works(crate::works::Filter::All).unwrap().is_empty(),
            "a refused instruction left a work behind"
        );
    }

    // ---- the passive lifter ----------------------------------------------

    fn tool_call(name: &str, input: Value) -> AgentEvent {
        AgentEvent::ToolCall {
            name: name.into(),
            input: Some(input),
        }
    }

    #[test]
    fn a_harnesss_own_question_becomes_a_question_card() {
        let lifted = lift(&tool_call(
            ASK_USER_QUESTION,
            json!({
                "questions": [{
                    "question": "Which database for the chat store?",
                    "header": "chat DB",
                    "options": [{ "label": "sqlite" }, { "label": "postgres" }]
                }]
            }),
        ));
        assert_eq!(lifted.len(), 1);
        assert_eq!(lifted[0].kind, CardKind::Question);
        assert_eq!(lifted[0].title, "Which database for the chat store?");
        assert_eq!(lifted[0].body, "chat DB");
        assert_eq!(lifted[0].options, vec!["sqlite", "postgres"]);
    }

    /// One call can ask three things, and three questions collapsed into one
    /// card is a card nobody can answer.
    #[test]
    fn every_question_in_one_call_becomes_its_own_card() {
        let lifted = lift(&tool_call(
            ASK_USER_QUESTION,
            json!({
                "questions": [
                    { "question": "which database?" },
                    { "question": "which port?" }
                ]
            }),
        ));
        assert_eq!(lifted.len(), 2);
        assert_eq!(lifted[1].title, "which port?");
    }

    /// The payload is another program's private interface, so the flat shape is
    /// read as well as the nested one, and string options as well as objects.
    #[test]
    fn a_flatter_question_payload_is_still_lifted() {
        let lifted = lift(&tool_call(
            ASK_USER_QUESTION,
            json!({ "question": "which port?", "options": ["8443", "443"] }),
        ));
        assert_eq!(lifted.len(), 1);
        assert_eq!(lifted[0].options, vec!["8443", "443"]);
    }

    #[test]
    fn a_call_with_nothing_question_shaped_in_it_lifts_nothing() {
        assert!(lift(&tool_call(ASK_USER_QUESTION, json!({ "questions": [] }))).is_empty());
        assert!(lift(&tool_call(ASK_USER_QUESTION, json!({ "note": "hello" }))).is_empty());
        assert!(lift(&tool_call(EXIT_PLAN_MODE, json!({ "plan": "   " }))).is_empty());
        assert!(lift(&tool_call("Bash", json!({ "command": "ls" }))).is_empty());
        assert!(lift(&AgentEvent::Message { text: "hello".into() }).is_empty());
    }

    /// Plan mode refuses every mutation, so a run that has reached here really
    /// is stopped until somebody says go — the one lifted case that blocks.
    #[test]
    fn a_plan_waiting_for_approval_is_lifted_as_a_blocker() {
        let lifted = lift(&tool_call(
            EXIT_PLAN_MODE,
            json!({ "plan": "1. port the lexer\n2. port the parser" }),
        ));
        assert_eq!(lifted.len(), 1);
        assert!(lifted[0].blocking);
        assert!(lifted[0].body.contains("port the lexer"));
        assert!(!lifted[0].options.is_empty(), "a plan is approved by a keystroke");
    }

    #[tokio::test]
    async fn a_lifted_card_is_marked_as_lifted_and_lands_on_the_runs_rail() {
        let (store, _, conversation) = working(ToolAccess::ReadOnly);
        let raised = lift_into_cards(
            &store,
            "run-1",
            &tool_call(ASK_USER_QUESTION, json!({ "question": "which port?" })),
        )
        .unwrap();
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].source, Source::Lifted);
        assert_eq!(raised[0].conversation_id, conversation);
        assert_eq!(raised[0].run_id.as_deref(), Some("run-1"));
    }

    /// **The reason `dedupe_key` exists.** A harness wired to Jod's MCP server
    /// asks the question twice — once by calling `ask_question` and once by
    /// printing its own tool call — and two rail rows for one question is worse
    /// than none, because answering one leaves the other open for ever.
    #[tokio::test]
    async fn a_question_asked_over_mcp_and_printed_by_the_harness_is_one_card() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        call(
            &server,
            "ask_question",
            json!({ "question": "Which database for the chat store?" }),
        )
        .await;
        // The same question as the harness spells it: different punctuation,
        // different case, extra whitespace — the differences the key survives.
        let lifted = lift_into_cards(
            &store,
            "run-1",
            &tool_call(
                ASK_USER_QUESTION,
                json!({ "questions": [{ "question": "  which database for the chat store  " }] }),
            ),
        )
        .unwrap();

        let card = only_card(&store, &conversation);
        assert_eq!(lifted.len(), 1);
        assert_eq!(lifted[0].id, card.id, "the second emission minted a card");
        assert_eq!(
            card.source,
            Source::Mcp,
            "the first card stands; the lift must not rewrite it"
        );
        assert_eq!(card.title, "Which database for the chat store?");
    }

    /// Replaying a run's events — which every fresh process does on rehydrate —
    /// must not produce a second copy of a question.
    #[tokio::test]
    async fn lifting_the_same_event_twice_produces_one_card() {
        let (store, _, conversation) = working(ToolAccess::ReadOnly);
        let event = tool_call(ASK_USER_QUESTION, json!({ "question": "which port?" }));
        let first = lift_into_cards(&store, "run-1", &event).unwrap();
        let again = lift_into_cards(&store, "run-1", &event).unwrap();
        assert_eq!(first[0].id, again[0].id);
        only_card(&store, &conversation);
    }

    /// De-duplication is per conversation, so two sessions asking the same
    /// question are two questions — answered by different agents.
    #[tokio::test]
    async fn two_runs_asking_one_question_get_a_card_each() {
        let (store, _, _) = working(ToolAccess::ReadOnly);
        let other = store
            .new_conversation(HarnessKind::ClaudeCode, "/tmp/other", None)
            .unwrap()
            .id;
        store.append_prompt(&other, "run-2", "and the tests").unwrap();
        let event = tool_call(ASK_USER_QUESTION, json!({ "question": "which port?" }));

        let first = lift_into_cards(&store, "run-1", &event).unwrap();
        let second = lift_into_cards(&store, "run-2", &event).unwrap();
        assert_ne!(first[0].id, second[0].id);
    }

    #[tokio::test]
    async fn a_run_nobody_is_watching_a_rail_for_lifts_nothing() {
        let (store, _, _) = working(ToolAccess::ReadOnly);
        let raised = lift_into_cards(
            &store,
            "run-nobody-has-heard-of",
            &tool_call(ASK_USER_QUESTION, json!({ "question": "which port?" })),
        )
        .unwrap();
        assert!(raised.is_empty());
    }

    /// `read_only` is a wide door, so a repeat has to collapse: an agent that
    /// records the same decision twice — a retried turn, a rewritten `why` —
    /// produces one row, because a full rail is an unread rail.
    #[tokio::test]
    async fn recording_one_decision_twice_in_different_words_is_one_card() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        for why in ["no server to run", "sqlite needs no server, and we deploy to one box"] {
            call(
                &server,
                "record_decision",
                json!({ "title": "chat DB", "chosen": "sqlite", "why": why }),
            )
            .await;
        }
        assert_eq!(
            only_card(&store, &conversation).body,
            "no server to run",
            "the first card stands rather than being rewritten"
        );
    }

    /// And the other half, which is why the key carries the *choice*: a
    /// decision that was reconsidered is a second card, not a silent no-op on
    /// the first. Collapsing it would leave the rail showing a choice that is
    /// no longer in force — worse than either a duplicate or a missing row.
    #[tokio::test]
    async fn reconsidering_a_decision_raises_a_second_card_rather_than_vanishing() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        for chosen in ["sqlite", "postgres"] {
            call(
                &server,
                "record_decision",
                json!({ "title": "chat DB", "chosen": chosen }),
            )
            .await;
        }
        let both = store
            .cards(&Query {
                conversation_id: Some(conversation),
                ..Query::default()
            })
            .unwrap();
        assert_eq!(both.len(), 2, "the second decision was swallowed by the first");
        assert_eq!(both[0].chosen.as_deref(), Some("postgres"));
    }

    /// Asking twice for one credential is one card, though: the second request
    /// says nothing the first did not, and two rows for one variable are two
    /// places to type the same key.
    #[tokio::test]
    async fn asking_twice_for_one_credential_is_one_card() {
        let (store, server, conversation) = working(ToolAccess::ReadOnly);
        for hint in ["the live key", "the live key, from the dashboard"] {
            call(
                &server,
                "request_secret",
                json!({ "name": "STRIPE_API_KEY", "hint": hint }),
            )
            .await;
        }
        assert_eq!(only_card(&store, &conversation).body, "the live key");
    }

    #[test]
    fn the_dedupe_key_ignores_case_punctuation_and_spacing_but_not_the_kind() {
        assert_eq!(
            dedupe_key(CardKind::Question, "Which DB?"),
            dedupe_key(CardKind::Question, "  which   db  ")
        );
        assert_ne!(
            dedupe_key(CardKind::Question, "which db"),
            dedupe_key(CardKind::Decision, "which db"),
            "a question and the decision that answers it are two different rows"
        );
        assert_ne!(
            dedupe_key(CardKind::Question, "which db"),
            dedupe_key(CardKind::Question, "which port")
        );
        // Capped, or a key computed from a whole plan would never match a
        // second emission that reworded one line near its end.
        assert!(dedupe_key(CardKind::Question, &"word ".repeat(200)).len() < 200);
    }

    // ---- the bus ---------------------------------------------------------

    use crate::team::{MailState, Scope};

    /// A two-member team, and a server answering as `lead`'s run.
    fn crew(access: ToolAccess) -> (Arc<Store>, Server) {
        let store = Arc::new(Store::in_memory().unwrap());
        for (name, run) in [("lead", "run-lead"), ("scout", "run-scout")] {
            store
                .join_scope(Scope::Team, "crew", name, HarnessKind::ClaudeCode, "", None)
                .unwrap();
            store
                .bind_member("crew", name, Some(run), Some("ses-1"))
                .unwrap();
        }
        let server = Server::new(Jod::with_store(store.clone()))
            .with_access(access)
            .for_run("run-lead");
        (store, server)
    }

    /// The property the whole design of sender identity exists for.
    #[tokio::test]
    async fn a_message_is_sent_as_the_run_that_is_calling_whatever_the_arguments_say() {
        let (store, server) = crew(ToolAccess::Delegate);
        call(
            &server,
            "send_message",
            // Every spelling an agent might try. None of them is read: there is
            // no argument for the sender, and this is the reason there is not.
            json!({
                "to": "scout",
                "text": "look at the parser",
                "from": "reljod",
                "sender": "reljod",
                "as": "reljod"
            }),
        )
        .await;
        let inbox = store.team_unread("crew", "scout").unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(
            inbox[0].from, "lead",
            "an agent named its own sender and Jod believed it"
        );
    }

    #[tokio::test]
    async fn a_session_with_no_run_behind_it_cannot_send_as_anybody() {
        let store = Arc::new(Store::in_memory().unwrap());
        store
            .join_scope(Scope::Team, "crew", "lead", HarnessKind::ClaudeCode, "", None)
            .unwrap();
        // No `for_run`: this is a session somebody opened by hand.
        let server = Server::new(Jod::with_store(store)).with_access(ToolAccess::Delegate);
        let answer = call(&server, "send_message", json!({ "to": "lead", "text": "hi" })).await;
        assert!(is_error_result(&answer), "{answer}");
        assert!(said(&answer).contains("no run behind it"), "{}", said(&answer));
    }

    /// The refusal that keeps a misconfiguration from becoming a wrong sender
    /// that works. Two sources disagreeing is a fault, not a choice.
    #[tokio::test]
    async fn a_claimed_run_that_disagrees_with_the_process_group_sends_nothing() {
        let (store, _) = crew(ToolAccess::Delegate);
        let server = Server::new(Jod::with_store(store.clone()))
            .with_access(ToolAccess::Delegate)
            .as_identity(Identity::Disputed {
                group: Some("run-lead".into()),
                claimed: "run-scout".into(),
            });
        let answer = call(
            &server,
            "send_message",
            json!({ "to": "scout", "text": "trust me" }),
        )
        .await;
        assert!(is_error_result(&answer), "{answer}");
        let why = said(&answer);
        assert!(why.contains("run-lead") && why.contains("run-scout"), "{why}");
        assert!(
            store.team_unread("crew", "scout").unwrap().is_empty(),
            "a server that cannot say who it is sent a message anyway"
        );
    }

    /// The process group is the authority; the environment only ever agrees
    /// with it or is refused. Asserted on `identify` itself, because this is
    /// the function somebody will later be tempted to replace with a parameter.
    #[test]
    fn identity_prefers_the_process_group_and_refuses_to_pick_a_winner() {
        let store = Store::in_memory().unwrap();
        // This test process is in some process group the store knows nothing
        // about, which is exactly the hand-started case.
        assert_eq!(identify(&store, None), Identity::Unknown);
        assert_eq!(
            identify(&store, Some("run-claimed")),
            Identity::Disputed {
                group: None,
                claimed: "run-claimed".into()
            },
            "an environment claim with no group to agree with is not identity on its own"
        );
        assert_eq!(
            identify(&store, Some("   ")),
            Identity::Unknown,
            "an empty claim is not a claim"
        );
    }

    #[tokio::test]
    async fn a_run_that_is_nobodys_teammate_is_told_so_rather_than_given_a_bus() {
        let store = Arc::new(Store::in_memory().unwrap());
        let server = Server::new(Jod::with_store(store))
            .with_access(ToolAccess::Delegate)
            .for_run("run-alone");
        let answer = call(&server, "roster", json!({})).await;
        assert!(is_error_result(&answer), "{answer}");
        assert!(said(&answer).contains("not a member"), "{}", said(&answer));
    }

    #[tokio::test]
    async fn reading_the_inbox_hands_each_message_over_exactly_once() {
        let (store, server) = crew(ToolAccess::Delegate);
        store
            .post(&Post::new(Scope::Team, "crew", "scout", "the parser is in core").to("lead"))
            .unwrap();

        let first: Value = serde_json::from_str(&said(
            &call(&server, "read_messages", json!({})).await,
        ))
        .unwrap();
        assert_eq!(first.as_array().unwrap().len(), 1);
        assert_eq!(first[0]["text"], "the parser is in core");
        assert!(
            first[0]["thread_id"].is_string(),
            "a message you cannot reply into is a dead end: {first}"
        );

        let again: Value =
            serde_json::from_str(&said(&call(&server, "read_messages", json!({})).await)).unwrap();
        assert!(
            again.as_array().unwrap().is_empty(),
            "the same instruction was handed over twice: {again}"
        );
    }

    #[tokio::test]
    async fn the_roster_names_who_is_addressable_and_never_the_caller() {
        let (_, server) = crew(ToolAccess::ReadOnly);
        let seen: Value =
            serde_json::from_str(&said(&call(&server, "roster", json!({})).await)).unwrap();
        assert_eq!(seen["you"], "lead");
        let names: Vec<&str> = seen["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["scout"]);
        assert_eq!(seen["members"][0]["harness"], "claude_code");
        assert_eq!(seen["members"][0]["idle"], true);
    }

    #[tokio::test]
    async fn a_reply_goes_back_to_the_sender_in_the_thread_it_answers() {
        let (store, server) = crew(ToolAccess::Delegate);
        store
            .post(&Post::new(Scope::Team, "crew", "scout", "where is the parser?").to("lead"))
            .unwrap();
        let read: Value =
            serde_json::from_str(&said(&call(&server, "read_messages", json!({})).await)).unwrap();
        let asked_id = read[0]["id"].as_i64().unwrap();
        let thread = read[0]["thread_id"].as_str().unwrap().to_string();

        let answer: Value = serde_json::from_str(&said(
            &call(
                &server,
                "reply",
                json!({ "message_id": asked_id, "text": "in core" }),
            )
            .await,
        ))
        .unwrap();
        assert_eq!(answer["thread_id"], thread, "a reply left its own thread");
        assert_eq!(answer["depth"], 1);
        assert_eq!(answer["to"][0], "scout");
        assert_eq!(store.mail_thread(&thread).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_message_to_a_name_nobody_answers_to_is_refused_by_name() {
        let (_, server) = crew(ToolAccess::Delegate);
        let answer = call(
            &server,
            "send_message",
            json!({ "to": "ghost", "text": "hello?" }),
        )
        .await;
        assert!(is_error_result(&answer), "{answer}");
        assert!(said(&answer).contains("ghost"), "{}", said(&answer));
    }

    /// G4 through the tools: an exchange that will not stop is stopped for it.
    #[tokio::test]
    async fn an_exchange_that_never_ends_is_refused_at_the_bound() {
        let (store, server) = crew(ToolAccess::Delegate);
        let bounds = store.bounds_for(Scope::Team, "crew").unwrap();
        // The scout keeps asking; the lead — this server — keeps answering.
        let mut last = match store
            .post(&Post::new(Scope::Team, "crew", "scout", "hop 0").to("lead"))
            .unwrap()
        {
            Sent::Queued { ids, .. } => ids[0],
            other => panic!("{other:?}"),
        };
        for hop in 1..(bounds.max_depth + 5) {
            let answer = call(
                &server,
                "reply",
                json!({ "message_id": last, "text": format!("hop {hop}") }),
            )
            .await;
            if is_error_result(&answer) {
                let why = said(&answer);
                assert!(why.contains("bound"), "{why}");
                assert!(why.contains("paused"), "{why}");
                return;
            }
            let sent: Value = serde_json::from_str(&said(&answer)).unwrap();
            let id = sent["message_ids"][0].as_i64().unwrap();
            // The scout answers straight back, which is what makes this a loop
            // rather than a monologue.
            last = match store
                .post(
                    &Post::new(Scope::Team, "crew", "scout", "and?")
                        .to("lead")
                        .replying_to(id),
                )
                .unwrap()
            {
                Sent::Queued { ids, .. } => ids[0],
                Sent::Bounded { .. } => return,
                other => panic!("{other:?}"),
            };
        }
        panic!("the exchange ran past every bound");
    }

    #[tokio::test]
    async fn asking_returns_the_answer_when_one_comes_back() {
        let (store, server) = crew(ToolAccess::Delegate);
        // The peer, answering as a peer does: it finds the question in its
        // inbox and replies to it.
        let peer = store.clone();
        tokio::spawn(async move {
            loop {
                let waiting = peer.team_unread("crew", "scout").unwrap();
                if let Some(question) = waiting.first() {
                    peer.post(
                        &Post::new(Scope::Team, "crew", "scout", "in core")
                            .to("lead")
                            .replying_to(question.id),
                    )
                    .unwrap();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });

        let answered: Value = serde_json::from_str(&said(
            &call(
                &server,
                "ask",
                json!({ "to": "scout", "text": "where is the parser?", "timeout_seconds": 10 }),
            )
            .await,
        ))
        .unwrap();
        assert_eq!(answered["replied"], true, "{answered}");
        assert_eq!(answered["text"], "in core");
        assert_eq!(answered["from"], "scout");
    }

    /// A5. The peer might be dead, and an agent that can wait for ever is how a
    /// fleet deadlocks.
    #[tokio::test]
    async fn asking_gives_up_at_its_deadline_rather_than_waiting_for_ever() {
        let (store, server) = crew(ToolAccess::Delegate);
        let started = std::time::Instant::now();
        let answered: Value = serde_json::from_str(&said(
            &call(
                &server,
                "ask",
                json!({ "to": "scout", "text": "still there?", "timeout_seconds": 1 }),
            )
            .await,
        ))
        .unwrap();
        assert_eq!(answered["replied"], false, "{answered}");
        assert!(answered["note"].as_str().unwrap().contains("no reply"));
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        // The question is still on the bus: giving up waiting is not unsending.
        assert_eq!(store.team_unread("crew", "scout").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_wait_can_never_be_asked_to_last_longer_than_the_cap() {
        // The argument is the model's; the bound is not. Asserted on the
        // constants rather than by waiting ten minutes for it — and the first
        // one at compile time, since a default above its own cap should never
        // reach a test run.
        const { assert!(ASK_DEADLINE_SECS <= MAX_ASK_DEADLINE_SECS) };
        assert_eq!(
            (MAX_ASK_DEADLINE_SECS + 10_000).clamp(1, MAX_ASK_DEADLINE_SECS),
            MAX_ASK_DEADLINE_SECS
        );
    }

    #[tokio::test]
    async fn a_handoff_moves_the_task_and_tells_the_recipient_in_one_call() {
        let (store, server) = crew(ToolAccess::Delegate);
        store.add_team_task("crew", "t1", "port the parser").unwrap();
        assert!(store.claim_task("t1", "lead").unwrap());

        let done: Value = serde_json::from_str(&said(
            &call(
                &server,
                "handoff",
                json!({ "to": "scout", "task_id": "t1", "text": "the tests are green" }),
            )
            .await,
        ))
        .unwrap();
        assert_eq!(done["handed_over"], "t1");

        let task = store
            .team_tasks("crew")
            .unwrap()
            .into_iter()
            .find(|t| t.id == "t1")
            .unwrap();
        assert_eq!(
            task.owner.as_deref(),
            Some("scout"),
            "ownership must move on the board, not only in the prose"
        );
        let told = store.team_unread("crew", "scout").unwrap();
        assert_eq!(told.len(), 1);
        assert!(told[0].text.contains("t1"), "{}", told[0].text);
        assert_eq!(
            store.envelope(told[0].id).unwrap().unwrap().kind,
            Kind::Handoff
        );
    }

    #[tokio::test]
    async fn a_handoff_of_a_task_somebody_else_holds_is_refused() {
        let (store, server) = crew(ToolAccess::Delegate);
        store.add_team_task("crew", "t1", "port the parser").unwrap();
        assert!(store.claim_task("t1", "scout").unwrap());

        let answer = call(
            &server,
            "handoff",
            json!({ "to": "scout", "task_id": "t1", "text": "yours" }),
        )
        .await;
        assert!(is_error_result(&answer), "{answer}");
        assert!(said(&answer).contains("not yours"), "{}", said(&answer));
    }

    #[tokio::test]
    async fn a_broadcast_reaches_every_teammate_and_never_the_sender() {
        let (store, server) = crew(ToolAccess::Delegate);
        store
            .join_scope(
                Scope::Team,
                "crew",
                "builder",
                HarnessKind::OpenCode,
                "",
                None,
            )
            .unwrap();
        let sent: Value = serde_json::from_str(&said(
            &call(&server, "send_message", json!({ "text": "standup in five" })).await,
        ))
        .unwrap();
        assert_eq!(sent["to"].as_array().unwrap().len(), 2);
        assert!(store.team_unread("crew", "lead").unwrap().is_empty());
        assert_eq!(store.team_unread("crew", "builder").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn mail_that_has_been_read_is_marked_delivered_rather_than_only_flagged() {
        let (store, server) = crew(ToolAccess::Delegate);
        let sent = store
            .post(&Post::new(Scope::Team, "crew", "scout", "hello").to("lead"))
            .unwrap();
        let Sent::Queued { ids, .. } = sent else {
            panic!("{sent:?}")
        };
        assert_eq!(
            store.envelope(ids[0]).unwrap().unwrap().state,
            MailState::Queued
        );
        call(&server, "read_messages", json!({})).await;
        assert_eq!(
            store.envelope(ids[0]).unwrap().unwrap().state,
            MailState::Delivered,
            "the traffic log would still call a read message unread"
        );
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

    // ---- the board ---------------------------------------------------------

    mod board {
        use super::*;
        use crate::projects::NewProject;
        use crate::works::Origin;

        /// A work with one engineer session on it, and a server speaking as
        /// that engineer's run.
        ///
        /// No manager above it, which is the plainer of the two shapes and the
        /// one every fixture that does not care about reporting wants.
        ///
        /// **The board is not empty when this returns.** `create_work_in` puts
        /// the instruction on it as the first task, which is production's own
        /// shape — a work always has the thing it was opened to do — and it is
        /// what `plan_work` plans on top of. Every assertion below counts that
        /// row rather than pretending a fresh work has nothing on it.
        fn on_a_work(access: ToolAccess) -> (Arc<Store>, Server, String, String) {
            let store = Arc::new(Store::in_memory().unwrap());
            let work = store.create_work_in("port the parser", None).unwrap();
            let engineer = engineer_in(&store, &work.id, None, "run-1");
            let server = Server::new(Jod::with_store(store.clone()))
                .with_access(access)
                .for_run("run-1");
            (store, server, work.id, engineer)
        }

        /// A session attached to a work, hanging under `parent`, with a run
        /// behind it — which is the join [`Server::raiser`] reads.
        fn engineer_in(
            store: &Store,
            work_id: &str,
            parent: Option<&str>,
            run_id: &str,
        ) -> String {
            let id = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
                .unwrap()
                .id;
            store
                .attach_conversation(&id, work_id, parent, Origin::Agent)
                .unwrap();
            store.append_prompt(&id, run_id, "do the thing").unwrap();
            id
        }

        /// A catalogued project with a manager conversation, which is what
        /// makes a conversation "a manager" as far as reporting is concerned.
        fn with_a_manager(store: &Store, dir: &str) -> String {
            std::fs::create_dir_all(dir).unwrap();
            let project = store.add_project(NewProject::at(dir).named("tetris")).unwrap();
            let (manager, _) = store
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();
            manager
        }

        fn tool(name: &str) -> Tool {
            catalogue()
                .into_iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is not in the catalogue"))
        }

        /// The titles on a board, in the order the board is in.
        fn titles(store: &Store, work_id: &str) -> Vec<String> {
            store
                .work_tasks(work_id)
                .unwrap()
                .into_iter()
                .map(|t| t.title)
                .collect()
        }

        /// A plan written straight through the store, for the tests that are
        /// about what happens to a board rather than about the tool that
        /// writes one.
        fn planned(store: &Store, work_id: &str, titles: &[&str]) -> Vec<crate::team::TeamTask> {
            store
                .plan_work(
                    work_id,
                    &crate::works::Plan {
                        tasks: titles
                            .iter()
                            .map(|title| crate::works::PlannedTask {
                                title: (*title).to_string(),
                                paths: Vec::new(),
                            })
                            .collect(),
                    },
                )
                .unwrap()
        }

        /// One task off a board, by the title it was planned under.
        ///
        /// By title rather than by index, because `plan_work` answers with the
        /// **whole** board — the work's own instruction included — so position
        /// in the returned list is not position in the plan.
        fn task_called(board: &[crate::team::TeamTask], title: &str) -> String {
            board
                .iter()
                .find(|t| t.title == title)
                .unwrap_or_else(|| panic!("no task called `{title}` on this board"))
                .id
                .clone()
        }

        fn body(store: &Store, conversation: &str) -> String {
            let queued = store.pending_for(conversation).unwrap();
            assert_eq!(
                queued.len(),
                1,
                "expected exactly one thing waiting for `{conversation}`, got {queued:?}"
            );
            queued[0].body.clone()
        }

        // ---- check 8 -------------------------------------------------------

        /// Each of the four sits at the level its effect earns, and the two
        /// that a manager reads or an engineer calls sit low enough to be
        /// reachable at all.
        ///
        /// Asserted against the catalogue rather than against the table above,
        /// because the table is what a human reads and this is what the
        /// dispatcher enforces. The pair disagreeing is the bug.
        #[test]
        fn the_board_tools_sit_at_the_level_their_effect_earns() {
            assert_eq!(tool("plan_work").needs, ToolAccess::Delegate);
            assert_eq!(tool("work_board").needs, ToolAccess::ReadOnly);
            assert_eq!(tool("complete_task").needs, ToolAccess::ReadOnly);
            assert_eq!(tool("stack_pull_requests").needs, ToolAccess::Delegate);
        }

        /// A manager is spawned at `delegate`, so every tool it is told to call
        /// has to be callable there. `complete_task` has a lower ceiling still:
        /// an engineer may be opened at `read_only`, and one that cannot report
        /// is one whose work never reaches anybody.
        #[test]
        fn a_manager_can_reach_all_four_and_an_engineer_can_always_report() {
            for name in ["plan_work", "work_board", "complete_task", "stack_pull_requests"] {
                assert!(
                    allows(ToolAccess::Delegate, tool(name).needs),
                    "a manager runs at `delegate` and cannot call `{name}`"
                );
            }
            assert!(
                allows(ToolAccess::ReadOnly, tool("complete_task").needs),
                "an engineer opened read-only cannot report what it did"
            );
        }

        /// The constraint has to be in the description, because that is where
        /// the manager reads it. A manager that learns "two tasks cannot claim
        /// one file" from a refusal has already spent a turn writing a plan
        /// that was never going to be accepted.
        #[test]
        fn plan_work_states_the_rule_before_the_call_rather_than_after_it() {
            let said = tool("plan_work").description.to_lowercase();
            assert!(
                said.contains("one task per engineer"),
                "`plan_work` does not say how a plan is shaped: {said}"
            );
            assert!(
                said.contains("refused"),
                "`plan_work` does not say a colliding plan is refused, so the manager \
                 finds out by being refused: {said}"
            );
            assert!(
                said.contains("same file"),
                "`plan_work` does not say what the collision is: {said}"
            );
            assert!(
                said.contains("stack"),
                "`plan_work` does not say that the order it is written in is the order the \
                 pull requests stack in, which is the fact that makes plan order matter: \
                 {said}"
            );
        }

        // ---- planning and reading a board ----------------------------------

        /// The ordinary path, end to end through the tool: a plan goes on, and
        /// the board reads back the same rows with the same paths in the same
        /// order.
        #[tokio::test]
        async fn a_plan_written_through_the_tool_is_the_board_the_tool_reads_back() {
            let (_, server, work, _) = on_a_work(ToolAccess::Delegate);
            let answer = call(
                &server,
                "plan_work",
                json!({
                    "work_id": work,
                    "tasks": [
                        { "title": "the board", "paths": ["core/src/works.rs"] },
                        { "title": "the tools", "paths": ["core/src/mcp.rs"] },
                        { "title": "read the harness docs" }
                    ]
                }),
            )
            .await;
            assert!(!is_error_result(&answer), "{answer}");
            let written: Value = serde_json::from_str(&said(&answer)).unwrap();

            let read: Value = serde_json::from_str(&said(
                &call(&server, "work_board", json!({ "work_id": work })).await,
            ))
            .unwrap();
            assert_eq!(read["tasks"], written["tasks"], "the two disagree: {read}");
            // Four, not three: the work's own instruction was already on the
            // board and a plan is written under it.
            assert_eq!(read["open"], json!(4), "{read}");
            assert_eq!(read["done"], json!(0), "{read}");
            assert_eq!(read["finished"], json!(false), "{read}");
            let order: Vec<&str> = read["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["title"].as_str().unwrap())
                .collect();
            assert_eq!(
                order,
                vec!["port the parser", "the board", "the tools", "read the harness docs"],
                "the board came back out of plan order: {read}"
            );
            assert_eq!(read["tasks"][1]["paths"], json!(["core/src/works.rs"]), "{read}");
            assert_eq!(
                read["tasks"][3]["paths"],
                json!([]),
                "a task that only reads must own nothing: {read}"
            );
        }

        /// The refusal is the feature, so it has to arrive with both titles in
        /// it. A manager told only "that plan is not allowed" has to guess
        /// which two of five tasks collided.
        #[tokio::test]
        async fn planning_two_tasks_onto_one_file_is_refused_and_names_both_of_them() {
            let (store, server, work, _) = on_a_work(ToolAccess::Delegate);
            let answer = call(
                &server,
                "plan_work",
                json!({
                    "work_id": work,
                    "tasks": [
                        { "title": "the board", "paths": ["core/src"] },
                        { "title": "the tools", "paths": ["core/src/mcp.rs"] }
                    ]
                }),
            )
            .await;
            assert!(is_error_result(&answer), "the collision was written: {answer}");
            let refusal = said(&answer);
            assert!(refusal.contains("the board"), "{refusal}");
            assert!(refusal.contains("the tools"), "{refusal}");
            assert!(refusal.contains("core/src/mcp.rs"), "{refusal}");
            assert_eq!(
                titles(&store, &work),
                vec!["port the parser".to_string()],
                "a refused plan left half a board behind"
            );
        }

        /// An unknown work reads back as an empty board, and an empty board is
        /// what a finished job looks like. Refusing is the only answer that
        /// does not let a mistyped id read as "there is nothing left to do".
        #[tokio::test]
        async fn reading_a_board_that_does_not_exist_is_refused_rather_than_answered_empty() {
            let (_, server, _, _) = on_a_work(ToolAccess::ReadOnly);
            let answer = call(&server, "work_board", json!({ "work_id": "no-such-work" })).await;
            assert!(is_error_result(&answer), "{answer}");
            assert!(said(&answer).contains("no-such-work"), "{}", said(&answer));
        }

        // ---- check 9 -------------------------------------------------------

        /// `last` is the engineer's only way to know whether it is the one
        /// holding the job up, so it must be false while anybody else is still
        /// working and true exactly once.
        #[tokio::test]
        async fn completing_a_task_says_it_was_the_last_one_only_when_it_was() {
            let (store, server, work, _) = on_a_work(ToolAccess::ReadOnly);
            let board = planned(&store, &work, &["the board", "the tools"]);

            let finish = |title: &str, report: &str| {
                let id = task_called(&board, title);
                let report = report.to_string();
                let server = &server;
                async move {
                    let answer = call(
                        server,
                        "complete_task",
                        json!({ "task_id": id, "report": report }),
                    )
                    .await;
                    assert!(!is_error_result(&answer), "{answer}");
                    serde_json::from_str::<Value>(&said(&answer)).unwrap()
                }
            };

            let first = finish("the board", "the board carries paths now").await;
            assert_eq!(first["last"], json!(false), "{first}");
            assert_eq!(first["still_open"], json!(2), "{first}");
            assert_eq!(first["task"], "the board", "{first}");

            let second = finish("the tools", "four tools, all wired").await;
            assert_eq!(second["last"], json!(false), "{second}");
            assert_eq!(second["still_open"], json!(1), "{second}");

            // The work's own instruction is a task like any other, and it is
            // the one that empties the board.
            let last = finish("port the parser", "and the parser is ported").await;
            assert_eq!(last["last"], json!(true), "{last}");
            assert_eq!(last["still_open"], json!(0), "{last}");

            let read: Value = serde_json::from_str(&said(
                &call(&server, "work_board", json!({ "work_id": work })).await,
            ))
            .unwrap();
            assert_eq!(read["finished"], json!(true), "{read}");
            assert_eq!(read["open"], json!(0), "{read}");
        }

        // ---- check 10 ------------------------------------------------------

        /// D4.4, asserted from both sides: the report is in the manager's
        /// queue, and no card exists anywhere.
        ///
        /// The card half is the one that matters. Cards cascade up the whole
        /// ancestor chain, so an engineer's routine "I finished" raised as one
        /// arrives on main's rail as well — which is the noise this change
        /// exists to remove, and it would come back the moment somebody
        /// reached for `raise_card` here because it was the nearest verb.
        ///
        /// **A task that is not the last one**, which is what "routine" means
        /// here and the whole of what this test covers. Finishing the last task
        /// on a board closes the work, and closing a work raises a card of its
        /// own that does cascade — see
        /// `the_last_report_closes_the_work_and_that_card_does_cascade`, which
        /// is the other half and is deliberately a different test.
        #[tokio::test]
        async fn a_routine_report_reaches_its_manager_and_raises_no_card() {
            let dir = std::env::temp_dir().join(format!("jod-mcp-report-{}", std::process::id()));
            let store = Arc::new(Store::in_memory().unwrap());
            let manager = with_a_manager(&store, &dir.to_string_lossy());
            let work = store.create_work_in("port the parser", None).unwrap();
            let engineer = engineer_in(&store, &work.id, Some(&manager), "run-1");
            let server = Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::ReadOnly)
                .for_run("run-1");
            let task = store.add_work_task(&work.id, "the tools").unwrap();

            let answer = call(
                &server,
                "complete_task",
                json!({ "task_id": task, "report": "four tools, all wired" }),
            )
            .await;
            assert!(!is_error_result(&answer), "{answer}");
            let said_back: Value = serde_json::from_str(&said(&answer)).unwrap();
            assert_eq!(said_back["reported_to"], json!(manager), "{said_back}");
            assert_eq!(said_back["delivered"], json!(true), "{said_back}");

            let delivered = body(&store, &manager);
            assert!(delivered.contains("four tools, all wired"), "{delivered}");
            assert!(
                delivered.contains("the tools"),
                "the manager cannot tell which task this was: {delivered}"
            );
            assert!(
                delivered.contains("work_board"),
                "the manager is not told how to check the board it now has to read: {delivered}"
            );
            assert!(
                store.pending_for(&engineer).unwrap().is_empty(),
                "the report was delivered back to its own author"
            );

            for conversation in [&manager, &engineer] {
                let cards = store
                    .cards(&Query {
                        conversation_id: Some(conversation.clone()),
                        ..Query::default()
                    })
                    .unwrap();
                assert!(
                    cards.is_empty(),
                    "reporting raised a card, which cascades to main: {cards:?}"
                );
            }

            std::fs::remove_dir_all(&dir).ok();
        }

        /// The other half, said plainly rather than left to be discovered.
        ///
        /// D4.4 stops the *routine* report travelling as a card and does not
        /// touch the cascade, deliberately: teaching `cards_in` to stop at a
        /// manager would silence a blocked engineer's question, which is the
        /// one message that must always reach Reljod. So finishing the last
        /// task still puts something on main's rail — not the report, but the
        /// work's own closing card, raised by `close_work` against the work's
        /// root session.
        ///
        /// That is within spec and it is worth a test rather than a sentence,
        /// because "an engineer's report raises no card" is true and reads as
        /// "nothing an engineer does raises a card", which is not.
        #[tokio::test]
        async fn the_last_report_closes_the_work_and_that_card_does_cascade() {
            let dir = std::env::temp_dir().join(format!("jod-mcp-closing-{}", std::process::id()));
            let store = Arc::new(Store::in_memory().unwrap());
            let manager = with_a_manager(&store, &dir.to_string_lossy());
            let work = store.create_work_in("port the parser", None).unwrap();
            let engineer = engineer_in(&store, &work.id, Some(&manager), "run-1");
            let server = Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::ReadOnly)
                .for_run("run-1");
            // The work's own instruction is its only task, so this one is the
            // last one.
            let task = store.work_tasks(&work.id).unwrap()[0].id.clone();

            let answer = call(
                &server,
                "complete_task",
                json!({ "task_id": task, "report": "the parser is ported" }),
            )
            .await;
            let said_back: Value = serde_json::from_str(&said(&answer)).unwrap();
            assert_eq!(said_back["last"], json!(true), "{said_back}");

            // The report itself still went to the manager and still raised
            // nothing.
            assert!(body(&store, &manager).contains("the parser is ported"));

            // What the manager does see on its rail is the work closing, which
            // cascaded up from the engineer's conversation. Not the report.
            let cascaded = store
                .cards(&Query {
                    subtree_of: Some(manager.clone()),
                    ..Query::default()
                })
                .unwrap();
            assert_eq!(cascaded.len(), 1, "expected only the closing card: {cascaded:?}");
            assert_eq!(cascaded[0].conversation_id, engineer);
            assert!(
                cascaded[0].title.starts_with("work "),
                "that is not the work's closing card: {:?}",
                cascaded[0].title
            );
            assert!(
                !cascaded[0].body.contains("the parser is ported"),
                "the report travelled as a card after all: {:?}",
                cascaded[0].body
            );

            std::fs::remove_dir_all(&dir).ok();
        }

        /// The nearest manager, not the first one found. An engineer that
        /// opened work of its own reports to the manager that owns the
        /// repository rather than to whatever sits above that.
        #[tokio::test]
        async fn a_report_climbs_only_as_far_as_the_nearest_manager() {
            let dir = std::env::temp_dir().join(format!("jod-mcp-nearest-{}", std::process::id()));
            let store = Arc::new(Store::in_memory().unwrap());
            let manager = with_a_manager(&store, &dir.to_string_lossy());
            let work = store.create_work_in("port the parser", None).unwrap();
            let senior = engineer_in(&store, &work.id, Some(&manager), "run-1");
            engineer_in(&store, &work.id, Some(&senior), "run-2");
            let server = Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::ReadOnly)
                .for_run("run-2");
            let task = store.add_work_task(&work.id, "the tools").unwrap();

            let answer = call(
                &server,
                "complete_task",
                json!({ "task_id": task, "report": "done" }),
            )
            .await;
            let said_back: Value = serde_json::from_str(&said(&answer)).unwrap();
            assert_eq!(
                said_back["reported_to"],
                json!(manager),
                "a report stopped at the session that spawned it instead of climbing to the \
                 manager: {said_back}"
            );
            assert!(
                store.pending_for(&senior).unwrap().is_empty(),
                "the session in between was told as well"
            );

            std::fs::remove_dir_all(&dir).ok();
        }

        // ---- check 11 ------------------------------------------------------

        /// No manager above it is the ordinary state of an engineer main
        /// started directly, and of every test. A report with no addressee is
        /// the failure this whole change exists to remove, so it goes to the
        /// parent rather than nowhere.
        #[tokio::test]
        async fn a_report_with_no_manager_above_it_goes_to_the_parent_conversation() {
            let store = Arc::new(Store::in_memory().unwrap());
            let work = store.create_work_in("port the parser", None).unwrap();
            let parent = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
                .unwrap()
                .id;
            let engineer = engineer_in(&store, &work.id, Some(&parent), "run-1");
            let server = Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::ReadOnly)
                .for_run("run-1");
            let task = store.add_work_task(&work.id, "the tools").unwrap();

            let answer = call(
                &server,
                "complete_task",
                json!({ "task_id": task, "report": "four tools, all wired" }),
            )
            .await;
            assert!(!is_error_result(&answer), "no manager was treated as a failure: {answer}");
            let said_back: Value = serde_json::from_str(&said(&answer)).unwrap();
            assert_eq!(said_back["reported_to"], json!(parent), "{said_back}");
            assert!(body(&store, &parent).contains("four tools, all wired"));
            assert!(store.pending_for(&engineer).unwrap().is_empty());
        }

        /// An empty report finishes the task and tells the manager nothing, so
        /// it is refused before the task is marked off rather than after.
        #[tokio::test]
        async fn a_report_with_nothing_in_it_is_refused_before_the_task_is_marked_done() {
            let (store, server, work, _) = on_a_work(ToolAccess::ReadOnly);
            let task = store.add_work_task(&work, "the tools").unwrap();
            let answer = call(
                &server,
                "complete_task",
                json!({ "task_id": task, "report": "   " }),
            )
            .await;
            // A malformed argument is a protocol error rather than a tool
            // result, which is how every other `BadParams` here answers.
            assert_eq!(error_code(&answer), INVALID_PARAMS, "{answer}");
            assert!(
                store
                    .work_tasks(&work)
                    .unwrap()
                    .iter()
                    .all(|t| t.status == "open"),
                "the task was completed by a call that was refused"
            );
        }

        // ---- the column an engineer is spawned onto ------------------------

        /// What `conversations.task_id` holds, read straight.
        ///
        /// A read, not a write. Nothing in these tests sets the column by
        /// hand — that is the shape of test that let a column with no writer
        /// look covered — so every value this returns was put there by
        /// [`spawn_onto_first_task`], which is the production writer.
        fn task_of(store: &Store, conversation_id: &str) -> Option<String> {
            let conn = store.conn.lock().expect("store lock poisoned");
            conn.query_row(
                "SELECT task_id FROM conversations WHERE id = ?1",
                rusqlite::params![conversation_id],
                |r| r.get(0),
            )
            .unwrap()
        }

        /// Everything opening a work does short of starting a process, which
        /// is exactly what `Server::open_work` does before it writes the
        /// column.
        ///
        /// `orchestrator::prepare_work` is the real thing — it creates the
        /// work, puts the instruction on its board, and attaches the session —
        /// and it exists as a separate function precisely so this half can be
        /// exercised without a supervisor. The half that cannot be reached from
        /// here is the spawn, and the tool's own call to
        /// [`spawn_onto_first_task`] sits on the far side of it.
        fn opened(store: &Store, instruction: &str) -> (String, String) {
            let prepared = crate::orchestrator::prepare_work(
                store,
                &crate::orchestrator::Opening::new(instruction, "/tmp"),
            )
            .unwrap();
            (prepared.work.id, prepared.conversation_id)
        }

        /// The engineer a work opens is spawned onto that work's first task,
        /// and the writer says which one it wrote.
        ///
        /// The first task is the work's own instruction — `create_work_in` puts
        /// it on the board — so a work always has one and there is no case
        /// where an engineer opens a work and owns nothing.
        #[test]
        fn opening_a_work_spawns_its_session_onto_the_works_first_task() {
            let store = Store::in_memory().unwrap();
            let (work, engineer) = opened(&store, "port the parser");
            assert_eq!(
                task_of(&store, &engineer),
                None,
                "the column starts empty, so the assertion below is about the writer"
            );

            let wrote = spawn_onto_first_task(&store, &work, &engineer, &[]);

            let board = store.work_tasks(&work).unwrap();
            assert_eq!(board.len(), 1, "a fresh work carries its instruction: {board:?}");
            assert_eq!(wrote.as_deref(), Some(board[0].id.as_str()));
            assert_eq!(task_of(&store, &engineer).as_deref(), Some(board[0].id.as_str()));
            assert_eq!(board[0].title, "port the parser");
        }

        /// The wire, proved through the thing on the other end of it.
        ///
        /// `Store::stack_for_work` ranks a pull request by its opener's task
        /// position and sorts every pull request with no task above the ranked
        /// ones. With the column written, the engineer's pull request is ranked
        /// and sits at the bottom of the stack; with the column null, both fall
        /// through to `detected_at_ms` and come back in the order they were
        /// opened.
        ///
        /// **They are opened in that failing order on purpose.** The unranked
        /// one is detected first, so a null column produces exactly the
        /// reversal this asserts against — which is what makes this a test of
        /// the writer rather than a test that two rows come back.
        #[test]
        fn the_task_a_session_is_spawned_onto_is_what_orders_its_pull_request() {
            let store = Store::in_memory().unwrap();
            let (work, engineer) = opened(&store, "port the parser");
            spawn_onto_first_task(&store, &work, &engineer, &[]);

            // A session on the same work that nobody spawned onto a task —
            // every session that existed before the column had a writer.
            let stranger = store
                .new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
                .unwrap()
                .id;
            for (number, conversation) in [(41, &stranger), (42, &engineer)] {
                store
                    .note_pull_requests(
                        &format!("https://github.com/Reljod/Jod/pull/{number}"),
                        &crate::prs::Attribution {
                            work_id: Some(work.clone()),
                            conversation_id: Some(conversation.clone()),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }

            let crate::prs::Stacking::Ready(stack) = store.stack_for_work(&work).unwrap() else {
                panic!("two pull requests are a stack");
            };
            let numbers: Vec<i64> = stack.prs.iter().map(|pr| pr.number.unwrap()).collect();
            assert_eq!(
                numbers,
                vec![42, 41],
                "the ranked pull request did not sink to the bottom of the stack, which is                  what a null `conversations.task_id` looks like: {numbers:?}"
            );
        }

        /// Continuing an engineer does not move it to a new task, so nothing on
        /// that path may touch the column.
        ///
        /// The call itself cannot get as far as a spawn from here — that needs
        /// a supervisor — and the guard does not depend on it doing so. What is
        /// asserted is that the column reads the same afterwards whatever
        /// `continue_agent` decided, which is the property a future change to
        /// that tool would break.
        #[tokio::test]
        async fn continuing_an_agent_leaves_the_task_it_was_spawned_onto_alone() {
            let store = Arc::new(Store::in_memory().unwrap());
            let (work, engineer) = opened(&store, "port the parser");
            let task = spawn_onto_first_task(&store, &work, &engineer, &[])
                .expect("a fresh work has a first task");
            store.append_prompt(&engineer, "run-1", "do the thing").unwrap();
            let server = Server::new(Jod::with_store(store.clone()))
                .with_access(ToolAccess::Delegate)
                .for_run("run-1");

            call(
                &server,
                "continue_agent",
                json!({ "run_id": "run-1", "prompt": "carry on" }),
            )
            .await;

            assert_eq!(
                task_of(&store, &engineer).as_deref(),
                Some(task.as_str()),
                "continuing an engineer moved it off the task it was spawned onto"
            );
        }

        // ---- where the manager put this engineer ---------------------------

        /// A settled `open_work` call, without a supervisor anywhere near it.
        ///
        /// `Server::opening_for` is where every placement argument is read,
        /// validated and refused, and it is split from the spawn precisely so
        /// this can be driven. A checkout is always passed: the fixture's
        /// conversation has no roots and no project, and resolving one is a
        /// different refusal from the ones being tested here.
        fn settle(args: Value) -> Result<Planned, ToolError> {
            let (_, server, _) = working(ToolAccess::Delegate);
            let raiser = server.raiser().unwrap();
            let mut args = args;
            args["instruction"] = json!("port the parser");
            // Only when the caller did not name one. The fixture's
            // conversation has no roots and no project, so a call with no
            // checkout is refused for a reason none of these tests is about —
            // but a test that brought its own git repository must keep it.
            if args.get("checkout").is_none() {
                args["checkout"] = json!("/tmp");
            }
            server.opening_for(&raiser, &args)
        }

        fn placement_of(args: Value) -> Option<crate::leases::Placement> {
            settle(args).expect("this call should have settled").opening.placement
        }

        fn refusal(args: Value) -> String {
            match settle(args) {
                Ok(planned) => panic!(
                    "this call should have been refused, and settled as {:?}",
                    planned.opening.placement
                ),
                Err(ToolError::BadParams(said)) | Err(ToolError::Refused(said)) => said,
                Err(other) => panic!("refused in the wrong shape: {other:?}"),
            }
        }

        /// Every placement the manager's brief names is one the schema will
        /// take.
        ///
        /// The brief tells every manager that `open_work` takes a placement and
        /// spells out all four. `obj()` emits no `additionalProperties: false`,
        /// so a value the schema does not offer is accepted, ignored and
        /// answered with a success — which is the exact failure this pair of
        /// lists exists to keep out.
        #[test]
        fn open_work_offers_every_placement_the_managers_brief_names() {
            let schema = tool("open_work").schema;
            let offered: Vec<&str> = schema["properties"]["placement"]["enum"]
                .as_array()
                .expect("`placement` is not an enum in the schema")
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(
                offered,
                crate::leases::PLACEMENT_IDS.to_vec(),
                "the schema and the placements have drifted apart"
            );
            for named in ["share_with", "paths"] {
                assert!(
                    schema["properties"][named].is_object(),
                    "`open_work` does not take `{named}`, so a manager passing it is ignored"
                );
            }
        }

        /// **The regression guard on the default.** Absent is `None`, not
        /// `Explore`, and the difference is a whole paragraph of the session's
        /// brief: an unplaced session is told to call `claim_worktree` when it
        /// needs to write, and an `explore` one is told it was opened to look
        /// and must report instead.
        #[tokio::test]
        async fn opening_a_work_with_no_placement_leaves_it_unplaced_rather_than_exploring() {
            assert_eq!(placement_of(json!({})), None);
        }

        #[tokio::test]
        async fn every_placement_a_manager_can_name_reaches_the_opening() {
            use crate::leases::Placement;
            assert_eq!(
                placement_of(json!({ "placement": "explore" })),
                Some(Placement::Explore)
            );
            assert_eq!(
                placement_of(json!({ "placement": "worktree" })),
                Some(Placement::Worktree)
            );
            assert_eq!(
                placement_of(json!({ "placement": "share", "share_with": "work-1" })),
                Some(Placement::Share { work_id: "work-1".into() })
            );
        }

        /// `share` means joining a worktree that belongs to a named work, so a
        /// share with nobody to share with is refused rather than turned into
        /// an empty work id that fails later somewhere less explicable.
        #[tokio::test]
        async fn sharing_without_saying_whose_worktree_is_refused_by_name() {
            let said = refusal(json!({ "placement": "share" }));
            assert!(said.contains("share_with"), "{said}");
            assert!(
                said.contains("worktree"),
                "the refusal does not offer the alternative: {said}"
            );
        }

        /// The mirror of it, and the one that would otherwise be silent. A
        /// manager that names a lender and forgets the placement gets an
        /// ordinary engineer on a branch of its own, is told the work opened,
        /// and has nothing in the answer to read that from.
        #[tokio::test]
        async fn naming_a_worktree_to_join_without_asking_to_share_is_refused() {
            let said = refusal(json!({ "share_with": "work-1" }));
            assert!(said.contains("share_with"), "{said}");
            assert!(said.contains("share"), "{said}");
        }

        /// A misspelt placement is refused rather than read as the nearest
        /// thing. Quietly becoming `explore` would leave an engineer that was
        /// meant to write with no writable root, and the first anyone heard of
        /// it would be that engineer reporting it could not do its task.
        #[tokio::test]
        async fn a_placement_nobody_defined_is_refused_and_lists_the_ones_that_exist() {
            let said = refusal(json!({ "placement": "wherever" }));
            assert!(said.contains("wherever"), "{said}");
            for id in crate::leases::PLACEMENT_IDS {
                assert!(said.contains(id), "the refusal leaves out `{id}`: {said}");
            }
        }

        /// Paths are tidied into the one form everything downstream compares,
        /// and the two shapes that would make ownership unenforceable are
        /// refused at the boundary rather than stored.
        #[tokio::test]
        async fn the_files_an_engineer_owns_are_normalised_and_the_unenforceable_ones_refused() {
            let planned = settle(json!({ "paths": ["./core/src/mcp.rs", "cli/src/tui/"] }))
                .expect("both of those are ordinary prefixes");
            assert_eq!(planned.paths, vec!["core/src/mcp.rs", "cli/src/tui"]);

            let absolute = refusal(json!({ "paths": ["/Users/reljod/Jod/core"] }));
            assert!(absolute.contains("/Users/reljod/Jod/core"), "{absolute}");
            let escaping = refusal(json!({ "paths": ["core/../../elsewhere"] }));
            assert!(escaping.contains("elsewhere"), "{escaping}");
        }

        /// A git checkout, made where a test may make one.
        fn a_checkout(name: &str) -> std::path::PathBuf {
            let dir = std::env::temp_dir().join(format!("jod-mcp-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "--quiet"]);
            std::fs::canonicalize(&dir).unwrap()
        }

        fn git(dir: &std::path::Path, args: &[&str]) {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git is on this machine");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        }

        /// `direct` on a repository that satisfies all three conditions is
        /// allowed through, which is the half that proves the gate is a gate
        /// rather than a refusal wearing one.
        #[tokio::test]
        async fn direct_is_allowed_in_a_fresh_clean_checkout_with_no_remote() {
            let dir = a_checkout("direct-ok");
            let planned = settle(json!({
                "placement": "direct",
                "checkout": dir.to_string_lossy(),
            }))
            .expect("no remote, no other work, nothing uncommitted");
            assert_eq!(planned.opening.placement, Some(crate::leases::Placement::Direct));
            std::fs::remove_dir_all(&dir).ok();
        }

        /// Check 21. Every failing condition at once, and `worktree` named as
        /// the thing to ask for instead.
        ///
        /// All of them together rather than the first, because a manager told
        /// only about the remote fixes that, asks again, and is then told about
        /// the uncommitted file — two turns spent learning two facts that were
        /// both true at the same moment.
        #[tokio::test]
        async fn direct_on_a_checkout_that_fails_its_conditions_is_refused_with_all_of_them() {
            let dir = a_checkout("direct-no");
            git(&dir, &["remote", "add", "origin", "https://github.com/Reljod/Jod.git"]);
            std::fs::write(dir.join("scratch.txt"), "uncommitted").unwrap();

            let said = refusal(json!({
                "placement": "direct",
                "checkout": dir.to_string_lossy(),
            }));
            assert!(said.contains("remote"), "the remote is not named: {said}");
            assert!(
                said.contains("uncommitted") || said.contains("scratch.txt"),
                "the dirty tree is not named: {said}"
            );
            assert!(
                said.contains("worktree"),
                "the refusal does not say what to ask for instead: {said}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// The files a manager gave an engineer land on the task it was spawned
        /// onto, and come back through the reader the rest of the crate uses.
        ///
        /// **The round trip is the assertion.** `works::paths_to_column` is
        /// private to its module, so the encoder here is a second one, and two
        /// encoders that agree on paper are two that diverge the first time one
        /// changes. `work_tasks` decodes with `paths_from_column`, so this
        /// fails the moment they stop matching.
        #[test]
        fn the_files_an_engineer_owns_are_recorded_on_the_task_it_is_spawned_onto() {
            let store = Store::in_memory().unwrap();
            let (work, engineer) = opened(&store, "port the parser");
            let owned = vec!["core/src/mcp.rs".to_string(), "cli/src/tui".to_string()];

            let task = spawn_onto_first_task(&store, &work, &engineer, &owned)
                .expect("a fresh work has a first task");

            let board = store.work_tasks(&work).unwrap();
            assert_eq!(board[0].id, task);
            assert_eq!(
                board[0].paths, owned,
                "the paths did not survive the column: {board:?}"
            );
        }

        /// An engineer that owns nothing writes nothing to the column, which is
        /// the honest state for anything exploratory and the state every task
        /// written before there were paths is already in.
        #[test]
        fn an_engineer_given_no_files_leaves_its_task_claiming_nothing() {
            let store = Store::in_memory().unwrap();
            let (work, engineer) = opened(&store, "read the harness docs");
            spawn_onto_first_task(&store, &work, &engineer, &[]);
            assert!(store.work_tasks(&work).unwrap()[0].paths.is_empty());
        }

        // ---- checks 29 and 30 ----------------------------------------------

        /// One pull request is not a stack, and the tool says so in the
        /// module's own words rather than in a second set of them.
        #[tokio::test]
        async fn stacking_a_work_with_one_pull_request_is_refused_and_says_it_found_one() {
            let (store, server, work, _) = on_a_work(ToolAccess::Delegate);
            store
                .note_pull_requests(
                    "https://github.com/Reljod/Jod/pull/41",
                    &crate::prs::Attribution {
                        work_id: Some(work.clone()),
                        ..Default::default()
                    },
                )
                .unwrap();

            let answer = call(&server, "stack_pull_requests", json!({ "work_id": work })).await;
            assert!(is_error_result(&answer), "a stack of one was linked: {answer}");
            assert_eq!(
                said(&answer),
                crate::prs::stack_refusal(1),
                "the tool wrote its own refusal instead of using the module's"
            );
        }

        /// Plan order, not finish order, all the way out through the tool.
        ///
        /// The pull requests are opened backwards on purpose. Opening them in
        /// plan order passes against a timestamp sort too, so it would prove an
        /// ordering exists without proving it came from the plan — which is the
        /// only part worth asserting, because finish order is the plausible
        /// wrong answer and it produces a stack whose bases are wrong while
        /// looking perfectly fine.
        #[tokio::test]
        async fn the_stack_a_manager_is_handed_is_in_plan_order_not_finish_order() {
            let (store, server, work, _) = on_a_work(ToolAccess::Delegate);
            let board = planned(&store, &work, &["the board", "placement", "stacking"]);

            // One engineer per planned task, and they finish 3, 1, 2.
            for (number, title) in [(43, "stacking"), (41, "the board"), (42, "placement")] {
                let conversation = store
                    .new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
                    .unwrap()
                    .id;
                store
                    .write(|tx| {
                        tx.execute(
                            "UPDATE conversations SET task_id = ?2 WHERE id = ?1",
                            rusqlite::params![conversation, task_called(&board, title)],
                        )?;
                        Ok(())
                    })
                    .unwrap();
                store
                    .note_pull_requests(
                        &format!("https://github.com/Reljod/Jod/pull/{number}"),
                        &crate::prs::Attribution {
                            work_id: Some(work.clone()),
                            conversation_id: Some(conversation),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }

            let answer = call(&server, "stack_pull_requests", json!({ "work_id": work })).await;
            assert!(!is_error_result(&answer), "{answer}");
            let stacked: Value = serde_json::from_str(&said(&answer)).unwrap();
            let numbers: Vec<i64> = stacked["bottom_to_top"]
                .as_array()
                .unwrap()
                .iter()
                .map(|pr| pr["number"].as_i64().unwrap())
                .collect();
            assert_eq!(
                numbers,
                vec![41, 42, 43],
                "the stack came back in finish order: {stacked}"
            );
            assert_eq!(stacked["count"], json!(3), "{stacked}");
            assert_eq!(
                stacked["command"], "gh stack link 41 42 43",
                "the command takes its argument order literally: {stacked}"
            );
            assert!(
                stacked["instruction"]
                    .as_str()
                    .unwrap()
                    .contains("base branch"),
                "the manager is not warned that linking rewrites a base: {stacked}"
            );
        }
    }
}
