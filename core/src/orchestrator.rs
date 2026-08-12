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
use crate::harness::{HarnessKind, PermissionPolicy, SpawnRequest, ToolAccess};
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
