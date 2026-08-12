//! The main chat: the one conversation that is always there.
//!
//! Every other conversation is a thread about a task. This one is the desk you
//! sit at. Instructions arrive here, and **it never does the work** — it
//! decides who does, hands the job over, and comes straight back to you.
//!
//! ## Why it is not simply an agent
//!
//! Hermes' main chat is a model with tools: it thinks and acts in one loop.
//! Jod cannot be that, and the charter is explicit about why — `jod-core` has
//! no model client, no prompt templates and no tools. So the orchestrator is
//! split along the same seam as everything else here:
//!
//! - **The thinking is delegated.** Deciding whether "fix the CI failure"
//!   belongs to the agent already looking at CI, or to a fresh one, is a
//!   judgement, and judgement happens inside a harness.
//! - **The effects are Jod's.** Spawning, scheduling, and writing the record of
//!   what was decided are things Jod does, from a parsed answer, under rules
//!   the router cannot talk its way past.
//!
//! That split is what makes the router's output *data*. It proposes; Jod
//! disposes. A router that asked for a permission Jod would not have granted is
//! refused by the same code that would refuse it from a webhook payload.
//!
//! ## Non-blocking, which is the whole point
//!
//! Sending an instruction returns as soon as the work has been *handed over*,
//! not when it is finished. A main chat that blocked on the task would be a
//! chat you cannot use while anything is happening — which is precisely when
//! you most want it, because that is when you want to ask for something else.
//!
//! ## Context, and the two clocks that bound it
//!
//! A resident chat grows for ever unless something bounds it, and the two
//! things that bound it are different clocks:
//!
//! - **Size.** Past a threshold the transcript costs more to carry than it is
//!   worth. This is the obvious trigger and the less useful one.
//! - **Your silence.** A conversation you have not touched for a day has
//!   almost certainly moved on, and the right moment to summarise is *before*
//!   the next thing starts rather than in the middle of it. Measured from
//!   `last_human_ms` rather than `updated_at_ms`, because six agents writing
//!   into the chat overnight is not you being present.
//!
//! Compaction itself is a delegated run too, for the same reason routing is.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{JodError, Result};
use crate::harness::{HarnessKind, PermissionPolicy, Resume, SpawnRequest, ToolAccess};
use crate::roots::{NewRoot, Root};
use crate::secrets::SecretMeta;
use crate::service::{AgentSummary, Jod, RunConversation};
use crate::store::Store;

/// How much transcript the main chat carries before it is worth compacting.
///
/// Characters rather than tokens because Jod cannot count tokens without a
/// model's tokeniser, and a character budget that is roughly right beats a
/// token budget that is exactly wrong about a model nobody told us we were
/// using. ~24k characters is on the order of 6-8k tokens: large enough that
/// ordinary use never trips it, small enough that a week of instructions does.
pub const COMPACT_CHARS: usize = 24_000;

/// How long the chat may sit without you before it is compacted.
///
/// A day. The point is to summarise at a natural break — after you have
/// stopped, before you start again — rather than mid-thought, and a day is
/// long enough that a night's sleep does not count as a break in the middle of
/// something.
pub const COMPACT_IDLE_MS: i64 = 24 * 60 * 60 * 1000;

/// Never compact a chat with less than this much in it.
///
/// Below this there is nothing to save and a summary would cost more than the
/// transcript it replaces — and, worse, it would replace specifics with prose
/// at exactly the point where the specifics are all there is.
pub const COMPACT_FLOOR_CHARS: usize = 4_000;

/// Why the main chat is due for compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactReason {
    /// The transcript got large.
    Size,
    /// You have not said anything for a while, so this is a natural break.
    Idle,
}

impl CompactReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            CompactReason::Size => "size",
            CompactReason::Idle => "idle",
        }
    }
}

/// Whether the main chat should be compacted, and why.
///
/// Pure, so the policy is testable without a database or a clock. `chars` is
/// the live window's size; `last_human_ms` is when you last said something.
pub fn should_compact(
    chars: usize,
    last_human_ms: Option<i64>,
    now_ms: i64,
) -> Option<CompactReason> {
    // The floor wins over both triggers. A short chat that has been quiet for a
    // week still has nothing worth summarising, and compacting it would trade
    // four exact sentences for one vague one.
    if chars < COMPACT_FLOOR_CHARS {
        return None;
    }
    if chars >= COMPACT_CHARS {
        return Some(CompactReason::Size);
    }
    // Idle is measured from *your* last message. A chat six agents wrote into
    // overnight has not been attended to, however much it moved.
    match last_human_ms {
        Some(at) if now_ms.saturating_sub(at) >= COMPACT_IDLE_MS => Some(CompactReason::Idle),
        _ => None,
    }
}

/// What the router decided to do with an instruction.
///
/// Deliberately small. Every variant is something Jod already knows how to do,
/// because a router that can propose an action Jod cannot perform is a router
/// that can fail in a way nobody can see coming.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Decision {
    /// Hand it to an agent already running.
    ///
    /// The interesting case, and the one a fresh-agent-per-instruction design
    /// gets wrong: "and also check the tests" belongs to the agent already
    /// holding that context, not to a stranger who would have to rebuild it.
    DelegateExisting {
        run_id: String,
        prompt: String,
        #[serde(default)]
        reason: String,
    },
    /// Start a new agent.
    DelegateNew {
        prompt: String,
        #[serde(default)]
        harness: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        reason: String,
    },
    /// Put it on the clock instead of doing it now.
    Schedule {
        name: String,
        prompt: String,
        cron: String,
        #[serde(default)]
        timezone: Option<String>,
        #[serde(default)]
        reason: String,
    },
    /// Make it a standing objective.
    Goal {
        name: String,
        objective: String,
        #[serde(default)]
        cron: Option<String>,
        #[serde(default)]
        reason: String,
    },
    /// Answer without delegating.
    ///
    /// For "what is running?" and "cancel that" — questions about Jod rather
    /// than work for an agent. Without this the router would have to spawn a
    /// whole agent to answer a question Jod can already answer.
    Reply { text: String },
}

impl Decision {
    pub fn kind(&self) -> &'static str {
        match self {
            Decision::DelegateExisting { .. } => "delegate_existing",
            Decision::DelegateNew { .. } => "delegate_new",
            Decision::Schedule { .. } => "schedule",
            Decision::Goal { .. } => "goal",
            Decision::Reply { .. } => "reply",
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Decision::DelegateExisting { reason, .. }
            | Decision::DelegateNew { reason, .. }
            | Decision::Schedule { reason, .. }
            | Decision::Goal { reason, .. } => reason,
            Decision::Reply { .. } => "",
        }
    }
}

/// One agent the router may hand work to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub run_id: String,
    pub name: String,
    pub harness: String,
    /// What it last said, which is the only summary of what it is holding.
    pub last: Option<String>,
    pub age_ms: i64,
}

/// Read the router's answer.
///
/// The router is an agent, so its output is prose that *contains* an answer
/// rather than being one. Three defences, each for a failure seen in the
/// extraction work already in `consolidate.rs`:
///
/// - A fenced block is unwrapped, because models fence JSON whatever they are
///   asked.
/// - The **last** decodable object wins, because a model that reconsiders
///   emits its correction second, and taking the first would act on a draft.
/// - An unparseable answer is an error, never a guess. The alternative is
///   inventing a delegation nobody asked for, and a wrong delegation spends
///   money and touches a repository.
pub fn parse_decision(output: &str) -> Result<Decision> {
    let cleaned = strip_fences(output);
    let mut found: Option<Decision> = None;

    // Scan every balanced `{…}` span rather than trying to find "the" object.
    // Prose around JSON is normal; JSON inside prose is what we want.
    let bytes: Vec<char> = cleaned.chars().collect();
    for start in 0..bytes.len() {
        if bytes[start] != '{' {
            continue;
        }
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for end in start..bytes.len() {
            let c = bytes[end];
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let span: String = bytes[start..=end].iter().collect();
                        if let Ok(d) = serde_json::from_str::<Decision>(&span) {
                            found = Some(d);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    found.ok_or_else(|| {
        JodError::Invalid(
            "the router did not answer with a decision this build understands".into(),
        )
    })
}

/// Drop a surrounding code fence, if there is one.
fn strip_fences(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    trimmed
        .lines()
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The instruction handed to the router.
///
/// Everything it needs to choose, and nothing it could act on directly. It is
/// told what is running because reusing a warm agent is the decision that
/// matters most, and it is told the shape of the answer because the parse is
/// the contract.
pub fn router_prompt(instruction: &str, running: &[Candidate], recent: &[String]) -> String {
    let mut p = String::from(
        "You are the routing half of an orchestrator. You do not do the work. \
         You decide who does, and you answer with one JSON object and nothing \
         else.\n\n",
    );

    p.push_str("Agents running right now:\n");
    if running.is_empty() {
        p.push_str("  (none)\n");
    } else {
        for c in running {
            p.push_str(&format!(
                "  - run_id {} · {} on {} · started {}s ago\n    last said: {}\n",
                c.run_id,
                c.name,
                c.harness,
                c.age_ms / 1000,
                c.last.as_deref().unwrap_or("(nothing yet)")
            ));
        }
    }

    if !recent.is_empty() {
        p.push_str("\nRecently in this chat:\n");
        for line in recent {
            p.push_str(&format!("  {line}\n"));
        }
    }

    p.push_str(&format!("\nThe instruction:\n{instruction}\n"));
    p.push_str(
        "\nChoose exactly one:\n\
         {\"action\":\"delegate_existing\",\"run_id\":\"…\",\"prompt\":\"…\",\"reason\":\"…\"}\n\
         {\"action\":\"delegate_new\",\"prompt\":\"…\",\"harness\":\"claude_code|open_code|agy\",\"name\":\"…\",\"reason\":\"…\"}\n\
         {\"action\":\"schedule\",\"name\":\"…\",\"prompt\":\"…\",\"cron\":\"0 2 * * *\",\"timezone\":\"…\",\"reason\":\"…\"}\n\
         {\"action\":\"goal\",\"name\":\"…\",\"objective\":\"…\",\"cron\":\"0 * * * *\",\"reason\":\"…\"}\n\
         {\"action\":\"reply\",\"text\":\"…\"}\n\n\
         Prefer an agent already running when the instruction continues what it \
         is already holding — it has the context and a new agent would have to \
         rebuild it. Prefer a schedule or a goal when the instruction says when \
         or says keep. Reply only for questions about Jod itself.\n",
    );
    p
}

/// The framing that turns a harness run into the orchestrator.
///
/// It gets Jod's tools over MCP, so it delegates by *calling* rather than by
/// describing — which is why this says so little about format and so much about
/// posture. The earlier design asked for a JSON decision and parsed it; that
/// allowed exactly one decision per turn and could not ask a follow-up question
/// before choosing. With tools, adding a capability is adding a tool.
pub fn orchestrator_preamble() -> &'static str {
    "You are Jod's main chat: Reljod's orchestrator.\n\n\
     **You do not do the work.** You decide who does, hand it over, and come \
     straight back. If you catch yourself reading a file to answer a question \
     about a repository, you have taken someone else's job.\n\n\
     You have Jod's own tools. Use them:\n\
     - `list_agents` **first**, almost always. Reusing an agent that is already \
       holding the context beats starting one that has to rebuild it, and it is \
       the decision that matters most.\n\
     - `continue_agent` when the instruction carries on what a run is already \
       doing. `delegate` when it does not.\n\
     - `schedule_create` when the instruction says *when*. `goal_create` when it \
       says *keep* or *until*.\n\
     - `recall` and `related` before asking Reljod something he has already told \
       you.\n\n\
     Answer in one or two sentences: what you did with it, and who has it now. \
     Say plainly when you delegated to an existing run rather than a new one, \
     and why — a routing decision nobody can see is one nobody can correct."
}

// ---- what a worker is told ------------------------------------------------

/// Everything a worker session needs to know at spawn that is true of *it*
/// rather than of Jod.
///
/// Gathered by the caller rather than read here, because the preamble has to be
/// renderable without a database: what it says is a decision worth testing on
/// its own, and a builder that read the store would only be testable by writing
/// to one.
#[derive(Debug, Clone)]
pub struct Brief<'a> {
    pub harness: HarnessKind,
    /// In the conversation's own order. The first is where an unqualified
    /// mention resolves.
    pub roots: &'a [Root],
    /// Names and hints, never values — [`SecretMeta`] cannot carry one.
    pub secrets: &'a [SecretMeta],
    /// Who this session can reach on the bus, as the roster spells them.
    pub peers: &'a [String],
    /// How much of Jod this session holds. `None` is a run launched without
    /// Jod's MCP server at all, which changes what it can be told to do — see
    /// [`preamble_lines`].
    pub tools: Option<ToolAccess>,
}

/// One line of a worker's brief, and who gets it.
///
/// The body is asserted identical across harnesses (E6.S1, G6.S4), so a line
/// that is *not* identical has to say so in the type rather than in a comment
/// somebody can forget to write. `why` is the measurement that forced it, and
/// `preamble_lines` is where to look for the ones that exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreambleLine {
    pub text: String,
    /// `None` when every harness is told this.
    pub only: Option<HarnessKind>,
    /// Why this line exists for one harness and not the others. Empty when it
    /// is shared.
    pub why: &'static str,
}

impl PreambleLine {
    fn shared(text: impl Into<String>) -> PreambleLine {
        PreambleLine {
            text: text.into(),
            only: None,
            why: "",
        }
    }

    fn only(harness: HarnessKind, why: &'static str, text: impl Into<String>) -> PreambleLine {
        PreambleLine {
            text: text.into(),
            only: Some(harness),
            why,
        }
    }
}

/// The framing every worker session gets, whatever harness runs it.
///
/// One preamble rather than one per harness, because the whole point of the
/// design is that the *experience* is Jod's rather than any harness's: the
/// rail, the roots, the secrets and the bus behave identically on all three, so
/// what an agent is told about them must too. Where a harness genuinely differs
/// — measured, in `docs/harness-support.md`, never assumed — the difference is
/// one tagged line, and the test above holds the rest identical.
pub fn worker_preamble(brief: &Brief) -> String {
    preamble_lines(brief)
        .into_iter()
        .filter(|line| line.only.is_none_or(|k| k == brief.harness))
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The preamble as tagged lines, so a test can hold the shared body still.
pub fn preamble_lines(brief: &Brief) -> Vec<PreambleLine> {
    let mut out = vec![PreambleLine::shared(
        "You are a session Jod started, working for Reljod. Jod is not a model — it holds \
         your roots, your credentials, your questions and the other sessions, and it is \
         watching a rail beside your output that Reljod reads. Everything below is how you \
         reach him and what you may touch.\n",
    )];

    out.extend(roots_lines(brief));
    out.extend(secrets_lines(brief));
    out.extend(rail_lines(brief));
    out.extend(bus_lines(brief));

    out.push(PreambleLine::shared(
        "\n## What is already written down\n\n\
         At the top of each root, read `AGENTS.md` or `CLAUDE.md` before you act in it: it is \
         the charter for that repository and it outranks your own habits. Procedures live in \
         `.agents/skills/`, `.claude/skills/` and `.claude/commands/` under the same roots — a \
         skill is instructions to follow, not a document to skim, and one that exists for the \
         job you were given is the job's answer.",
    ));
    out.push(PreambleLine::shared(
        "\nA check that cannot pass is a **blocked** ending, not a puzzle. Never skip, delete \
         or weaken a test, swap a real integration for a mock, widen an error to swallow a \
         failure, or narrow a check to the part that already passes. Write down what is \
         missing, what you tried and what you need, and stop — blocked is a successful \
         ending here.",
    ));
    out
}

fn roots_lines(brief: &Brief) -> Vec<PreambleLine> {
    let mut out = vec![PreambleLine::shared("## Where you may work\n")];
    if brief.roots.is_empty() {
        out.push(PreambleLine::shared(
            "Nobody has given this session a directory. Say so rather than picking one — a \
             session that guesses where it is working writes into somewhere nobody is looking.",
        ));
        return out;
    }
    for root in brief.roots {
        out.push(PreambleLine::shared(format!(
            "- `{}` — {}",
            root.path.display(),
            if root.writable {
                "**writable**, a worktree claimed for this work"
            } else {
                "**read-only**"
            }
        )));
    }
    // D5, stated as the rule it is rather than as a description of the flags.
    out.push(PreambleLine::shared(
        "\nA read-only root is Reljod's real checkout, and he may be editing it while you \
         read it. **Before you change, create, move or delete anything, claim a worktree.** \
         Claiming cuts a branch of your own and gives you one writable root; the checkout \
         stays beside it, readable, so you can still diff against what he is doing. A sibling \
         session on the same repository in this work is offered the worktree you claimed \
         before a second one is cut, so ask before you cut.",
    ));
    out.push(PreambleLine::shared(
        "This is a convention, not a sandbox. Nothing here stops you writing outside your \
         roots; a write that lands in a read-only root raises a card with your name on it \
         rather than being quietly kept, which is worse for you than asking.",
    ));
    if brief.roots.len() > 1 {
        out.push(PreambleLine::only(
            HarnessKind::OpenCode,
            // Measured: `opencode run` takes exactly one `--dir`, and a second
            // one crashes the process before any model call. See
            // docs/harness-support.md — this line is where those roots exist at
            // all under OpenCode.
            "OpenCode accepts exactly one --dir and repeating it is a hard error, so its \
             other roots reach the agent as prose or not at all",
            "Only the first of those directories was passed to OpenCode as a flag — it takes \
             one and refuses a second. The rest are yours to read by absolute path; you were \
             simply not handed them.",
        ));
    }
    out.push(PreambleLine::only(
        HarnessKind::Agy,
        // Measured: AGY builds its workspace purely from `--add-dir` and its
        // own settings; the shell's working directory is not in it.
        "AGY builds its workspace from --add-dir alone, so the shell's working directory is \
         not in it and an agent that assumes otherwise writes into a scratch directory",
        "Your workspace is exactly the directories above. The directory this process started \
         in is not one of them, so a relative path is not what you think it is — use absolute \
         ones.",
    ));
    out
}

fn secrets_lines(brief: &Brief) -> Vec<PreambleLine> {
    let mut out = vec![PreambleLine::shared("\n## Credentials\n")];
    if brief.secrets.is_empty() {
        out.push(PreambleLine::shared(
            "You have none. If the job needs one, ask for it by name and stop — see below.",
        ));
    } else {
        for secret in brief.secrets {
            out.push(PreambleLine::shared(match secret.hint.trim() {
                "" => format!("- `${}`", secret.name),
                hint => format!("- `${}` — {hint}", secret.name),
            }));
        }
        out.push(PreambleLine::shared(
            "\nThose are **environment variables**, already in your process environment. You \
             are told the names and never the values, and you cannot get at a value: Jod \
             injects it at spawn and scrubs it back out of everything you print, so echoing \
             one gets you the redaction marker and nothing else. Do not try — it wastes a \
             turn and puts a marker where a value should be in your own transcript.",
        ));
    }
    // E3.S5, and the sentence the whole of D3 exists to make sayable.
    out.push(PreambleLine::shared(
        "\nA credential you do not have is a **blocked** ending, never a reason to invent \
         one. Do not guess a key, do not read one out of another project, do not stub the \
         call out to get past it. Ask for it by name, say what is blocked, and stop.",
    ));
    out
}

fn rail_lines(brief: &Brief) -> Vec<PreambleLine> {
    let mut out = vec![PreambleLine::shared("\n## Telling Reljod things\n")];
    let Some(_) = brief.tools else {
        // Honest rather than aspirational: this run has no Jod tools, so
        // naming them would send it hunting for something that is not there.
        // The lifter is what makes the rail work anyway.
        out.push(PreambleLine::shared(
            "This session was started without Jod's own tools, so you have no way to write to \
             the rail directly. Jod reads your output instead and lifts your questions and \
             plan approvals onto it, so ask in whatever way your harness normally asks and it \
             will reach him. Say what you decided in your prose, with the alternatives, for \
             the same reason.",
        ));
        return out;
    };
    out.push(PreambleLine::shared(
        "Everything you want a person to see goes on the rail, through these. All three \
         return at once — nobody has to be looking, and none of them stops you working:\n\n\
         - `record_decision` — the moment you choose between real alternatives: a library, a \
           schema, an approach. Give the options you chose between, not only the winner; \
           Reljod overrules a decision that carries its alternatives by pressing a number, \
           and one that does not costs a conversation.\n\
         - `ask_question` — something you cannot work out and he can. It returns a card id \
           and the answer arrives in a later turn, so ask and carry on with whatever does \
           not depend on it. Mark it blocking only when you genuinely cannot proceed; that \
           waits, and even then it gives up rather than hanging.\n\
         - `request_secret` — a credential you need, **by name**. It cannot carry a value and \
           you will never be shown one.\n",
    ));
    out.push(PreambleLine::shared(
        "Prefer deciding and recording it to asking. You were delegated to because you can \
         decide; a question is right when the answer is a preference only Reljod holds, and \
         wrong when it is a judgement you were sent to make.",
    ));
    out
}

fn bus_lines(brief: &Brief) -> Vec<PreambleLine> {
    let mut out = vec![PreambleLine::shared("\n## The other sessions\n")];
    if brief.tools.is_none() {
        out.push(PreambleLine::shared(
            "You cannot reach them from this session — it holds none of Jod's tools. Work \
             alone, and leave anything another session needs to know in your final answer.",
        ));
        return out;
    }
    match brief.peers {
        [] => out.push(PreambleLine::shared(
            "Nobody else is on this work yet. If Jod starts one beside you, it appears in \
             `roster` — read it before you assume you are alone.",
        )),
        peers => out.push(PreambleLine::shared(format!(
            "Reachable from here: {}. `roster` is the current list, with who is idle and who \
             already has mail waiting.",
            peers
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
    // G6.S1 and G6.S3, in the order they matter to a session that is about to
    // interrupt somebody.
    out.push(PreambleLine::shared(
        "\n- **Read your inbox before you ask anything.** `read_messages` hands over what is \
           waiting, once. A question somebody already answered costs two turns and reads as \
           not listening.\n\
         - **A message costs a turn of theirs.** Jod wakes the recipient to read it, and that \
           is money spent now. Send when they need it, not when it would be tidy.\n\
         - `send_message` to tell, `ask` when you cannot continue without the answer — it \
           waits, with a deadline, and tells you plainly when nobody replied. `reply` keeps \
           an exchange in one thread. `handoff` moves a task and tells them in one call.\n\
         - **Ownership of code is a lease, not an announcement.** Claim the worktree; do not \
           send a message saying you are editing something. Two agents who both announced it \
           are two agents editing it.\n\
         - **Report up, ask sideways.** Anything for Reljod — what you found, what you \
           decided, what is blocked — goes on a card. A question for a peer goes on the bus. \
           Put a finding on the bus and nobody with authority sees it; put a question to a \
           peer on a card and it waits for a person who was never the right one to ask.",
    ));
    out
}

/// What [`hand_to_orchestrator`] did, for a caller that has its own way of
/// saying so. The CLI prints; the TUI pushes a notice; the Telegram bridge
/// edits a progress bubble.
pub struct Handed {
    pub agent: AgentSummary,
    /// `(reason, chars)` when the live window has grown past a threshold.
    pub compaction_due: Option<(&'static str, usize)>,
    /// The main chat this landed in. Returned rather than looked up again by
    /// the caller, because "the pinned conversation" is a get-or-create and a
    /// second resolution is a second chance to disagree with the first.
    pub conversation_id: String,
}

/// Give an instruction to the pinned main chat.
///
/// **Every way into the main chat comes through here.** `jod main`, the TUI's
/// `/main`, and the Telegram bridge all call this one function, because "which
/// conversation, which tools, which permission mode" is a set of decisions with
/// four bugs already behind it, and a second copy would be a second place for
/// the fifth to hide. It lives in `core` rather than in the CLI for exactly that
/// reason: the bridge is here, and a bridge that could not reach this function
/// would have had to grow its own version of it.
///
/// `carried` is prior context the harness has no session for: after `/harness`,
/// the pin moves to a conversation the target has never seen, so the summary of
/// what came before has to travel in the framing or it is lost. `None` on every
/// ordinary turn, where the harness's own session is holding the thread — and
/// `None` from the Telegram bridge, which has no thread state of its own: a
/// switch happens in the TUI, which holds the summary and passes it on its own
/// next turn.
///
/// `run_name` is the other thing a caller varies, and it is cosmetic — the name
/// a run answers to in `jod ls`. The console passes `main`; the bridge passes the
/// chat's [`crate::telegram::session_key`] so a listing says which phone chat
/// started a run. Everything load-bearing is fixed here.
pub async fn hand_to_orchestrator(
    jod: &Jod,
    instruction: &str,
    kind: HarnessKind,
    cwd: PathBuf,
    carried: Option<String>,
    run_name: &str,
) -> Result<Handed> {
    let store = jod.store().ok_or(JodError::StoreRequired)?;
    let id = store.main_conversation(kind, &cwd.display().to_string())?;
    let now = chrono::Utc::now().timestamp_millis();

    // Compaction is checked before the instruction goes out, not after: the
    // right moment to summarise is *between* things, and doing it mid-turn
    // would mean the turn that triggered it ran against the old window anyway.
    let live = store.live_window(&id)?;
    let chars: usize = live.iter().map(|m| m.text.len()).sum();
    let compaction_due = should_compact(chars, store.last_human_ms(&id)?, now)
        .map(|reason| (reason.as_str(), chars));

    // The orchestrator is a harness run holding Jod's own tools, so it
    // delegates by calling them rather than by describing what it would do.
    // `Resume` keeps it one conversation across restarts.
    //
    // `spawn_agent_in(.., Existing)` and not `spawn_agent`: the plain form
    // binds `RunConversation::New`, which minted a *second* conversation per
    // instruction — unpinned, titled with the first line of the preamble, and
    // holding the entire transcript, while the pinned `main` conversation this
    // function had just fetched stayed empty. `jod main` read the pinned one
    // and truthfully reported nothing there. A main chat that does not
    // accumulate is not a chat.
    let agent = jod
        .spawn_agent_in(
            SpawnRequest {
                name: run_name.to_string(),
                harness: kind,
                prompt: instruction.to_string(),
                // The preamble, then whatever a harness switch left the new
                // harness needing. That order and not the other one: the
                // preamble is the standing brief — who this run is and what its
                // verbs are — and the summary is material it applies to. Leading
                // with a transcript would have the model reading history before
                // it knows what it is reading it for.
                system: Some(match &carried {
                    Some(context) => format!("{}\n\n{context}", orchestrator_preamble()),
                    None => orchestrator_preamble().to_string(),
                }),
                cwd,
                model: None,
                // Not `Ask`. `Ask` is plan mode, and plan mode refuses every
                // mutation — including the MCP tool calls that *are* this run's
                // entire job. Caught by running it: the orchestrator dutifully
                // called `schedule_list`, `list_agents` and `recall`, then reached
                // for `ExitPlanMode`, could not find it, and wrote a plan file
                // instead of arming the schedule it had been asked for.
                //
                // Its confinement is `ToolAccess`, not the permission mode. The
                // mutations that matter here are Jod's own verbs, and those are
                // already scoped by the access level; the permission axis bounds
                // what it may do to the *machine*, which for a chat that only
                // delegates should be little — but it cannot be nothing, or it
                // cannot delegate at all.
                permission: PermissionPolicy::AcceptEdits,
                resume: store.resume_for(&id)?,
                tools: Some(ToolAccess::Orchestrate),
                ..SpawnRequest::default()
            },
            RunConversation::Existing(id.clone()),
        )
        .await?;

    // The spawn already recorded the instruction as this conversation's user
    // turn; this call is what returns its id, and appending is keyed to the run
    // so the second call finds the first row rather than writing a duplicate.
    let message = store.append_prompt(&id, &agent.id, instruction)?;
    store.touch_human(&id, now)?;
    store.record_delegation(&Delegation {
        id: 0,
        conversation_id: id.clone(),
        message_id: message,
        kind: "orchestrate".into(),
        run_id: Some(agent.id.clone()),
        schedule_name: None,
        goal_name: None,
        // What it decided goes here once the run reports; this row exists from
        // the moment the instruction is handed over, so a chat that was
        // interrupted still shows what it was doing.
        reason: String::new(),
        at_ms: now,
    })?;

    Ok(Handed {
        agent,
        compaction_due,
        conversation_id: id,
    })
}

// ---- opening a work -------------------------------------------------------

/// What to open, and how to run its first session.
///
/// A value rather than eight arguments, for the reason [`crate::works::Titling`]
/// is one: the caller varies two things — the instruction and where it happens
/// — and everything else has a default that was chosen on purpose and should
/// stay chosen unless somebody means to change it.
#[derive(Debug, Clone)]
pub struct Opening {
    pub instruction: String,
    /// Reljod's real checkout. It becomes the session's **read-only** root, and
    /// no worktree is cut until the session claims one.
    pub checkout: PathBuf,
    pub harness: HarnessKind,
    pub permission: PermissionPolicy,
    pub model: Option<String>,
    /// The session that asked for this, when one did. Sessions may open work
    /// below themselves, which is what makes the tree deeper than two levels.
    pub parent: Option<String>,
    /// How much of Jod the first session gets. `Delegate` by default: it may
    /// raise cards, read the bus, talk to its siblings and spawn children of
    /// its own, and it may not arm a schedule that spends money at 2am.
    pub tools: ToolAccess,
}

impl Opening {
    pub fn new(instruction: impl Into<String>, checkout: impl Into<PathBuf>) -> Opening {
        Opening {
            instruction: instruction.into(),
            checkout: checkout.into(),
            harness: HarnessKind::ClaudeCode,
            // Not `Ask`. `Ask` is plan mode, which refuses every mutation —
            // including the tool calls that are the session's whole job. The
            // same trap `hand_to_orchestrator` documents, and it costs a run to
            // discover rather than a compile.
            permission: PermissionPolicy::AcceptEdits,
            model: None,
            parent: None,
            tools: ToolAccess::Delegate,
        }
    }

    pub fn on(mut self, harness: HarnessKind) -> Opening {
        self.harness = harness;
        self
    }

    pub fn under(mut self, parent: impl Into<String>) -> Opening {
        self.parent = Some(parent.into());
        self
    }

    pub fn with_permission(mut self, permission: PermissionPolicy) -> Opening {
        self.permission = permission;
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Opening {
        self.model = Some(model.into());
        self
    }
}

/// A work that has been opened and put to work.
pub struct Opened {
    pub work: crate::works::Work,
    /// The first session's conversation. Every descendant's cards cascade to
    /// its rail.
    pub conversation_id: String,
    /// What its siblings will address it as on the bus.
    pub name: String,
    pub agent: AgentSummary,
    /// The throwaway naming it, when one could be started. `None` means the
    /// work keeps the title it opened with — its instruction's first words —
    /// which is a worse name and not a failure.
    pub titler: Option<String>,
}

/// Everything opening a work does before a process exists.
pub struct Prepared {
    pub work: crate::works::Work,
    pub conversation_id: String,
    pub name: String,
    /// The first session's launch, preamble and all.
    pub request: SpawnRequest,
}

/// Open the work, put a session in it, and point that session at the checkout.
///
/// Split from [`open_work`] at the seam where a *process* becomes necessary, so
/// the part with all the decisions in it can be tested without a supervisor:
/// that the work opens with a board, that the session is a node in the tree,
/// and that the checkout arrives read-only with **no worktree cut**.
///
/// Three things happen in an order that is not arbitrary:
///
/// 1. The work is created **with a board**, so "every task is complete" is a
///    state it can actually reach — `create_work` puts the instruction on it.
/// 2. The session is attached to the work under its parent, which is what makes
///    it a node in the tree rather than a loose conversation.
/// 3. The checkout is added as a **read-only** root. Nothing here cuts a
///    worktree, per D5: a session starts in the real thing and claims a branch
///    of its own the moment it needs to change something. Cutting one now would
///    be a worktree per delegation, most of them never written to.
pub fn prepare_work(store: &Store, opening: &Opening) -> Result<Prepared> {
    let work = store.create_work(&opening.instruction)?;
    let checkout = crate::roots::normalise(&opening.checkout);

    let conversation = store.new_conversation(
        opening.harness,
        &checkout.to_string_lossy(),
        Some(&work.title),
    )?;
    let session = store.attach_conversation(
        &conversation.id,
        &work.id,
        opening.parent.as_deref(),
        match &opening.parent {
            // Who opened it decides nothing about how it runs and everything
            // about how the tree reads: a session that spawned a session is the
            // reason the forest is deeper than two levels.
            Some(_) => crate::works::Origin::Agent,
            None => crate::works::Origin::Orchestrator,
        },
    )?;
    store.add_root(&conversation.id, NewRoot::reading(&checkout))?;

    let roots = store.roots(&conversation.id)?;
    let secrets = store.secrets_for(Some(&conversation.id), Some(&work.id))?;
    let peers: Vec<String> = store
        .roster(crate::team::Scope::Work, &work.id, &session.name)?
        .into_iter()
        .map(|m| m.name)
        .collect();

    let request = SpawnRequest {
        name: work.title.clone(),
        harness: opening.harness,
        prompt: opening.instruction.clone(),
        system: Some(worker_preamble(&Brief {
            harness: opening.harness,
            roots: &roots,
            secrets: &secrets,
            peers: &peers,
            tools: Some(opening.tools),
        })),
        cwd: checkout,
        model: opening.model.clone(),
        permission: opening.permission,
        resume: Resume::Fresh,
        tools: Some(opening.tools),
        ..SpawnRequest::default()
    };
    Ok(Prepared {
        work,
        conversation_id: conversation.id,
        name: session.name,
        request,
    })
}

/// Open a work, and put its first session on Reljod's actual checkout.
///
/// **This function does not block and must never learn to.** It returns as soon
/// as the first session is *spawned* — not when it has said anything, not when
/// the titler has answered, and certainly not when the work is done. The main
/// chat is the thing you reach for while something is already running, so an
/// orchestrator that waited would be unusable at exactly the moment it matters.
/// That property is why the titler runs detached below rather than being
/// awaited, and why nothing here reads the session's output.
pub async fn open_work(jod: &std::sync::Arc<Jod>, opening: Opening) -> Result<Opened> {
    let store = jod.store().ok_or(JodError::StoreRequired)?;
    let prepared = prepare_work(store, &opening)?;
    let agent = jod
        .spawn_agent_in(
            prepared.request,
            RunConversation::Existing(prepared.conversation_id.clone()),
        )
        .await?;

    Ok(Opened {
        titler: start_titler(jod, &prepared.work, opening.harness).await,
        work: prepared.work,
        conversation_id: prepared.conversation_id,
        name: prepared.name,
        agent,
    })
}

/// Start the throwaway that names a work, and arrange for it to be settled.
///
/// Detached on purpose, twice over. The spawn is not awaited to completion, so
/// the caller is not held up by a model call; and the failure of the whole
/// thing is an `Option`, not an error, because D6's titler is a *nicety*. A
/// work whose name is its instruction's first eight words is findable and
/// slightly ugly; a delegation that failed because a paraphrase did not arrive
/// is neither.
async fn start_titler(
    jod: &std::sync::Arc<Jod>,
    work: &crate::works::Work,
    harness: HarnessKind,
) -> Option<String> {
    let store = jod.store()?;
    // Subscribed before the spawn, or a titler that answers immediately
    // finishes before anything is listening and the work keeps its fallback
    // name for no reason.
    let events = jod.subscribe();
    let conversation = store.open_titler(harness).ok()?;
    let request = crate::works::Titling::new(work)
        .with_harness(harness)
        .request();
    let agent = jod
        .spawn_agent_in(request, RunConversation::Existing(conversation.id.clone()))
        .await
        .ok()?;

    let jod = jod.clone();
    let work_id = work.id.clone();
    let run_id = agent.id.clone();
    tokio::spawn(async move {
        let output = titler_output(events, &run_id).await;
        if let Some(store) = jod.store() {
            // `finish_titling` deletes the titler's conversation whatever it
            // said, so the ordinary failure — an empty answer — still clears
            // the throwaway rather than leaving a session in the fleet that
            // nobody opened and nobody will.
            if let Err(e) = store.finish_titling(&work_id, &conversation.id, &output) {
                eprintln!("[jod] could not settle the titler for {work_id}: {e}");
            }
        }
    });
    Some(agent.id)
}

/// Everything one run said, for the titler to be read out of.
///
/// A copy of the CLI's collector rather than a call into it: `jod-core` cannot
/// depend on the CLI, and this is the only place in core that has to wait for a
/// run's words. If a third caller appears it belongs in `service`.
async fn titler_output(
    mut events: crate::broadcast::Receiver<crate::AgentEnvelope>,
    run_id: &str,
) -> String {
    use crate::broadcast::error::RecvError;
    let mut said = String::new();
    loop {
        match events.recv().await {
            Ok(envelope) if envelope.agent_id == run_id => match envelope.event {
                crate::AgentEvent::Message { text } => {
                    said.push_str(&text);
                    said.push('\n');
                }
                crate::AgentEvent::Finished { .. } => return said,
                _ => {}
            },
            Ok(_) => {}
            // Nothing more is coming. Whatever was said is still worth reading,
            // and an empty answer is a fallback title rather than an error.
            Err(RecvError::Closed) => return said,
            Err(RecvError::Lagged(_)) => continue,
        }
    }
}

/// A delegation, as recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delegation {
    pub id: i64,
    pub conversation_id: String,
    pub message_id: Option<i64>,
    pub kind: String,
    pub run_id: Option<String>,
    pub schedule_name: Option<String>,
    pub goal_name: Option<String>,
    pub reason: String,
    pub at_ms: i64,
}

impl Store {
    /// The main chat, created the first time it is asked for.
    ///
    /// Get-or-create rather than a setup step, because a main chat that has to
    /// be initialised is one that is missing exactly when you first need it.
    pub fn main_conversation(
        &self,
        harness: crate::harness::HarnessKind,
        cwd: &str,
    ) -> Result<String> {
        if let Some(id) = self.pinned_conversation()? {
            return Ok(id);
        }
        let created = self.new_conversation(harness, cwd, None)?;
        let id = created.id.clone();
        self.write(|tx| {
            tx.execute(
                "UPDATE conversations SET pinned = 1, title = 'main' WHERE id = ?1",
                rusqlite::params![id],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    pub fn pinned_conversation(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT id FROM conversations WHERE pinned = 1",
                [],
                |r| r.get(0),
            )
            .ok())
    }

    /// Note that a person said something, which is the clock compaction reads.
    pub fn touch_human(&self, conversation_id: &str, at_ms: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE conversations SET last_human_ms = ?2 WHERE id = ?1",
                rusqlite::params![conversation_id, at_ms],
            )?;
            Ok(())
        })
    }

    pub fn last_human_ms(&self, conversation_id: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT last_human_ms FROM conversations WHERE id = ?1",
                rusqlite::params![conversation_id],
                |r| r.get(0),
            )
            .ok()
            .flatten())
    }

    /// Write down what was decided and what it turned into.
    pub fn record_delegation(&self, d: &Delegation) -> Result<i64> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO delegations
                   (conversation_id, message_id, kind, run_id, schedule_name,
                    goal_name, reason, at_ms)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                rusqlite::params![
                    d.conversation_id,
                    d.message_id,
                    d.kind,
                    d.run_id,
                    d.schedule_name,
                    d.goal_name,
                    d.reason,
                    d.at_ms
                ],
            )?;
            Ok(tx.last_insert_rowid())
        })
    }

    /// What this chat has set in motion, newest first.
    pub fn delegations(&self, conversation_id: &str, limit: usize) -> Result<Vec<Delegation>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, message_id, kind, run_id, schedule_name,
                    goal_name, reason, at_ms
               FROM delegations WHERE conversation_id = ?1
              ORDER BY at_ms DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![conversation_id, limit as i64], |r| {
            Ok(Delegation {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                message_id: r.get(2)?,
                kind: r.get(3)?,
                run_id: r.get(4)?,
                schedule_name: r.get(5)?,
                goal_name: r.get(6)?,
                reason: r.get(7)?,
                at_ms: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    // ---- when to compact ----

    #[test]
    fn a_small_recent_chat_is_left_alone() {
        assert_eq!(should_compact(500, Some(0), 1_000), None);
    }

    #[test]
    fn a_large_chat_is_compacted_however_recently_you_spoke() {
        assert_eq!(
            should_compact(COMPACT_CHARS + 1, Some(1_000), 1_001),
            Some(CompactReason::Size)
        );
    }

    /// The trigger that matters: summarise at a natural break, not mid-thought.
    #[test]
    fn a_chat_you_have_not_touched_for_a_day_is_compacted_at_the_break() {
        let chars = COMPACT_FLOOR_CHARS + 1;
        assert_eq!(
            should_compact(chars, Some(0), DAY),
            Some(CompactReason::Idle)
        );
        assert_eq!(should_compact(chars, Some(0), DAY - 1), None);
    }

    /// Below the floor there is nothing to save, and a summary would trade
    /// exact sentences for a vague one at the point where the specifics are
    /// all there is.
    #[test]
    fn a_short_chat_is_never_compacted_however_long_it_has_been_quiet() {
        assert_eq!(should_compact(COMPACT_FLOOR_CHARS - 1, Some(0), DAY * 30), None);
    }

    /// Idle is measured from *your* last message. Six agents writing into the
    /// chat overnight is not you being present.
    #[test]
    fn a_chat_you_have_never_spoken_in_is_not_idle() {
        assert_eq!(should_compact(COMPACT_FLOOR_CHARS + 1, None, DAY * 30), None);
    }

    // ---- reading the router ----

    #[test]
    fn a_bare_decision_parses() {
        let d = parse_decision(r#"{"action":"reply","text":"nothing is running"}"#).unwrap();
        assert_eq!(d, Decision::Reply { text: "nothing is running".into() });
    }

    /// Models fence JSON whatever they are asked.
    #[test]
    fn a_fenced_decision_parses() {
        let out = "```json\n{\"action\":\"delegate_new\",\"prompt\":\"port it\"}\n```";
        match parse_decision(out).unwrap() {
            Decision::DelegateNew { prompt, .. } => assert_eq!(prompt, "port it"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_decision_buried_in_prose_still_parses() {
        let out = "I think this continues the parser work, so:\n\
                   {\"action\":\"delegate_existing\",\"run_id\":\"abc\",\"prompt\":\"and the tests\"}\n\
                   That keeps the context.";
        match parse_decision(out).unwrap() {
            Decision::DelegateExisting { run_id, .. } => assert_eq!(run_id, "abc"),
            other => panic!("got {other:?}"),
        }
    }

    /// A model that reconsiders emits its correction second, so taking the
    /// first object would act on a draft it had already withdrawn.
    #[test]
    fn the_last_decision_wins_when_the_router_changes_its_mind() {
        let out = "{\"action\":\"delegate_new\",\"prompt\":\"first thought\"}\n\
                   Actually that agent is already on it:\n\
                   {\"action\":\"delegate_existing\",\"run_id\":\"warm\",\"prompt\":\"second\"}";
        match parse_decision(out).unwrap() {
            Decision::DelegateExisting { run_id, .. } => assert_eq!(run_id, "warm"),
            other => panic!("got {other:?}"),
        }
    }

    /// Guessing here would invent a delegation nobody asked for, and a wrong
    /// delegation spends money and touches a repository.
    #[test]
    fn an_unreadable_answer_is_an_error_rather_than_a_guess() {
        assert!(parse_decision("I would delegate this to Bob").is_err());
        assert!(parse_decision("").is_err());
        assert!(parse_decision("{\"action\":\"invent_something\"}").is_err());
    }

    #[test]
    fn a_decision_with_braces_in_its_text_still_parses() {
        let out = r#"{"action":"reply","text":"use {} for an empty set"}"#;
        match parse_decision(out).unwrap() {
            Decision::Reply { text } => assert!(text.contains("{}")),
            other => panic!("got {other:?}"),
        }
    }

    // ---- the prompt ----

    fn candidate(id: &str, last: &str) -> Candidate {
        Candidate {
            run_id: id.into(),
            name: "port-the-parser".into(),
            harness: "claude_code".into(),
            last: Some(last.into()),
            age_ms: 60_000,
        }
    }

    /// Reusing a warm agent is the decision that matters most, so the router
    /// cannot make it without being told what is warm.
    #[test]
    fn the_router_is_told_what_is_already_running() {
        let p = router_prompt("and run the tests", &[candidate("r1", "lexer landed")], &[]);
        assert!(p.contains("r1"));
        assert!(p.contains("lexer landed"));
        assert!(p.contains("and run the tests"));
    }

    #[test]
    fn the_router_is_told_plainly_when_nothing_is_running() {
        let p = router_prompt("do a thing", &[], &[]);
        assert!(p.contains("(none)"), "{p}");
    }

    /// The parse is the contract, so the shape has to be in the prompt.
    #[test]
    fn every_action_the_parser_accepts_is_offered_to_the_router() {
        let p = router_prompt("x", &[], &[]);
        for action in [
            "delegate_existing",
            "delegate_new",
            "schedule",
            "goal",
            "reply",
        ] {
            assert!(p.contains(action), "{action} is not offered");
        }
    }

    #[test]
    fn every_offered_action_round_trips_through_the_parser() {
        for probe in [
            r#"{"action":"delegate_existing","run_id":"r","prompt":"p"}"#,
            r#"{"action":"delegate_new","prompt":"p"}"#,
            r#"{"action":"schedule","name":"n","prompt":"p","cron":"0 2 * * *"}"#,
            r#"{"action":"goal","name":"n","objective":"o"}"#,
            r#"{"action":"reply","text":"t"}"#,
        ] {
            assert!(parse_decision(probe).is_ok(), "{probe}");
        }
    }

    // ---- what a worker is told ----

    fn root(path: &str, writable: bool) -> Root {
        Root {
            id: 1,
            conversation_id: "c1".into(),
            path: PathBuf::from(path),
            writable,
            position: 0,
            origin: crate::roots::Origin::Human,
            added_at_ms: 0,
        }
    }

    fn secret(name: &str, hint: &str) -> SecretMeta {
        SecretMeta {
            id: 1,
            name: name.into(),
            scope: crate::secrets::Scope::Work,
            scope_id: "w1".into(),
            hint: hint.into(),
            length: 32,
            redactable: true,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn brief<'a>(
        harness: HarnessKind,
        roots: &'a [Root],
        secrets: &'a [SecretMeta],
        peers: &'a [String],
    ) -> Brief<'a> {
        Brief {
            harness,
            roots,
            secrets,
            peers,
            tools: Some(ToolAccess::Delegate),
        }
    }

    /// E6.S1 and G6.S4, asserted rather than described: the experience is
    /// Jod's, so what a worker is told about it cannot be one of three things.
    #[test]
    fn the_body_of_the_preamble_is_identical_on_every_harness() {
        let roots = [root("/repo", false), root("/repo-worktree", true)];
        let secrets = [secret("STRIPE_API_KEY", "the live key")];
        let peers = ["scout".to_string()];

        let shared = |harness| -> Vec<String> {
            preamble_lines(&brief(harness, &roots, &secrets, &peers))
                .into_iter()
                .filter(|l| l.only.is_none())
                .map(|l| l.text)
                .collect()
        };
        let claude = shared(HarnessKind::ClaudeCode);
        assert_eq!(claude, shared(HarnessKind::OpenCode));
        assert_eq!(claude, shared(HarnessKind::Agy));
        assert!(!claude.is_empty());
    }

    /// A per-harness line is allowed, and it has to have been measured. The
    /// reason lives in the type so it cannot be a comment somebody deletes.
    #[test]
    fn every_per_harness_line_says_why_it_is_not_shared() {
        let roots = [root("/repo", false), root("/other", false)];
        let peers = [];
        for harness in [HarnessKind::ClaudeCode, HarnessKind::OpenCode, HarnessKind::Agy] {
            for line in preamble_lines(&brief(harness, &roots, &[], &peers)) {
                if line.only.is_some() {
                    assert!(
                        line.why.len() > 30,
                        "a per-harness line with no measurement behind it: {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_harness_is_told_only_its_own_lines() {
        let roots = [root("/repo", false), root("/other", false)];
        let opencode = worker_preamble(&brief(HarnessKind::OpenCode, &roots, &[], &[]));
        let claude = worker_preamble(&brief(HarnessKind::ClaudeCode, &roots, &[], &[]));
        assert!(opencode.contains("it takes one and refuses a second"));
        assert!(
            !claude.contains("refuses a second"),
            "Claude Code was told about OpenCode's directory flag"
        );
        assert!(worker_preamble(&brief(HarnessKind::Agy, &roots, &[], &[]))
            .contains("workspace is exactly the directories above"));
    }

    /// D5: the rule the read-only default exists to state.
    #[test]
    fn a_worker_is_told_which_roots_it_may_write_to_and_that_it_must_claim_first() {
        let roots = [root("/repo", false), root("/repo-worktree", true)];
        let said = worker_preamble(&brief(HarnessKind::ClaudeCode, &roots, &[], &[]));
        assert!(said.contains("/repo` — **read-only**"), "{said}");
        assert!(said.contains("/repo-worktree` — **writable**"), "{said}");
        assert!(said.contains("claim a worktree"));
        // Roots are not a sandbox, and the preamble must not imply they are.
        assert!(said.contains("not a sandbox"));
    }

    /// E3.S5. Names, that they are environment variables, that echoing is
    /// pointless, and that a missing one is a blocked ending.
    #[test]
    fn a_worker_is_told_the_names_of_its_secrets_and_never_a_value() {
        let secrets = [secret("STRIPE_API_KEY", "the live key")];
        let said = worker_preamble(&brief(HarnessKind::ClaudeCode, &[], &secrets, &[]));
        assert!(said.contains("$STRIPE_API_KEY"), "{said}");
        assert!(said.contains("the live key"));
        assert!(said.contains("environment variables"));
        assert!(said.contains("redaction marker"));
        assert!(said.contains("**blocked** ending, never a reason to invent one"));
    }

    #[test]
    fn a_worker_with_no_secrets_is_still_told_what_to_do_about_a_missing_one() {
        let said = worker_preamble(&brief(HarnessKind::ClaudeCode, &[], &[], &[]));
        assert!(said.contains("You have none"), "{said}");
        assert!(said.contains("**blocked** ending"));
    }

    #[test]
    fn a_worker_is_told_the_card_tools_and_when_each_one_is_right() {
        let said = worker_preamble(&brief(HarnessKind::ClaudeCode, &[], &[], &[]));
        for tool in ["record_decision", "ask_question", "request_secret"] {
            assert!(said.contains(tool), "{tool} is not named: {said}");
        }
        assert!(said.contains("Prefer deciding and recording it to asking"));
    }

    /// A run without Jod's tools must not be sent hunting for them. The rail
    /// still works for it — through the lifter — and it is told how.
    #[test]
    fn a_session_without_jods_tools_is_told_the_truth_about_the_rail() {
        let said = worker_preamble(&Brief {
            harness: HarnessKind::ClaudeCode,
            roots: &[],
            secrets: &[],
            peers: &[],
            tools: None,
        });
        assert!(!said.contains("record_decision"), "{said}");
        assert!(said.contains("lifts your questions"));
        assert!(said.contains("Work alone"));
    }

    /// G6: the protocol section, and the two sentences it exists for.
    #[test]
    fn a_worker_is_told_the_protocol_and_who_it_can_reach() {
        let peers = ["scout".to_string(), "builder".to_string()];
        let said = worker_preamble(&brief(HarnessKind::ClaudeCode, &[], &[], &peers));
        assert!(said.contains("`scout`") && said.contains("`builder`"), "{said}");
        assert!(said.contains("Read your inbox before you ask anything"));
        assert!(said.contains("costs a turn of theirs"));
        assert!(said.contains("Ownership of code is a lease, not an announcement"));
        assert!(said.contains("Report up, ask sideways"));
    }

    #[test]
    fn a_worker_alone_on_a_work_is_told_to_check_rather_than_assume() {
        let said = worker_preamble(&brief(HarnessKind::ClaudeCode, &[], &[], &[]));
        assert!(said.contains("Nobody else is on this work yet"), "{said}");
        assert!(said.contains("roster"));
    }

    #[test]
    fn a_worker_is_pointed_at_the_charter_and_the_skills_under_its_roots() {
        let said = worker_preamble(&brief(HarnessKind::ClaudeCode, &[root("/repo", false)], &[], &[]));
        assert!(said.contains("AGENTS.md"), "{said}");
        assert!(said.contains(".agents/skills/"));
        assert!(said.contains("blocked** ending, not a puzzle"));
    }

    #[test]
    fn a_session_nobody_gave_a_directory_is_told_to_say_so_rather_than_guess() {
        let said = worker_preamble(&brief(HarnessKind::ClaudeCode, &[], &[], &[]));
        assert!(said.contains("Nobody has given this session a directory"), "{said}");
    }

    // ---- opening a work ----

    /// E4.S4's own check, minus the process: one instruction naming a folder
    /// produces a titled work and a session with the folder as a **read-only**
    /// root and no worktree yet.
    #[test]
    fn opening_a_work_puts_a_session_on_the_real_checkout_read_only() {
        let s = store();
        let opened = prepare_work(&s, &Opening::new("port the parser", "/tmp/repo")).unwrap();

        assert!(!opened.work.title.is_empty(), "an unnamed work is unfindable");
        assert_eq!(opened.work.state, crate::works::State::Open);

        let roots = s.roots(&opened.conversation_id).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, PathBuf::from("/tmp/repo"));
        assert!(
            !roots[0].writable,
            "a session starts in Reljod's checkout and may not write to it"
        );
        assert!(
            s.work_leases(&opened.work.id).unwrap().is_empty(),
            "a worktree was cut before any session asked for one"
        );
    }

    /// A work with no task can never be complete, so "all done" would be a
    /// sentence about an empty list rather than a state.
    #[test]
    fn a_work_opens_with_the_instruction_on_its_board() {
        let s = store();
        let opened = prepare_work(&s, &Opening::new("port the parser", "/tmp/repo")).unwrap();
        let board = s.work_tasks(&opened.work.id).unwrap();
        assert_eq!(board.len(), 1);
        assert!(board[0].title.contains("port the parser"));
    }

    #[test]
    fn the_first_session_is_a_node_of_the_work_rather_than_a_loose_conversation() {
        let s = store();
        let opened = prepare_work(&s, &Opening::new("port the parser", "/tmp/repo")).unwrap();
        let sessions = s.work_sessions(&opened.work.id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].conversation_id, opened.conversation_id);
        assert_eq!(sessions[0].origin, crate::works::Origin::Orchestrator);
        assert_eq!(sessions[0].parent, None);
        assert!(!opened.name.is_empty(), "a session its siblings cannot address");
    }

    /// What makes the tree deeper than two levels: a session opening work of
    /// its own, below itself.
    #[test]
    fn a_session_may_open_work_under_itself() {
        let s = store();
        let parent = prepare_work(&s, &Opening::new("port the parser", "/tmp/repo")).unwrap();
        let child = prepare_work(
            &s,
            &Opening::new("and the tests", "/tmp/repo").under(&parent.conversation_id),
        )
        .unwrap();

        let sessions = s.work_sessions(&child.work.id).unwrap();
        assert_eq!(sessions[0].parent.as_deref(), Some(parent.conversation_id.as_str()));
        assert_eq!(sessions[0].origin, crate::works::Origin::Agent);
    }

    /// The session is launched with the brief, not with a bare prompt: what it
    /// may write to, which credentials exist and how to reach the rail are all
    /// facts about *this* session and reach it nowhere else.
    #[test]
    fn the_first_session_is_launched_with_its_brief() {
        let s = store();
        s.put_secret(
            "STRIPE_API_KEY",
            crate::secrets::Scope::Global,
            "",
            "sk-test-not-a-real-key",
            "the test key",
        )
        .unwrap();
        let opened = prepare_work(&s, &Opening::new("port the parser", "/tmp/repo")).unwrap();

        let brief = opened.request.system.unwrap();
        assert!(brief.contains("/tmp/repo` — **read-only**"), "{brief}");
        assert!(brief.contains("claim a worktree"));
        assert!(brief.contains("$STRIPE_API_KEY"));
        assert!(brief.contains("record_decision"));
        assert!(
            !brief.contains("sk-test-not-a-real-key"),
            "a preamble carried a credential's value into the model's context"
        );
        assert_eq!(opened.request.prompt, "port the parser");
        assert_eq!(opened.request.tools, Some(ToolAccess::Delegate));
        // Plan mode refuses every mutation, including the tool calls that are
        // the session's whole job.
        assert_ne!(opened.request.permission, PermissionPolicy::Plan);
    }

    #[test]
    fn opening_a_work_with_nothing_to_do_is_refused() {
        assert!(prepare_work(&store(), &Opening::new("   ", "/tmp/repo")).is_err());
    }

    // ---- the pinned chat ----

    fn store() -> Store {
        Store::in_memory().unwrap()
    }

    /// A main chat that has to be initialised is one that is missing exactly
    /// when you first need it.
    #[test]
    fn the_main_chat_is_created_the_first_time_it_is_asked_for() {
        let s = store();
        assert_eq!(s.pinned_conversation().unwrap(), None);
        let id = s.main_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp").unwrap();
        assert_eq!(s.pinned_conversation().unwrap().as_deref(), Some(id.as_str()));
    }

    /// A second pinned chat splits where instructions land, and you find out
    /// by losing something.
    #[test]
    fn asking_twice_returns_the_same_chat() {
        let s = store();
        let first = s.main_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp").unwrap();
        let second = s.main_conversation(crate::harness::HarnessKind::OpenCode, "/elsewhere").unwrap();
        assert_eq!(first, second);
    }

    /// A pin left on the thread a switch compacted away is a main chat you
    /// cannot reach: the next instruction goes to the handed-over conversation
    /// and the summary sits in one nobody opens again.
    #[test]
    fn the_pin_follows_a_harness_switch() {
        use crate::conversation::{NewMessage, Role};
        let s = store();
        let id = s.main_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp").unwrap();
        s.append_message(&id, NewMessage::new(Role::User, "count the rust files")).unwrap();

        let switch = s
            .switch_harness(&id, crate::harness::HarnessKind::OpenCode, "counted 47 rust files", "harness")
            .unwrap();

        assert_eq!(
            s.pinned_conversation().unwrap().as_deref(),
            Some(switch.conversation.id.as_str()),
            "the pin should have moved to the conversation the switch minted"
        );
        // And the get-or-create agrees, which is the call every turn makes.
        assert_eq!(
            s.main_conversation(crate::harness::HarnessKind::OpenCode, "/tmp").unwrap(),
            switch.conversation.id
        );
    }

    /// Only the pinned thread carries the pin. Switching an ordinary
    /// conversation must not mint a second main chat.
    #[test]
    fn an_unpinned_switch_leaves_the_pin_alone() {
        use crate::conversation::{NewMessage, Role};
        let s = store();
        let main = s.main_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp").unwrap();
        let other = s.new_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp", None).unwrap();
        s.append_message(&other.id, NewMessage::new(Role::User, "something else")).unwrap();

        s.switch_harness(&other.id, crate::harness::HarnessKind::OpenCode, "the other thing", "harness")
            .unwrap();

        assert_eq!(s.pinned_conversation().unwrap().as_deref(), Some(main.as_str()));
    }

    #[test]
    fn the_human_clock_is_separate_from_the_conversations_own() {
        let s = store();
        let id = s.main_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp").unwrap();
        assert_eq!(s.last_human_ms(&id).unwrap(), None);
        s.touch_human(&id, 12_345).unwrap();
        assert_eq!(s.last_human_ms(&id).unwrap(), Some(12_345));
    }

    /// A router that silently picks is one nobody can correct, so the reason
    /// is recorded alongside what it chose.
    #[test]
    fn a_delegation_records_what_was_chosen_and_why() {
        let s = store();
        let id = s.main_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp").unwrap();
        s.record_delegation(&Delegation {
            id: 0,
            conversation_id: id.clone(),
            message_id: None,
            kind: "delegate_existing".into(),
            run_id: Some("r1".into()),
            schedule_name: None,
            goal_name: None,
            reason: "it is already holding the parser context".into(),
            at_ms: 1_000,
        })
        .unwrap();

        let all = s.delegations(&id, 10).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].run_id.as_deref(), Some("r1"));
        assert!(all[0].reason.contains("parser context"));
    }
}
