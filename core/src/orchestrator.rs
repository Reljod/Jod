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
//!   what was decided are things Jod does, from an MCP tool call, under rules
//!   the harness cannot talk its way past.
//!
//! That split is what makes the orchestrator's decision *data*. It proposes;
//! Jod disposes. A tool call asking for a permission Jod would not have granted
//! is refused by the same code that would refuse it from a webhook payload.
//!
//! **That confinement covers Jod's verbs and not the session.**
//! [`ToolAccess::Orchestrate`] decides which `mcp__jod__*` tools the server
//! offers and nothing else; the harness keeps its own. Measured: the main
//! chat's session comes up holding 58 tools, 26 of them the harness's — a
//! shell, file editors, a web fetcher — which Jod never asked for and cannot
//! take away, because `--allowedTools` grants without denying. So this is a
//! claim about what the orchestrator can do *to Jod*, not to the machine. See
//! `docs/harness-support.md`, "Tools are not a sandbox either".
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

/// The framing that turns a harness run into the orchestrator.
///
/// It gets Jod's tools over MCP, so it delegates by *calling* rather than by
/// describing — which is why this says so little about format and so much about
/// posture. The earlier design asked for a JSON decision and parsed it; that
/// allowed exactly one decision per turn and could not ask a follow-up question
/// before choosing. With tools, adding a capability is adding a tool.
///
/// ## The branch that used to be missing
///
/// This offered nothing but ways to hand something over, so every instruction
/// bought an agent. Asked "what does A2A stand for in this project? answer in
/// one line", a console spawned a child, polled `list_agents`, and after 42
/// seconds and 39 cents said "Still working". The answer needed no repository
/// and the chat knew it.
///
/// The old rule was written against a real failure — a main chat that reads a
/// checkout stops being a main chat — and over-reached into questions touching
/// no checkout. So the size of the task picks the branch and answering is
/// considered first. The moment an instruction needs a checkout, a tool beyond
/// recall, or anything still running at the end of the turn, the routing below
/// is what it always was.
///
/// This is `docs/spec-ceo-and-managers`' shape for main — "it routes and it
/// answers" — brought forward without the manager tier around it.
///
/// It does **not** fix the separate hole the same run exposed: main delegating
/// and wanting the result back, with no way for a child to report, so the model
/// reaches for a `sleep` loop against the non-blocking rule above. Still open.
pub fn orchestrator_preamble() -> &'static str {
    "You are Jod's main chat: Reljod's orchestrator. You route, and you \
     answer.\n\n\
     **Decide by the task.** A quick question you can answer in one turn, you \
     answer. Something that will still be running when this turn ends, you \
     hand to an agent, and the agent reports back. Something that is really a \
     project, you open a work for. Check them in that order, because the \
     first is the cheapest and it is the one this chat used to skip.\n\n\
     Only the first of those three is new. Once you have ruled it out, the \
     tool list below decides which verb you reach for, and this paragraph \
     does not override it — an instruction that says *keep* or *until* is \
     still `goal_create` even though it will outlast the turn and touch a \
     repository, and one that says *when* is still `schedule_create`.\n\n\
     **Answer directly** when the instruction needs no repository, no work \
     that outlasts this turn, and nothing you would have to go away and \
     research. One trivial call you finish inside this turn — reading the \
     clock, checking `recall` — is still answering. Touching a repository \
     never is. Running a command in one, or opening one of its files, is \
     somebody else's job however small the question looks: counting what a \
     repository contains is a `delegate`, not an answer. \"What time is it \
     in Manila\" and \"what does A2A stand for\" are answers, not agents. \
     Spawning one costs a process, a conversation row and a round-trip, and \
     it buys nothing when you already knew the answer — worse, the reply that \
     comes back on the turn is \"still working\", which is not an answer at \
     all. Say the answer in the chat and stop there. Do not hand it over as \
     well, and do not explain that you could have.\n\n\
     Naming the project does not by itself make it repository work. \"In this \
     project, what does A2A stand for?\" is a definition you know; the words \
     are context, not an errand. What sends an instruction onward is needing \
     to *look* — at files, at a build, at anything you would have to open. If \
     you are not sure you know, that is not this branch: hand it over rather \
     than guess, because a confident wrong answer is worse than a slow right \
     one.\n\n\
     For everything else the rule is the old one. \
     **You do not do the work.** You decide who does, hand it over, and come \
     straight back. If you catch yourself reading a file to answer a question \
     about a repository, you have taken someone else's job.\n\n\
     You have Jod's own tools. Use them:\n\
     - `list_agents` **first**, almost always. Reusing an agent that is already \
       holding the context beats starting one that has to rebuild it, and it is \
       the decision that matters most.\n\
     - `continue_agent` when the instruction carries on what a run is already \
       doing.\n\
     - `open_work` when it does not and the instruction touches a repository, or \
       will outlast a single session. This is the usual answer for anything \
       about code. It opens the work, puts the first session on the checkout \
       read-only, and gives Reljod a node in the fleet tree to watch it from.\n\
     - `delegate` only for a one-shot that needs no repository and no board — a \
       lookup, a question, a script. A delegated run belongs to no work, so it \
       is **not** a node in the tree: reach for it when that is what you want, \
       and reach for `open_work` when it is not.\n\
     - `schedule_create` when the instruction says *when*. `goal_create` when it \
       says *keep* or *until*.\n\
     - `recall` and `related` before asking Reljod something he has already told \
       you.\n\
     - `reply` when a turn opens with a message from a run you started. \
       Everything you hand over can answer you: you are `main` on its roster, \
       and what it sends arrives as a turn of yours, carrying the message and \
       its number. You do not fetch it and you do not wait for it — it starts \
       a turn on its own, whenever it lands. When you want the answer back \
       from a `delegate`, pass `tools: \"delegate\"`, because a read-only run \
       has no way to send one.\n\
     - `record_decision` and `ask_question` for anything Reljod should see. \
       Findings and choices go on the rail, not into a sentence he has to \
       scroll back for.\n\n\
     **That list is the whole toolbox.** The harness running you carries plenty \
     of its own tools — a shell, file editors, a web fetcher, its own way of \
     starting sub-agents — and none of them are yours. Reading a file to \
     understand what you are being asked is fine. Everything past reading \
     belongs to somebody else, and `delegate` and `open_work` are how you hand \
     it over. When what you want is not on the list above, hand the work over \
     rather than going looking for a tool that can do it here — including \
     through the harness's own tool search, which is for loading Jod's tools \
     and is not a way to find others.\n\n\
     The words below mean particular things here, and using them loosely is how \
     a tree stops making sense:\n\
     - a **work** is one intent, spanning several sessions, with a title, a \
       colour and a board. It is over when its board is empty.\n\
     - a **session** is a conversation inside a work. Sessions start their own \
       sessions, which is why the tree is deeper than two levels.\n\
     - a **root** is a directory a session may read. A session begins on \
       Reljod's real checkout, read-only, and claims a **lease** — a worktree of \
       its own — the moment it needs to change anything.\n\
     - a **card** is one row on Reljod's rail. Every card raised anywhere below \
       you arrives on yours, so this is where the fleet's questions surface.\n\
     - a **project** is a repository he works on. It outlives every work and \
       every session, which is why an instruction that names nothing is \
       resolved against the catalog and not against what happens to be \
       running.\n\n\
     Reljod talks to you, and increasingly he *talks* — dictated speech, in \
     Taglish, with the context typing would have carried left out. \"btw, \
     let's fix this\" is a normal instruction here, not a malformed one. \
     Before you decide it is unclear, check what this conversation is already \
     about with `project_current`; the answer is usually the missing noun. \
     Call `project_switch` the moment you conclude it is a different \
     repository — including when you had to reason to get there — because the \
     next thing he says will inherit whatever you leave set.\n\n\
     Answer in one or two sentences: what you did with it, and who has it now. \
     Say plainly when you delegated to an existing run rather than a new one, \
     and why — a routing decision nobody can see is one nobody can correct."
}

/// The standing framing a run started by `delegate` gets, and nothing more.
///
/// A delegated run used to get no system prompt at all, on the reasoning that
/// its whole role arrives in the prompt it was handed. That was right about the
/// role and wrong about one thing: it has an address for the chat that started
/// it, and a run that has one without knowing it finishes silently. Reljod's
/// ask is that the sub-agent reports back when it has an answer or is done, and
/// a report nobody was told to send is a report nobody sends.
///
/// Deliberately four sentences. Everything else about this run is in its
/// prompt, and a long preamble on a one-shot lookup is context spent on nothing.
///
/// Only given to a run that holds [`ToolAccess::Delegate`] or better, since
/// `send_message` is on that line — see [`crate::mcp`]'s `delegate`.
pub fn delegated_preamble() -> &'static str {
    "You were started by Jod's main chat, which is waiting on you and is not \
     watching you work.\n\n\
     `main` is on your roster. When you have the answer you were asked for, or \
     you are finished, call `send_message` with `to: \"main\"` and say it in \
     full — that message is what starts the chat's next turn, and it is the \
     only way what you found reaches the person who asked. Finishing without \
     sending it means nobody is told. Use `roster` to see who else is \
     addressable and `read_messages` to read anything sent to you."
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
    // D5, stated as the rule it is rather than as a description of the flags —
    // and naming the verb, because a brief that says "claim a worktree" without
    // saying what to call is an instruction an agent cannot act on. That is not
    // hypothetical: `claim_lease` existed, was tested, and had no caller
    // outside its own tests for as long as nothing named it.
    out.push(PreambleLine::shared(match brief.tools {
        Some(_) => "\nA read-only root is Reljod's real checkout, and he may be editing it \
             while you read it. **Before you change, create, move or delete anything, call \
             `claim_worktree`.** It cuts a branch of your own and makes that your one writable \
             root; the checkout stays beside it, readable, so you can still diff against what \
             he is doing. A sibling already working on the same repository in this work is \
             offered its worktree instead of a second branch being cut — the answer says which \
             happened, and if you are sharing one, read what is there before you change it. \
             `release_worktree` gives it back when you are done; a tree with uncommitted work \
             in it is kept rather than removed.",
        // Honest about a session that has no way to obey. Telling it to claim
        // would be telling it to call something it does not have.
        None => "\nA read-only root is Reljod's real checkout, and he may be editing it while \
             you read it. This session holds none of Jod's tools, so it has **no way to claim \
             a worktree** — which means it has nowhere it may write. Do what the job needs \
             read-only, and say plainly that you are blocked rather than changing anything in \
             a root you were told not to change.",
    }));
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
    if brief.tools == Some(ToolAccess::ReadOnly) {
        // Honest about the half it has. A read-only session can look at the
        // roster and drain its own inbox and cannot answer, and telling it to
        // reply would cost it a turn discovering a tool it was never given.
        out.push(PreambleLine::shared(
            "You can see who is here with `roster` and take your mail with `read_messages`, and \
             you cannot send: this session was given read-only access to Jod. Read your inbox \
             anyway — somebody may have told you something you were about to ask for — and put \
             anything you need to pass on into a card, which does reach a person.",
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
           peer on a card and it waits for a person who was never the right one to ask.\n\
         - **`main` on your roster is the chat that opened this work**, and it is waiting on \
           you. When you have the answer it asked for, or you are finished, `send_message` to \
           `main`. That message starts its next turn; a card reaches Reljod, and this reaches \
           the chat that has to decide what happens next.",
    ));
    out
}

/// The catalog, and what this instruction was taken to be about.
///
/// Prepended to the orchestrator's framing on every turn, because the thing it
/// most often needs is the noun the instruction left out and a tool call to
/// fetch it is a round-trip on the critical path of a dictated sentence.
///
/// Pure, and takes what it renders rather than reading the store, for the same
/// reason [`Brief`] does: what the orchestrator is told is a decision worth
/// testing on its own, and a builder that opened a database could only be
/// tested by writing to one.
pub fn project_context(
    catalog: &[crate::projects::Project],
    settled: Option<&crate::projects::Resolution>,
    current: Option<&crate::projects::Project>,
) -> String {
    use crate::projects::How;

    if catalog.is_empty() {
        return String::from(
            "Reljod's project catalog is empty. If he mentions working somewhere, \
             put it in the catalog with `project_add` — until a repository is \
             listed, every later instruction about it has to name the path in full.",
        );
    }

    let mut out = String::from("Reljod's projects, most recently worked in first:\n");
    for p in catalog {
        out.push_str(&format!("  - {}\n", p.summary_line()));
    }

    match (current, settled.map(|r| r.how)) {
        (Some(p), Some(How::Sticky)) => {
            out.push_str(&format!(
                "\nThis conversation is about **{}**, carried over: nothing in this \
                 instruction named a project. That is ordinary for dictated speech and \
                 usually right — but it is also the way this gets quietly wrong, so if \
                 the instruction reads like it belongs somewhere else, say so and call \
                 `project_switch` rather than delegating into the wrong repository.\n",
                p.name
            ));
        }
        (Some(p), Some(How::Inferred)) => {
            out.push_str(&format!(
                "\nThis instruction named **{}**, so that is what the conversation is \
                 now about.\n",
                p.name
            ));
        }
        (Some(p), _) => {
            out.push_str(&format!("\nThis conversation is about **{}**.\n", p.name));
        }
        (None, _) => {
            out.push_str(
                "\nThis conversation is not about any project yet, and this instruction \
                 did not settle it — it either named none, or named more than one. Do \
                 not pick for him silently: ask which, or say which you are assuming \
                 and why, and call `project_switch` so the next instruction inherits \
                 the right one.\n",
            );
        }
    }
    out
}

/// The mode, but never one that cannot act.
///
/// A chat that only delegates still has to be able to call the tools that
/// delegate. Below `AcceptEdits` it cannot, and the failure is the bad kind —
/// it reads and reasons and describes, so it looks like it is working right up
/// until nothing was started. Named rather than inlined so the floor is one
/// decision with one reason attached to it.
fn at_least_acting(mode: PermissionPolicy) -> PermissionPolicy {
    if crate::mcp::permits(mode, PermissionPolicy::AcceptEdits) {
        // `mode` is at or above `accept_edits`, so it is safe to honour.
        mode
    } else {
        PermissionPolicy::AcceptEdits
    }
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
    /// Which project this instruction was taken to be about, when that was
    /// settled before the orchestrator ran.
    ///
    /// Returned so the caller can *show* it. A sticky resolution is right most
    /// of the time and silently wrong the rest, and the difference only becomes
    /// correctable if Reljod can see which one he just got.
    pub project: Option<crate::projects::Resolution>,
}

/// Give an instruction to the pinned main chat.
///
/// **Every way into the main chat comes through here** — `jod main`, the TUI's
/// `/main`, and the Telegram bridge — because "which conversation, which tools,
/// which permission mode" has four bugs behind it already
/// (`tests/e2e/main-chat/REPORT.md`) and a second copy would be a second place
/// for the fifth to hide. In `core` rather than the CLI because the bridge is
/// here too.
///
/// `carried` is prior context the harness has no session for: after `/harness`
/// the pin moves to a conversation the target has never seen, so the summary
/// travels in the framing or it is lost. `None` on every ordinary turn, and
/// `None` from the bridge, which holds no thread state of its own.
///
/// `run_name` is cosmetic — the name a run answers to in `jod ls`. The console
/// passes `main`; the bridge passes the chat's
/// [`crate::telegram::session_key`] so a listing says which phone chat started
/// it. Everything load-bearing is fixed here.
///
/// `permission` is the operator's chosen mode, and it used to be a constant —
/// **the top of the chain that made `auto` a lie.** The console showed `auto`,
/// this span the orchestrator up in `accept_edits` anyway, its MCP server took
/// that ceiling, and `open_work` capped every background session against it. So
/// work delegated from an `auto` chat ran two levels down in a mode where
/// headless Claude Code has nobody to ask, and refused `git init`.
pub async fn hand_to_orchestrator(
    jod: &Jod,
    instruction: &str,
    kind: HarnessKind,
    cwd: PathBuf,
    carried: Option<String>,
    run_name: &str,
    permission: PermissionPolicy,
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

    // Settled here rather than left to the model, and settled *before* the
    // turn: naming a project is not a judgement call, and paying a round-trip
    // to be told what the words already said would put a model in the way of
    // every dictated sentence. What the model gets is the residue — the cases
    // that genuinely need judgement — plus the catalog to judge against.
    //
    // A catalog that cannot be read is not a reason to refuse the instruction:
    // the orchestrator worked without projects until now and still can.
    let settled = store.settle_project(&id, instruction).unwrap_or(None);
    let catalog = store.projects(false).unwrap_or_default();
    let current = store.current_project(&id).unwrap_or(None);
    let projects = project_context(&catalog, settled.as_ref(), current.as_ref());

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
                // The catalog goes after the preamble and before any carried
                // summary, matching the same rule: standing brief, then the
                // state it applies to, then history.
                system: Some(match &carried {
                    Some(context) => {
                        format!("{}\n\n{projects}\n\n{context}", orchestrator_preamble())
                    }
                    None => format!("{}\n\n{projects}", orchestrator_preamble()),
                }),
                cwd,
                model: None,
                // The operator's mode, floored at `AcceptEdits`.
                //
                // **The floor is the part with a bug behind it.** Plan mode
                // refuses every mutation, including the MCP calls that *are*
                // this run's job: caught live, the orchestrator called
                // `schedule_list`, reached for `ExitPlanMode`, and wrote a plan
                // file instead of arming the schedule. Below `AcceptEdits` the
                // chat is not cautious, it is inert while appearing to work.
                //
                // Above the floor it passes straight through, so a console in
                // `auto` hands its work to sessions in `auto`. Confinement is
                // `ToolAccess` either way; the permission axis bounds what it
                // may do to the *machine*.
                permission: at_least_acting(permission),
                // Asked against `kind` — the harness this spawn actually
                // launches — and not bare, because the pinned conversation is
                // resolved by `main_conversation` without reference to it. An
                // old `/harness` switch therefore leaves the pin naming one
                // harness while the console runs another, and a session id read
                // off that row goes straight to a program that never issued it.
                resume: store.resume_for(&id, kind)?,
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
        project: settled,
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

    // The third parameter is the *model*, not a name.
    //
    // It used to be `Some(&work.title)`, and the cost of that one substitution
    // was the entire feature: a fresh work's title is the truncated
    // instruction, `prefer_conversation_settings` copies the conversation's
    // model onto every request, and so every work session was launched with
    // `--model "You are checking Jod's own plumbing end to"`. Both harnesses
    // refused it and exited 1. No work session had ever successfully started.
    //
    // It survived because `prepare_work`'s tests inspect the `SpawnRequest` it
    // returns and never spawn anything — the field was populated, so it looked
    // right, and nothing asked whether the value was a model. Found by an
    // end-to-end script that actually launched a harness.
    let conversation = store.new_conversation(
        opening.harness,
        &checkout.to_string_lossy(),
        opening.model.as_deref(),
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
        // The same roots and secrets the preamble describes, actually handed
        // to the run.
        //
        // These were fetched above and used only to write the prose, so the
        // brief named a `$STRIPE_API_KEY` nothing put in the environment and
        // directories no `--add-dir` granted. Every construction site ended
        // `..SpawnRequest::default()`, handing the supervisor's injection and
        // redaction an empty list on every real run.
        //
        // The failure was invisible in the worst way: "the value appears
        // nowhere in the database" passed *trivially*, because no value had
        // ever been near it.
        //
        // Names only, never values — the supervisor resolves them at exec.
        roots: roots.iter().map(|r| r.path.clone()).collect(),
        secrets: secrets.iter().map(|s| s.name.clone()).collect(),
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
    let conversation = store.open_titler(&work.id, harness).ok()?;
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
        self.set_pinned_conversation(&id)?;
        Ok(id)
    }

    /// Make this the main chat, and the only one.
    ///
    /// The `pinned = 0` first is not tidiness. `pinned_conversation` is a
    /// `query_row` with no `ORDER BY`, so a second pinned row does not fail
    /// loudly — it makes which conversation counts as main depend on the order
    /// SQLite happens to return, and `hand_to_orchestrator` would start
    /// appending Reljod's instructions to whichever one won. One statement
    /// clearing the flag and one setting it, in a single transaction, is what
    /// keeps "the main chat" a question with one answer.
    pub fn set_pinned_conversation(&self, id: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute("UPDATE conversations SET pinned = 0 WHERE pinned = 1", [])?;
            tx.execute(
                "UPDATE conversations SET pinned = 1, title = 'main' WHERE id = ?1",
                rusqlite::params![id],
            )?;
            Ok(())
        })
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

    /// Hang one conversation under another, with no work involved.
    ///
    /// [`crate::works::Store::attach_conversation`] is the richer form and
    /// needs a work to attach to. A `delegate`d run has none by design, and
    /// declining to record its parent because there is no work to record it
    /// *under* is how a delegation ended up leaving no trace anywhere: the run
    /// existed, and nothing in the database said who had asked for it.
    ///
    /// This writes the one column the cascade and the tree both read, and
    /// nothing else. It does **not** put the conversation in the fleet tree —
    /// that is keyed on `work_id`, and a loose run is loose on purpose.
    pub fn set_conversation_parent(&self, child: &str, parent: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE conversations SET parent_conversation_id = ?2 WHERE id = ?1",
                rusqlite::params![child, parent],
            )?;
            Ok(())
        })
    }

    /// Who started this conversation, when anybody did.
    pub fn parent_conversation(&self, conversation_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT parent_conversation_id FROM conversations WHERE id = ?1",
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

    // ---- what the chat may do ----

    /// **Regression: the console said `auto` and the work ran in `edits`.**
    ///
    /// Three constants in series threw the operator's mode away — this floor
    /// when it was `PermissionPolicy::AcceptEdits` outright, the per-run MCP
    /// server that never received `--max-permission`, and `open_work` capping
    /// against the ceiling that produced. The visible symptom was two levels
    /// down: a background session refusing `git init` in a directory it had
    /// been told to create, while the status bar said everything was
    /// auto-approved.
    ///
    /// Both halves are asserted. Passing the mode through is the fix; keeping
    /// the floor is what stops the fix turning a cautious mode into an inert
    /// chat that reads and reasons and never starts anything.
    #[test]
    fn the_chat_takes_the_operators_mode_but_never_one_that_cannot_delegate() {
        assert_eq!(
            at_least_acting(PermissionPolicy::Bypass),
            PermissionPolicy::Bypass,
            "a console in auto still handed its work to accept_edits"
        );
        assert_eq!(
            at_least_acting(PermissionPolicy::AcceptEdits),
            PermissionPolicy::AcceptEdits
        );
        for inert in [PermissionPolicy::Plan, PermissionPolicy::Ask] {
            assert_eq!(
                at_least_acting(inert),
                PermissionPolicy::AcceptEdits,
                "{inert:?} would leave the chat unable to call the tools that are its whole job"
            );
        }
    }

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
        // The verb by the name it is registered under, not a paraphrase.
        assert!(said.contains("`claim_worktree`"), "{said}");
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

    /// **Every verb the brief names has to exist.** A preamble that tells an
    /// agent to call something the catalogue does not advertise costs it a turn
    /// discovering that, and reads to it as Jod being broken — and there is no
    /// compiler and no test that would otherwise notice, because a prompt is a
    /// string. This is the check that would have caught `claim_worktree` being
    /// described and never registered.
    #[test]
    fn every_tool_the_preamble_tells_an_agent_to_call_is_one_that_exists() {
        let roots = [root("/repo", false), root("/repo-worktree", true)];
        let secrets = [secret("STRIPE_API_KEY", "the live key")];
        let peers = ["scout".to_string()];
        let registered: Vec<&str> = crate::mcp::catalogue().iter().map(|t| t.name).collect();

        for harness in [HarnessKind::ClaudeCode, HarnessKind::OpenCode, HarnessKind::Agy] {
            for tools in [
                Some(ToolAccess::ReadOnly),
                Some(ToolAccess::Delegate),
                Some(ToolAccess::Orchestrate),
                None,
            ] {
                let said = worker_preamble(&Brief {
                    harness,
                    roots: &roots,
                    secrets: &secrets,
                    peers: &peers,
                    tools,
                });
                for span in tools_named_in(said.as_str()) {
                    assert!(
                        registered.contains(&span),
                        "the brief tells a {} session to call `{span}`, which no tool is \
                         registered under",
                        harness.label()
                    );
                }
            }
        }

        // The orchestrator's preamble is the other one that names tools, and it
        // was not covered here — which is how it came to define what a **work**
        // is while naming no tool that opens one. A misspelling in it fails the
        // same way a misspelling in a worker's brief does: silently, as a model
        // reaching for something that is not there.
        for span in tools_named_in(orchestrator_preamble()) {
            assert!(
                registered.contains(&span),
                "the orchestrator is told to call `{span}`, which no tool is registered under"
            );
        }
    }

    /// Anything in backticks that looks like a tool name: lower case,
    /// underscores, no path separators or spaces.
    fn tools_named_in(said: &str) -> Vec<&str> {
        said.split('`')
            .skip(1)
            .step_by(2)
            .filter(|span| {
                !span.is_empty()
                    && span.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    && span.contains('_')
            })
            .collect()
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

    /// A session told to reply with a tool it was never given spends a turn
    /// discovering that, and reads the refusal as Jod being broken.
    #[test]
    fn a_read_only_session_is_not_told_to_send_messages_it_cannot_send() {
        let peers = ["scout".to_string()];
        let said = worker_preamble(&Brief {
            harness: HarnessKind::ClaudeCode,
            roots: &[],
            secrets: &[],
            peers: &peers,
            tools: Some(ToolAccess::ReadOnly),
        });
        assert!(said.contains("you cannot send"), "{said}");
        assert!(!said.contains("`send_message`"));
        assert!(said.contains("read_messages"), "reading is still worth doing");
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

    /// A drifting noun is a bug, so the orchestrator is given the vocabulary
    /// the rest of the system uses rather than left to invent synonyms for it.
    #[test]
    fn the_orchestrator_is_taught_the_words_the_rest_of_jod_uses() {
        let said = orchestrator_preamble();
        for word in [
            "**work**",
            "**session**",
            "**root**",
            "**lease**",
            "**card**",
            "**project**",
        ] {
            assert!(said.contains(word), "{word} is not defined for the orchestrator");
        }
        assert!(said.contains("read-only"));
        assert!(said.contains("**You do not do the work.**"));
    }

    /// E4.S4's other half, and the one that went missing.
    ///
    /// The vocabulary above shipped and this did not: the preamble defined what
    /// a **work** is and then never named the tool that opens one, so the
    /// orchestrator knew the noun and had no verb for it and every instruction
    /// about a repository went to `delegate`. A `delegate`d run has no
    /// `work_id`, and `Store::forest_of` selects on `work_id IS NOT NULL` —
    /// so the fleet tree showed nothing, correctly, because there was nothing.
    ///
    /// The test above passes on a preamble with no `open_work` in it at all,
    /// which is exactly how the gap stayed green. This one does not.
    #[test]
    fn the_orchestrator_is_told_which_tool_opens_a_work() {
        let said = orchestrator_preamble();
        assert!(
            said.contains("`open_work`"),
            "the orchestrator is taught what a work is and not how to open one"
        );
        let opens = said.find("`open_work`").expect("checked above");
        let delegates = said.find("`delegate` only").expect("delegate is still offered");
        assert!(
            opens < delegates,
            "`delegate` is offered before `open_work`, so the cheaper and less \
             visible of the two reads as the default"
        );
    }

    /// The answer arrives carried, and this pins which verb the chat is sent
    /// to when it does.
    ///
    /// Worth a test of its own because the obvious wording is wrong and was
    /// written first. This bullet said `read_messages`, which reads as the
    /// natural verb for "mail arrived" and would have spent a turn finding an
    /// empty inbox: `hand_mail_to_conversation` marks the message delivered as
    /// it queues it, and `drain_inbox` selects `delivered = 0`. What actually
    /// reaches the chat is the message and its number, already in the turn, so
    /// the only verb it needs is the one that answers into the same thread.
    #[test]
    fn the_orchestrator_is_told_an_answer_arrives_carried_rather_than_fetched() {
        let said = orchestrator_preamble();
        assert!(
            said.contains("carrying the message and its number"),
            "the chat is not told the answer is already in front of it: {said}"
        );
        assert!(
            said.contains("You do not fetch it and you do not wait for it"),
            "{said}"
        );
        assert!(
            !said.contains("`read_messages` when a turn opens"),
            "the chat is sent to an inbox that this path has already emptied: {said}"
        );
        assert!(
            said.contains("pass `tools: \"delegate\"`"),
            "a read-only child cannot report back, and the chat has to know it: {said}"
        );
    }

    /// R5: `ToolAccess::Orchestrate` decides which of *Jod's* tools the main
    /// chat gets and nothing else. The harness hands it a shell, file editors,
    /// a web fetcher, its own sub-agent spawner and a tool search on top of
    /// them, and no flag Jod passes takes any of those away — measured, with
    /// the transcripts, in `docs/harness-support.md` under "Tools are not a
    /// sandbox either".
    ///
    /// So the only thing Jod can say about the boundary today is *said*, in the
    /// preamble. That is guidance rather than a wall, and this test is the
    /// least it has to keep saying: a main chat that is never told the Jod
    /// tools are all of them will reach for the shell the moment it wants
    /// something they do not cover, which is exactly what it did.
    #[test]
    fn the_orchestrator_is_told_that_jods_tools_are_the_whole_toolbox() {
        let said = orchestrator_preamble();
        assert!(
            said.contains("**That list is the whole toolbox.**"),
            "the orchestrator is handed a harness full of other tools and never \
             told they are out of bounds: {said}"
        );
        // Named one by one rather than left to a general rule, because a
        // general rule is one the model can decide does not cover a shell.
        for reached_for in ["shell", "sub-agents", "tool search"] {
            assert!(
                said.contains(reached_for),
                "`{reached_for}` is not named as something outside the toolbox"
            );
        }
        assert!(
            said.contains("Reading a file to understand what you are being asked is fine"),
            "reading has to stay allowed, or the chat cannot see what it is routing"
        );
    }

    /// The preamble offered no branch for answering at all, so a question the
    /// chat already knew the answer to still bought a spawned agent. The branch
    /// has to be stated, and it has to be stated *before* the handing-over
    /// verbs — after them it reads as an exception to the rule rather than as
    /// the first thing to check.
    #[test]
    fn the_orchestrator_is_told_it_may_answer_a_quick_question_itself() {
        let said = orchestrator_preamble();
        assert!(said.contains("**Answer directly**"), "{said}");
        assert!(said.contains("What time is it in Manila"), "{said}");
        assert!(said.contains("what does A2A stand for"), "{said}");

        let answers = said.find("**Answer directly**").expect("checked above");
        let hands_over = said.find("**You do not do the work.**").expect("still there");
        assert!(
            answers < hands_over,
            "the handing-over rule comes before the answer branch, so answering \
             reads as an exception rather than as the first thing to check"
        );
    }

    /// The three sizes, in Reljod's own terms. Drop any one and the branch is a
    /// two-way choice again.
    #[test]
    fn the_orchestrator_is_told_the_task_decides_which_branch_it_takes() {
        let said = orchestrator_preamble();
        assert!(said.contains("**Decide by the task.**"), "{said}");
        assert!(said.contains("answer in one turn"), "{said}");
        assert!(said.contains("still be running when this turn ends"), "{said}");
        assert!(said.contains("really a project, you open a work for"), "{said}");
    }

    /// The summary of the three sizes is a way in, not a routing table, and on
    /// its first live run it behaved like one: "keep working until the README
    /// explains what jod main does" went to `open_work` because it will outlast
    /// the turn and touch a repository, when `tests/e2e/main-chat/REPORT.md`
    /// records that same instruction arming a goal. The paragraph now says
    /// which of the two wins.
    #[test]
    fn the_summary_of_the_three_sizes_does_not_override_the_verb_list() {
        let said = orchestrator_preamble();
        assert!(said.contains("Only the first of those three is new"), "{said}");
        assert!(said.contains("this paragraph does not override it"), "{said}");
        assert!(said.contains("is still `goal_create`"), "{said}");
        assert!(said.contains("is still `schedule_create`"), "{said}");
    }

    /// Answering is bounded by three things, and the bound is the whole reason
    /// this is not a licence to do the work. Losing any of them lets the chat
    /// start reading a checkout, which is the failure the old rule was written
    /// against and which this change must not reintroduce.
    #[test]
    fn the_answer_branch_is_bounded_rather_than_open_ended() {
        let said = orchestrator_preamble();
        assert!(said.contains("needs no repository"), "{said}");
        assert!(said.contains("no work that outlasts this turn"), "{said}");
        assert!(said.contains("nothing you would have to go away and research"), "{said}");
        assert!(
            said.contains("Touching a repository \\\nnever is")
                || said.contains("Touching a repository never is"),
            "a trivial in-turn call is still answering, and a repository still is not: {said}"
        );
        assert!(
            said.contains("counting what a repository contains is a `delegate`, not an answer"),
            "the first live run of this branch answered \"count the files in this repository\" \
             itself, with a shell command, which is the failure the old rule existed for: {said}"
        );
        assert!(
            said.contains("hand it over rather than guess"),
            "an unsure orchestrator has to delegate, not answer: {said}"
        );
        assert!(
            said.contains("**You do not do the work.**"),
            "the old rule still governs everything past the answer branch: {said}"
        );
    }

    /// The observed failure named the project — "what does the acronym A2A
    /// stand for **in this project**" — and the chat read those three words as
    /// an errand into a checkout. It is a definition, and the preamble has to
    /// say so or the same phrasing routes the same way again.
    #[test]
    fn naming_the_project_does_not_by_itself_make_it_repository_work() {
        let said = orchestrator_preamble();
        assert!(
            said.contains("Naming the project does not by itself make it repository work"),
            "{said}"
        );
        assert!(said.contains("needing to *look*"), "{said}");
    }

    // ---- what the orchestrator is told about projects ----

    fn catalogued(name: &str, notes: &str) -> crate::projects::Project {
        crate::projects::Project {
            id: name.into(),
            name: name.into(),
            path: PathBuf::from(format!("/home/reljod/repo/{name}")),
            remote: None,
            aliases: Vec::new(),
            state: crate::projects::State::Active,
            colour: "cyan".into(),
            notes: notes.into(),
            created_at_ms: 0,
            last_touched_ms: 0,
        }
    }

    fn resolution(how: crate::projects::How) -> crate::projects::Resolution {
        crate::projects::Resolution {
            id: 1,
            conversation_id: "c".into(),
            project_id: Some("tetris".into()),
            utterance: "btw, let's fix this".into(),
            how,
            reason: String::new(),
            corrected: false,
            decided_at_ms: 0,
        }
    }

    /// The catalog is the noun a dictated instruction left out, so it has to be
    /// in the framing rather than a tool call away.
    #[test]
    fn the_orchestrator_is_told_the_whole_catalog() {
        let catalog = [catalogued("tetris", ""), catalogued("jod", "the agent")];
        let said = project_context(&catalog, None, None);
        assert!(said.contains("tetris"), "{said}");
        assert!(said.contains("jod"), "{said}");
        assert!(said.contains("the agent"), "a project's note was dropped: {said}");
    }

    /// A carried project is right most of the time and silently wrong the rest,
    /// so the framing has to say which kind of answer this is.
    #[test]
    fn a_sticky_project_is_flagged_as_carried_rather_than_stated_as_fact() {
        let catalog = [catalogued("tetris", "")];
        let said = project_context(
            &catalog,
            Some(&resolution(crate::projects::How::Sticky)),
            Some(&catalog[0]),
        );
        assert!(said.contains("carried over"), "{said}");
        assert!(said.contains("project_switch"), "no way out was offered: {said}");
    }

    /// A project he actually named needs no hedging.
    #[test]
    fn a_named_project_is_stated_plainly() {
        let catalog = [catalogued("tetris", "")];
        let said = project_context(
            &catalog,
            Some(&resolution(crate::projects::How::Inferred)),
            Some(&catalog[0]),
        );
        assert!(said.contains("named **tetris**"), "{said}");
        assert!(!said.contains("carried over"), "an explicit name was hedged: {said}");
    }

    /// The case the string matcher deliberately refuses to decide must reach
    /// the model as a question, not as an absence.
    #[test]
    fn an_unsettled_instruction_tells_the_orchestrator_not_to_pick_silently() {
        let catalog = [catalogued("tetris", ""), catalogued("jod", "")];
        let said = project_context(&catalog, None, None);
        assert!(said.contains("not about any project yet"), "{said}");
        assert!(said.contains("Do not pick for him silently"), "{said}");
    }

    /// An empty catalog must read as "add one", not as a broken listing.
    #[test]
    fn an_empty_catalog_says_how_to_fill_it() {
        let said = project_context(&[], None, None);
        assert!(said.contains("project_add"), "{said}");
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
        // `put_secret` writes a real file under the process-wide `JOD_HOME`, so
        // this test has to own that variable for as long as it runs.
        //
        // Without the lock and the override it landed in whichever temporary
        // home another test happened to have set — and, when none had, in the
        // developer's actual `~/.jod/secrets/`. That was observed: an ordinary
        // `cargo test` left a `STRIPE_API_KEY.<hash>` file in a real Jod home.
        // Nothing was exposed, because the value is this test's own, but
        // `put_secret` upserts by name, so on a box with that key genuinely
        // stored the suite would have overwritten a live credential.
        //
        // The guard restores the previous value rather than unsetting it: on a
        // machine where `JOD_HOME` is configured, unsetting it would send every
        // later reader to `~/.jod` instead of the home it was told about.
        let _home = crate::secrets::tests::Home::new("jod-brief");

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
        assert!(brief.contains("`claim_worktree`"));
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

    /// A work's session is launched with a model, not with its own title.
    ///
    /// This test exists because it was not. `new_conversation`'s third
    /// parameter is `model: Option<&str>` and it was being handed
    /// `Some(&work.title)` — so every session ever opened for a work was
    /// launched with `--model` set to the truncated instruction, and every one
    /// of them was refused by its harness and exited 1. The feature had never
    /// once worked outside a test.
    ///
    /// Asserting the *absence* of the title matters as much as the presence of
    /// the model: a later refactor that reaches for a human-readable string
    /// here would be reintroducing exactly this, and a test that only checked
    /// `model == Some("opus")` would pass while `None` quietly became the
    /// title again.
    #[test]
    fn a_work_session_is_launched_with_a_model_rather_than_with_its_own_title() {
        let s = store();
        let opened = prepare_work(
            &s,
            &Opening {
                model: Some("opus".into()),
                ..Opening::new("port the parser to the new lexer", "/tmp/repo")
            },
        )
        .unwrap();

        assert_eq!(opened.request.model.as_deref(), Some("opus"));

        let title = &s.work(&opened.request.name).ok().flatten().map(|w| w.title);
        let _ = title;
        assert!(
            !opened
                .request
                .model
                .as_deref()
                .unwrap_or_default()
                .contains("port the parser"),
            "the instruction reached the model field: {:?}",
            opened.request.model
        );

        // And with no model asked for, none is invented.
        let bare = prepare_work(&s, &Opening::new("port the parser", "/tmp/repo")).unwrap();
        assert_eq!(
            bare.request.model, None,
            "a work with no model chose one for itself: {:?}",
            bare.request.model
        );
    }

    /// The brief and the request must agree, because the brief is a promise.
    ///
    /// This test exists because they did not. `prepare_work` fetched the roots
    /// and the secrets, wrote both into the preamble, and then built a
    /// `SpawnRequest` that carried neither — so the agent was told a
    /// credential existed and the supervisor was handed an empty list to
    /// inject. Both halves were tested and correct in isolation; nothing
    /// asserted that the thing describing them also *passed* them.
    ///
    /// Asserting the request rather than the prose is the point. A test that
    /// only read the preamble would have gone on passing throughout.
    #[test]
    fn the_session_is_handed_the_roots_and_secrets_its_brief_promises() {
        let _home = crate::secrets::tests::Home::new("jod-wiring");

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

        let brief = opened.request.system.as_deref().unwrap();

        // The name the brief advertises is the name the supervisor is asked to
        // resolve. Not the value: nothing on this path ever holds one.
        assert!(brief.contains("$STRIPE_API_KEY"));
        assert!(
            opened.request.secrets.contains(&"STRIPE_API_KEY".to_string()),
            "the brief promised a credential the request does not carry: {:?}",
            opened.request.secrets
        );

        // Likewise the directories. A root the harness is never granted is a
        // root Jod claimed to have given and did not.
        assert!(
            !opened.request.roots.is_empty(),
            "a session was handed no roots at all"
        );
        assert!(
            opened
                .request
                .roots
                .iter()
                .any(|r| r.to_string_lossy().contains("repo")),
            "the checkout named in the brief is missing from the request: {:?}",
            opened.request.roots
        );
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

    /// A delegated run has no work, so `attach_conversation` cannot record who
    /// asked for it. Without this the conversation is a root nothing points at,
    /// and the delegation leaves no trace anywhere in the database.
    #[test]
    fn a_workless_conversation_can_still_say_who_started_it() {
        let s = store();
        let parent = s.main_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp").unwrap();
        let child = s
            .new_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap()
            .id;

        s.set_conversation_parent(&child, &parent).unwrap();

        assert_eq!(s.parent_conversation(&child).unwrap().as_deref(), Some(parent.as_str()));
        assert!(
            s.work_for_conversation(&child).unwrap().is_none(),
            "linking a parent must not smuggle the child into a work — a \
             delegated run is loose on purpose"
        );
    }

    /// The tree is keyed on `work_id`, so a loose run stays out of it however
    /// its parentage is recorded. Stated as a test because the obvious "fix"
    /// for an invisible delegation is to relax that filter, and relaxing it
    /// puts every throwaway lookup on Reljod's fleet screen.
    #[test]
    fn a_delegated_run_is_linked_but_still_not_in_the_fleet_tree() {
        let s = store();
        let parent = s.main_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp").unwrap();
        let child = s
            .new_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp", None)
            .unwrap()
            .id;
        s.set_conversation_parent(&child, &parent).unwrap();

        let ids: Vec<String> = s.forest().unwrap().into_iter().map(|n| n.id.id).collect();
        assert!(!ids.contains(&child), "a workless conversation reached the fleet tree");
    }
}
