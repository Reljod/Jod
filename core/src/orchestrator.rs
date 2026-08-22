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
//! [`ToolAccess::Orchestrate`] decides which `mcp__jod__*` tools the MCP server
//! offers and nothing else; the harness keeps its own. Measured with the flags
//! this file builds, the main chat's session comes up holding 58 tools, of
//! which 26 are the harness's — a shell, file editors, a web fetcher, its own
//! sub-agent spawner — and Jod asked for none of them and cannot take them
//! away. `--allowedTools` grants without denying. So the sentences above are
//! true of everything the orchestrator does *to Jod* and are not a claim about
//! what it can do to the machine. See `docs/harness-support.md`, "Tools are not
//! a sandbox either", for the transcripts and for the one mechanism that does
//! withhold.
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
use crate::harness::{HarnessKind, PermissionPolicy, Resume, Role, SpawnRequest, ToolAccess};
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
/// This opened with "you do not do the work" and then offered nothing but ways
/// to hand something over, so every instruction bought an agent. The failure
/// that made the case: a console on a fresh store was asked "what does the
/// acronym A2A stand for in this project? answer in one line", spawned a child
/// called `a2a-acronym-lookup`, polled `list_agents` waiting for it, and after
/// 42 seconds and 39 cents said "Still working — the lookup agent is
/// mid-search." Reljod never got the answer. It needed no repository, and the
/// chat knew it.
///
/// The old rule was written against a real failure — a main chat that starts
/// reading a checkout stops being a main chat — and it over-reached into
/// questions that touch no checkout at all. So the size of the task picks the
/// branch, and answering is the first one considered. Everything past it is
/// unchanged: the moment an instruction needs a checkout, a tool beyond recall,
/// or anything still running when the turn ends, the routing below is exactly
/// what it always was.
///
/// This is the shape `docs/spec-ceo-and-managers.md` settles on for main — "it
/// routes and it answers", and "main may route and may run repo-less one-shots".
/// The manager tier that spec adds around it has since shipped too, which is
/// why the tool list below names `ask_manager` and says `open_work` is not
/// main's to call.
///
/// ## Why the branch left again
///
/// Answering was main's for one release and it is the assistant's now. The
/// branch itself was right and the layer it sat on was wrong: main's turn *was*
/// the routing decision, so the console stayed busy for as long as the decision
/// took, and a second instruction typed while it thought came back `queued —
/// sends when this turn ends`. Main is the thing you reach for while something
/// is already running, so a main chat you cannot use while it thinks is the one
/// failure it cannot have.
///
/// So the whole decision — answer, delegate, or hand to a manager — moved down
/// one layer into [`assistant_preamble`], which runs in a conversation of its
/// own that nobody is typing into. What is left here is intake: read what
/// Reljod said, call `ask_assistant`, come back. One tool call, and the box is
/// free again.
///
/// Schedules and goals stayed, and that is not an oversight. Arming one spends
/// money at 2am with nobody watching, which is the reason
/// `docs/spec-ceo-and-managers.md` kept them with main in the first place, and
/// moving the routing decision changes nothing about that argument.
pub fn orchestrator_preamble() -> &'static str {
    "You are Jod's main chat: Reljod's front desk. You take what he says and \
     you hand it straight on. You do not route it, you do not pick a \
     repository, and you do not answer it yourself.\n\n\
     **One instruction, one `ask_assistant`, and your turn is over.** Pass his \
     words through exactly as he said them — do not summarise them, do not \
     tidy them up, do not resolve which project he meant. The assistant below \
     you is the one that decides whether the instruction needs a repository, a \
     one-shot agent, or nothing but an answer, and it reports back onto your \
     rail. Even a question you are sure you know the answer to goes to it. \
     That costs one cheap hop and it buys the thing this chat is for: Reljod \
     cannot type a second instruction while your turn is in flight, so a turn \
     that stops to think is a chat he cannot use at the moment he most wants \
     it.\n\n\
     **You never wait, for anything.** Not for the assistant, not for a run \
     below it, not for a card to be answered. Everything you hand over reaches \
     you later as its own event. There is no branch here that ends in you \
     watching something.\n\n\
     Two instructions do not go to the assistant, and they are the two that \
     spend money at 2am with nobody watching. An instruction that says *when* \
     is still `schedule_create`, and one that says *keep* or *until* is \
     still `goal_create`. Arming those stays your call.\n\n\
     **You do not do the work.** You never did, and now you do not decide who \
     does either. If you catch yourself reading a file, comparing two \
     projects, or working out what he probably meant, you have taken the \
     assistant's job as well as an engineer's.\n\n\
     You have Jod's own tools. Use them:\n\
     - `ask_assistant` for everything Reljod asks you, in his own words. It \
       returns as soon as the assistant has taken the instruction, which is \
       long before anything has been done about it. There is one assistant and \
       it remembers, so a follow-up reaches something that heard the first \
       half; if it is mid-turn your instruction is queued and reaches it at \
       the start of its next one. What it makes of the instruction comes back \
       to you either as a card on your rail or as a message that starts a turn \
       of yours. This is very nearly the only verb you have.\n\
     - `schedule_create` when the instruction says *when*. `goal_create` when it \
       says *keep* or *until*.\n\
     - `recall` and `related` before asking Reljod something he has already told \
       you, and `remember` for something he has just told you that the next \
       conversation will need.\n\
     - `list_agents` when he asks what is running, and **once** — a second call \
       in the same turn is refused, because the only reason to look twice is to \
       wait for something to change, and you do not wait.\n\
     - `stop_agent` for something running that he has said should not be.\n\
     - `reply` when a turn opens with a message from a run below you. \
       Everything Jod starts can answer you: you are `main` on its roster, \
       and what it sends arrives as a turn of yours, carrying the message and \
       its number. You do not fetch it and you do not wait for it — it starts \
       a turn on its own, whenever it lands.\n\
     - `record_decision` and `ask_question` for anything Reljod should see. \
       Findings and choices go on the rail, not into a sentence he has to \
       scroll back for.\n\n\
     `open_work` is **not yours to call**, and neither is `ask_manager` or \
     `delegate`. All three are refused at the tool boundary and the refusal \
     names `ask_assistant`, which was the answer anyway. A `delegate` pointed \
     at a project's checkout is refused too — that was the old way round the \
     rule, and there is no way round this one, because handing the whole \
     instruction on is the only verb you have.\n\n\
     **That list is the whole toolbox.** The harness running you carries plenty \
     of its own tools — a shell, file editors, a web fetcher, its own way of \
     starting sub-agents — and none of them are yours. Reading a file to \
     understand what you are being asked is fine. Everything past reading \
     belongs to somebody else, and `ask_assistant` is how you hand it over. \
     When what you want is not on the list above, hand the work over \
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
     let's fix this\" is a normal instruction here, not a malformed one. It is \
     also not yours to interpret. Hand it on as he said it: the catalog and \
     what this conversation is already about travel with it, so the assistant \
     reads the same words you did with the same context beside them, and it is \
     the one that has to work out the missing noun. Asking him to say it again \
     more clearly is the one reply that is always wrong here.\n\n\
     Answer in one or two sentences: that you have passed it on, and to whom. \
     Then stop."
}

/// What `conversations.origin` says about an assistant's conversation.
///
/// A string rather than a [`crate::works::Origin`] variant because that enum is
/// the *session* origin — who opened a session inside a work — and an assistant
/// belongs to no work. The column is shared, so the value has to be spelled
/// somewhere; spelling it once here is what keeps the writer and the two
/// readers (the recursion guard in [`crate::mcp`], and the scratch sweep) from
/// disagreeing about it.
pub const ASSISTANT_ORIGIN: &str = "assistant";

/// Where the pointer to the assistant's standing conversation is kept.
///
/// In `settings` rather than in a column, for the reason
/// [`crate::works::MAX_ENGINEERS_SETTING`] is: `settings` is key and value, so
/// it needs no migration. It is also the honest shape — the fact is "which
/// conversation is the assistant's", which is one row for the whole database,
/// not a property of every conversation.
///
/// Read by [`Store::assistant_conversation`] and moved by the carry-forward
/// behind [`Store::continue_as_new`], which is the only other thing that may
/// change which row it names.
pub const ASSISTANT_SETTING: &str = "assistant_conversation_id";

/// What handing an instruction to an assistant produced.
#[derive(Debug, Clone)]
pub struct Assisted {
    /// The run now carrying the instruction, when one was started.
    ///
    /// `None` when the assistant was already mid-turn: the instruction was
    /// queued for its next turn rather than starting a second session beside
    /// the first. See [`Assisted::queued`].
    pub run_id: Option<String>,
    /// The assistant's conversation — the same one every time.
    pub conversation_id: String,
    /// What the row answers to in the fleet.
    pub name: String,
    /// Whether the instruction was queued rather than started.
    ///
    /// Not a failure and not a delay Reljod pays for: [`hand_to_assistant`]
    /// returned just as fast either way, and the queued instruction is
    /// delivered into the assistant's next turn by
    /// [`crate::ticker::Ticker::tick_deliveries`], batched with anything else
    /// that arrived meanwhile. It is reported because the caller says something
    /// different — "it is on it" against "it will read it when this turn ends"
    /// — and because a distinction nobody can see is one nobody can debug.
    pub queued: bool,
    /// Whether the assistant's thread is due for compaction, and why.
    ///
    /// The same pair [`Handed::compaction_due`] carries for main, computed by
    /// the same [`should_compact`] against the same two clocks. A standing
    /// conversation that never compacts grows until the harness refuses it, and
    /// the assistant is a standing conversation now.
    pub compaction_due: Option<(&'static str, usize)>,
}

/// Give one instruction to the assistant, and come straight back.
///
/// **Returns as soon as the instruction has been taken, and must never learn to
/// wait.** This is called from inside main's own turn, and main's turn is the
/// thing blocking the console: everything this function does before it returns
/// is time Reljod cannot type into the box. There is nothing here to await
/// beyond a spawn.
///
/// ## One standing assistant, interrupted rather than duplicated
///
/// The assistant used to be created fresh for every instruction and never
/// resumed. The argument for that was serialisation — a standing assistant
/// would make instruction two wait behind instruction one, moving the console's
/// block down a layer instead of removing it — and it is answered here rather
/// than ignored.
///
/// There are two cases and neither of them blocks:
///
/// - **The assistant is free.** Its conversation is resumed, the instruction
///   becomes its next user turn, and the run starts. This is the ordinary case.
/// - **The assistant is mid-turn.** The instruction is queued in
///   [`crate::delivery`] as a [`crate::delivery::Kind::Human`] item and
///   delivered into the assistant's *next* turn by
///   [`crate::ticker::Ticker::tick_deliveries`], batched with anything else that
///   arrived meanwhile. That module's own header calls batching a feature: an
///   agent reading everything that changed in one go answers more coherently
///   than one woken repeatedly with a line each.
///
/// A second run must not simply be spawned beside the first, and that is the
/// part worth being precise about. Both would resume the *same* harness session
/// id, so two processes would be extending one transcript at once — which is
/// not two assistants working in parallel, it is one transcript being written
/// by two hands.
///
/// ## What the standing thread buys
///
/// The assistant remembers. It can hand out work off a message that arrived
/// after its turn began, it knows what Reljod asked for a minute ago without
/// being told again, and a follow-up that only makes sense against the previous
/// instruction — "no, the other one" — lands somewhere that understands it.
/// None of that was possible when the thread was thrown away every turn.
///
/// ## Compaction
///
/// A standing conversation grows until the harness refuses it, so this checks
/// the same two clocks main is checked against — see [`should_compact`] — and
/// reports the verdict on [`Assisted::compaction_due`]. When it is due and
/// nothing is in flight, the summariser is started here and the instruction is
/// queued behind it, so the assistant's next turn begins from the summary
/// rather than from a transcript nobody can afford.
pub async fn hand_to_assistant(
    jod: &std::sync::Arc<Jod>,
    instruction: &str,
    kind: HarnessKind,
    cwd: PathBuf,
    permission: PermissionPolicy,
) -> Result<Assisted> {
    let store = jod.store().ok_or(JodError::StoreRequired)?;
    let (conversation_id, _fresh) =
        store.assistant_conversation(kind, &cwd.display().to_string())?;
    let now = chrono::Utc::now().timestamp_millis();

    // **The assistant has to be told which repository this is about.**
    //
    // `settle_project` ran before main's turn and wrote the answer onto *main's*
    // conversation. The assistant's thread has its own pointer, and the first
    // thing it is asked to do is call `ask_manager` with a project name; left
    // out, it would have to guess from the words alone on every instruction that
    // named no repository, which is the case dictated speech produces most
    // often. So the catalog and the settled project travel with the instruction,
    // exactly as they do into main's own turn — and they are re-read on every
    // instruction rather than once, because main's answer is the thing that
    // moves between them.
    //
    // Best-effort in every part. A catalog that cannot be read is not a reason
    // to refuse the instruction: routing worked without projects until they
    // existed and still can.
    let catalog = store.projects(false).unwrap_or_default();
    let inherited = store
        .pinned_conversation()
        .ok()
        .flatten()
        .and_then(|main| store.current_project(&main).ok())
        .flatten();
    // Written onto the assistant's own row as well as into the prose, so
    // `project_current` and everything the assistant starts agree with what it
    // was told. `How::Inherited` is not a variant here, and `Human` is the
    // honest one: this is not the assistant inferring anything, it is main's
    // settled answer being carried down.
    if let Some(project) = &inherited {
        if let Err(e) = store.set_current_project(
            &conversation_id,
            Some(&project.id),
            instruction,
            crate::projects::How::Human,
            "carried down from the main chat, which settled it before this turn",
        ) {
            eprintln!("[jod] could not carry the project into an assistant: {e}");
        }
    }
    let projects = project_context(&catalog, None, inherited.as_ref());

    // Checked before the instruction goes out, not after: the right moment to
    // summarise is *between* things, and a compaction started mid-turn would
    // summarise a thread that is still being written to.
    let live = store.live_window(&conversation_id)?;
    let chars: usize = live.iter().map(|m| m.text.len()).sum();
    let compaction_due = should_compact(chars, store.last_human_ms(&conversation_id)?, now)
        .map(|reason| (reason.as_str(), chars));

    // A turn of this conversation already in flight, which is the one fact that
    // decides between the two endings below. Read from the runs that wrote into
    // it, which is how [`crate::delivery`] asks the same question.
    let busy = store.conversation_is_busy(&conversation_id)?;

    if busy || compaction_due.is_some() {
        // Queued, not dropped and not waited on. `ref_id` is the instant it
        // arrived: the sources this queue was built for number themselves — a
        // card id, a message id — and an instruction handed straight down from
        // main has no id of its own to carry, so the one thing that certainly
        // distinguishes it is when it got here.
        store.enqueue_delivery(
            &conversation_id,
            crate::delivery::Kind::Human,
            &format!("ask-{now}"),
            instruction,
        )?;
        // The same `touch_human` a started turn gets, and for the same reason:
        // the idle clock is measured from when *Reljod* last said something, and
        // an instruction he typed is him saying something whether it started a
        // turn or joined a queue.
        store.touch_human(&conversation_id, now)?;

        // Only when nothing is in flight. A summariser started on top of a
        // running turn would summarise a transcript still being written, and one
        // started on top of another summariser would race it onto the same
        // thread — which is why the summariser runs *in* the assistant's
        // conversation rather than detached: being in it is what makes
        // `conversation_is_busy` say so, and one turn at a time falls out of
        // that rather than out of a second flag that could disagree with it.
        let run_id = match compaction_due.is_some() && !busy {
            true => start_assistant_compaction(jod, &conversation_id, kind, &cwd, permission).await,
            false => None,
        };
        return Ok(Assisted {
            run_id,
            conversation_id,
            name: ASSISTANT_MEMBER.to_string(),
            queued: true,
            compaction_due,
        });
    }

    // **A thread with no harness session still has its record, and it has to
    // travel in the prompt.**
    //
    // `resume_for` answers `Fresh` in two cases that both matter here: the turn
    // straight after a compaction, where the summary is the only thing this
    // conversation contains, and the turn after the assistant's harness changed
    // under it, where the whole thread is on the other side. Nothing in
    // `crate::runner` can stream a transcript into a harness — see
    // `Store::handoff_text`, which exists for exactly this — so a `Fresh` spawn
    // that says nothing about the past is an assistant that has quietly
    // forgotten, on a turn nobody would think to check.
    //
    // Empty means there is genuinely nothing to carry, which is the very first
    // instruction, and then there is nothing to say.
    let resume = store.resume_for(&conversation_id, kind)?;
    let carried = match resume {
        Resume::Fresh => store
            .handoff_text(&conversation_id)
            .ok()
            .filter(|text| !text.trim().is_empty()),
        _ => None,
    };

    let agent = jod
        .spawn_agent_in(
            SpawnRequest {
                name: format!("assistant {}", crate::harness::default_name(instruction)),
                harness: kind,
                // Unconditional, unlike every other role tag here, because
                // `ask_assistant` takes no harness argument — there is never a
                // named one for the row to outrank. Which harness and model the
                // routing layer runs on is a standing setting, and the reason
                // the roles table exists at all: the decision is cheap and
                // paying frontier prices for it is the thing this fixes.
                role: Some(Role::Assistant),
                prompt: instruction.to_string(),
                // The standing brief first, then the state it applies to,
                // then whatever history is being carried — the same order
                // `hand_to_orchestrator` uses, and for the same reason: a model
                // reading a catalog before it knows what it is reading it for is
                // reading it twice.
                system: Some(match &carried {
                    Some(record) => {
                        format!("{}\n\n{projects}\n\n{record}", assistant_preamble())
                    }
                    None => format!("{}\n\n{projects}", assistant_preamble()),
                }),
                cwd,
                model: None,
                // The same floor main and a manager run under, for the same
                // reason: below `AcceptEdits` this run cannot call the tools
                // that are its entire job, so it would look like it was
                // working and be inert.
                permission: at_least_acting(permission),
                // Resumed, exactly as a manager is. This is the standing thread
                // whose memory is the point; starting fresh here would throw
                // away everything the last instruction established and leave the
                // assistant answering a follow-up it has never heard the
                // beginning of. When there is no session to resume, the record
                // above is what stands in for one.
                resume,
                // It starts agents and it talks to managers, and that is all.
                // Not `Orchestrate`: arming a schedule or a goal spends money
                // at 2am with nobody watching, and that power stays with main.
                tools: Some(ToolAccess::Delegate),
                ..SpawnRequest::default()
            },
            RunConversation::Existing(conversation_id.clone()),
        )
        .await?;

    // The instruction as this conversation's user turn, so the transcript reads
    // as a conversation rather than as a system prompt with answers under it.
    store.append_prompt(&conversation_id, &agent.id, instruction)?;
    store.touch_human(&conversation_id, now)?;

    // **The return leg, which the assistant did not have.**
    //
    // Every bus tool refuses a run that belongs to no addressing scope, and the
    // assistant's conversation belongs to no work — so `send_message` and
    // `reply` answered `run … is not a member of any team or work`, and the
    // assistant's only route home was a card. A card is a row on Reljod's rail;
    // it does not start a turn of main's, so anything the assistant merely
    // *answered* — the "what time is it in Manila" branch of its brief — landed
    // in a transcript nobody opens and reached him not at all.
    //
    // This is the same channel `delegate` opens for the run it starts, and for
    // the same reason: a two-party team holding this run and `main`, so that
    // what the assistant says arrives as a turn of main's rather than as a row
    // somebody has to go looking for.
    //
    // Best-effort. A channel that could not be opened is a reason to say so and
    // carry on — the run is already going, and a card still reaches the rail.
    if let Err(e) = store.open_return_channel(&agent.id, ASSISTANT_MEMBER, kind) {
        eprintln!("[jod] the assistant has no way to answer main: {e}");
    }

    Ok(Assisted {
        run_id: Some(agent.id),
        conversation_id,
        name: agent.name,
        queued: false,
        compaction_due,
    })
}

/// What the assistant is called on the bus when it answers main.
///
/// A fixed name rather than the run's, unlike `delegate`'s one-shots. Those are
/// named after the errand because that is all they are; this is one standing
/// layer, and main reading `assistant` on its roster is reading the thing it
/// handed the instruction to. `main` and `reljod` are the two reserved names and
/// this is deliberately neither.
pub const ASSISTANT_MEMBER: &str = "assistant";

/// Start the summariser that compacts the assistant's thread, and arrange for
/// the compaction to be finished when it answers.
///
/// Detached from the caller in the way [`start_titler`] is: the spawn is not
/// awaited to completion, so main's turn is not held up by a model call, and the
/// whole thing returns an `Option` because a compaction that could not be
/// started is a thread that carries on exactly as it was. The instruction that
/// triggered it has already been queued by then, so nothing is lost either way.
///
/// **It runs inside the assistant's own conversation, and that is a departure
/// from how the console does it.** The console detaches its summariser so that
/// "summarise this" never enters the transcript being summarised. Here the run
/// has to be visible to [`Store::conversation_is_busy`], because that is what
/// stops a second summariser — or an ordinary turn — starting on top of this
/// one, and nobody is sitting in front of this thread holding a `busy` flag the
/// way the console does. The cost is one housekeeping turn in the transcript,
/// and [`Store::continue_as_new`] compacts it away along with everything else a
/// moment later.
///
/// The summary itself comes from the run, because Jod has no model client — the
/// same rule the rest of [`crate::conversation`]'s compaction is under.
async fn start_assistant_compaction(
    jod: &std::sync::Arc<Jod>,
    conversation_id: &str,
    kind: HarnessKind,
    cwd: &std::path::Path,
    permission: PermissionPolicy,
) -> Option<String> {
    let store = jod.store()?;
    // Subscribed before the spawn, or a summariser that answers quickly finishes
    // before anything is listening and the thread is never compacted.
    let events = jod.subscribe();
    // With a session, it summarises what it is already holding. Without one —
    // the turn straight after a previous compaction — it has never seen this
    // thread, so the record has to travel in the prompt instead.
    let resume = store.resume_for(conversation_id, kind).ok()?;
    let material = match resume {
        Resume::Fresh => store
            .handoff_text(conversation_id)
            .ok()
            .filter(|text| !text.trim().is_empty()),
        _ => None,
    };
    let prompt = match &material {
        Some(record) => format!("{SUMMARISE_RECORD}\n\n{record}"),
        None => SUMMARISE.to_string(),
    };
    let agent = jod
        .spawn_agent_in(
            SpawnRequest {
                name: "assistant compaction".to_string(),
                harness: kind,
                prompt,
                // Nothing to brief. It is summarising a transcript, not being
                // the assistant, and handing it the assistant's standing brief
                // would invite it to act on what it is reading.
                system: None,
                cwd: cwd.to_path_buf(),
                model: None,
                permission,
                resume: match material {
                    Some(_) => Resume::Fresh,
                    None => resume,
                },
                // No Jod verbs. This run answers a question; it does not act.
                tools: None,
                // Housekeeping, which is the whole reason that row exists: one
                // short summary of one transcript should not pay frontier
                // prices. The same tag `start_titler` uses.
                role: Some(Role::Housekeeping),
                ..SpawnRequest::default()
            },
            RunConversation::Existing(conversation_id.to_string()),
        )
        .await
        .ok()?;

    let jod = jod.clone();
    let conversation_id = conversation_id.to_string();
    let run_id = agent.id.clone();
    tokio::spawn(async move {
        let said = titler_output(events, &run_id).await;
        let Some(store) = jod.store() else {
            return;
        };
        // An empty answer is left alone rather than papered over.
        // `continue_as_new` refuses an empty summary precisely so that a thread
        // cannot be compacted into nothing, and inventing a placeholder here
        // would walk straight through that guard. The thread carries on as it
        // was, still over the threshold, and the next instruction tries again.
        if said.trim().is_empty() {
            eprintln!("[jod] the assistant's summariser said nothing, so nothing was compacted");
            return;
        }
        match store.continue_as_new(
            &conversation_id,
            said.trim(),
            crate::conversation::COMPACTED,
        ) {
            Ok(carried) => eprintln!(
                "[jod] compacted the assistant: {} chars became {}",
                carried.compaction.before_chars, carried.compaction.after_chars
            ),
            Err(e) => eprintln!("[jod] could not compact the assistant: {e}"),
        }
    });
    Some(agent.id)
}

/// What a summariser is asked for when it is already holding the thread.
///
/// A copy of the console's wording rather than a call into it: `jod-core` cannot
/// depend on the CLI, and this is the only place in core that has to ask for a
/// summary. If a third caller appears, this and [`SUMMARISE_RECORD`] belong
/// beside [`crate::conversation::COMPACTED`], which is already the one place
/// both sides agree on what a compaction is called.
const SUMMARISE: &str = "Summarise this conversation so that an agent who has \
     not seen any of it can carry it on. Cover what was asked, what was decided \
     and why, what was handed to whom, and anything still open. Names, ids and \
     numbers exactly as they were said. Prose, no preamble, no offer to help.";

/// The same request, for a summariser that has to be handed the record.
const SUMMARISE_RECORD: &str = "Below is the record of a conversation. \
     Summarise it so that an agent who has not seen any of it can carry it on. \
     Cover what was asked, what was decided and why, what was handed to whom, \
     and anything still open. Names, ids and numbers exactly as they were said. \
     Prose, no preamble, no offer to help.";

/// The framing the assistant gets, and the routing decision that used to be
/// main's.
///
/// **This is where the branch lives now.** Answer, `ask_manager`, `delegate` or
/// `continue_agent` — the same four outcomes [`orchestrator_preamble`] used to
/// choose between, moved into a conversation nobody is typing into, so that
/// thinking about the choice costs Reljod nothing.
///
/// **The assistant is one standing thread, and this brief has to say so.** It
/// used to be created fresh for one instruction and never resumed, and the
/// brief told it as much — "there is no thread here to remember". That is no
/// longer true, and a standing conversation still being told it is disposable
/// is one that will not use the transcript in front of it.
///
/// The old design's argument was serialisation, and [`hand_to_assistant`]
/// answers it: an instruction arriving mid-turn is queued and delivered into the
/// next turn rather than blocked on. So the brief now has to cover the case that
/// argument was avoiding — a turn that opens with something that arrived after
/// the turn before it started — and say plainly that the assistant may hand out
/// work from it.
///
/// It reports two ways, and both are here because they answer different
/// questions. A **card** is a row on Reljod's rail: it cascades up
/// `parent_conversation_id`, the assistant's parent is main, and it is how a
/// decision reaches him without interrupting anything. A **message to `main`**
/// starts a turn of the main chat, which is what an *answer* needs — an answer
/// left on a rail is an answer he has to go and find. The assistant had no
/// second route at all until `hand_to_assistant` opened one, so everything it
/// merely answered landed in this transcript and reached nobody.
pub fn assistant_preamble() -> &'static str {
    "You are Jod's assistant. Reljod's main chat takes what he says and hands \
     it to you exactly as he said it, and what happens to it is your \
     decision.\n\n\
     **You are one standing conversation, not a fresh one per instruction.** \
     This transcript is yours and it carries on: what you were asked ten \
     minutes ago, what you handed to whom, and what came back is all above \
     you, and you are expected to use it. A follow-up that only makes sense \
     against an earlier instruction — \"no, the other one\", \"that one, but \
     for the web repo\" — is answered from the thread rather than by asking \
     him to say it again.\n\n\
     **You get interrupted, and that is the normal way things reach you.** \
     Reljod does not wait for your turn to end before saying the next thing. \
     Anything that arrives while you are working is held and handed to you at \
     the start of your next turn, several at once if several arrived. So when \
     a turn opens with messages, read them first and read all of them: they \
     are new instructions, they may change or cancel what you were about to \
     do, and the newest one wins where two disagree. **You may hand out work \
     from them exactly as you would from anything else** — `ask_manager`, \
     `delegate` and `continue_agent` are all still yours on a turn that began \
     with an interruption. Nothing about arriving late makes an instruction \
     smaller.\n\n\
     **Decide by the task, in this order.**\n\n\
     **Answer directly** when the instruction needs no repository, no work \
     that outlasts this turn, and nothing you would have to go away and \
     research. One trivial call you finish inside this turn — checking \
     `recall`, reading the clock — is still answering. Touching a repository \
     never is. Running a command in one, or opening one of its files, is \
     somebody else's job however small the question looks: counting what a \
     repository contains is a `delegate`, not an answer. \"What time is it \
     in Manila\" and \"what does A2A stand for\" are answers, not agents. \
     Spawning one costs a process, a conversation row and a round-trip, and \
     it buys nothing when you already knew the answer — worse, the reply that \
     comes back on the turn is \"still working\", which is not an answer at \
     all. Say the answer and stop there. Do not hand it over as well, and do \
     not explain that you could have.\n\n\
     Naming the project does not by itself make it repository work. \"In this \
     project, what does A2A stand for?\" is a definition you know; the words \
     are context, not an errand. What sends an instruction onward is needing \
     to *look* — at files, at a build, at anything you would have to open. If \
     you are not sure you know, that is not this branch: hand it over rather \
     than guess, because a confident wrong answer is worse than a slow right \
     one.\n\n\
     Past that branch the rule is the old one. **You do not do the work.** You \
     decide who does, hand it over, and come straight back. If you catch \
     yourself reading a file to answer a question about a repository, you have \
     taken someone else's job.\n\n\
     - `ask_manager` for **anything that touches a repository**. Every project \
       has a manager that owns it, remembers every instruction about it, and \
       runs its own engineers. You hand the instruction over and come straight \
       back; it decides whether to continue an agent or open new work, and it \
       raises a card that reaches Reljod. This is the usual answer for \
       anything about code.\n\
     - `delegate` for a one-shot that needs a tool but no repository — a \
       lookup, a fetch, a calculation. That is what a scratch session is, and \
       it belongs to no work, so it is **not** a node in the tree. When you \
       want the answer back from it, pass `tools: \"delegate\"`, because a \
       read-only run has no way to send you one.\n\
     - `continue_agent` instead of `delegate` when a scratch session that has \
       finished was working on this same subject. `list_agents` names the ones \
       recent enough to be worth continuing and shows the last message each \
       one sent; judge from that. Continue one **only** if this instruction \
       carries on what it was doing — a scratch session holds no checkout, so \
       the one thing it has that a fresh session does not is the subject it \
       was already talking about, and reusing it across subjects buys nothing \
       and muddles what it knows. Different subject, new session.\n\
     - `stop_agent` for something you started that should not be running.\n\
     - `record_decision` and `ask_question` for anything Reljod should see.\n\n\
     **Never wait for a busy one.** A scratch session that is still running is \
     never the one to continue, whatever it is working on. Start a new one \
     beside it. Waiting for a session to free up is the exact block this layer \
     was built to remove, rebuilt one level down where it is harder to \
     see.\n\n\
     **Look once.** One `list_agents` in a turn, and the second call is \
     refused. One look at the fleet is a decision; a second look is a poll \
     loop, and the thing you were about to wait for arrives as its own event \
     whether you watch for it or not. The same goes for the harness's shell: \
     `sleep`, `until` and `while` are not how you find out what happened \
     here.\n\n\
     When you genuinely cannot tell which repository he means — two named in \
     one breath, or none named and nothing set — use `ask_question` rather \
     than picking. Guessing here does not produce a visible mistake; it \
     produces an invisible one, where an instruction lands in another \
     repository's manager and reads as perfectly ordinary there. Call \
     `project_switch` the moment you work out this is a different repository \
     from the one you were handed — including when you had to reason to get \
     there. It sets the main chat's own pointer rather than yours, because \
     main is what the next instruction is resolved against, so what you settle \
     here is what the next thing Reljod says will inherit.\n\n\
     **Finish by reporting, and there are two ways to do it.** Reljod is \
     looking at the main chat, not at this transcript, so a turn that ends \
     only here has reached nobody.\n\
     - **Answered it yourself?** `send_message` to `main`, with the answer in \
       it. That starts a turn of the main chat, so what you found out arrives \
       where he is already looking. A card is the wrong shape for an answer — \
       it is a row he has to notice and open, and he asked a question.\n\
     - **Handed it on?** `record_decision` with what you did and who has it \
       now, in one or two sentences. A card cascades onto his rail and \
       interrupts nothing, which is right for something that is now somebody \
       else's to finish.\n\n\
     `main` is on your roster and it is the address the main chat answers to. \
     Then say the same one or two sentences as your reply."
}

/// What handing an instruction to a manager produced.
#[derive(Debug, Clone)]
pub struct Managed {
    /// The run now carrying the instruction.
    pub run_id: String,
    /// The manager's conversation, stable across every instruction about this
    /// project.
    pub conversation_id: String,
    /// The project it was routed to, by name. Returned rather than assumed,
    /// because a routing decision nobody can see is one nobody can correct.
    pub project: String,
    /// Whether this project had no manager until now.
    pub started_fresh: bool,
}

/// Resume a project's manager with one instruction, and come straight back.
///
/// Non-blocking, like everything main does. The manager answers into its own
/// transcript and raises a card, which is how the answer reaches Reljod's rail
/// — see [`manager_preamble`]. Waiting here would make main sit through a model
/// call on every instruction about a repository.
///
/// Mirrors [`hand_to_orchestrator`]: get-or-create the conversation, resume it
/// on the harness being used, append the instruction as its user turn. It does
/// *not* settle a project against the instruction, because the caller has
/// already decided which project this is — that is what `ask_manager`'s
/// `project` argument means.
pub async fn hand_to_manager(
    jod: &Jod,
    project_id: &str,
    instruction: &str,
    kind: Option<HarnessKind>,
    permission: PermissionPolicy,
) -> Result<Managed> {
    let store = jod.store().ok_or(JodError::StoreRequired)?;
    let project = store
        .project(project_id)?
        .ok_or_else(|| JodError::Invalid(format!("no project `{project_id}`")))?;
    // `None` is "nobody named one", which is not the same thing as somebody
    // naming the default — and the `manager` role sits exactly between those
    // two cases. [`SpawnRequest::harness`] cannot tell them apart, so the
    // distinction has to survive in the argument that reaches here.
    let harness = kind.unwrap_or(HarnessKind::ClaudeCode);
    let (conversation_id, started_fresh) = store.manager_conversation(&project.id, harness)?;
    let now = chrono::Utc::now().timestamp_millis();

    let agent = jod
        .spawn_agent_in(
            SpawnRequest {
                name: format!("{}-manager", project.name),
                harness,
                // Only when the caller named no harness of its own. An explicit
                // argument outranks the roles table, and this is how that stays
                // true through a field that has no empty value.
                role: kind.is_none().then_some(Role::Manager),
                prompt: instruction.to_string(),
                // Read here rather than defaulted in the preamble, because a
                // manager told a number that is not the one `open_work`
                // enforces plans against a budget it does not have.
                system: Some(manager_preamble(
                    &project.name,
                    store.max_engineers_per_project()?,
                )),
                cwd: project.path.clone(),
                model: None,
                // The same floor main runs under, and for the same reason: a
                // manager below `AcceptEdits` cannot call the tools that are
                // its entire job, so it would look like it was working and be
                // inert.
                permission: at_least_acting(permission),
                resume: store.resume_for(&conversation_id, harness)?,
                // It starts agents, so it needs to be able to. Not
                // `Orchestrate`: arming a schedule or a goal spends money at 2am
                // with nobody watching, and that power stays with main rather
                // than being multiplied by the number of repositories Reljod
                // owns.
                tools: Some(ToolAccess::Delegate),
                ..SpawnRequest::default()
            },
            RunConversation::Existing(conversation_id.clone()),
        )
        .await?;

    store.append_prompt(&conversation_id, &agent.id, instruction)?;
    store.touch_human(&conversation_id, now)?;

    Ok(Managed {
        run_id: agent.id,
        conversation_id,
        project: project.name,
        started_fresh,
    })
}

/// The framing a project manager gets.
///
/// A manager owns one repository and everything happening in it. Main routes an
/// instruction here and comes straight back; this run decides who does the
/// work.
///
/// It is a *resumed conversation*, not a resident process. It answers and the
/// process exits, so its context is the transcript rather than anything held in
/// memory — which is why the brief tells it to look at `list_agents` first
/// rather than assuming it remembers what is running.
///
/// **Reuse is decided on availability, not on subject.** The brief used to say
/// to continue "an agent already doing this", which reads as a topical test: a
/// manager applying it opens a cold session the moment an instruction changes
/// subject, even with an engineer of this project sitting idle beside it. That
/// is the wrong trade. The idle engineer already holds the repository — its
/// layout, its conventions, what it shipped an hour ago — and a fresh session
/// has to rebuild all of it from the checkout, which is both slow and a source
/// of decisions the previous session had already made better. So the rule is
/// the plain one: free engineer takes the instruction, whatever it is about,
/// and a second session is for when everybody is busy, stalled, or absent.
/// `list_agents` answers that question directly in `idle` and `reuse` — see
/// [`crate::mcp`] — so the manager is reading a field rather than judging.
///
/// Takes the project's name because a manager that has to work out which
/// repository it owns can get that wrong, and everything it does afterwards
/// inherits the mistake.
///
/// **And it takes the engineer cap as a number, not as a word.** A preamble
/// that says "a few at once" is one every manager interprets differently, and
/// the interpretation shows up as either an idle laptop or a machine running
/// nine harnesses. The number is `Store::max_engineers_per_project`, and `0`
/// means no cap — the same spelling the other settings-backed knobs use for
/// their escape hatch.
///
/// The `list_agents`-first rule above is not reopened by any of the planning
/// text below it. It is upstream of all of this: the manager still decides who
/// is free before it decides what to give them, and the cap is counted from the
/// same call.
pub fn manager_preamble(project: &str, max_engineers: usize) -> String {
    // Spelled once, because it is stated twice — as the budget and as the
    // reason not to spend it.
    let cap = if max_engineers == 0 {
        "There is **no cap** on how many engineers this project may run at once: \
         the limit is set to 0, which means unlimited."
            .to_string()
    } else {
        format!(
            "**You may run up to {max_engineers} engineers at once on this project**, and \
             `open_work` is refused when that many are already live. Count what is running \
             before you plan rather than after — the `list_agents` call you already make first \
             is where the count comes from, so this costs you nothing extra. A stalled \
             engineer counts too: it is still a process holding a worktree, and the refusal \
             names it separately so you can see that stopping it is the way forward."
        )
    };
    format!(
        "You are the project manager for **{project}**. You own this repository \
         and everything happening in it. Reljod's main chat routes anything \
         about {project} to you and comes straight back; deciding who does it \
         is your job.\n\n\
         **You do not do the work either.** You are a manager, not an \
         engineer. If you catch yourself reading a file to answer a question \
         about this repository, you have taken one of your own agents' \
         jobs.\n\n\
         **Call `list_agents` with `project: \"{project}\"` first, every \
         time.** That is the decision that matters most, and you cannot \
         remember the answer between instructions — you are resumed for each \
         one.\n\n\
         **Then hand the instruction to an engineer who is free, whatever it \
         is about.** The call tells you who that is: every agent carries \
         `free`, the page lists their run ids in `idle` newest first, and \
         `reuse` says in one sentence what to do. Do not work it out from \
         `status` yourself — `busy: false` is *not* the same as free, because a \
         stalled agent is not busy either. An engineer \
         of this project who is not busy is your answer for *any* instruction \
         about {project}, not only for one that carries on what it was last \
         doing. It already holds the repository in its head: it knows the \
         layout, the conventions, what it just shipped and why. A cold session \
         has to buy all of that again, and it buys it wrong at least some of \
         the time. Continue the newest free one with `continue_agent`.\n\n\
         Do not open a second session beside a free one because the new \
         instruction looks like a different subject. Different subject, same \
         repository, same engineer.\n\n\
         The three cases where you open something new instead:\n\
         - **Every engineer is busy.** `busy: true` means working and not \
           stuck, and interrupting it would cost you the turn it is in the \
           middle of. Open a second session beside it.\n\
         - **The only free-looking one is stalled.** `stalled_for_ms` says so, \
           and a stalled agent *cannot be continued* — it is still `running` \
           because it is, but it has produced nothing for that long and it will \
           not answer you. Say so out loud, start a fresh session beside it, \
           and leave the stalled one alone. Stopping it is Reljod's call, not \
           yours.\n\
         - **There is no engineer at all.** First instruction about this \
           repository, or the last one was killed. Then `open_work`.\n\n\
         Two work streams that genuinely have to run at the same time are a \
         reason to open a second session. Two instructions arriving one after \
         the other are not.\n\n\
         **Write the plan before you hand anything out.** An engineer takes one \
         task, does it, reports to you and stops — the thinking about what the \
         tasks are is yours. Call `plan_work` once with the whole breakdown, \
         giving every task the files only that engineer will touch. The call is \
         **refused** if two tasks claim the same file, and it names both of \
         them, so a plan that would have had two engineers editing one file \
         costs you an error rather than a merge conflict somebody discovers \
         three hours later. An instruction that is one task for one engineer is \
         still one task: call `plan_work` with a single task and hand it out.\n\n\
         **Ask first whether it splits at all.** Most instructions do not, and \
         a one-task plan is the right answer for those. Splitting something \
         indivisible costs a cold session that has to read the repository from \
         scratch and buys nothing. Two tasks may run at once only when **both** \
         are true: they touch no file in common, and neither one needs the \
         other's output. Miss the first and `plan_work` refuses you; miss the \
         second and it does not, and you get two engineers where the later one \
         sits waiting on work that has not happened yet. Where one task's \
         output is another's input, write them in that order and hand out only \
         the first — the plan is the whole breakdown, and handing it out is a \
         separate thing paced by what has finished.\n\n\
         **The order you write the tasks in is the order the pull requests \
         stack in.** `stack_pull_requests` ranks them by their position in the \
         plan, so writing the breakdown in dependency order is not bookkeeping \
         — it is what makes the stack come out with the right bases. A plan \
         written in the order things occurred to you produces a stack whose \
         bases are wrong and which looks fine.\n\n\
         {cap}\n\n\
         **Being under the cap is not a reason to reach it.** Two engineers who \
         each spend their first turn reading the same three files have cost more \
         than one engineer reading them once. Split when the pieces are \
         genuinely independent and each is worth a whole session, not to use the \
         budget up.\n\n\
         **Decide where each engineer writes, and say why.** `open_work` takes \
         a placement and it is your call, not the engineer's:\n\
         - `explore` — read-only. No branch, no worktree, no pull request. This \
           is the right answer for anything that only looks: a review, a \
           search, a question about how something works. It is the default, \
           because reading is the reversible one.\n\
         - `worktree` — a branch and worktree of the engineer's own, cut before \
           its session starts. Anything that writes gets this.\n\
         - `share` — join the worktree another work already holds, named by \
           that work's id. Two engineers in one directory, kept apart by the \
           files your plan gave each of them.\n\
         - `direct` — write in Reljod's real checkout. Gated on three facts \
           you do not get to overrule: no git remote, no other work on this \
           project, and nothing uncommitted in the tree. Ask for it when any of \
           those is false and you are refused with every failing reason at \
           once.\n\n\
         **A worktree that finished its task opens a pull request, and you are \
         the one who has to ask for it.** Where the project has a git remote, \
         tell every engineer you place on a worktree to open a **draft** pull \
         request from its branch through the `create-pr` skill before it \
         reports — one per worktree. An engineer placed as `explore` opens \
         none, because it has no branch to open one from. Nobody runs \
         `gh pr create` by hand: a pull request opened without the session that \
         did the work is a pull request with no evidence in it, which is what \
         the skill exists to prevent. Once they are open, \
         `stack_pull_requests` gives you the order and the command that links \
         them. **Merging is never yours and never an agent's** — it is \
         `merge_pr.sh` and a person.\n\n\
         **You are the one who tells main it is finished.** Read `work_board`. \
         While any task on it is open the job is not done, and a card saying it \
         is would be false — an engineer reporting its own task complete is not \
         the job being complete. When the board is empty, raise one card saying \
         what the whole job produced, in your own words. Not a relay of each \
         engineer's report: Reljod asked for one thing and wants to be told \
         about one thing.\n\n\
         Your tools:\n\
         - `list_agents`, scoped to {project}, before anything else.\n\
         - `plan_work` to write the whole breakdown down, once, with the files \
           each task owns.\n\
         - `work_board` to read that board back — who owns what, what is still \
           open, what is done.\n\
         - `continue_agent` for anything a free engineer can take, which is \
           most things. Read `reuse` in the `list_agents` answer — it names the \
           run to continue when there is one. It is also how you get around the \
           engineer cap honestly: reusing a free engineer adds no process, so \
           it is never refused.\n\
         - `open_work` when nobody is free, or nobody exists. Unlike main you \
           may call it, but it is the second answer, not the first. It is where \
           the placement is chosen.\n\
         - `stack_pull_requests` once the engineers on one job have opened \
           theirs. It returns the order and the command; it does not push \
           anything itself, and merging is still `merge_pr.sh` and a person.\n\
         - `delegate` for a one-shot that needs no board — a lookup, a check, \
           a script.\n\
         - `stop_agent` for something you started that should not be running.\n\
         - `recall`, `related`, `remember` and `record_decision` for what this \
           project has learned. Memory is most of why a manager is worth \
           having: you are the one conversation that has seen every instruction \
           about {project}.\n\n\
         **Finish by raising a card.** `record_decision` with what you did and \
         who has it now, in one or two sentences. Reljod is looking at the main \
         chat, not at this transcript — a card cascades up to his rail and is \
         the only way your answer reaches him. A routing decision nobody can \
         see is one nobody can correct. Where you handed a job out rather than \
         answering it, the card that says it is **finished** waits for the board \
         to be empty.\n\n\
         Then say the same one or two sentences as your reply."
    )
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

/// One task, handed to one engineer, with the files only it may change.
///
/// This is the whole of what makes a session an *engineer* rather than a
/// worker: a worker is given an instruction and works out the rest, and an
/// engineer is given a task its manager already broke out of a larger job and
/// is told which files belong to it. The difference matters because engineers
/// run beside each other. Two of them editing one file is a merge conflict
/// neither can see coming, and the plan that placed them is the only thing that
/// keeps them apart.
///
/// Carried on the [`Brief`] rather than read from the board, for the reason the
/// rest of the brief is: the preamble has to be renderable without a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub task_id: String,
    pub title: String,
    /// Repository-relative path prefixes. Empty is ordinary and means the task
    /// claims no files, which is the honest state for anything exploratory.
    pub paths: Vec<String>,
    /// What to call to report. Always `complete_task`.
    ///
    /// A field rather than a literal in the prose because the preamble is
    /// checked against the tool catalogue by name, and a verb spelled out in
    /// two places is a verb that gets renamed in one of them. Build it with
    /// [`Assignment::new`] and the right value is the only one it can have.
    pub manager: String,
}

/// The tool an engineer reports through.
///
/// **This is not the only place the name is spelled, and pretending otherwise
/// would be worse than having no constant at all.** The catalogue in
/// [`crate::mcp`] spells `complete_task` again, as a literal, in another file —
/// a constant that *looks* like a single source of truth while there are two
/// stops the next person from going to check. What holds the two together is
/// `the_tool_an_engineer_reports_through_is_one_it_can_actually_call`, which
/// asserts this value names a registered tool at a level every engineer can
/// reach. Rename either spelling and that test says so.
pub const REPORTING_TOOL: &str = "complete_task";

impl Assignment {
    pub fn new(
        task_id: impl Into<String>,
        title: impl Into<String>,
        paths: Vec<String>,
    ) -> Assignment {
        Assignment {
            task_id: task_id.into(),
            title: title.into(),
            paths,
            manager: REPORTING_TOOL.to_string(),
        }
    }
}

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
    /// The task this session exists to do, and the files it owns.
    ///
    /// `None` is every session nobody planned — a `delegate`, a scheduled run,
    /// a work opened straight from an instruction. Those get exactly the
    /// preamble they got before this field existed, byte for byte.
    pub assignment: Option<Assignment>,
    /// How the manager placed it.
    ///
    /// **`None` is not [`Placement::Explore`], and collapsing the two would be
    /// a real loss.** `None` means nobody decided: the session starts on a
    /// read-only checkout and claims a worktree of its own the moment it needs
    /// to write, which is what every session has done since D5 and what
    /// `continue_agent` still does. `Some(Placement::Explore)` means a manager
    /// decided this one is here to read and must *not* write — it is a
    /// prohibition, and the brief says so. Rendering the unplaced case as
    /// `Explore` would silently strip the claim instruction from every existing
    /// spawn and tell an ordinary worker it had been sent to look.
    pub placement: Option<crate::leases::Placement>,
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

    out.extend(assignment_lines(brief));
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

/// What an engineer is told about the one task it was given.
///
/// Empty for every session that has no assignment, which is what keeps the
/// preamble byte-identical for every caller that existed before this section
/// did. Every line is [`PreambleLine::shared`]: nothing here is a fact about a
/// harness, so nothing here may differ by one.
fn assignment_lines(brief: &Brief) -> Vec<PreambleLine> {
    let Some(task) = &brief.assignment else {
        return Vec::new();
    };
    let mut out = vec![
        PreambleLine::shared("\n## Your one task\n"),
        PreambleLine::shared(format!(
            "Your manager broke a larger job into tasks and gave you this one:\n\n\
             > **{}**\n\n\
             It is task `{}` on that job's board.",
            task.title.trim(),
            task.task_id
        )),
    ];
    out.push(PreambleLine::shared(match task.paths.as_slice() {
        [] => "\nThis task claims no files, which is the ordinary state for anything \
               exploratory. Read what you need and change nothing: somebody else on this \
               job owns every file you can see."
            .to_string(),
        paths => format!(
            "\n**The files you own: {}.** Nothing else in this repository is yours to change, \
             even when you can see it needs changing. Somebody else may be holding it right \
             now, and a change outside your paths is a merge conflict with a colleague you \
             cannot see and were never introduced to.",
            paths
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }));
    out.push(PreambleLine::shared(
        "\n**Something outside your paths needs changing?** Say so in your report and stop. \
         Do not do it, and do not ask another engineer to do it. Your manager is the one who \
         can widen the plan; you are not, and neither is the engineer you would be asking.",
    ));
    // Written after an engineer hit this for real. The first one to add a field
    // to `TeamTask` had to touch two files in `cli/src/tui/` that nobody owned,
    // because their test literals stopped compiling — and a rule with no
    // carve-out would have told it to stop and report with the workspace
    // broken, which is worse for every other engineer on the job than the
    // twelve lines it actually wrote. The plan cannot see this coming either:
    // the paths it refuses on are the ones the manager named, and which
    // literals a struct change breaks is not knowable when the plan is written.
    out.push(PreambleLine::shared(
        "\n**One carve-out, and it is narrow: mechanical fallout from your own change.** \
         Adding a field to a shared struct breaks every literal that constructs it, often in \
         files nobody planned for. Fixing those is allowed and expected — you caused them, \
         they are not judgement calls, and leaving the tree uncompilable blocks every other \
         engineer on this job. It stops at the mechanical: adding the field, updating the \
         callers of a signature you changed, fixing an import. The moment a fix requires you \
         to decide what the right *value* or the right *behaviour* is, it is a change, it is \
         outside your paths, and it goes in your report instead.",
    ));
    // Both remaining lines name verbs, so both have to be honest about a
    // session that was launched without Jod's MCP server at all. That shape is
    // not hypothetical — `rail_lines` and `bus_lines` below each carry the same
    // branch — and telling such a session to report through a tool it does not
    // have would be the exact failure the tool-existence sweep exists to catch.
    match brief.tools {
        Some(_) => {
            out.push(PreambleLine::shared(format!(
                "\n**Report with `{}` when you are done, and only then.** Your report goes to \
                 your manager and to nobody above it. Reljod is not reading this transcript \
                 and does not see your prose — the report is the whole of what he will be \
                 told you did, so write it as the account of the work rather than as a note \
                 that it happened.",
                task.manager
            )));
            out.push(PreambleLine::shared(
                "\n**You are still blocked the ordinary way.** `ask_question` and \
                 `request_secret` still go to Reljod, because a manager that is not running \
                 cannot answer them and a question routed to a sleeping manager is a question \
                 nobody answers. Blocked is still a successful ending.\n",
            ));
        }
        None => {
            out.push(PreambleLine::shared(
                "\n**Say your report in your final answer.** This session holds none of Jod's \
                 tools, so there is no way for you to file one yourself — Jod reads your \
                 output and carries it to your manager. Write it as the account of the work \
                 rather than as a note that the work happened, because it is the whole of \
                 what anybody will be told you did.\n\n\
                 Anything you are blocked on goes in the same answer, in the same words you \
                 would have asked Reljod. Blocked is still a successful ending.\n",
            ));
        }
    }
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
    let direct = matches!(brief.placement, Some(crate::leases::Placement::Direct));
    for root in brief.roots {
        out.push(PreambleLine::shared(format!(
            "- `{}` — {}",
            root.path.display(),
            match (root.writable, direct) {
                // The one placement where the writable root is not a worktree.
                // Describing Reljod's own checkout as "a worktree claimed for
                // this work" would be the single most expensive sentence in the
                // preamble to get wrong.
                (true, true) => "**writable**, and it is Reljod's own checkout",
                (true, false) => "**writable**, a worktree claimed for this work",
                (false, _) => "**read-only**",
            }
        )));
    }
    out.push(PreambleLine::shared(match &brief.placement {
        None => unplaced_claim(brief.tools).to_string(),
        Some(placement) => placed_claim(placement, brief.tools),
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

/// What a session nobody placed is told about writing. Today's paragraph,
/// unchanged, and it is the one every existing caller still renders.
///
/// D5, stated as the rule it is rather than as a description of the flags — and
/// naming the verb, because a brief that says "claim a worktree" without saying
/// what to call is an instruction an agent cannot act on. That is not
/// hypothetical: `claim_lease` existed, was tested, and had no caller outside
/// its own tests for as long as nothing named it.
fn unplaced_claim(tools: Option<ToolAccess>) -> &'static str {
    match tools {
        Some(tools) if tools.may_delegate() => {
            "\nA read-only root is Reljod's real checkout, and he may be editing it \
             while you read it. **Before you change, create, move or delete anything, call \
             `claim_worktree`.** It cuts a branch of your own and makes that your one writable \
             root; the checkout stays beside it, readable, so you can still diff against what \
             he is doing. A sibling already working on the same repository in this work is \
             offered its worktree instead of a second branch being cut — the answer says which \
             happened, and if you are sharing one, read what is there before you change it. \
             `release_worktree` gives it back when you are done; a tree with uncommitted work \
             in it is kept rather than removed."
        }
        // Honest about a session that holds Jod's tools and not *that* one.
        // `claim_worktree` cuts a branch and `release_worktree` removes a
        // directory, so both sit on `delegate`'s line — see [`crate::mcp`] —
        // and a read-only session is filtered out of them before it ever sees
        // the catalogue. It is the level `ToolAccess::unattended` hands a
        // scheduled run and the level `capped_for` clamps anything built from
        // outside down to, so this is not a rare shape. Naming the verb here
        // anyway would send exactly those runs looking for a tool they were
        // deliberately not given.
        Some(_) => {
            "\nA read-only root is Reljod's real checkout, and he may be editing it while you \
             read it. This session was given read-only access to Jod, and cutting a worktree \
             is not on that side of the line — so there is **no way for you to claim one**, \
             and nowhere you may write. Do what the job needs read-only, and say plainly that \
             you are blocked rather than changing anything in a root you were told not to \
             change."
        }
        // The same again for a session that holds no Jod tools at all. Telling
        // it to claim would be telling it to call something it does not have.
        None => {
            "\nA read-only root is Reljod's real checkout, and he may be editing it while \
             you read it. This session holds none of Jod's tools, so it has **no way to claim \
             a worktree** — which means it has nowhere it may write. Do what the job needs \
             read-only, and say plainly that you are blocked rather than changing anything in \
             a root you were told not to change."
        }
    }
}

/// What a session its manager *placed* is told about writing.
///
/// Two axes, and they are not the same question. The placement says what this
/// engineer is here to do; the access level says which verbs it actually holds.
/// A manager can place an engineer on a worktree and give it read-only access
/// to Jod in the same call, and the brief has to be true about both — so every
/// arm below that would otherwise name `claim_worktree` or `release_worktree`
/// checks `may_delegate` first and says the honest thing instead. Naming a verb
/// the server filters out of the catalogue costs the session a turn and reads
/// to it as Jod being broken; that is the bug
/// `a_read_only_session_is_not_told_to_claim_a_worktree_it_cannot_claim` was
/// written for, and the placement split composes with it rather than replacing
/// it.
fn placed_claim(placement: &crate::leases::Placement, tools: Option<ToolAccess>) -> String {
    use crate::leases::Placement;
    let may_claim = matches!(tools, Some(t) if t.may_delegate());
    match placement {
        // Never names the verb, whatever the access level. `explore` is a
        // prohibition rather than a starting position: a session that reads the
        // word "claim" here promotes itself to a writer nobody planned a file
        // for, which is exactly what the placement exists to stop.
        Placement::Explore => {
            let mut said = String::from(
                "\nYour manager placed this session as **explore**: you are here to read, and \
                 you hold no writable root by design. No branch was cut for you and none will \
                 be.",
            );
            if may_claim {
                said.push_str(
                    " Needing to write is something to **report and stop on**, not something \
                     to fix by cutting a worktree of your own. The manager chose this \
                     placement knowing what the task was; if it was wrong, it is the manager \
                     who gets to change it.",
                );
            } else {
                said.push_str(
                    " This session was given read-only access to Jod as well, so there is no \
                     way for you to claim one in any case. Needing to write is something to \
                     report and stop on.",
                );
            }
            said.push_str(
                " Reljod may be editing the checkout you are reading, so what you see is a \
                 snapshot rather than a settled state.",
            );
            said
        }
        Placement::Worktree => {
            let mut said = String::from(
                "\nThe writable root above is a worktree on a branch of its own, cut for you \
                 before this session started — your manager placed you as **worktree** because \
                 this task writes, so you did not have to ask and there is nothing to claim. \
                 Reljod's checkout stays beside it, read-only, so you can still diff against \
                 what he is doing.",
            );
            if may_claim {
                said.push_str(
                    " **Do not call `claim_worktree`; you already have one.** \
                     `release_worktree` gives it back when the task is finished — a tree with \
                     uncommitted work in it is kept rather than removed.",
                );
            } else {
                // The combination a manager can produce in one call: placed to
                // write, and handed read-only access to Jod. Both halves are
                // true and the brief says both.
                said.push_str(
                    " This session holds read-only access to Jod, so claiming and releasing a \
                     worktree are not verbs you have — which costs you nothing here, because \
                     the worktree was already cut and it stays until somebody else gives it \
                     back.",
                );
            }
            said
        }
        Placement::Share { work_id } => {
            let mut said = format!(
                "\nThe writable root above is a worktree **somebody else is already working \
                 in**. Your manager placed you as **share** so the two of you are in one \
                 directory rather than on two branches, and work `{work_id}` is the one that \
                 holds it. The other engineer owns files in this tree that are not yours; \
                 yours are the ones named above and nothing else."
            );
            if tools.is_some() {
                said.push_str(
                    " `work_board` on that work says which files it owns, and reading it is \
                     cheaper than finding out by conflict.",
                );
            }
            said.push_str(
                " Read what is there before you change anything, and **never rebase, reset or \
                 force-push a branch you are sharing** — the other engineer's commits are on \
                 it, it is working from the same files on disk, and neither of those is \
                 recoverable from your side.",
            );
            said
        }
        Placement::Direct => String::from(
            "\nYou are writing in **Reljod's own checkout**. Your manager placed you as \
             **direct**, which is only allowed on a repository with no remote, no other work \
             in flight and nothing uncommitted — so the tree was clean when you started and \
             everything that appears in it from now on is yours. There is no branch between \
             you and his working tree and no worktree to throw away, so a mistake here is a \
             mistake in the real thing. Commit what the task asked for and nothing else, and \
             leave anything you are unsure about uncommitted and in your report.",
        ),
    }
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
///
/// `permission` is the operator's chosen mode, and it used to be a constant.
/// **That constant was the top of the chain that made `auto` a lie.** The
/// console showed `auto`, this function span the orchestrator up in
/// `accept_edits` anyway, its MCP server took the same ceiling, and `open_work`
/// capped every background session against it — so work delegated from a chat
/// the operator had put in `auto` ran two levels down in a mode where headless
/// Claude Code has nobody to ask, and refused `git init`. Three hard-coded
/// values in series, each defensible alone.
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
    // Read before `main_conversation`, which creates one when there is none.
    // Whether a main chat already existed is what tells a `/harness` switch
    // apart from the very first turn — see the `role` tag below.
    let existed = store.pinned_conversation()?.is_some();
    let id = store.main_conversation(kind, &cwd.display().to_string())?;
    let resume = store.resume_for(&id, kind)?;
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
                // **The floor, not the value, is the part with a bug behind
                // it.** Plan mode refuses every mutation — including the MCP
                // tool calls that *are* this run's entire job. Caught by
                // running it: the orchestrator dutifully called
                // `schedule_list`, `list_agents` and `recall`, then reached for
                // `ExitPlanMode`, could not find it, and wrote a plan file
                // instead of arming the schedule it had been asked for. So a
                // mode below `AcceptEdits` would not make the chat cautious, it
                // would make it inert while still appearing to work.
                //
                // Above the floor it passes straight through, which is the fix:
                // a console in `auto` now hands its work to sessions in `auto`.
                // Its confinement is `ToolAccess` either way — the mutations
                // that matter here are Jod's own verbs, already scoped by the
                // access level, and the permission axis bounds what it may do
                // to the *machine*.
                permission: at_least_acting(permission),
                // Asked against `kind` — the harness this spawn actually
                // launches — and not bare, because the pinned conversation is
                // resolved by `main_conversation` without reference to it. An
                // old `/harness` switch therefore leaves the pin naming one
                // harness while the console runs another, and a session id read
                // off that row goes straight to a program that never issued it.
                resume: resume.clone(),
                tools: Some(ToolAccess::Orchestrate),
                // **Not while the console is deliberately moving main to
                // another harness.** `apply_role` sets a request's harness
                // whenever the resume is `Fresh`, and `resume_for` returns
                // `Fresh` for exactly one reason on an existing main chat: this
                // turn is the first on a harness `/harness` has just switched
                // to. Tagging the role there would drag the thread straight
                // back to whatever the row names, and the switch would be
                // silently defeated rather than merely overridden — the new
                // session would be minted on the old harness and every later
                // turn would resume it.
                //
                // Main's very first turn is the other `Fresh` case and it is
                // the opposite situation: nobody has expressed a preference
                // yet, so the row is the only thing that has said anything and
                // it should be honoured. The two are told apart by whether a
                // main chat existed before this call.
                //
                // **This is an inference, and it is worth saying so plainly.**
                // What the condition below really means is "the operator has
                // just switched harness", and it works that out from two
                // things standing in for it: a main chat already existed, and
                // the resume came back `Fresh`. Nothing records the switch
                // directly. The clean expression is a `harness_named: bool` on
                // [`SpawnRequest`] — the caller knows whether anybody chose the
                // harness, and `harness` being a bare `HarnessKind` is the only
                // reason it cannot say so — and that is where to start if
                // anyone revisits this.
                role: (existed || resume != Resume::Fresh).then_some(Role::Main),
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
    /// Which layer of the chain of command this session is, for the roles
    /// table to answer against.
    ///
    /// `Engineer` by default, because that is what the first session on a work
    /// is. A caller that named its own harness sets this to `None`: an explicit
    /// argument outranks the row, and [`SpawnRequest::harness`] has no way to
    /// say whether anybody named it.
    pub role: Option<Role>,
    /// Where the manager decided this engineer is allowed to write.
    ///
    /// `None` — the default, and what every caller that predates placement
    /// passes — is *unplaced*: the checkout arrives read-only, nothing is cut,
    /// and the session claims a worktree for itself when it needs one. That is
    /// today's behaviour exactly, which is what lets this land without changing
    /// a single existing spawn.
    ///
    /// `Some(..)` is a decision somebody made, and [`prepare_work`] acts on it
    /// before the session's brief is written: `Worktree` claims a lease,
    /// `Share` joins another work's, `Direct` makes the checkout itself
    /// writable, and `Explore` deliberately does none of those. It has to
    /// happen here rather than at the tool boundary because the conversation
    /// a lease binds its roots to does not exist until this function creates
    /// it.
    ///
    /// **`Direct` is not gated here.** `leases::direct_is_allowed` is the gate
    /// and it lives at the tool boundary, where the refusal can name every
    /// failing condition at once and point at `worktree` instead. By the time
    /// a placement reaches this struct somebody has already decided.
    pub placement: Option<crate::leases::Placement>,
    /// The task this engineer was spawned onto, when a manager planned one.
    ///
    /// Carried straight through to the session's [`Brief`], which is the only
    /// thing that reads it. Writing `conversations.task_id` is the tool
    /// boundary's job, not this one's.
    pub assignment: Option<Assignment>,
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
            role: Some(Role::Engineer),
            placement: None,
            assignment: None,
        }
    }

    /// Place this engineer where its manager decided it writes.
    pub fn placed(mut self, placement: crate::leases::Placement) -> Opening {
        self.placement = Some(placement);
        self
    }

    /// Give it the one task it exists to do, and the files that come with it.
    pub fn assigned(mut self, assignment: Assignment) -> Opening {
        self.assignment = Some(assignment);
        self
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
    /// The worktree the placement claimed, when it claimed one.
    ///
    /// `None` for an unplaced session, for `Explore` and for `Direct` — none of
    /// which takes a lease. Returned rather than left for the caller to look up
    /// because the caller is the one that has to tell the manager where its
    /// engineer is writing, and a tool answer that says "worktree" without
    /// saying which directory is one the manager cannot check.
    pub claim: Option<crate::leases::Claim>,
}

/// Everything opening a work does before a process exists.
pub struct Prepared {
    pub work: crate::works::Work,
    pub conversation_id: String,
    pub name: String,
    /// The first session's launch, preamble and all.
    pub request: SpawnRequest,
    /// What the placement claimed, when it claimed anything. See
    /// [`Opened::claim`].
    pub claim: Option<crate::leases::Claim>,
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
    let checkout = crate::roots::normalise(&opening.checkout);

    // Which repository this is, decided from the checkout rather than asked
    // for. `open_work` used to touch the catalog not at all: it took a path,
    // never called `project_for_path`, never set `current_project_id` on the
    // conversation it created, and never wrote a resolution row. So the work
    // could not be grouped by repository and the child session did not inherit
    // the project either — it had to re-derive it from whatever it was told.
    //
    // Not an error when it finds nothing. A directory nobody catalogued is
    // still somewhere to work, and refusing here would make the catalog a
    // gate rather than a convenience.
    let project = store.project_for_path(&checkout).unwrap_or(None);
    let work = store.create_work_in(&opening.instruction, project.as_ref().map(|p| p.id.as_str()))?;

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
    store.add_root(
        &conversation.id,
        match opening.placement {
            // The one placement whose writable root is the checkout itself.
            // Added writable from the start rather than added read-only and
            // promoted, because `add_root` resolves a conflict in favour of the
            // *incoming* write and a reader that arrived first would look like
            // it had been demoted when it never was.
            Some(crate::leases::Placement::Direct) => NewRoot {
                path: checkout.clone(),
                writable: true,
                origin: crate::roots::Origin::Human,
            },
            _ => NewRoot::reading(&checkout),
        },
    )?;

    // The session inherits the project, so everything it starts does too and
    // nothing below it has to guess. `How::Inferred` because the checkout said
    // so — this is not Reljod naming a repository, and it is not a sticky
    // pointer carrying; it is a path resolving, and the resolution row is what
    // makes a wrong one auditable.
    if let Some(project) = &project {
        if let Err(e) = store.set_current_project(
            &conversation.id,
            Some(&project.id),
            &opening.instruction,
            crate::projects::How::Inferred,
            &format!("the work was opened in `{}`", checkout.display()),
        ) {
            eprintln!("[jod] could not record the work's project: {e}");
        }
    }

    // Whatever the manager decided, done *before* the roots are read — the
    // brief describes them, and a brief that says "the writable root above is a
    // worktree cut for you" above a list holding only a read-only checkout is
    // the kind of lie that costs the session its first turn.
    //
    // Nothing here for `Explore`, which is the point of it, and nothing for
    // `Direct`, which was handled by the root above. `None` is every caller
    // that predates placement and behaves exactly as it did.
    let claim = match &opening.placement {
        Some(crate::leases::Placement::Worktree) => {
            Some(store.claim_lease(&work.id, &conversation.id, &checkout)?)
        }
        Some(crate::leases::Placement::Share { work_id }) => {
            Some(store.share_lease(&work.id, &conversation.id, work_id, &checkout)?)
        }
        None | Some(crate::leases::Placement::Explore) | Some(crate::leases::Placement::Direct) => {
            None
        }
    };

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
            assignment: opening.assignment.clone(),
            placement: opening.placement.clone(),
        })),
        cwd: checkout,
        model: opening.model.clone(),
        permission: opening.permission,
        resume: Resume::Fresh,
        tools: Some(opening.tools),
        role: opening.role,
        // The same roots and secrets the preamble describes, actually handed
        // to the run.
        //
        // These were fetched above and used only to write the prose. So the
        // brief told the agent that `$STRIPE_API_KEY` existed and nothing ever
        // put it in the environment, and it named directories no `--add-dir`
        // ever granted. Every construction site in the workspace ended
        // `..SpawnRequest::default()`, so the supervisor's injection and
        // redaction — both tested, both correct — were being handed an empty
        // list on every real run.
        //
        // Worth stating because the failure was invisible in the worst way: a
        // run told to print a secret printed nothing, and "the value appears
        // nowhere in the database" passed *trivially*, because no value had
        // ever been near it. A green check that is green for the wrong reason
        // is the one thing this repo keeps producing.
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
        claim,
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
        claim: prepared.claim,
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
    let mut request = crate::works::Titling::new(work)
        .with_harness(harness)
        .request();
    // The cheapest win in the whole roles table, and the reason `housekeeping`
    // is a layer at all. Naming a work is one short summary of one instruction,
    // and it has been paying frontier prices for it because nothing ever told
    // it otherwise. Set here rather than in `Titling` because which model a
    // throwaway runs on is a setting about Jod's spending, not a fact about
    // titling.
    request.role = Some(Role::Housekeeping);
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

    /// The conversation that owns one project, created the first time it is
    /// asked for. Says whether it had to create it.
    ///
    /// Get-or-create for the same reason [`Store::main_conversation`] is: a
    /// manager that has to be set up is one that is missing exactly when you
    /// first need it.
    ///
    /// **It mirrors `main_conversation`'s shape and deliberately not its
    /// mechanism.** Main is found through `pinned = 1`, and
    /// [`Store::pinned_conversation`] is a `query_row` with no `LIMIT` and no
    /// ordering — so a manager row carrying that flag would not fail loudly, it
    /// would make which conversation counts as "main" depend on the order
    /// SQLite happens to return, and [`hand_to_orchestrator`] would start
    /// appending Reljod's instructions to a project manager's transcript. A
    /// manager is found through `projects.manager_conversation_id`, and its
    /// `pinned` stays 0.
    ///
    /// One manager per project, not one per project per harness. Its value is
    /// that it remembers the repository, and splitting it by harness would
    /// split that memory for a reason that has nothing to do with the
    /// repository. Moving it to another harness is [`Store::resume_for`]'s job,
    /// exactly as it is for main.
    ///
    /// The `harness` argument is therefore only used when creating one, and
    /// `cwd` comes from the project rather than the caller: a manager that owns
    /// a repository and sits in some other directory is one whose sessions
    /// start in the wrong place.
    pub fn manager_conversation(
        &self,
        project_id: &str,
        harness: crate::harness::HarnessKind,
    ) -> Result<(String, bool)> {
        let project = self
            .project(project_id)?
            .ok_or_else(|| JodError::Invalid(format!("no project `{project_id}`")))?;

        // Checked against `conversations` rather than trusted, because
        // `ON DELETE SET NULL` covers a deleted conversation but a database
        // restored from a partial backup would not. A dangling id here would
        // resume a transcript that is not there.
        if let Some(existing) = &project.manager_conversation_id {
            if self.conversation(existing)?.is_some() {
                return Ok((existing.clone(), false));
            }
        }

        let created = self.new_conversation(harness, &project.path.to_string_lossy(), None)?;
        self.set_conversation_title(&created.id, &project.name)?;
        // Set here rather than left for the first instruction to resolve, so
        // everything the manager starts inherits the right project and nothing
        // below it has to guess. `How::Human` because this conversation exists
        // *because of* that project — there is nothing inferred about it.
        self.set_current_project(
            &created.id,
            Some(&project.id),
            "",
            crate::projects::How::Human,
            &format!("this is {}'s manager", project.name),
        )?;
        // Hang it under the main chat, which is the edge the rail reads.
        //
        // Both preambles promise Reljod that a manager answers onto his rail —
        // `ask_manager` returns "It will raise a card on your rail", and the
        // manager is told a card "cascades up to his rail and is the only way
        // your answer reaches him". None of that was true. Cards cascade along
        // `parent_conversation_id` (`Store::cards_in`, `subtree_cte`), an
        // engineer is hung under its manager by `open_work`, and the manager
        // was hung under nothing — so the chain reached one link short of the
        // person it exists to report to. Main's rail said "nothing waiting"
        // while the work sat finished on a rail nobody opens.
        //
        // Safe to do even though everything then hangs under main, because
        // `Jod::cascade_stop` already exempts the pinned conversation by name
        // and says why: stopping the chat you are typing into must not stop the
        // machine. This makes the data say what that comment already assumed.
        let main = self.pinned_conversation()?;
        self.write(|tx| {
            tx.execute(
                "UPDATE projects SET manager_conversation_id = ?2 WHERE id = ?1",
                rusqlite::params![project.id, created.id],
            )?;
            // Only when there *is* a main chat. A manager created before one
            // exists keeps a null parent rather than pointing at nothing, and
            // the next one created will hang correctly.
            if let Some(main) = &main {
                tx.execute(
                    "UPDATE conversations SET parent_conversation_id = ?2 WHERE id = ?1",
                    rusqlite::params![created.id, main],
                )?;
            }
            Ok(())
        })?;
        Ok((created.id, true))
    }

    /// The one conversation the assistant runs in, created the first time it is
    /// asked for. Says whether it had to create it.
    ///
    /// **Get-or-create, the same shape as [`Store::manager_conversation`], and
    /// this reverses what it used to be.** The assistant was created fresh for
    /// every instruction and never resumed, on the argument that a standing one
    /// would *serialise* — instruction two waiting behind instruction one, the
    /// console's block moved down a layer rather than removed.
    ///
    /// That argument was answered rather than ignored. A second instruction
    /// arriving while the assistant is mid-turn is queued in
    /// [`crate::delivery`] and delivered into its **next** turn, batched with
    /// anything else that arrived meanwhile, exactly as a card answer is. So
    /// nothing waits: [`hand_to_assistant`] still returns as soon as the
    /// instruction has been taken, and the assistant reads what came in when it
    /// is next able to act on it. What the standing thread buys is the thing a
    /// per-instruction one could never have — the assistant remembers what
    /// Reljod asked for a minute ago, and it can hand out work off a message
    /// that arrived after its turn had already begun.
    ///
    /// **It is found through `settings`, not through a flag on the row.** Main
    /// is found through `pinned = 1` and a manager through
    /// `projects.manager_conversation_id`; the assistant belongs to no project
    /// and must never take the pin, so the pointer needs a home of its own.
    /// `settings` is key and value, so it needs no migration, and the
    /// carry-forward behind [`Store::continue_as_new`] moves it when a
    /// compaction opens the thread's continuation — otherwise the next
    /// instruction would resume the thread that was just compacted away.
    ///
    /// Two things are set on the row and they are the two that make it the
    /// assistant: `origin` says what it is, which is what the recursion guard in
    /// [`crate::mcp`] reads, and `parent_conversation_id` hangs it under main so
    /// the cards it raises cascade onto Reljod's rail.
    ///
    /// **It is deliberately not `ephemeral` any more.** That flag puts a
    /// conversation in the scratch lane, and every query in that lane archives a
    /// conversation once its latest run completes and deletes it once it has
    /// been archived long enough — which is precisely what must not happen to a
    /// standing thread. A conversation swept away between two instructions is a
    /// conversation that forgets, and the whole point of this change is that it
    /// does not. The scratch lane still owns what the assistant *starts*: a
    /// `delegate` opens its own ephemeral conversation, and that is unchanged.
    pub fn assistant_conversation(
        &self,
        harness: crate::harness::HarnessKind,
        cwd: &str,
    ) -> Result<(String, bool)> {
        // Checked against `conversations` rather than trusted, for the reason
        // `manager_conversation` checks its own pointer: a settings row is not a
        // foreign key, so a database restored from a partial backup — or one
        // whose assistant thread was deleted by hand — would name a transcript
        // that is not there, and resuming it would fail every turn.
        if let Some(existing) = self.setting(ASSISTANT_SETTING)? {
            if self.conversation(&existing)?.is_some() {
                return Ok((existing, false));
            }
        }

        let created = self.new_conversation(harness, cwd, None)?;
        let main = self.pinned_conversation()?;
        self.write(|tx| {
            tx.execute(
                "UPDATE conversations SET origin = ?2, title = 'assistant' WHERE id = ?1",
                rusqlite::params![created.id, ASSISTANT_ORIGIN],
            )?;
            // Only when there is a main chat to hang it under. An assistant
            // started before one exists keeps a null parent rather than
            // pointing at nothing — the same rule `manager_conversation`
            // follows, and for the same reason.
            if let Some(main) = &main {
                tx.execute(
                    "UPDATE conversations SET parent_conversation_id = ?2 WHERE id = ?1",
                    rusqlite::params![created.id, main],
                )?;
            }
            Ok(())
        })?;
        self.set_setting(ASSISTANT_SETTING, &created.id)?;
        Ok((created.id, true))
    }

    /// What kind of thing opened this conversation.
    ///
    /// Read back rather than inferred, because the one caller that needs it —
    /// the guard that stops an assistant starting another assistant — is asking
    /// about the conversation it is *in*, and nothing else in a tool call can
    /// tell it that.
    pub fn conversation_origin(&self, conversation_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT origin FROM conversations WHERE id = ?1",
                rusqlite::params![conversation_id],
                |r| r.get(0),
            )
            .ok())
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
            // Unplaced and unassigned, which is what every caller that predates
            // D4 passes and therefore what the regression guards below have to
            // be measuring.
            assignment: None,
            placement: None,
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

    /// **Every verb the brief names has to exist, and has to be one the run it
    /// is given to can actually call.** A preamble that tells an agent to call
    /// something the catalogue does not advertise costs it a turn discovering
    /// that, and reads to it as Jod being broken — and there is no compiler and
    /// no test that would otherwise notice, because a prompt is a string. This
    /// is the check that would have caught `claim_worktree` being described and
    /// never registered.
    ///
    /// The access half was added after the same bug turned up one layer over.
    /// Checking a name against the *whole* catalogue asks the wrong question,
    /// because no run ever sees the whole catalogue: [`crate::mcp`] filters it
    /// per run by what that run's [`ToolAccess`] allows, so a tool that exists
    /// and is above the run's level reaches the model exactly as a misspelling
    /// does — as something that is not there. That is how every project manager
    /// came to be told that memory was most of why it was worth having while
    /// `remember` sat a level above it, and how a read-only worker came to be
    /// told to claim a worktree it had no verb for.
    ///
    /// So each preamble is checked against the level its runs are really
    /// spawned with: a manager at [`ToolAccess::Delegate`] (see
    /// [`hand_to_manager`]), main at [`ToolAccess::Orchestrate`] (see
    /// [`hand_to_orchestrator`]), a delegated run at `Delegate` or better
    /// because that is the only case that gets the preamble at all, and a
    /// worker at whatever `open_work` was asked for, which is every level.
    ///
    /// **The assignment and the placement are swept too, and they have to be.**
    /// A worker brief rendered only with `assignment: None` never renders the
    /// engineer section at all, so the verb an engineer reports through would
    /// sit outside this check entirely — proved present by a `contains` call
    /// somewhere else, which says a string is in the prose and nothing about
    /// whether a tool answers to it. The same argument covers the placement
    /// arms: each one is a different paragraph and only one of them renders on
    /// any given session.
    ///
    /// **The swept task owns paths with slashes in them, on purpose.**
    /// [`tools_named_in`] treats any backticked span of lower case and
    /// underscores as a tool name, and the engineer section prints each owned
    /// path in backticks. A task owning `my_module` would fail here with "says
    /// to call `my_module`, which no tool is registered under" — a real false
    /// positive with a baffling message. A slash rules the span out, so do not
    /// simplify [`task`]'s paths to bare words.
    #[test]
    fn every_tool_the_preamble_tells_an_agent_to_call_is_one_that_exists() {
        let roots = [root("/repo", false), root("/repo-worktree", true)];
        let secrets = [secret("STRIPE_API_KEY", "the live key")];
        let peers = ["scout".to_string()];
        let placements = [
            None,
            Some(crate::leases::Placement::Explore),
            Some(crate::leases::Placement::Worktree),
            Some(crate::leases::Placement::Share {
                work_id: "w-first".into(),
            }),
            Some(crate::leases::Placement::Direct),
        ];

        for harness in [HarnessKind::ClaudeCode, HarnessKind::OpenCode, HarnessKind::Agy] {
            for tools in [
                Some(ToolAccess::ReadOnly),
                Some(ToolAccess::Delegate),
                Some(ToolAccess::Orchestrate),
                None,
            ] {
                for assignment in [None, Some(task())] {
                    for placement in &placements {
                        let said = worker_preamble(&Brief {
                            harness,
                            roots: &roots,
                            secrets: &secrets,
                            peers: &peers,
                            tools,
                            assignment: assignment.clone(),
                            placement: placement.clone(),
                        });
                        every_named_tool_is_callable(
                            &said,
                            tools,
                            &format!("the brief for a {} worker", harness.label()),
                        );
                    }
                }
            }
        }

        // The orchestrator's preamble is the other one that names tools, and it
        // was not covered here — which is how it came to define what a **work**
        // is while naming no tool that opens one. A misspelling in it fails the
        // same way a misspelling in a worker's brief does: silently, as a model
        // reaching for something that is not there.
        every_named_tool_is_callable(
            orchestrator_preamble(),
            Some(ToolAccess::Orchestrate),
            "the orchestrator's preamble",
        );
        // Both arms of the cap sentence, because they are two different
        // strings and only one of them is rendered on any given machine.
        for max_engineers in [0, 3] {
            every_named_tool_is_callable(
                &manager_preamble("tetris", max_engineers),
                Some(ToolAccess::Delegate),
                "a project manager's preamble",
            );
        }
        // Only ever handed to a run that may delegate — `Server::delegate`
        // makes the system prompt conditional on `may_delegate`, because
        // telling a read-only run to report back would be telling it to call a
        // tool it has not been given.
        every_named_tool_is_callable(
            delegated_preamble(),
            Some(ToolAccess::Delegate),
            "a delegated run's preamble",
        );
    }

    /// A project manager is spawned by [`hand_to_manager`] with
    /// [`ToolAccess::Delegate`] and never anything else, so its brief is the one
    /// preamble whose reachable set is fixed. Stated on its own, and not only as
    /// one arm of the sweep above, because this is the sentence that was untrue
    /// for as long as `remember` needed `Orchestrate`: a manager was told memory
    /// was most of why it was worth having, and handed no way to write any.
    #[test]
    fn every_tool_a_manager_is_told_to_use_is_one_a_manager_can_reach() {
        every_named_tool_is_callable(
            &manager_preamble("tetris", 3),
            Some(ToolAccess::Delegate),
            "a project manager's preamble",
        );
    }

    /// The assistant is where most of main's verbs went, so it names more tools
    /// than anything else and is the brief most exposed to this failing
    /// quietly. [`hand_to_assistant`] spawns it at [`ToolAccess::Delegate`],
    /// which is what it is checked against.
    ///
    /// Checked here at a call site rather than inside
    /// [`every_named_tool_is_callable`], where a merge briefly put it: that
    /// helper is handed one preamble and asked about that one, so a second
    /// preamble wired into its body would be re-checked on every call and would
    /// report failures against whichever brief happened to be passed.
    #[test]
    fn every_tool_the_assistant_is_told_to_use_is_one_it_can_reach() {
        every_named_tool_is_callable(
            assistant_preamble(),
            Some(ToolAccess::Delegate),
            "the assistant's preamble",
        );
    }

    // ---- what a manager is told about planning ----

    /// **Check 26.** A manager that is told to break work down and handed no
    /// verb for writing the breakdown down is in exactly the trap `claim_lease`
    /// sat in before `claim_worktree` named it: an instruction it cannot
    /// follow. All three verbs are named, and `list_agents` still comes first,
    /// because deciding who is free is upstream of deciding what to give them.
    #[test]
    fn a_manager_is_given_the_verbs_for_planning_the_board_and_the_stack() {
        let said = manager_preamble("tetris", 3);
        for verb in ["`plan_work`", "`work_board`", "`stack_pull_requests`"] {
            assert!(said.contains(verb), "{verb} is not named to the manager: {said}");
        }
        let first = said.find("`list_agents`").expect("the existing rule is still there");
        for later in ["`plan_work`", "`work_board`", "`stack_pull_requests`"] {
            assert!(
                first < said.find(later).expect("named above"),
                "{later} is offered before `list_agents`, so planning reads as the first \
                 thing to do and deciding who is free reads as an afterthought"
            );
        }
        // The refusal, said out loud rather than left to be discovered. A
        // manager that learns the constraint from an error has spent a turn.
        assert!(said.contains("**refused** if two tasks claim the same file"), "{said}");
        // And the four placements, which is the other decision that is its own.
        for placement in crate::leases::PLACEMENT_IDS {
            assert!(
                said.contains(&format!("`{placement}`")),
                "the placement `{placement}` is not offered to the manager: {said}"
            );
        }
    }

    /// **Check 37.** A preamble that says "a few at once" is one every manager
    /// reads differently, and the difference shows up as either an idle laptop
    /// or nine harnesses on one machine. The number is stated, and so are both
    /// halves of the test for running two engineers side by side — disjoint
    /// files *and* neither waiting on the other. Dropping either half is how
    /// you get two engineers where the second one sits idle waiting for work
    /// that has not happened yet.
    #[test]
    fn a_manager_is_told_the_cap_as_a_number_and_both_tests_for_running_two_at_once() {
        let said = manager_preamble("tetris", 4);
        assert!(said.contains("up to 4 engineers at once"), "{said}");
        assert!(said.contains("no file in common"), "{said}");
        assert!(said.contains("neither one needs the other's output"), "{said}");
        assert!(
            said.contains("**both**"),
            "the two conditions are listed without saying both are required: {said}"
        );
        // Reuse is the honest way around the cap, and it is the behaviour the
        // preamble already asks for first — the two rules push the same way.
        assert!(said.contains("reusing a free engineer adds no process"), "{said}");
        // A wedged session must be readable out of the refusal, or a manager at
        // the cap because of one has no path forward at all.
        assert!(said.contains("A stalled engineer counts too"), "{said}");
        // And the restraint, which is the half a budget does not imply.
        assert!(said.contains("not a reason to reach it"), "{said}");
        assert!(said.contains("Ask first whether it splits at all"), "{said}");
    }

    /// `0` is the escape hatch, spelled the way the other settings-backed knobs
    /// spell theirs. A manager told "up to 0 engineers" would open none.
    #[test]
    fn a_project_with_no_cap_is_told_it_has_none_rather_than_told_zero() {
        let said = manager_preamble("tetris", 0);
        assert!(said.contains("**no cap**"), "{said}");
        assert!(
            !said.contains("up to 0 engineers"),
            "a manager was told its budget was nought: {said}"
        );
    }

    /// **Nothing opens a pull request unless the manager asks for one.**
    ///
    /// The spec covers *stacking* pull requests and never says who opens them,
    /// which reads as a gap in the plan and is one. The automatic ask engineer
    /// C is wiring is opt-in and off by default, so on a machine where nobody
    /// turned it on the manager is the only thing that would ever cause a pull
    /// request to exist — and `stack_pull_requests` on a work with none is a
    /// refusal rather than a stack. Both ends are asserted: that the manager is
    /// told to ask for the draft, and that merging is still not its call.
    #[test]
    fn a_manager_is_told_who_opens_the_pull_requests_before_it_stacks_them() {
        let said = manager_preamble("tetris", 3);
        assert!(said.contains("opens a pull request"), "{said}");
        assert!(said.contains("**draft**"), "a pull request that is not a draft: {said}");
        assert!(
            said.contains("`create-pr` skill"),
            "the manager is told to ask for a pull request and not what opens one: {said}"
        );
        assert!(
            said.contains("An engineer placed as `explore` opens \nnone")
                || said.contains("An engineer placed as `explore` opens none"),
            "a read-only engineer has no branch, and the preamble has to say so: {said}"
        );
        assert!(
            said.contains("Nobody runs `gh pr create` by hand"),
            "shelling out to `gh` is how an evidence-free pull request gets opened: {said}"
        );
        assert!(
            said.contains("**Merging is never yours and never an agent's**"),
            "{said}"
        );
        assert!(said.contains("`merge_pr.sh` and a person"), "{said}");
    }

    /// **Check 38.** The one thing that falls out of D5.2 and has to be said
    /// out loud, because nothing about writing a plan suggests it: the order
    /// the manager writes the tasks in is the order the pull requests stack in.
    /// A plan written in the order things occurred to it produces a stack whose
    /// bases are wrong and which looks fine from the outside.
    #[test]
    fn a_manager_is_told_the_order_it_plans_in_is_the_order_the_stack_comes_out_in() {
        let said = manager_preamble("tetris", 3);
        assert!(
            said.contains("The order you write the tasks in is the order the pull requests \
                           stack in."),
            "{said}"
        );
        assert!(said.contains("dependency order"), "{said}");
        assert!(
            said.contains("bases are wrong"),
            "the consequence is left out, so the rule reads as bookkeeping: {said}"
        );
    }

    /// The manager is the only voice main hears about a project, and the card
    /// that says a job is finished has to wait for the board rather than for
    /// the first engineer that reports.
    #[test]
    fn a_manager_is_told_it_decides_when_a_job_is_finished_by_reading_the_board() {
        let said = manager_preamble("tetris", 3);
        assert!(said.contains("You are the one who tells main it is finished."), "{said}");
        assert!(said.contains("While any task on it is open the job is not done"), "{said}");
        assert!(
            said.contains("Not a relay of each engineer's report"),
            "{said}"
        );
        // Nothing above this was reopened: the reuse-first rule is still there,
        // in the same words, and so is the card at the end.
        assert!(
            said.contains("Call `list_agents` with `project: \"tetris\"` first, every"),
            "{said}"
        );
        assert!(said.contains("**Finish by raising a card.**"), "{said}");
    }

    /// Assert that everything `said` names both exists and is within reach of a
    /// run holding `access`.
    ///
    /// Two passes, because the two halves cannot be asked the same way round.
    /// *Existence* has to start from the prose and guess which backticked spans
    /// were meant as tools, since a misspelling is by definition not in the
    /// catalogue and nothing but the shape of the word gives it away. *Reach*
    /// starts from the catalogue instead and looks for each real tool's name in
    /// backticks, which is both exact and the only way to see the tools the
    /// shape heuristic misses: `remember`, `recall`, `delegate`, `roster` and
    /// `reply` carry no underscore and are indistinguishable from ordinary
    /// English by spelling alone. That gap is not hypothetical — it is why
    /// `remember` sat a level above every manager that was told to call it for
    /// as long as it did.
    ///
    /// The failure reads the same either way, which is the point: whether the
    /// name is wrong or merely out of reach, the model looks for it in its tool
    /// list, does not find it, and spends the turn concluding Jod is broken.
    fn every_named_tool_is_callable(said: &str, access: Option<ToolAccess>, whose: &str) {
        let catalogue = crate::mcp::catalogue();
        for span in tools_named_in(said) {
            assert!(
                catalogue.iter().any(|t| t.name == span),
                "{whose} says to call `{span}`, which no tool is registered under"
            );
        }
        for tool in catalogue
            .iter()
            .filter(|t| backticked(said).any(|span| span == t.name))
        {
            let Some(access) = access else {
                panic!(
                    "{whose} says to call `{}`, and that run holds none of Jod's tools at all",
                    tool.name
                );
            };
            assert!(
                crate::mcp::allows(access, tool.needs),
                "{whose} says to call `{}`, which needs `{}` access — and that run is spawned \
                 with `{}`, so the tool is filtered out of its catalogue before it ever sees it",
                tool.name,
                tool.needs.as_str(),
                access.as_str(),
            );
        }

    }

    /// Anything in backticks that looks like a tool name: lower case,
    /// underscores, no path separators or spaces.
    fn tools_named_in(said: &str) -> Vec<&str> {
        backticked(said)
            .filter(|span| {
                span.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    && span.contains('_')
                    && !ANSWER_FIELDS.contains(span)
            })
            .collect()
    }

    /// Every span between a pair of backticks.
    fn backticked(said: &str) -> impl Iterator<Item = &str> {
        said.split('`').skip(1).step_by(2).filter(|s| !s.is_empty())
    }

    /// Fields of a tool's *answer* that are spelled exactly like a tool name.
    /// The manager's brief quotes `stalled_for_ms` out of what `list_agents`
    /// returns, in the same backticks it uses for the verbs, and the shape
    /// heuristic cannot tell a field from a verb. Listing the few that exist
    /// keeps the check strict; loosening the heuristic instead would quietly
    /// stop it noticing whole classes of misspelling.
    const ANSWER_FIELDS: &[&str] = &["stalled_for_ms"];

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
            assignment: None,
            placement: None,
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
            assignment: None,
            placement: None,
        });
        assert!(said.contains("you cannot send"), "{said}");
        assert!(!said.contains("`send_message`"));
        assert!(said.contains("read_messages"), "reading is still worth doing");
    }

    /// The same mistake in the other half of the brief. `claim_worktree` sits on
    /// `delegate`'s line because it cuts a branch, so a read-only session is
    /// filtered out of it — and read-only is what a scheduled run gets and what
    /// anything built from outside is clamped to, which makes this the common
    /// case rather than an odd one. The brief has to say it has nowhere to
    /// write, not hand it a verb the server will refuse.
    #[test]
    fn a_read_only_session_is_not_told_to_claim_a_worktree_it_cannot_claim() {
        let roots = [root("/repo", false)];
        let said = worker_preamble(&Brief {
            harness: HarnessKind::ClaudeCode,
            roots: &roots,
            secrets: &[],
            peers: &[],
            tools: Some(ToolAccess::ReadOnly),
            assignment: None,
            placement: None,
        });
        assert!(said.contains("no way for you to claim one"), "{said}");
        assert!(!said.contains("`claim_worktree`"), "{said}");
        assert!(!said.contains("`release_worktree`"), "{said}");
        // Still told what to do instead, which is the whole point of saying it.
        assert!(said.contains("blocked"), "{said}");
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

    // ---- what an engineer is told about its one task ----

    /// The same brief the helper above builds, plus a task and a placement.
    ///
    /// Both are threaded through one constructor so a test that means to change
    /// one of them cannot change the other by accident, which is exactly the
    /// mistake the regression guards below exist to catch.
    fn engineer<'a>(
        roots: &'a [Root],
        tools: Option<ToolAccess>,
        assignment: Option<Assignment>,
        placement: Option<crate::leases::Placement>,
    ) -> Brief<'a> {
        Brief {
            harness: HarnessKind::ClaudeCode,
            roots,
            secrets: &[],
            peers: &[],
            tools,
            assignment,
            placement,
        }
    }

    /// **Both paths have a slash in them and that is load-bearing.** The
    /// engineer section prints each owned path in backticks, and
    /// [`tools_named_in`] reads any backticked span of lower case and
    /// underscores as a tool name. A task owning `my_module` would fail
    /// [`every_tool_the_preamble_tells_an_agent_to_call_is_one_that_exists`]
    /// with "says to call `my_module`, which no tool is registered under",
    /// which is a false positive with a message nobody would understand.
    /// Simplifying these to bare words would reintroduce it.
    fn task() -> Assignment {
        Assignment::new(
            "5d9c7f02-1f1c-4a2f-9f3a-0b1c2d3e4f50",
            "Teach the board which files a task owns",
            vec!["core/src/works.rs".into(), "core/src/team.rs".into()],
        )
    }

    /// **The one thing a constant in this file cannot promise on its own.**
    ///
    /// [`REPORTING_TOOL`] is what the engineer section tells a session to call,
    /// and the tool it names is registered by a literal in another file with no
    /// compiler relationship to this one. So the constant is a single spelling
    /// of the name only as long as something checks the two against each other,
    /// and this is that something.
    ///
    /// The reach half matters as much as the existence half. An engineer is
    /// spawned at whatever access `open_work` was asked for, and the lowest of
    /// those is [`ToolAccess::ReadOnly`] — the level a scheduled run gets and
    /// the level anything built from outside is clamped to. A reporting tool
    /// above that line would leave exactly those engineers told to report and
    /// unable to, which is the state this whole change exists to remove.
    #[test]
    fn the_tool_an_engineer_reports_through_is_one_it_can_actually_call() {
        let catalogue = crate::mcp::catalogue();
        let tool = catalogue
            .iter()
            .find(|t| t.name == REPORTING_TOOL)
            .unwrap_or_else(|| {
                panic!(
                    "engineers are told to report with `{REPORTING_TOOL}`, and no tool is \
                     registered under that name"
                )
            });
        assert!(
            crate::mcp::allows(ToolAccess::ReadOnly, tool.needs),
            "`{REPORTING_TOOL}` needs `{}` access, so an engineer spawned read-only is told \
             to report and handed no way to do it",
            tool.needs.as_str()
        );
    }

    /// **Check 22, and the regression guard that lets this land at all.**
    ///
    /// Every caller that existed before engineers did passes no assignment, and
    /// what those sessions are told must not have moved by one character. The
    /// assertion is that the section is purely *additive*: strip the lines
    /// `assignment_lines` produced out of an engineer's preamble and what is
    /// left is exactly the preamble an unassigned session gets, in the same
    /// order. Nothing else in `preamble_lines` may read the field.
    ///
    /// Written this way rather than against a checked-in copy of the old string
    /// on purpose. A frozen copy would fail on every later edit to the worker
    /// prose, and the failure would say nothing about whether assignments had
    /// leaked into it — which is the only thing this test is for. The wording
    /// itself is pinned by the tests above, which were not touched.
    #[test]
    fn a_brief_with_no_assignment_renders_exactly_the_preamble_it_did_before() {
        let roots = [root("/repo", false), root("/repo-worktree", true)];
        let plain = engineer(&roots, Some(ToolAccess::Delegate), None, None);
        let assigned = engineer(&roots, Some(ToolAccess::Delegate), Some(task()), None);

        assert!(
            assignment_lines(&plain).is_empty(),
            "a session with no task was still told about one"
        );

        let section = assignment_lines(&assigned);
        assert!(!section.is_empty(), "an assigned session was told nothing about its task");
        let stripped: Vec<PreambleLine> = preamble_lines(&assigned)
            .into_iter()
            .filter(|line| !section.contains(line))
            .collect();
        assert_eq!(
            stripped,
            preamble_lines(&plain),
            "the engineer section did not only add lines — it changed one"
        );

        // And the plain rendering names none of it, which is the cheap version
        // of the same claim and the one that reads in a failure message.
        let said = worker_preamble(&plain);
        assert!(!said.contains("Your one task"), "{said}");
        assert!(!said.contains(REPORTING_TOOL), "{said}");
    }

    /// **Check 23.** The three things an engineer cannot do its job without:
    /// what the task is, which files are its own, and how to report.
    #[test]
    fn an_engineer_is_told_its_task_every_path_it_owns_and_how_to_report() {
        let roots = [root("/repo", false)];
        let said = worker_preamble(&engineer(
            &roots,
            Some(ToolAccess::Delegate),
            Some(task()),
            None,
        ));
        assert!(said.contains("Teach the board which files a task owns"), "{said}");
        assert!(said.contains("5d9c7f02-1f1c-4a2f-9f3a-0b1c2d3e4f50"), "{said}");
        for path in ["core/src/works.rs", "core/src/team.rs"] {
            assert!(said.contains(path), "`{path}` is not named as a file it owns: {said}");
        }
        // The verb by the name it is registered under, not a paraphrase.
        assert!(said.contains("`complete_task`"), "{said}");
        // And the two rules that make owning files mean anything.
        assert!(said.contains("Nothing else in this repository is yours to change"), "{said}");
        assert!(said.contains("Say so in your report and stop"), "{said}");
        // Escalation is not the manager's to swallow — see D4.4.
        assert!(said.contains("`ask_question`") && said.contains("`request_secret`"), "{said}");
    }

    /// A task that claims no files still has to say so, or an engineer reads
    /// the absence as permission.
    #[test]
    fn an_engineer_whose_task_claims_no_files_is_told_that_it_claims_none() {
        let roots = [root("/repo", false)];
        let assignment = Assignment::new("t-1", "Find out why the poller is quiet", Vec::new());
        let said = worker_preamble(&engineer(
            &roots,
            Some(ToolAccess::Delegate),
            Some(assignment),
            None,
        ));
        assert!(said.contains("This task claims no files"), "{said}");
        assert!(said.contains("change nothing"), "{said}");
    }

    /// **The carve-out, and it is the one bullet here that was found by running
    /// the spec rather than by writing it.**
    ///
    /// The first engineer to add a field to `TeamTask` had to touch two files
    /// under `cli/src/tui/` that nobody owned, because their test literals
    /// stopped compiling. A path rule with no carve-out would have told it to
    /// stop and report with the workspace uncompilable, which blocks every
    /// other engineer on the job. Both halves are asserted: that the mechanical
    /// fix is allowed, and that the line where it stops is stated — otherwise
    /// the carve-out reads as a general licence to fix what it finds.
    #[test]
    fn an_engineer_may_repair_the_mechanical_fallout_of_its_own_change_and_no_more() {
        let roots = [root("/repo", false)];
        let said = worker_preamble(&engineer(
            &roots,
            Some(ToolAccess::Delegate),
            Some(task()),
            None,
        ));
        assert!(said.contains("mechanical fallout from your own change"), "{said}");
        assert!(said.contains("every literal that constructs it"), "{said}");
        assert!(
            said.contains("leaving the tree uncompilable blocks every other"),
            "the carve-out is stated without the reason it exists: {said}"
        );
        assert!(
            said.contains("what the right *value* or the right *behaviour* is"),
            "the carve-out has no edge, so it reads as a licence to fix anything: {said}"
        );
    }

    /// **An engineer with none of Jod's tools is told the truth about how to
    /// report, and this was a live bug until the tool sweep was widened.**
    ///
    /// A run launched without Jod's MCP server holds no `complete_task`, no
    /// `ask_question` and no `request_secret`, and the section was naming all
    /// three of them to it. `rail_lines` and `bus_lines` beside it already
    /// carry this branch; the engineer section did not, because every arm of
    /// `every_tool_the_preamble_tells_an_agent_to_call_is_one_that_exists`
    /// passed `assignment: None` and so never rendered it. Nothing has to be
    /// lost — Jod reads the session's output and carries it — but the session
    /// has to be told that is how it works rather than sent after three tools
    /// it does not have.
    #[test]
    fn an_engineer_without_jods_tools_is_told_to_report_in_its_final_answer() {
        let roots = [root("/repo", false)];
        let said = worker_preamble(&engineer(&roots, None, Some(task()), None));
        assert!(said.contains("Say your report in your final answer"), "{said}");
        assert!(said.contains("no way for you to file one yourself"), "{said}");
        assert!(said.contains("Blocked is still a successful ending"), "{said}");
        for absent in ["`complete_task`", "`ask_question`", "`request_secret`"] {
            assert!(
                !said.contains(absent),
                "a session holding none of Jod's tools was sent after {absent}: {said}"
            );
        }
        // It still owns its files and still stops at their edge. Losing the
        // tools does not widen what it may change.
        assert!(said.contains("core/src/works.rs"), "{said}");
        assert!(said.contains("Say so in your report and stop"), "{said}");
    }

    /// **Check 24.** Nothing in the engineer section is a fact about a harness,
    /// so nothing in it may differ by one — which is also what keeps
    /// [`the_body_of_the_preamble_is_identical_on_every_harness`] passing now
    /// that the section exists.
    #[test]
    fn the_engineer_section_is_shared_so_every_harness_is_told_the_same_thing() {
        let roots = [root("/repo", false)];
        let assigned = engineer(&roots, Some(ToolAccess::Delegate), Some(task()), None);
        for line in assignment_lines(&assigned) {
            assert!(
                line.only.is_none(),
                "a line of the engineer's task section is tagged to one harness: {line:?}"
            );
        }

        let shared = |harness| -> Vec<String> {
            preamble_lines(&Brief {
                harness,
                ..assigned.clone()
            })
            .into_iter()
            .filter(|l| l.only.is_none())
            .map(|l| l.text)
            .collect()
        };
        let claude = shared(HarnessKind::ClaudeCode);
        assert_eq!(claude, shared(HarnessKind::OpenCode));
        assert_eq!(claude, shared(HarnessKind::Agy));
    }

    // ---- what a placement changes about where you may write ----

    /// **Check 25, first half.** `explore` is a prohibition, not a starting
    /// position: a session told it may claim promotes itself to a writer its
    /// manager planned no files for.
    #[test]
    fn an_exploring_engineer_is_never_sent_to_claim_a_worktree() {
        let roots = [root("/repo", false)];
        let said = worker_preamble(&engineer(
            &roots,
            Some(ToolAccess::Delegate),
            None,
            Some(crate::leases::Placement::Explore),
        ));
        assert!(said.contains("placed this session as **explore**"), "{said}");
        assert!(said.contains("no writable root by design"), "{said}");
        assert!(said.contains("report and stop on"), "{said}");
        assert!(
            !said.contains("`claim_worktree`"),
            "a session placed to read was handed the verb that cuts a branch: {said}"
        );
    }

    /// **Check 25, second half.** A worktree claimed at spawn has to be
    /// described as claimed, or the engineer spends its first turn claiming one
    /// it already has and is told it already has one.
    #[test]
    fn an_engineer_placed_on_a_worktree_is_told_it_already_holds_one() {
        let roots = [root("/repo", false), root("/repo-worktree", true)];
        let said = worker_preamble(&engineer(
            &roots,
            Some(ToolAccess::Delegate),
            None,
            Some(crate::leases::Placement::Worktree),
        ));
        assert!(said.contains("cut for you before this session started"), "{said}");
        assert!(said.contains("Do not call `claim_worktree`; you already have one."), "{said}");
        assert!(said.contains("`release_worktree`"), "{said}");
    }

    /// **The two axes are different questions, and the brief has to be true
    /// about both.**
    ///
    /// A manager can place an engineer on a worktree and hand it read-only
    /// access to Jod in the same `open_work` call. The worktree is real — Jod
    /// cut it — and `claim_worktree` and `release_worktree` are still filtered
    /// out of that session's catalogue, so naming either would send it after a
    /// tool the server will refuse. This is the composition that
    /// [`a_read_only_session_is_not_told_to_claim_a_worktree_it_cannot_claim`]
    /// guards for an unplaced session, checked again with a placement on top of
    /// it.
    #[test]
    fn a_read_only_engineer_on_a_worktree_is_told_it_holds_one_and_cannot_claim() {
        let roots = [root("/repo", false), root("/repo-worktree", true)];
        let said = worker_preamble(&engineer(
            &roots,
            Some(ToolAccess::ReadOnly),
            None,
            Some(crate::leases::Placement::Worktree),
        ));
        assert!(said.contains("cut for you before this session started"), "{said}");
        assert!(said.contains("read-only access to Jod"), "{said}");
        assert!(!said.contains("`claim_worktree`"), "{said}");
        assert!(!said.contains("`release_worktree`"), "{said}");
    }

    /// The same again for the placement that reads. Every arm of the split has
    /// to survive a session that cannot call the verb, not only the ones where
    /// naming it would have been natural.
    #[test]
    fn a_read_only_exploring_engineer_is_told_it_could_not_claim_one_anyway() {
        let roots = [root("/repo", false)];
        let said = worker_preamble(&engineer(
            &roots,
            Some(ToolAccess::ReadOnly),
            None,
            Some(crate::leases::Placement::Explore),
        ));
        assert!(said.contains("no way for you to claim one in any case"), "{said}");
        assert!(!said.contains("`claim_worktree`"), "{said}");
    }

    /// Sharing a worktree is the placement where the danger is another person
    /// rather than Reljod, and rebasing is how one engineer destroys the
    /// other's afternoon without either of them noticing.
    #[test]
    fn an_engineer_sharing_a_worktree_is_told_who_holds_it_and_never_to_rebase_it() {
        let roots = [root("/repo", false), root("/repo-worktree", true)];
        let said = worker_preamble(&engineer(
            &roots,
            Some(ToolAccess::Delegate),
            None,
            Some(crate::leases::Placement::Share {
                work_id: "w-first".into(),
            }),
        ));
        assert!(said.contains("somebody else is already working in"), "{said}");
        assert!(said.contains("w-first"), "the lender is not named: {said}");
        assert!(said.contains("`work_board`"), "no way to find out whose files: {said}");
        assert!(said.contains("never rebase, reset or force-push"), "{said}");
    }

    /// The rarest placement and the only one with nothing between the session
    /// and Reljod's own tree, so the brief says that in those words.
    #[test]
    fn an_engineer_writing_in_reljods_checkout_is_told_there_is_no_branch_beneath_it() {
        let roots = [root("/repo", true)];
        let said = worker_preamble(&engineer(
            &roots,
            Some(ToolAccess::Delegate),
            None,
            Some(crate::leases::Placement::Direct),
        ));
        assert!(said.contains("**Reljod's own checkout**"), "{said}");
        assert!(said.contains("no branch between you and his working tree"), "{said}");
        assert!(said.contains("Commit what the task asked for and nothing else"), "{said}");
        // And the root itself is not described as a worktree, which is the
        // single most expensive sentence here to get wrong.
        assert!(said.contains("**writable**, and it is Reljod's own checkout"), "{said}");
        assert!(!said.contains("a worktree claimed for this work"), "{said}");
    }

    /// **A placement nobody stated is not `explore`.**
    ///
    /// Collapsing the two would read as a tidy simplification and would
    /// silently strip the claim instruction out of every session Jod has ever
    /// started, because `None` is what `delegate`, `continue_agent` and every
    /// unplanned `open_work` pass. The two renderings have to stay different.
    #[test]
    fn an_unplaced_session_is_told_to_claim_and_a_placed_one_is_not() {
        let roots = [root("/repo", false)];
        let unplaced = worker_preamble(&engineer(&roots, Some(ToolAccess::Delegate), None, None));
        let exploring = worker_preamble(&engineer(
            &roots,
            Some(ToolAccess::Delegate),
            None,
            Some(crate::leases::Placement::Explore),
        ));
        assert!(unplaced.contains("`claim_worktree`"), "{unplaced}");
        assert!(!exploring.contains("`claim_worktree`"), "{exploring}");
        assert_ne!(unplaced, exploring);
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
    /// The test above passes on a preamble that names no verb for repository
    /// work at all, which is exactly how the gap stayed green. This one does
    /// not.
    ///
    /// The verb used to be `open_work`, then `ask_manager`, and the layer it
    /// belongs to has moved twice. Main routes nothing now, so what this pins is
    /// the assistant's brief, on the same two counts as before: `ask_manager` is
    /// named, and it is named before the cheaper, less visible alternative.
    #[test]
    fn the_assistant_is_told_which_tool_reaches_a_repository() {
        let said = assistant_preamble();
        assert!(
            said.contains("`ask_manager`"),
            "the assistant is given the routing decision and not the tool that carries it"
        );
        let asks = said.find("`ask_manager`").expect("checked above");
        let delegates = said
            .find("`delegate` for a one-shot")
            .expect("delegate is still offered");
        assert!(
            asks < delegates,
            "`delegate` is offered before `ask_manager`, so the cheaper and less \
             visible of the two reads as the default"
        );
    }

    /// And main is told, in the same words the tool boundary uses, that none of
    /// those three verbs are its own any more.
    ///
    /// The refusal in [`crate::mcp`] is the enforcement and this is not. It is
    /// here because a model that reaches for a tool and is refused has spent a
    /// turn discovering a rule somebody could simply have told it, and because
    /// this is the paragraph most likely to be dropped as redundant once the
    /// refusal exists.
    #[test]
    fn the_orchestrator_is_told_routing_is_no_longer_its_job() {
        let said = orchestrator_preamble();
        for verb in ["`ask_manager`", "`delegate`", "`open_work`"] {
            assert!(said.contains(verb), "{verb} is not named as gone: {said}");
        }
        assert!(
            said.contains("`ask_assistant`"),
            "and nothing names the one verb it does have: {said}"
        );
        // Named before the three it has lost, so the brief reads as an
        // instruction rather than as a list of complaints.
        let asks = said.find("`ask_assistant`").expect("checked above");
        let refused = said
            .find("`open_work` is **not yours to call**")
            .expect("checked below");
        assert!(asks < refused, "main is told what it cannot do first: {said}");
    }

    /// And it is told plainly that the verb it used to reach for is gone.
    ///
    /// The tool boundary refuses it whatever the preamble says — that is the
    /// enforcement — but a model that reaches for a tool and is refused has
    /// spent a turn discovering a rule it could have been told. Worth pinning
    /// because it is the paragraph most likely to be dropped as redundant.
    #[test]
    fn the_orchestrator_is_told_open_work_is_not_its_to_call() {
        let said = orchestrator_preamble();
        assert!(
            said.contains("`open_work` is **not yours to call**"),
            "nothing tells main that its old verb is gone: {said}"
        );
        assert!(
            said.contains("refused too"),
            "and nothing closes the `delegate`-at-a-checkout route around it: {said}"
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
    }

    /// The other half of the sentence above, which moved with `delegate`.
    ///
    /// Main no longer starts one-shots, so the run whose answer has to find its
    /// way home is the assistant's. A read-only child cannot send one, and a
    /// caller that does not know that gets silence and reads it as the run
    /// having failed.
    #[test]
    fn the_assistant_is_told_a_read_only_child_cannot_report_back() {
        let said = assistant_preamble();
        assert!(said.contains("pass `tools: \"delegate\"`"), "{said}");
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

    /// Nothing offered a branch for answering at all, so a question the router
    /// already knew the answer to still bought a spawned agent. The branch has
    /// to be stated, and it has to be stated *before* the handing-over verbs —
    /// after them it reads as an exception to the rule rather than as the first
    /// thing to check.
    ///
    /// It lived in main's brief for one release and it lives in the assistant's
    /// now, for the reason [`assistant_preamble`] gives: the branch is a model
    /// turn, and a model turn inside main's turn is a console Reljod cannot
    /// type into.
    #[test]
    fn the_assistant_is_told_it_may_answer_a_quick_question_itself() {
        let said = assistant_preamble();
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

    /// And main does not get that branch back by the side door.
    ///
    /// This is the assumption the spec states plainly and offers to be
    /// corrected on: main answers nothing itself, not even a definition it
    /// knows, because the branch that decides whether to answer is exactly the
    /// thinking that used to hold the console. A brief that lets main answer
    /// "when it is obvious" reintroduces it, because whether it is obvious is
    /// itself the judgement.
    #[test]
    fn the_orchestrator_does_not_answer_anything_itself() {
        let said = orchestrator_preamble();
        assert!(
            !said.contains("**Answer directly**"),
            "the answer branch is back in main's brief: {said}"
        );
        assert!(
            said.contains("you do not answer it yourself"),
            "and nothing says so plainly: {said}"
        );
        assert!(
            said.contains("Even a question you are sure you know the answer to goes to it"),
            "the tempting exception has to be closed by name: {said}"
        );
    }

    /// The three sizes, in Reljod's own terms, in the order they are checked.
    /// Drop any one and the branch is a two-way choice again.
    #[test]
    fn the_assistant_is_told_the_task_decides_which_branch_it_takes() {
        let said = assistant_preamble();
        assert!(said.contains("**Decide by the task, in this order.**"), "{said}");

        let answer = said.find("**Answer directly**").expect("the cheapest first");
        let manager = said.find("`ask_manager` for").expect("then a repository");
        let one_shot = said.find("`delegate` for a one-shot").expect("then a one-shot");
        assert!(
            answer < manager && manager < one_shot,
            "the three sizes are not offered cheapest first, so the branch that used \
             to be skipped is the one that reads as the exception again: {said}"
        );
    }

    /// Schedules and goals did not move, and this is the test that says so.
    ///
    /// Everything else main used to decide went to the assistant. These two did
    /// not, because arming one spends money at 2am with nobody watching —
    /// `docs/spec-ceo-and-managers.md`, open question 4 — and nothing about
    /// moving the routing decision changes that argument. Without this the
    /// obvious tidy-up is to send them on with the rest.
    #[test]
    fn schedules_and_goals_stay_with_main_rather_than_going_to_the_assistant() {
        let said = orchestrator_preamble();
        assert!(said.contains("is still `schedule_create`"), "{said}");
        assert!(said.contains("is still `goal_create`"), "{said}");
        assert!(
            said.contains("do not go to the assistant"),
            "they have to be marked as the exception to handing everything over, or \
             the general rule swallows them: {said}"
        );
        assert!(
            !assistant_preamble().contains("`schedule_create`"),
            "the assistant must not arm a schedule: it holds `delegate` access, and \
             `schedule_create` needs `orchestrate`"
        );
        assert!(!assistant_preamble().contains("`goal_create`"));
    }

    /// Answering is bounded by three things, and the bound is the whole reason
    /// this is not a licence to do the work. Losing any of them lets the
    /// answering layer start reading a checkout, which is the failure the old
    /// rule was written against and which this change must not reintroduce.
    #[test]
    fn the_answer_branch_is_bounded_rather_than_open_ended() {
        let said = assistant_preamble();
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
        let said = assistant_preamble();
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
            manager_conversation_id: None,
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

    // ---- a project's manager ----

    mod managers {
        use super::*;
        use crate::projects::NewProject;

        fn catalogued_at(store: &Store, dir: &str, name: &str) -> crate::projects::Project {
            std::fs::create_dir_all(dir).unwrap();
            store.add_project(NewProject::at(dir).named(name)).unwrap()
        }

        /// Check 10. Get-or-create, for the same reason the main chat is: a
        /// manager that has to be set up is one that is missing exactly when
        /// you first need it.
        #[test]
        fn a_projects_manager_is_created_once_and_found_again() {
            let s = store();
            let dir = format!("/tmp/jod-mc-{}-a", std::process::id());
            let project = catalogued_at(&s, &dir, "tetris");

            let (first, fresh) = s
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();
            assert!(fresh, "the first call has to say it created one");

            let (again, fresh_again) = s
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();
            assert_eq!(first, again, "a second call minted a second manager");
            assert!(!fresh_again, "and it must not claim to have created one");

            // Asked on a different harness, it is still the same manager. Its
            // value is that it remembers the repository, and splitting it by
            // harness would split that memory for a reason that has nothing to
            // do with the repository.
            let (on_codex, _) = s
                .manager_conversation(&project.id, HarnessKind::OpenCode)
                .unwrap();
            assert_eq!(first, on_codex, "the manager was split by harness");

            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn two_projects_get_two_managers() {
            let s = store();
            let base = format!("/tmp/jod-mc-{}-b", std::process::id());
            let one = catalogued_at(&s, &format!("{base}/tetris"), "tetris");
            let two = catalogued_at(&s, &format!("{base}/pacman"), "pacman");

            let (a, _) = s.manager_conversation(&one.id, HarnessKind::ClaudeCode).unwrap();
            let (b, _) = s.manager_conversation(&two.id, HarnessKind::ClaudeCode).unwrap();
            assert_ne!(a, b, "both projects share one manager");

            std::fs::remove_dir_all(&base).ok();
        }

        /// The correction that would otherwise have broken the main chat.
        ///
        /// A manager mirrors `main_conversation`'s *shape* and not its
        /// mechanism. `pinned_conversation` is a `query_row` with no `LIMIT`
        /// and no ordering and does not error on a second row, so a manager
        /// carrying `pinned = 1` would not fail loudly — it would make which
        /// conversation is "main" depend on SQLite's row order, and
        /// `hand_to_orchestrator` would start appending Reljod's instructions
        /// to a project manager's transcript.
        #[test]
        fn creating_a_manager_does_not_disturb_the_main_chat() {
            let s = store();
            let dir = format!("/tmp/jod-mc-{}-c", std::process::id());
            let project = catalogued_at(&s, &dir, "tetris");
            let main = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();

            let (manager, _) = s
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();

            assert_ne!(manager, main);
            assert_eq!(
                s.pinned_conversation().unwrap(),
                Some(main.clone()),
                "the main chat moved when a manager was created"
            );
            // And it stays put however many managers exist.
            let other = catalogued_at(&s, &format!("{dir}-2"), "pacman");
            s.manager_conversation(&other.id, HarnessKind::ClaudeCode).unwrap();
            assert_eq!(s.pinned_conversation().unwrap(), Some(main));

            std::fs::remove_dir_all(&dir).ok();
            std::fs::remove_dir_all(format!("{dir}-2")).ok();
        }

        /// A manager's card still reaches Reljod after main compacts itself.
        ///
        /// Cards cascade along `parent_conversation_id`, a manager is hung
        /// under main when it is created, and compaction opens a *fresh* main.
        /// The edges stayed on the thread that was compacted away, so the
        /// managers went on reporting upward into a conversation nobody opens —
        /// which silently undid the link that makes a manager's answer reach
        /// Reljod at all.
        ///
        /// Observed as a fleet showing `alpha [3 cards]` and `gamma [8 cards]`
        /// beside a rail reading "nothing waiting — no agent has asked
        /// anything". Nobody has to do anything for it: main compacts itself.
        ///
        /// Driven through `cards` rather than by reading the column, because
        /// the column being right is not the claim — the claim is that the card
        /// arrives.
        #[test]
        fn a_managers_card_still_reaches_main_after_it_compacts() {
            use crate::cards::{NewCard, Query};

            let s = store();
            let dir = format!("/tmp/jod-mc-{}-compact", std::process::id());
            let project = catalogued_at(&s, &dir, "tetris");
            let main = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
            let (manager, _) = s
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();
            for turn in 0..3 {
                s.append_prompt(&main, &format!("run-{turn}"), "go").unwrap();
            }

            // What happens on its own when a context fills.
            s.continue_as_new(&main, "so far", "full").unwrap();
            let now = s.pinned_conversation().unwrap().unwrap();
            assert_ne!(now, main, "the pin moved, which is the premise");

            s.raise_card(NewCard {
                conversation_id: manager,
                title: "the README edit is done".into(),
                ..NewCard::default()
            })
            .unwrap();

            let rail = s
                .cards(&Query {
                    subtree_of: Some(now),
                    ..Query::default()
                })
                .unwrap();
            assert!(
                rail.iter().any(|c| c.title == "the README edit is done"),
                "the manager still reports to whichever conversation is main: {:?}",
                rail.iter().map(|c| &c.title).collect::<Vec<_>>(),
            );

            std::fs::remove_dir_all(&dir).ok();
        }

        /// A question still owed to Reljod follows the chat it was asked in.
        ///
        /// The rail shows the subtree of the conversation being viewed, and
        /// compaction moves the pin to a fresh one. A card raised by main
        /// itself — which is how `ask_question` reaches Reljod — stayed on the
        /// thread that was compacted away and dropped off the rail. A blocking
        /// question asked shortly before a compaction simply disappeared, and
        /// main compacts itself.
        #[test]
        fn an_open_card_on_the_main_chat_survives_its_compaction() {
            use crate::cards::{NewCard, Query};

            let s = store();
            let main = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
            for turn in 0..3 {
                s.append_prompt(&main, &format!("run-{turn}"), "go").unwrap();
            }
            s.raise_card(NewCard {
                conversation_id: main.clone(),
                title: "which web did you mean".into(),
                ..NewCard::default()
            })
            .unwrap();
            // And one already dealt with, which is history and must stay put.
            let answered = s
                .raise_card(NewCard {
                    conversation_id: main.clone(),
                    title: "something already settled".into(),
                    ..NewCard::default()
                })
                .unwrap();
            s.answer_card(answered.id, None, Some("yes")).unwrap();

            s.continue_as_new(&main, "so far", "full").unwrap();
            let now = s.pinned_conversation().unwrap().unwrap();

            let open: Vec<String> = s
                .cards(&Query {
                    subtree_of: Some(now),
                    ..Query::default()
                })
                .unwrap()
                .iter()
                .map(|c| c.title.clone())
                .collect();
            assert!(
                open.contains(&"which web did you mean".to_string()),
                "the question is still owed, so it is still on the rail: {open:?}",
            );

            // The settled one stays where it happened. Moving it would rewrite
            // which conversation actually asked and answered it.
            let stayed: Option<String> = {
                let conn = s.conn.lock().expect("store lock poisoned");
                conn.query_row(
                    "SELECT conversation_id FROM cards WHERE title = ?1",
                    rusqlite::params!["something already settled"],
                    |r| r.get(0),
                )
                .ok()
            };
            assert_eq!(stayed.as_deref(), Some(main.as_str()));
        }

        /// The backfill, for the consoles that have already compacted."""
        ///
        /// `carry_forward` moves these edges now, but main compacts itself on a
        /// timer, so any console that has been up a while is already carrying
        /// managers parented to a thread nobody opens — and a rail that has
        /// been quietly empty ever since.
        #[test]
        fn the_backfill_reattaches_managers_left_on_an_old_main_chat() {
            use crate::cards::{NewCard, Query};

            let s = store();
            let dir = format!("/tmp/jod-mc-{}-reattach", std::process::id());
            let project = catalogued_at(&s, &dir, "tetris");
            let main = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();
            let (manager, _) = s
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();
            for turn in 0..3 {
                s.append_prompt(&main, &format!("run-{turn}"), "go").unwrap();
            }
            s.continue_as_new(&main, "so far", "full").unwrap();
            let now = s.pinned_conversation().unwrap().unwrap();

            // Put it back the way the old code left it.
            s.write(|tx| {
                tx.execute(
                    "UPDATE conversations SET parent_conversation_id = ?2 WHERE id = ?1",
                    rusqlite::params![manager, main],
                )?;
                Ok(())
            })
            .unwrap();
            s.raise_card(NewCard {
                conversation_id: manager.clone(),
                title: "the README edit is done".into(),
                ..NewCard::default()
            })
            .unwrap();
            let rail = |root: &str| {
                s.cards(&Query {
                    subtree_of: Some(root.to_string()),
                    ..Query::default()
                })
                .unwrap()
                .iter()
                .map(|c| c.title.clone())
                .collect::<Vec<_>>()
            };
            assert!(
                rail(&now).is_empty(),
                "the bug, reproduced: the rail is empty while the work is done",
            );

            let (_, sql) = crate::store::MIGRATIONS
                .iter()
                .find(|(name, _)| name.starts_with("0026"))
                .expect("the backfill migration");
            s.write(|tx| {
                tx.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();

            assert!(
                rail(&now).contains(&"the README edit is done".to_string()),
                "after the backfill it arrives: {:?}",
                rail(&now),
            );

            std::fs::remove_dir_all(&dir).ok();
        }

        /// A manager handed to another harness stays that project's manager.
        ///
        /// A manager is found through `projects.manager_conversation_id`, and
        /// `switch_harness` compacts the thread into a *new* conversation. The
        /// pointer was left on the old one — which still exists, so
        /// `manager_conversation` happily returned it — and the next
        /// `ask_manager` resumed the thread the switch had handed away, on the
        /// harness it had been handed away from. The switch was undone without
        /// a word and the summary sat in a conversation nobody opens again.
        ///
        /// Observed by switching alpha's manager to OpenCode: the console ended
        /// up in `alpha → OpenCode` while the catalog still named the Claude
        /// Code row.
        #[test]
        fn a_manager_handed_to_another_harness_is_still_the_projects_manager() {
            let s = store();
            let dir = format!("/tmp/jod-mc-{}-switch", std::process::id());
            let project = catalogued_at(&s, &dir, "tetris");
            let (manager, _) = s
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();
            // Something to carry over; a switch refuses an empty thread.
            for turn in 0..3 {
                s.append_prompt(&manager, &format!("run-{turn}"), "go").unwrap();
            }

            let switched = s
                .switch_harness(&manager, HarnessKind::OpenCode, "so far", "moving")
                .unwrap();
            let now = switched.conversation.id;
            assert_ne!(now, manager, "the switch opens a new thread, as it should");

            let (found, fresh) = s
                .manager_conversation(&project.id, HarnessKind::OpenCode)
                .unwrap();
            assert_eq!(
                found, now,
                "the project has to follow its manager onto the new harness",
            );
            assert!(!fresh, "and must not start a third conversation");

            std::fs::remove_dir_all(&dir).ok();
        }

        /// The promise both preambles make, held to by the data.
        ///
        /// `ask_manager` tells Reljod "It will raise a card on your rail", and
        /// the manager preamble tells the manager a card "cascades up to his
        /// rail and is the only way your answer reaches him". Neither was true:
        /// cards cascade along `parent_conversation_id`, and a manager was
        /// created with none — so main's rail read "nothing waiting" while a
        /// finished piece of work sat on a rail nobody opens.
        ///
        /// Driven through `cards` rather than by reading the column, because
        /// the column being set is not the claim — the claim is that the card
        /// arrives.
        #[test]
        fn a_managers_card_reaches_the_main_chats_rail() {
            use crate::cards::{NewCard, Query};

            let s = store();
            let dir = format!("/tmp/jod-mc-{}-rail", std::process::id());
            let project = catalogued_at(&s, &dir, "tetris");
            let main = s.main_conversation(HarnessKind::ClaudeCode, "/tmp").unwrap();

            let (manager, _) = s
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();
            s.raise_card(NewCard {
                conversation_id: manager.clone(),
                title: "the README edit is done".into(),
                ..NewCard::default()
            })
            .unwrap();

            let rail = s
                .cards(&Query {
                    subtree_of: Some(main.clone()),
                    ..Query::default()
                })
                .unwrap();
            assert!(
                rail.iter().any(|c| c.title == "the README edit is done"),
                "main's rail must carry what its manager raised: {:?}",
                rail.iter().map(|c| &c.title).collect::<Vec<_>>(),
            );

            // And the cascade stays one-way: a manager must not be handed
            // Reljod's own questions, which would be an answer landing on the
            // wrong agent.
            s.raise_card(NewCard {
                conversation_id: main,
                title: "a question for Reljod".into(),
                ..NewCard::default()
            })
            .unwrap();
            let below = s
                .cards(&Query {
                    subtree_of: Some(manager),
                    ..Query::default()
                })
                .unwrap();
            assert!(
                !below.iter().any(|c| c.title == "a question for Reljod"),
                "the cascade runs upward only",
            );

            std::fs::remove_dir_all(&dir).ok();
        }

        /// A manager knows which project it owns from the moment it exists, so
        /// everything it starts inherits it and nothing below has to guess.
        #[test]
        fn a_manager_is_titled_and_pointed_at_its_own_project() {
            let s = store();
            let dir = format!("/tmp/jod-mc-{}-d", std::process::id());
            let project = catalogued_at(&s, &dir, "tetris");

            let (manager, _) = s
                .manager_conversation(&project.id, HarnessKind::ClaudeCode)
                .unwrap();

            let conversation = s.conversation(&manager).unwrap().unwrap();
            assert_eq!(conversation.title, "tetris");
            // Compared against the path the catalogue stored, not against the
            // string handed to `catalogued_at`. `projects::normalise`
            // canonicalises on the way in, on purpose, so that one directory has
            // one spelling; on macOS `/tmp` is a symlink and the stored path
            // comes back as `/private/tmp/…`. Asserting against the raw string
            // was really asserting that no symlink was involved, which is a fact
            // about the host and not about managers. This still bites: a manager
            // pointed at any other checkout fails it exactly as before.
            assert_eq!(
                conversation.cwd,
                project.path.to_string_lossy(),
                "a manager sits in its own checkout"
            );
            assert_eq!(
                s.current_project(&manager).unwrap().map(|p| p.id),
                Some(project.id),
                "a manager that does not know its own project is one whose \
                 sessions have to be told"
            );

            std::fs::remove_dir_all(&dir).ok();
        }

        /// Check 9. `open_work` used to touch the catalog not at all, so
        /// "which works are on Jod?" was unanswerable and the child session did
        /// not inherit the project either.
        #[test]
        fn a_work_opened_in_a_catalogued_checkout_records_its_project() {
            let s = store();
            let dir = format!("/tmp/jod-mc-{}-e", std::process::id());
            let project = catalogued_at(&s, &dir, "tetris");

            let opened = prepare_work(&s, &Opening::new("port the parser", &dir)).unwrap();

            assert_eq!(
                opened.work.project_id.as_deref(),
                Some(project.id.as_str()),
                "the work does not know which repository it is about"
            );
            assert_eq!(
                s.current_project(&opened.conversation_id).unwrap().map(|p| p.id),
                Some(project.id),
                "the session did not inherit the project, so everything it \
                 starts will have to guess"
            );

            std::fs::remove_dir_all(&dir).ok();
        }

        /// And an uncatalogued directory is still somewhere to work. Refusing
        /// here would make the catalog a gate rather than a convenience.
        #[test]
        fn a_work_opened_somewhere_uncatalogued_still_opens() {
            let s = store();
            let opened = prepare_work(&s, &Opening::new("port the parser", "/tmp")).unwrap();
            assert_eq!(opened.work.project_id, None);
            assert_eq!(s.current_project(&opened.conversation_id).unwrap(), None);
        }
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

    // ---- the assistant's standing conversation ----

    /// Mark a run of `conversation` as still going, so
    /// `Store::conversation_is_busy` says so.
    ///
    /// Both halves are needed and neither is enough: the busy check joins
    /// `messages` to `runs`, so a run row with no message is invisible to it and
    /// a message whose run has no row is too.
    fn running_run(store: &Store, conversation: &str, run_id: &str) {
        store
            .append_prompt(conversation, run_id, "the turn before")
            .unwrap();
        store
            .save_run(&crate::store::StoredRun {
                id: run_id.into(),
                name: "assistant".into(),
                harness: "claude_code".into(),
                status: "running".into(),
                cwd: "/tmp".into(),
                session_id: Some(format!("session-{run_id}")),
                pid: None,
                pgid: None,
                created_at_ms: 0,
                summary: serde_json::Value::Null,
            })
            .unwrap();
    }

    /// The two things that make the assistant's conversation what it is,
    /// asserted on the row rather than on the code that writes it.
    ///
    /// `origin` is how the recursion guard in `crate::mcp` recognises one, and
    /// the parent is what makes the card it raises cascade onto Reljod's rail.
    /// Miss the parent and the assistant works perfectly and reports to nobody,
    /// which is the failure `manager_conversation` already had once.
    #[test]
    fn the_assistants_conversation_hangs_under_main_and_says_what_it_is() {
        let store = Store::in_memory().unwrap();
        let main = store
            .main_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();

        let (assistant, fresh) = store
            .assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();

        assert!(fresh, "the first ask has to create it");
        assert_eq!(
            store.conversation_origin(&assistant).unwrap().as_deref(),
            Some(ASSISTANT_ORIGIN)
        );
        assert_eq!(
            store.parent_conversation(&assistant).unwrap().as_deref(),
            Some(main.as_str()),
            "an assistant that hangs under nothing answers onto a rail nobody reads"
        );
        assert_ne!(
            store.pinned_conversation().unwrap().as_deref(),
            Some(assistant.as_str()),
            "an assistant must never take the pin — main is the one conversation \
             that holds it, and a second pinned row makes which one counts as main \
             depend on the order SQLite returns"
        );
    }

    /// One assistant, however many times it is asked for.
    ///
    /// This is the reversal, and the assertion that says so. It used to open a
    /// conversation per instruction, so the assistant forgot everything between
    /// one instruction and the next; the second call now finds the first one's
    /// thread. What stops that serialising is the queue, which the interrupt
    /// test below covers.
    #[test]
    fn every_ask_for_the_assistant_reaches_the_same_conversation() {
        let store = Store::in_memory().unwrap();
        store
            .main_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();

        let (first, first_fresh) = store
            .assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        let (second, second_fresh) = store
            .assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();

        assert_eq!(
            first, second,
            "an assistant that is minted per instruction cannot remember the \
             instruction before it, which is the whole point of standing"
        );
        assert!(
            first_fresh && !second_fresh,
            "only the first ask creates one"
        );
    }

    /// The standing thread is not in the scratch lane, so nothing sweeps it
    /// away between two instructions.
    ///
    /// It used to be marked `ephemeral`, which was right when it was one
    /// conversation per instruction. Every query in that lane starts at
    /// `ephemeral = 1`: `scratch_ready_to_archive` hides the row once its
    /// latest run completes, and `scratch_ready_to_delete` removes it — and its
    /// messages, and its cards — once it has been hidden long enough. A
    /// standing conversation that gets tidied away is a standing conversation
    /// that forgets, on a timer, with nothing saying it happened.
    #[test]
    fn the_standing_assistant_is_never_swept_away_as_scratch() {
        let store = Store::in_memory().unwrap();
        store
            .main_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        let (assistant, _) = store
            .assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        // A finished turn, which is the state the archive sweep looks for.
        store.append_prompt(&assistant, "run-1", "do it").unwrap();
        store
            .save_run(&crate::store::StoredRun {
                id: "run-1".into(),
                name: "assistant".into(),
                harness: "claude_code".into(),
                status: "completed".into(),
                cwd: "/tmp".into(),
                session_id: Some("session-1".into()),
                pid: None,
                pgid: None,
                created_at_ms: 0,
                summary: serde_json::Value::Null,
            })
            .unwrap();

        assert!(
            !store.is_ephemeral(&assistant).unwrap(),
            "the assistant is in the scratch lane, so the sweeps will archive \
             and then delete the thread that is supposed to stand"
        );
        assert!(
            !store
                .scratch_ready_to_archive()
                .unwrap()
                .contains(&assistant),
            "the archive sweep has the assistant's standing thread on its list"
        );
        assert!(
            !store
                .scratch_runs()
                .unwrap()
                .values()
                .any(|c| *c == assistant),
            "the assistant's runs read as scratch, so `list_agents` and the \
             loose pane treat the standing layer as an errand"
        );
    }

    /// An instruction arriving while the assistant is mid-turn is queued, not
    /// blocked on and not dropped.
    ///
    /// **This is the objection the old design was built around, and the
    /// answer to it.** A standing assistant was rejected because instruction
    /// two would wait behind instruction one; it does not wait, it joins the
    /// delivery queue and reaches the assistant at the start of its next turn,
    /// batched with anything else that arrived meanwhile. `hand_to_assistant`
    /// returns without spawning anything, which is what keeps main's turn — and
    /// therefore the console — free.
    #[tokio::test]
    async fn an_instruction_to_a_busy_assistant_is_queued_for_its_next_turn() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        store
            .main_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        let (assistant, _) = store
            .assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        running_run(&store, &assistant, "run-1");
        let jod = Jod::with_store(store.clone());

        let taken = hand_to_assistant(
            &jod,
            "actually, do the other one first",
            HarnessKind::ClaudeCode,
            PathBuf::from("/tmp"),
            PermissionPolicy::default(),
        )
        .await
        .unwrap();

        assert!(
            taken.queued,
            "a busy assistant must not be spawned on top of"
        );
        assert_eq!(
            taken.run_id, None,
            "nothing was started, so there is no run to name — a second run here \
             would be two processes extending one harness session"
        );
        assert_eq!(taken.conversation_id, assistant);

        let waiting = store.pending_for(&assistant).unwrap();
        assert_eq!(
            waiting.len(),
            1,
            "the instruction was dropped rather than queued"
        );
        assert_eq!(waiting[0].kind, crate::delivery::Kind::Human);
        assert_eq!(waiting[0].body, "actually, do the other one first");

        // And a second one joins it rather than replacing it or starting
        // anything. Batching is the intended behaviour: the assistant reads both
        // in one turn.
        hand_to_assistant(
            &jod,
            "and check the CI while you are there",
            HarnessKind::ClaudeCode,
            PathBuf::from("/tmp"),
            PermissionPolicy::default(),
        )
        .await
        .unwrap();
        let waiting = store.pending_for(&assistant).unwrap();
        assert_eq!(
            waiting.len(),
            2,
            "the second instruction overwrote the first"
        );
        let injection = store.plan_injection(&assistant, false).unwrap().unwrap();
        assert!(
            injection.prompt.contains("do the other one first")
                && injection.prompt.contains("check the CI"),
            "both queued instructions have to reach the same turn: {}",
            injection.prompt
        );
    }

    /// The assistant's thread is compacted on the same two clocks main's is.
    ///
    /// A standing conversation that never compacts grows until the harness
    /// refuses it, and nothing is watching this one the way the console watches
    /// main. `hand_to_assistant` therefore computes the verdict itself and says
    /// so on what it returns, and the instruction that found the thread over the
    /// threshold is queued behind the summariser rather than starting a turn
    /// against a transcript that is about to be replaced.
    #[tokio::test]
    async fn a_full_assistant_thread_is_due_for_compaction_and_the_instruction_waits() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        store
            .main_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        let (assistant, _) = store
            .assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        // Over `COMPACT_CHARS`, which is the size trigger.
        let long = "x".repeat(COMPACT_CHARS + 1);
        store.append_prompt(&assistant, "run-1", &long).unwrap();
        let jod = Jod::with_store(store.clone());

        let taken = hand_to_assistant(
            &jod,
            "one more thing",
            HarnessKind::ClaudeCode,
            PathBuf::from("/tmp"),
            PermissionPolicy::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            taken.compaction_due.map(|(why, _)| why),
            Some("size"),
            "nothing measures the assistant's context, so it grows until the \
             harness refuses the turn"
        );
        assert!(
            taken.queued,
            "a turn started against a transcript about to be summarised away is \
             a turn spent on a context that is being replaced"
        );
        assert_eq!(
            store.pending_for(&assistant).unwrap().len(),
            1,
            "the instruction has to survive the compaction it triggered"
        );
    }

    /// Everything the assistant needs survives its own compaction.
    ///
    /// Compaction opens a *fresh* conversation seeded with a summary, and four
    /// things about the old row have to move onto it or the assistant carries on
    /// looking fine and reaching nobody:
    ///
    /// - the **pointer**, or the next instruction resumes the thread that was
    ///   just compacted away and the compaction is silently undone;
    /// - the **parent edge**, or its cards stop cascading onto Reljod's rail —
    ///   the exact failure `a_managers_card_still_reaches_main_after_it_compacts`
    ///   records one level up;
    /// - the **origin**, or the run no longer counts as an assistant and the
    ///   guard that stops it starting another one stops applying;
    /// - the **queue**, or an instruction handed to it a moment before is
    ///   injected into a conversation nothing resumes any more.
    #[test]
    fn compacting_the_assistant_carries_its_whole_identity_forward() {
        use crate::cards::{NewCard, Query};

        let store = Store::in_memory().unwrap();
        let main = store
            .main_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        let (assistant, _) = store
            .assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        store
            .append_prompt(&assistant, "run-1", "what is running")
            .unwrap();
        store
            .enqueue_delivery(
                &assistant,
                crate::delivery::Kind::Human,
                "ask-1",
                "and the other thing",
            )
            .unwrap();
        store
            .raise_card(NewCard {
                conversation_id: assistant.clone(),
                title: "handed the README to tetris".into(),
                ..NewCard::default()
            })
            .unwrap();

        store.continue_as_new(&assistant, "so far", "full").unwrap();

        let (now, fresh) = store
            .assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        assert_ne!(now, assistant, "the compaction did not open a new thread");
        assert!(
            !fresh,
            "the pointer did not move, so the next instruction opened a third \
             assistant instead of continuing the compacted one"
        );
        assert_eq!(
            store.conversation_origin(&now).unwrap().as_deref(),
            Some(ASSISTANT_ORIGIN),
            "the continuation does not read as an assistant, so the recursion \
             guard no longer applies to it"
        );
        assert_eq!(
            store.parent_conversation(&now).unwrap().as_deref(),
            Some(main.as_str()),
            "the continuation reports to nobody"
        );
        let rail: Vec<String> = store
            .cards(&Query {
                subtree_of: Some(main.clone()),
                ..Query::default()
            })
            .unwrap()
            .iter()
            .map(|c| c.title.clone())
            .collect();
        assert!(
            rail.contains(&"handed the README to tetris".to_string()),
            "the assistant's open card fell off Reljod's rail: {rail:?}"
        );
        let waiting = store.pending_for(&now).unwrap();
        assert_eq!(
            waiting.len(),
            1,
            "the queued instruction stayed on the thread that was compacted away"
        );
        assert_eq!(waiting[0].body, "and the other thing");
    }

    /// The assistant can answer main, which is the thing it could not do.
    ///
    /// **The measured failure, as an assertion.** Every bus tool derives sender
    /// identity from the run and refuses one that belongs to no addressing
    /// scope. The assistant's conversation belongs to no work, and nothing ever
    /// gave its runs a scope — `delegate` opens a return channel for the run it
    /// starts and `hand_to_assistant` did not — so `send_message` and `reply`
    /// answered `run … is not a member of any team or work`. Reproduced before
    /// it was fixed: `Store::caller_for_run` on a run in the assistant's
    /// conversation returned `None`.
    ///
    /// What that cost is the whole of the "answer directly" branch of the
    /// assistant's brief. A card reaches Reljod's rail, which is right for work
    /// handed on; an *answer* left on a rail is an answer he has to go and find,
    /// and the branch exists precisely so that "what time is it in Manila" comes
    /// straight back. It went into a transcript nobody opens.
    ///
    /// Asserted on the store rather than by spawning, because opening the
    /// channel is the half that was missing and starting a harness process is
    /// not something a unit test can do. That `hand_to_assistant` actually calls
    /// this is checked below, off the source, for the same reason the
    /// never-waits check is.
    #[test]
    fn the_assistant_has_a_way_to_answer_main() {
        let store = Store::in_memory().unwrap();
        let main = store
            .main_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        // A main chat that has actually run, because `main_chat_is_resumable`
        // is what decides whether mail to it starts a turn: a pinned
        // conversation with no harness session behind it would have to be woken
        // into a fresh context, and an orchestrator woken having forgotten what
        // it delegated is worse than one not woken at all.
        store
            .record_session(&main, HarnessKind::ClaudeCode, "ses-main")
            .unwrap();
        let (assistant, _) = store
            .assistant_conversation(HarnessKind::ClaudeCode, "/tmp")
            .unwrap();
        store
            .append_prompt(&assistant, "run-1", "what time is it in Manila")
            .unwrap();

        // Before the channel: no identity, so every bus tool refuses.
        assert!(
            store.caller_for_run("run-1").unwrap().is_none(),
            "the premise of this test is that a run of the assistant's \
             conversation is on no roster until something puts it on one"
        );

        let name = store
            .open_return_channel("run-1", ASSISTANT_MEMBER, HarnessKind::ClaudeCode)
            .unwrap()
            .expect("a channel, because there is a main chat to report to");
        assert_eq!(name, ASSISTANT_MEMBER);

        let caller = store
            .caller_for_run("run-1")
            .unwrap()
            .expect("the run resolves to a member from its first tool call");
        assert_eq!(caller.name, ASSISTANT_MEMBER);
        let roster = store
            .roster(caller.scope, &caller.team, &caller.name)
            .unwrap();
        let main = roster
            .iter()
            .find(|a| a.name == crate::team::MAIN)
            .expect("`main` has to be on the assistant's roster or it cannot answer");
        assert!(
            main.can_be_woken,
            "a message to `main` has to start a turn, or the answer is a row in \
             a table rather than something Reljod is told"
        );
    }

    /// `hand_to_assistant` opens that channel on every turn it starts.
    ///
    /// The store test above proves the channel works; this proves it is asked
    /// for. Off the source, for the reason the never-waits check is: what stands
    /// between the assistant and a return leg is one call, and a unit test
    /// cannot reach it without starting a harness process.
    #[test]
    fn handing_an_instruction_over_opens_the_assistants_return_channel() {
        let source = include_str!("orchestrator.rs");
        let start = source
            .find("pub async fn hand_to_assistant(")
            .expect("hand_to_assistant is in this file");
        let body = &source[start..];
        let end = body[1..]
            .find("\n/// ")
            .map(|at| at + 1)
            .unwrap_or(body.len());
        assert!(
            body[..end].contains("open_return_channel("),
            "the assistant is started with no way to answer main, so anything it \
             answers itself reaches nobody"
        );
    }

    /// A thread with no harness session is handed its own record.
    ///
    /// `resume_for` answers `Fresh` in two cases the standing assistant reaches
    /// on its own: the turn straight after a compaction, where the summary is
    /// the only thing the conversation contains, and the turn after its harness
    /// changed under it. Nothing in `crate::runner` can stream a transcript into
    /// a harness — `Store::handoff_text` exists because of that — so a `Fresh`
    /// spawn that carries nothing is an assistant that has silently forgotten,
    /// on a turn nobody would think to check.
    ///
    /// Asserted off the source, because what a spawn is *given* is only
    /// observable by starting a harness process. What can be checked exactly is
    /// that the function reaches for the record at all, which is the whole of
    /// the difference between remembering and not.
    #[test]
    fn a_resumeless_assistant_thread_carries_its_record_in_the_prompt() {
        let source = include_str!("orchestrator.rs");
        let body = body_of(source, "pub async fn hand_to_assistant(");
        assert!(
            body.contains("handoff_text("),
            "a turn spawned `Fresh` on the standing thread would start from \
             nothing, and the turn after every compaction is exactly that turn"
        );
    }

    /// `hand_to_assistant` returns when the instruction has been *taken*, and
    /// there is nothing in it that could wait for a run to say anything.
    ///
    /// **Asserted against the source, because there is no type that holds it.**
    /// The property is the absence of an await on output, and a test that proved
    /// it by running would have to start a real harness process and then prove a
    /// negative about how long it did not take. What can be checked exactly is
    /// that the function never reaches for the two mechanisms in this file that
    /// wait for a run's output — subscribing to the event bus, and the loop that
    /// drains it — which is how `start_titler` deliberately does not wait
    /// either.
    ///
    /// This is the property the whole design exists for. Main calls this inside
    /// its own turn, and its turn is the console: an await on output added here
    /// would put a model call back in front of every instruction Reljod types,
    /// and nothing else in the codebase would notice.
    #[test]
    fn handing_an_instruction_to_the_assistant_never_waits_for_a_run() {
        let source = include_str!("orchestrator.rs");
        let body = body_of(source, "pub async fn hand_to_assistant(");
        for waiting in ["titler_output(", ".recv(", "while let"] {
            assert!(
                !body.contains(waiting),
                "`hand_to_assistant` contains `{waiting}`, which is how a function \
                 in this file waits for a run to say something. It must return \
                 when the instruction has been taken and not when a run has \
                 answered."
            );
        }

        // The summariser *does* collect a run's output — that is what a summary
        // is — so the property here is not that it never waits but that it never
        // waits on the caller's thread. The collector has to sit inside the
        // detached task, which is exactly what `start_titler` does and for the
        // same reason: main's turn is the console.
        let body = body_of(source, "async fn start_assistant_compaction(");
        let detached = body
            .find("tokio::spawn(")
            .expect("the summariser's output has to be collected off the caller's thread");
        let collected = body
            .find("titler_output(")
            .expect("something has to read what the summariser said");
        assert!(
            collected > detached,
            "`start_assistant_compaction` waits for the summariser before it \
             returns, which puts a model call in front of every instruction \
             Reljod types"
        );
    }

    /// One function's source, from its signature to whatever is declared next.
    ///
    /// The two checks above both read the file they are in, and both want the
    /// same slice. `\n/// ` is the boundary because every item at the top level
    /// of this file opens with a doc comment.
    fn body_of<'a>(source: &'a str, signature: &str) -> &'a str {
        let start = source
            .find(signature)
            .expect("this function is in this file");
        let body = &source[start..];
        let end = body[1..]
            .find("\n/// ")
            .map(|at| at + 1)
            .unwrap_or(body.len());
        &body[..end]
    }
}


