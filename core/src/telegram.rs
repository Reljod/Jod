//! Telegram: the transport that puts Jod on a phone.
//!
//! This is the most exposed surface Jod has. A message that reaches [`Bridge`]
//! becomes a prompt to an agent harness that can run a shell, so everything
//! here is written from the assumption that the next `getUpdates` will return
//! a message from someone hostile.
//!
//! # Transport: long polling, not a webhook
//!
//! Telegram offers both. A webhook is fewer moving parts *if* you already have
//! what it needs, and Jod does not: `setWebhook` requires a public HTTPS URL
//! with a certificate Telegram will accept, and Jod's box is a personal VPS
//! behind NAT with no certificate and no reverse proxy in front of it. Getting
//! a webhook working means acquiring a domain, a certificate, a renewal cron
//! and an inbound port — four new failure modes, each of which fails *silently*
//! by simply never delivering a message.
//!
//! `getUpdates` needs an outbound TCP connection and nothing else, which is
//! exactly what a NATed box has. The costs are real and handled here:
//!
//! - **Only one poller may run per token.** A second one makes Telegram return
//!   409 `Conflict` to *both*, so the failure is mutual rather than
//!   last-writer-wins. [`BotError::Conflict`] is therefore fatal: the loop
//!   stops rather than fighting for the token, because a poller that retries
//!   through a conflict keeps the other one broken too.
//! - **The offset is the acknowledgement.** Telegram holds an update until a
//!   later `getUpdates` asks for `offset > update_id`; see [`next_offset`].
//!   Advancing the offset before the work is done means a crash loses the
//!   message, and advancing it after means a crash re-runs it. Jod advances
//!   *before* dispatch, on purpose: re-running an unbounded agent turn on a
//!   restart is worse than dropping it, and the person who sent it is sitting
//!   in the chat and can send it again.
//!
//! # Crate: raw JSON over `reqwest`
//!
//! Jod sends three Bot API methods — `getUpdates`, `sendMessage`,
//! `editMessageText` — and reads four object shapes. `teloxide` is the obvious
//! choice and the wrong one here: it brings a dispatcher, a dialogue state
//! machine, `dptree` and derive macros to build a bot framework, and Jod
//! already *has* the framework ([`crate::service::Jod`]). `frankenstein` is
//! lighter but still generates the whole API surface. Both wrap the same
//! `reqwest` call this module makes directly, and Jod ships as a single static
//! binary onto a VPS, where every transitive crate is weight in the image and
//! another thing to audit. `rustls` rather than the system OpenSSL for the same
//! reason.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{JodError, Result};
use crate::event::AgentEvent;
use crate::harness::{HarnessKind, PermissionPolicy, Resume, SpawnRequest};
use crate::service::Jod;

// ---------------------------------------------------------------------------
// Length
// ---------------------------------------------------------------------------

/// Telegram's per-message ceiling, in UTF-16 code units.
///
/// The unit matters more than the number. Telegram counts UTF-16 code units,
/// not Unicode scalar values, so every character outside the BMP — emoji, most
/// notably — costs **two**. A 3,000-emoji reply is legal by Rust `char` count
/// and rejected by the API, and the failure lands on exactly the messages a
/// person is most likely to send from a phone.
pub const MAX_MESSAGE_UTF16: usize = 4096;

/// Room kept in every chunk for the `\n```` ` ``` ` ` that closes a code fence a
/// split had to interrupt.
const FENCE_CLOSE_COST: usize = 4;

/// Length of `s` as Telegram measures it.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Cut `s` into pieces of at most `max` UTF-16 units.
///
/// Two invariants beyond the length. The cut lands on a `char` boundary, so a
/// surrogate pair is never severed into two half-emoji. And a piece never ends
/// on an unpaired backslash: after [`escape_markdown_v2`] the text is full of
/// `\.` pairs, and a cut between the two turns a following chunk's first
/// character into a live formatting character.
fn hard_split(s: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    if utf16_len(s) <= max {
        return vec![s.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut len = 0usize;
    for ch in s.chars() {
        let w = ch.len_utf16();
        if len + w > max {
            let mut done = std::mem::take(&mut cur);
            let mut carry = String::new();
            if ends_with_lone_backslash(&done) {
                done.pop();
                carry.push('\\');
            }
            out.push(done);
            len = utf16_len(&carry);
            cur = carry;
        }
        cur.push(ch);
        len += w;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Whether `s` ends in a backslash that is itself escaping something — an odd
/// run of trailing backslashes.
fn ends_with_lone_backslash(s: &str) -> bool {
    s.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1
}

/// Split a reply into messages Telegram will accept, at the default ceiling.
pub fn split_for_telegram(text: &str) -> Vec<String> {
    split_at_utf16_limit(text, MAX_MESSAGE_UTF16)
}

/// Split `text` into pieces of at most `limit` UTF-16 units, preferring line
/// boundaries and never leaving a code fence open.
///
/// A split inside a fence is the one that actually hurts: the first message
/// renders as an unterminated code block and the second renders the agent's
/// code as prose, with underscores and asterisks eating characters. When a
/// chunk has to end mid-fence this closes the fence and reopens it — with its
/// original language tag — at the top of the next chunk.
pub fn split_at_utf16_limit(text: &str, limit: usize) -> Vec<String> {
    if utf16_len(text) <= limit {
        return vec![text.to_string()];
    }
    let mut s = Splitter {
        limit: limit.max(FENCE_CLOSE_COST + 2),
        out: Vec::new(),
        chunk: String::new(),
        len: 0,
        open_fence: None,
    };
    for line in text.split('\n') {
        s.push_line(line);
    }
    s.finish()
}

struct Splitter {
    limit: usize,
    out: Vec<String>,
    chunk: String,
    len: usize,
    /// The verbatim opening fence line, kept so a reopened block keeps its
    /// language and still highlights.
    open_fence: Option<String>,
}

impl Splitter {
    /// How much of `limit` a chunk's own text may use, after reserving room to
    /// close a fence this chunk had to interrupt.
    fn budget(&self) -> usize {
        if self.open_fence.is_some() {
            self.limit - FENCE_CLOSE_COST
        } else {
            self.limit
        }
    }

    fn push_line(&mut self, line: &str) {
        let reopen = self.open_fence.as_deref().map(utf16_len).unwrap_or(0);
        let room = self
            .limit
            .saturating_sub(FENCE_CLOSE_COST + reopen + 1)
            .max(1);
        for piece in hard_split(line, room) {
            self.push_piece(&piece);
        }
        if is_fence(line) {
            self.open_fence = match self.open_fence {
                Some(_) => None,
                None => Some(line.to_string()),
            };
        }
    }

    fn push_piece(&mut self, piece: &str) {
        let sep = usize::from(!self.chunk.is_empty());
        if self.len + sep + utf16_len(piece) > self.budget() {
            self.flush(true);
        }
        if !self.chunk.is_empty() {
            self.chunk.push('\n');
            self.len += 1;
        }
        self.chunk.push_str(piece);
        self.len += utf16_len(piece);
    }

    fn flush(&mut self, reopen: bool) {
        if self.chunk.is_empty() {
            return;
        }
        let mut done = std::mem::take(&mut self.chunk);
        self.len = 0;
        if self.open_fence.is_some() {
            done.push_str("\n```");
        }
        self.out.push(done);
        if reopen {
            if let Some(fence) = self.open_fence.clone() {
                self.len = utf16_len(&fence);
                self.chunk = fence;
            }
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.flush(false);
        self.out
    }
}

fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// The characters MarkdownV2 reserves outside of any entity.
pub const MARKDOWN_V2_SPECIALS: [char; 18] = [
    '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
];

/// The exact MarkdownV2 escape rule, as the Bot API states it: **in all places,
/// the characters `_ * [ ] ( ) ~ ` > # + - = | { } . !` must be preceded by
/// `\`.** Every one of the eighteen, unconditionally, whether or not it looks
/// like it could start an entity — a bare `.` at the end of a sentence is a
/// 400.
///
/// Note what the rule does *not* say: `\` itself is not on the list, so a
/// backslash already in the text stays unescaped and Telegram reads it as an
/// escape for whatever follows. There is no way to express a literal backslash
/// under this rule, which is one of the reasons [`Formatting::Plain`] is the
/// default.
pub fn escape_markdown_v2(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for ch in text.chars() {
        if MARKDOWN_V2_SPECIALS.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// How a message body is rendered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Formatting {
    /// No `parse_mode`. **The default, deliberately.**
    ///
    /// An agent's reply is arbitrary text it was never told to make
    /// Telegram-safe, and MarkdownV2's failure mode is not "renders plainly" —
    /// it is a 400 that drops the whole message, so the person watching their
    /// phone sees nothing at all. Plain text always arrives. Markdown is worth
    /// it only where the formatting is Jod's own and known-good.
    #[default]
    Plain,
    /// `parse_mode=MarkdownV2`, with [`escape_markdown_v2`] applied to the body.
    MarkdownV2,
}

impl Formatting {
    pub fn parse_mode(&self) -> Option<&'static str> {
        match self {
            Formatting::Plain => None,
            Formatting::MarkdownV2 => Some("MarkdownV2"),
        }
    }

    /// The body as it goes on the wire. Escaping happens before splitting, so
    /// the 4096 ceiling is measured against what Telegram actually receives.
    pub fn render(&self, text: &str) -> String {
        match self {
            Formatting::Plain => text.to_string(),
            Formatting::MarkdownV2 => escape_markdown_v2(text),
        }
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// The kinds of chat a message can arrive in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatKind {
    Private,
    Group,
    Supergroup,
    Channel,
    /// A `type` Telegram added after this was written. Treated as a group, and
    /// therefore per-user rather than shared — an unknown chat kind should
    /// isolate histories, not merge them.
    Unknown,
}

impl ChatKind {
    pub fn parse(s: &str) -> ChatKind {
        match s {
            "private" => ChatKind::Private,
            "group" => ChatKind::Group,
            "supergroup" => ChatKind::Supergroup,
            "channel" => ChatKind::Channel,
            _ => ChatKind::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ChatKind::Private => "private",
            ChatKind::Group => "group",
            ChatKind::Supergroup => "supergroup",
            ChatKind::Channel => "channel",
            ChatKind::Unknown => "unknown",
        }
    }
}

/// The conversation a message belongs to, derived from the message alone.
///
/// Three rules, and the first is the one that matters. **When an update carries
/// no chat id, the sender's id stands in for it.** Defaulting to a constant
/// instead — or to the chat kind alone — collapses every user into one shared
/// session, and the symptom is not a crash but one person's history being read
/// back to another. The other two follow Telegram's own grain: a thread is a
/// place several people are talking about one thing, so it is shared; a group
/// without threads is several conversations interleaved in one room, so it is
/// per-user.
pub fn session_key(
    chat_id: Option<i64>,
    kind: ChatKind,
    thread_id: Option<i64>,
    user_id: i64,
) -> String {
    let chat = chat_id.unwrap_or(user_id);
    match (kind, thread_id) {
        (ChatKind::Private, _) => format!("telegram:private:{chat}:{user_id}"),
        (k, Some(thread)) => format!("telegram:{}:{chat}:t{thread}", k.as_str()),
        (k, None) => format!("telegram:{}:{chat}:u{user_id}", k.as_str()),
    }
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

/// What the allowlist decided about a sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Allowed,
    Refused,
}

/// The set of Telegram user ids allowed to drive Jod.
///
/// Default-deny with no way out: an empty allowlist refuses everyone, and there
/// is no "allow all" switch. Hermes ships one and documents it as "NOT
/// recommended for bots with terminal access", which is the whole story — the
/// flag exists, so somebody sets it, and then the bot is a public shell.
#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    ids: HashSet<i64>,
}

impl Allowlist {
    pub fn new(ids: impl IntoIterator<Item = i64>) -> Allowlist {
        Allowlist {
            ids: ids.into_iter().collect(),
        }
    }

    /// Read a comma- or whitespace-separated list of numeric ids.
    ///
    /// A malformed entry is an error rather than a silent skip: dropping one
    /// unparseable id from an allowlist locks its owner out with no signal,
    /// and dropping one from a *deny* reading would do the opposite.
    pub fn parse(text: &str) -> Result<Allowlist> {
        let mut ids = HashSet::new();
        for token in text.split([',', ' ', '\t', '\n']).filter(|t| !t.is_empty()) {
            let id = token.parse::<i64>().map_err(|_| {
                JodError::Invalid(format!("`{token}` is not a Telegram user id"))
            })?;
            ids.insert(id);
        }
        Ok(Allowlist { ids })
    }

    pub fn decide(&self, user_id: i64) -> Access {
        if self.ids.contains(&user_id) {
            Access::Allowed
        } else {
            Access::Refused
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Wire types — only the fields Jod reads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TgMessage>,
    #[serde(default)]
    pub edited_message: Option<TgMessage>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TgMessage {
    #[serde(default)]
    pub message_id: i64,
    #[serde(default)]
    pub from: Option<TgUser>,
    #[serde(default)]
    pub chat: Option<TgChat>,
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TgUser {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TgChat {
    pub id: i64,
    #[serde(rename = "type", default)]
    pub kind: String,
}

/// Telegram's envelope. `ok` decides which half of it is populated.
///
/// The bound is spelled out because `#[serde(default)]` on `result` would
/// otherwise make the derive demand `T: Default` — a requirement the wire
/// format does not have.
#[derive(Debug, Clone, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct ApiResponse<T> {
    pub ok: bool,
    #[serde(default)]
    pub result: Option<T>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub error_code: Option<i32>,
    #[serde(default)]
    pub parameters: Option<ResponseParameters>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResponseParameters {
    #[serde(default)]
    pub retry_after: Option<u64>,
    #[serde(default)]
    pub migrate_to_chat_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Why an update was dropped without a decision about its sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreReason {
    /// Not a message at all — a callback query, a poll answer, a chat member
    /// change.
    NotAMessage,
    /// A message with no `from`: a channel post, signed by the channel.
    NoSender,
    /// Another bot. Answering one is how two bots talk to each other forever.
    FromBot,
    /// A photo, a sticker, a location — nothing Jod can turn into a prompt.
    NoText,
}

/// A message from someone not on the allowlist.
///
/// Kept rather than discarded, because "nobody has ever tried" and "somebody
/// tries every hour" are very different facts about a bot that can reach a
/// shell — and only one of them is visible if refusals leave no trace. The
/// preview is truncated: an audit trail should not become a place to store an
/// attacker's payload in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub update_id: i64,
    pub user_id: i64,
    pub username: Option<String>,
    pub chat_id: i64,
    pub preview: String,
    pub at_ms: i64,
}

/// How far Jod is willing to admit one update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    Ignored {
        update_id: i64,
        reason: IgnoreReason,
    },
    Refused(Refusal),
    Handle(IncomingMessage),
}

/// An update that survived every check, ready to become a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    pub update_id: i64,
    pub message_id: i64,
    pub chat_id: i64,
    pub user_id: i64,
    pub thread_id: Option<i64>,
    pub kind: ChatKind,
    pub text: String,
    /// The [`session_key`] this message belongs to.
    pub session: String,
}

/// The longest refused text kept in a [`Refusal`].
const PREVIEW_CHARS: usize = 96;

/// Decide what to do with one update, without touching anything.
///
/// The order is load-bearing: the shape checks come first so a malformed
/// update can never reach the allowlist and be *refused* — which would file a
/// refusal against user id 0 — and the allowlist comes before any parsing of
/// the text, so an unauthorised sender's content is never interpreted.
pub fn classify(update: &Update, allow: &Allowlist) -> Inbound {
    let msg = match update.message.as_ref().or(update.edited_message.as_ref()) {
        Some(m) => m,
        None => {
            return Inbound::Ignored {
                update_id: update.update_id,
                reason: IgnoreReason::NotAMessage,
            }
        }
    };
    let from = match msg.from.as_ref() {
        Some(u) => u,
        None => {
            return Inbound::Ignored {
                update_id: update.update_id,
                reason: IgnoreReason::NoSender,
            }
        }
    };
    if from.is_bot {
        return Inbound::Ignored {
            update_id: update.update_id,
            reason: IgnoreReason::FromBot,
        };
    }
    let chat_id = msg.chat.as_ref().map(|c| c.id);
    if allow.decide(from.id) == Access::Refused {
        let raw = msg.text.clone().unwrap_or_default();
        return Inbound::Refused(Refusal {
            update_id: update.update_id,
            user_id: from.id,
            username: from.username.clone(),
            chat_id: chat_id.unwrap_or(from.id),
            preview: raw.chars().take(PREVIEW_CHARS).collect(),
            at_ms: chrono::Utc::now().timestamp_millis(),
        });
    }
    let text = match msg.text.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => {
            return Inbound::Ignored {
                update_id: update.update_id,
                reason: IgnoreReason::NoText,
            }
        }
    };
    let kind = msg
        .chat
        .as_ref()
        .map(|c| ChatKind::parse(&c.kind))
        .unwrap_or(ChatKind::Private);
    Inbound::Handle(IncomingMessage {
        update_id: update.update_id,
        message_id: msg.message_id,
        chat_id: chat_id.unwrap_or(from.id),
        user_id: from.id,
        thread_id: msg.message_thread_id,
        kind,
        text,
        session: session_key(chat_id, kind, msg.message_thread_id, from.id),
    })
}

/// The `offset` for the next `getUpdates`.
///
/// Telegram redelivers every update until one is acknowledged, and the only
/// acknowledgement is asking for a higher offset. `max(update_id) + 1` — not
/// `last + 1`, because the array's order is not guaranteed and one
/// out-of-order response would rewind the offset and replay the batch forever.
/// An empty batch leaves the offset alone; a long poll that times out with
/// nothing must not look like an acknowledgement of anything.
pub fn next_offset(current: Option<i64>, updates: &[Update]) -> Option<i64> {
    let highest = updates.iter().map(|u| u.update_id).max();
    match (current, highest) {
        (cur, None) => cur,
        (None, Some(h)) => Some(h + 1),
        (Some(cur), Some(h)) => Some(cur.max(h + 1)),
    }
}

// ---------------------------------------------------------------------------
// Errors and rate limits
// ---------------------------------------------------------------------------

pub type BotResult<T> = std::result::Result<T, BotError>;

#[derive(Debug, Clone, thiserror::Error)]
pub enum BotError {
    /// Telegram told us to slow down. `retry_after` is its instruction in
    /// seconds, and it is not a suggestion.
    #[error("rate limited by Telegram ({description})")]
    RateLimited {
        retry_after: Option<u64>,
        description: String,
    },
    /// 409 — another `getUpdates` is live on this token. Fatal by design; see
    /// the module docs.
    #[error("another getUpdates poller holds this token: {0}")]
    Conflict(String),
    /// 401/403 — a bad token, or the bot was blocked or removed. Retrying
    /// cannot fix either.
    #[error("Telegram refused the token or the chat: {0}")]
    Unauthorized(String),
    #[error("Telegram API error {code}: {description}")]
    Api { code: i32, description: String },
    #[error("could not reach Telegram: {0}")]
    Transport(String),
}

impl BotError {
    /// Whether waiting and trying again could plausibly work.
    pub fn is_retryable(&self) -> bool {
        matches!(self, BotError::RateLimited { .. } | BotError::Transport(_))
    }
}

/// Turn Telegram's `ok: false` envelope into an error that says what to do.
///
/// 429 is the one worth separating: it is the only code that carries an
/// instruction, and treating it as a generic failure is how a bot gets its
/// token throttled for an hour instead of a minute.
pub fn classify_api_error(
    code: i32,
    description: &str,
    parameters: Option<&ResponseParameters>,
) -> BotError {
    let retry_after = parameters.and_then(|p| p.retry_after);
    match code {
        429 => BotError::RateLimited {
            retry_after,
            description: description.to_string(),
        },
        409 => BotError::Conflict(description.to_string()),
        401 | 403 => BotError::Unauthorized(description.to_string()),
        _ if retry_after.is_some() => BotError::RateLimited {
            retry_after,
            description: description.to_string(),
        },
        _ => BotError::Api {
            code,
            description: description.to_string(),
        },
    }
}

/// The longest Jod will back off on its own initiative.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// How long to wait before attempt `attempt + 1`.
///
/// When Telegram supplies `retry_after` it is used **verbatim and uncapped**.
/// Capping it would mean retrying before the flood wait expires, which restarts
/// the wait — a cap here makes the outage longer, not shorter. Only Jod's own
/// guess, used when the server said nothing, is exponential and bounded.
pub fn backoff(attempt: u32, retry_after: Option<u64>) -> Duration {
    match retry_after {
        Some(secs) => Duration::from_secs(secs),
        None => {
            let secs = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
            Duration::from_secs(secs).min(MAX_BACKOFF)
        }
    }
}

// ---------------------------------------------------------------------------
// Long-run feedback
// ---------------------------------------------------------------------------

/// The text of the single edit-in-place bubble a long run gets.
///
/// One message, edited, rather than a stream of updates: a phone turns each new
/// message into a notification, so per-tool breadcrumbs would buzz a pocket
/// forty times for one run. A heartbeat that is silently rewritten in place
/// answers the only question being asked — is it still alive — without that.
pub fn progress_text(elapsed: Duration) -> String {
    let mins = elapsed.as_secs() / 60;
    match mins {
        0 => "⏳ Working — under a minute".to_string(),
        1 => "⏳ Working — 1 min".to_string(),
        n => format!("⏳ Working — {n} min"),
    }
}

/// The completion notice that replaces the bubble.
pub fn completion_text(ok: bool, elapsed: Duration) -> String {
    let mins = elapsed.as_secs() / 60;
    let mark = if ok { "✅ Done" } else { "❌ Failed" };
    match mins {
        0 => format!("{mark} — under a minute"),
        1 => format!("{mark} — 1 min"),
        n => format!("{mark} — {n} min"),
    }
}

/// The minute count to show, or `None` when the bubble already says the right
/// thing.
///
/// `editMessageText` with unchanged text is a 400 (`message is not modified`)
/// *and* it spends rate-limit budget, so the check is not just tidiness.
pub fn progress_due(shown: Option<u64>, elapsed: Duration) -> Option<u64> {
    let mins = elapsed.as_secs() / 60;
    match shown {
        Some(prev) if prev == mins => None,
        _ => Some(mins),
    }
}

/// What the reply loop should do about one event from the run it is watching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing to say. Tool calls and thinking are deliberately silent here —
    /// Telegram is a mobile inbox, not a terminal.
    Quiet,
    /// The run ended. `text` is the final answer, when the harness gave one.
    Done { text: Option<String>, ok: bool },
}

pub fn step_for(event: &AgentEvent) -> Step {
    match event {
        AgentEvent::Finished { text, is_error, .. } => Step::Done {
            text: text.clone(),
            ok: !is_error,
        },
        AgentEvent::Error { message } => Step::Done {
            text: Some(message.clone()),
            ok: false,
        },
        _ => Step::Quiet,
    }
}

// ---------------------------------------------------------------------------
// The transport seam
// ---------------------------------------------------------------------------

/// The three Bot API calls Jod makes.
///
/// A trait rather than a concrete client so that everything above it — the
/// poll loop, chunked delivery, the progress bubble, rate-limit retry — is
/// exercised by tests with no token and no network. Futures are declared
/// `Send` explicitly, because a run is dispatched onto its own task while the
/// poller keeps polling.
pub trait BotApi: Send + Sync {
    fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_s: u64,
    ) -> impl Future<Output = BotResult<Vec<Update>>> + Send;

    fn send_message(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        text: &str,
        formatting: Formatting,
    ) -> impl Future<Output = BotResult<i64>> + Send;

    fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        formatting: Formatting,
    ) -> impl Future<Output = BotResult<()>> + Send;
}

/// How many times a send is retried before the message is given up on.
const MAX_SEND_ATTEMPTS: u32 = 4;

/// Send `text` as however many messages it takes, honouring flood waits.
///
/// Chunks go out in order and each is retried independently, so a flood wait
/// partway through a long reply resumes rather than restarting — a restart
/// would re-send the chunks that already arrived.
pub async fn deliver<B: BotApi>(
    bot: &B,
    chat_id: i64,
    thread_id: Option<i64>,
    text: &str,
    formatting: Formatting,
) -> BotResult<Vec<i64>> {
    let body = formatting.render(text);
    let mut ids = Vec::new();
    for chunk in split_for_telegram(&body) {
        ids.push(send_with_retry(bot, chat_id, thread_id, &chunk, formatting).await?);
    }
    Ok(ids)
}

/// One `sendMessage`, retried while Telegram says to wait.
///
/// The body is already rendered, so this passes [`Formatting::Plain`] on to the
/// wire call for the escaping and only uses `formatting` for `parse_mode`.
async fn send_with_retry<B: BotApi>(
    bot: &B,
    chat_id: i64,
    thread_id: Option<i64>,
    body: &str,
    formatting: Formatting,
) -> BotResult<i64> {
    let mut attempt = 0u32;
    loop {
        match bot.send_message(chat_id, thread_id, body, formatting).await {
            Ok(id) => return Ok(id),
            Err(e) if e.is_retryable() && attempt + 1 < MAX_SEND_ATTEMPTS => {
                let retry_after = match &e {
                    BotError::RateLimited { retry_after, .. } => *retry_after,
                    _ => None,
                };
                tokio::time::sleep(backoff(attempt, retry_after)).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// The real client
// ---------------------------------------------------------------------------

/// Seconds Telegram holds a `getUpdates` open with nothing to say.
pub const POLL_TIMEOUT_S: u64 = 50;

/// The Bot API over HTTPS.
///
/// Untested here on purpose: exercising it needs a live bot token, and the
/// charter forbids inventing one or standing up a fake that would turn a green
/// suite into a claim about code that was never run. Everything this type does
/// beyond the HTTP call itself lives in the pure functions above, which are
/// tested.
///
/// **This is the only thing in Jod that needs an HTTPS client**, and therefore
/// the only reason the tree contains a TLS stack at all — `reqwest` pulls
/// `rustls`, which pulls `aws-lc-rs`, which pulls a million lines of vendored C
/// in `aws-lc-sys`. That is a large price for one caller, so it is behind a
/// feature: `--no-default-features` builds a Jod with no TLS in it whatsoever.
///
/// The alternative considered and rejected was swapping the crypto provider for
/// `ring` via `rustls-no-provider`. It is smaller, but it requires installing a
/// default provider at startup, and forgetting that is a *runtime panic on the
/// first TLS call* — in a daemon meant to run unattended for weeks. Trading a
/// 3am panic for a shorter build is the wrong way round.
#[cfg(feature = "telegram")]
pub struct HttpBot {
    client: reqwest::Client,
    base: String,
}

#[cfg(feature = "telegram")]
impl HttpBot {
    pub fn new(token: &str) -> Result<HttpBot> {
        let client = reqwest::Client::builder()
            // Longer than the long poll, or every quiet minute looks like a
            // network failure.
            .timeout(Duration::from_secs(POLL_TIMEOUT_S + 30))
            .build()
            .map_err(|e| JodError::Invalid(format!("could not build an HTTPS client: {e}")))?;
        Ok(HttpBot {
            client,
            base: format!("https://api.telegram.org/bot{token}"),
        })
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: serde_json::Value,
    ) -> BotResult<T> {
        let resp = self
            .client
            .post(format!("{}/{method}", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| BotError::Transport(e.to_string()))?;
        // The status code alone is not enough: Telegram puts `retry_after`
        // in the body, and the body is present on every error.
        let parsed: ApiResponse<T> = resp
            .json()
            .await
            .map_err(|e| BotError::Transport(format!("unreadable response: {e}")))?;
        if !parsed.ok {
            return Err(classify_api_error(
                parsed.error_code.unwrap_or(0),
                parsed.description.as_deref().unwrap_or("no description"),
                parsed.parameters.as_ref(),
            ));
        }
        parsed
            .result
            .ok_or_else(|| BotError::Transport("ok response with no result".to_string()))
    }
}

#[cfg(feature = "telegram")]
impl BotApi for HttpBot {
    fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_s: u64,
    ) -> impl Future<Output = BotResult<Vec<Update>>> + Send {
        let mut body = serde_json::json!({
            "timeout": timeout_s,
            // Everything else — callbacks, reactions, member changes — is
            // noise Jod has no answer for, and asking for it costs bandwidth
            // on a poll that runs all day.
            "allowed_updates": ["message"],
        });
        if let Some(o) = offset {
            body["offset"] = serde_json::json!(o);
        }
        async move { self.call("getUpdates", body).await }
    }

    fn send_message(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        text: &str,
        formatting: Formatting,
    ) -> impl Future<Output = BotResult<i64>> + Send {
        let mut body = serde_json::json!({ "chat_id": chat_id, "text": text });
        if let Some(t) = thread_id {
            body["message_thread_id"] = serde_json::json!(t);
        }
        if let Some(mode) = formatting.parse_mode() {
            body["parse_mode"] = serde_json::json!(mode);
        }
        async move {
            let msg: TgMessage = self.call("sendMessage", body).await?;
            Ok(msg.message_id)
        }
    }

    fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        formatting: Formatting,
    ) -> impl Future<Output = BotResult<()>> + Send {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
        });
        if let Some(mode) = formatting.parse_mode() {
            body["parse_mode"] = serde_json::json!(mode);
        }
        async move {
            let _: serde_json::Value = self.call("editMessageText", body).await?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// The poller
// ---------------------------------------------------------------------------

/// The most refusals kept in memory before the oldest are dropped.
const REFUSAL_LOG_CAP: usize = 256;

/// One `getUpdates` loop: fetch, acknowledge, classify, record refusals.
///
/// Deliberately knows nothing about [`Jod`]. Dispatch belongs to the caller,
/// which is what lets a test drive a whole poll cycle — offset arithmetic,
/// default-deny, the refusal record — against a fake bot with no store, no
/// harness and no token.
pub struct Poller<B: BotApi> {
    bot: B,
    allow: Allowlist,
    timeout_s: u64,
    offset: Mutex<Option<i64>>,
    refusals: Mutex<Vec<Refusal>>,
}

impl<B: BotApi> Poller<B> {
    pub fn new(bot: B, allow: Allowlist) -> Poller<B> {
        Poller {
            bot,
            allow,
            timeout_s: POLL_TIMEOUT_S,
            offset: Mutex::new(None),
            refusals: Mutex::new(Vec::new()),
        }
    }

    pub fn bot(&self) -> &B {
        &self.bot
    }

    pub fn offset(&self) -> Option<i64> {
        *self.offset.lock().expect("offset lock")
    }

    /// Every refusal still in memory, oldest first.
    pub fn refusals(&self) -> Vec<Refusal> {
        self.refusals.lock().expect("refusal lock").clone()
    }

    /// One round trip. Advances the offset before returning, so the caller
    /// cannot forget to.
    pub async fn poll_once(&self) -> BotResult<Vec<Inbound>> {
        let offset = self.offset();
        let updates = self.bot.get_updates(offset, self.timeout_s).await?;
        *self.offset.lock().expect("offset lock") = next_offset(offset, &updates);

        let mut out = Vec::with_capacity(updates.len());
        for update in &updates {
            let verdict = classify(update, &self.allow);
            if let Inbound::Refused(r) = &verdict {
                self.record_refusal(r.clone());
            }
            out.push(verdict);
        }
        Ok(out)
    }

    /// A refused message is never answered — not even with "no". A reply
    /// confirms the bot exists and that this id is worth trying again.
    fn record_refusal(&self, refusal: Refusal) {
        eprintln!(
            "[jod/telegram] refused user {} ({}) in chat {}",
            refusal.user_id,
            refusal.username.as_deref().unwrap_or("no username"),
            refusal.chat_id,
        );
        let mut log = self.refusals.lock().expect("refusal lock");
        if log.len() >= REFUSAL_LOG_CAP {
            log.remove(0);
        }
        log.push(refusal);
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Everything the bridge needs that is not code.
#[derive(Debug, Clone)]
pub struct Config {
    pub token: String,
    pub allow: Allowlist,
    /// Where an agent launched from a chat runs.
    pub cwd: PathBuf,
    pub harness: HarnessKind,
    /// How much a chat-launched agent may do without asking. Left at
    /// [`PermissionPolicy::Ask`]: nobody is watching a phone-launched run
    /// closely enough to catch a `Bypass` going wrong.
    pub permission: PermissionPolicy,
}

impl Config {
    /// Build a config from raw values, so the parsing has a test that does not
    /// have to mutate the process environment.
    pub fn from_parts(token: Option<String>, allowed: Option<String>, cwd: PathBuf) -> Result<Config> {
        let token = token
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| JodError::Invalid("JOD_TELEGRAM_TOKEN is not set".to_string()))?;
        let allow = match allowed.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => Allowlist::parse(s)?,
            None => Allowlist::default(),
        };
        if allow.is_empty() {
            // Refused at startup rather than at the first message: a bot that
            // silently answers nobody looks exactly like a bot with a bad
            // token, and the owner debugs the wrong thing for an hour.
            return Err(JodError::Invalid(
                "JOD_TELEGRAM_ALLOWED_USERS is empty, so the bot would refuse everyone".to_string(),
            ));
        }
        Ok(Config {
            token,
            allow,
            cwd,
            harness: HarnessKind::ClaudeCode,
            permission: PermissionPolicy::Ask,
        })
    }

    pub fn from_env() -> Result<Config> {
        Config::from_parts(
            std::env::var("JOD_TELEGRAM_TOKEN").ok(),
            std::env::var("JOD_TELEGRAM_ALLOWED_USERS").ok(),
            crate::service::default_cwd(),
        )
    }
}

// ---------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------

/// How often the progress bubble is reconsidered while a run is in flight.
const PROGRESS_TICK: Duration = Duration::from_secs(20);

/// Telegram in, [`Jod`] out.
///
/// One instance owns one bot token and therefore one poll loop; see the module
/// docs on why a second is fatal rather than merely wasteful.
pub struct Bridge<B: BotApi> {
    poller: Poller<B>,
    jod: Arc<Jod>,
    cwd: PathBuf,
    harness: HarnessKind,
    permission: PermissionPolicy,
    /// Session key → the harness session id to resume next time.
    ///
    /// **In memory only.** Persisting it needs a migration and a `store.rs`
    /// this change does not own, so a restart currently starts every chat
    /// fresh. The mapping is deterministic ([`session_key`]), so making it
    /// durable later is a table, not a redesign.
    sessions: Mutex<HashMap<String, String>>,
}

impl<B: BotApi + 'static> Bridge<B> {
    pub fn new(bot: B, jod: Arc<Jod>, config: &Config) -> Arc<Bridge<B>> {
        Arc::new(Bridge {
            poller: Poller::new(bot, config.allow.clone()),
            jod,
            cwd: config.cwd.clone(),
            harness: config.harness,
            permission: config.permission,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn poller(&self) -> &Poller<B> {
        &self.poller
    }

    /// Poll until something fatal happens.
    ///
    /// Retryable failures back off and continue; a conflict or a bad token
    /// stops the loop, because neither one gets better by asking again.
    pub async fn run(self: Arc<Self>) -> BotResult<()> {
        let mut attempt = 0u32;
        loop {
            let batch = match self.poller.poll_once().await {
                Ok(b) => {
                    attempt = 0;
                    b
                }
                Err(e) if e.is_retryable() && attempt + 1 < MAX_SEND_ATTEMPTS => {
                    let retry_after = match &e {
                        BotError::RateLimited { retry_after, .. } => *retry_after,
                        _ => None,
                    };
                    eprintln!("[jod/telegram] poll failed, retrying: {e}");
                    tokio::time::sleep(backoff(attempt, retry_after)).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            };
            for item in batch {
                if let Inbound::Handle(msg) = item {
                    let me = Arc::clone(&self);
                    // Its own task: a run can take half an hour, and the poll
                    // loop has to keep acknowledging updates throughout or
                    // Telegram redelivers the whole backlog.
                    tokio::spawn(async move {
                        if let Err(e) = me.handle(msg).await {
                            eprintln!("[jod/telegram] handling failed: {e}");
                        }
                    });
                }
            }
        }
    }

    /// Turn one message into a run, and report that run back into the chat.
    ///
    /// Untested: it needs a `Jod` with a store and an installed harness
    /// binary. The decisions it makes are all delegated to functions that are
    /// tested — [`step_for`], [`progress_due`], [`progress_text`],
    /// [`deliver`], [`session_key`].
    pub async fn handle(self: Arc<Self>, msg: IncomingMessage) -> BotResult<()> {
        // Subscribe before spawning, or a run that finishes quickly finishes
        // into a channel nobody is listening to.
        let mut events = self.jod.subscribe();

        let resume = match self.sessions.lock().expect("session lock").get(&msg.session) {
            Some(id) => Resume::Session(id.clone()),
            None => Resume::Fresh,
        };
        let request = SpawnRequest {
            name: msg.session.clone(),
            harness: self.harness,
            prompt: msg.text.clone(),
            cwd: self.cwd.clone(),
            model: None,
            permission: self.permission,
            resume,
        };
        let agent = match self.jod.spawn_agent(request).await {
            Ok(a) => a,
            Err(e) => {
                deliver(
                    self.poller.bot(),
                    msg.chat_id,
                    msg.thread_id,
                    &format!("❌ Could not start: {e}"),
                    Formatting::Plain,
                )
                .await?;
                return Ok(());
            }
        };

        let started = Instant::now();
        let bubble = self
            .poller
            .bot()
            .send_message(
                msg.chat_id,
                msg.thread_id,
                &progress_text(Duration::ZERO),
                Formatting::Plain,
            )
            .await
            .ok();
        let mut shown = Some(0u64);

        let outcome = loop {
            let tick = tokio::time::sleep(PROGRESS_TICK);
            tokio::pin!(tick);
            tokio::select! {
                received = events.recv() => match received {
                    Ok(envelope) => {
                        if envelope.agent_id != agent.id {
                            continue;
                        }
                        if let AgentEvent::Started { session_id: Some(sid), .. } = &envelope.event {
                            self.sessions
                                .lock()
                                .expect("session lock")
                                .insert(msg.session.clone(), sid.clone());
                        }
                        match step_for(&envelope.event) {
                            Step::Quiet => continue,
                            Step::Done { text, ok } => break Some((text, ok)),
                        }
                    }
                    // A lagging receiver has missed events, not the run. Keep
                    // watching rather than reporting a failure that did not
                    // happen.
                    Err(crate::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(crate::broadcast::error::RecvError::Closed) => break None,
                },
                _ = &mut tick => {
                    if let (Some(id), Some(mins)) = (bubble, progress_due(shown, started.elapsed())) {
                        let text = progress_text(started.elapsed());
                        // A failed heartbeat edit is not worth failing the run
                        // over; the answer still arrives.
                        if self
                            .poller
                            .bot()
                            .edit_message_text(msg.chat_id, id, &text, Formatting::Plain)
                            .await
                            .is_ok()
                        {
                            shown = Some(mins);
                        }
                    }
                }
            }
        };

        let (text, ok) = outcome.unwrap_or((None, false));
        if let Some(id) = bubble {
            let _ = self
                .poller
                .bot()
                .edit_message_text(
                    msg.chat_id,
                    id,
                    &completion_text(ok, started.elapsed()),
                    Formatting::Plain,
                )
                .await;
        }
        if let Some(answer) = text.filter(|t| !t.trim().is_empty()) {
            deliver(
                self.poller.bot(),
                msg.chat_id,
                msg.thread_id,
                &answer,
                Formatting::Plain,
            )
            .await?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- fake transport ----------------------------------------------------

    #[derive(Default)]
    struct Sent {
        chat_id: i64,
        text: String,
    }

    /// A [`BotApi`] that never touches a socket. Scripted responses go in,
    /// everything that was asked of it comes out.
    #[derive(Default)]
    struct FakeBot {
        batches: Mutex<Vec<BotResult<Vec<Update>>>>,
        /// Errors to return from `send_message`, consumed one per call before
        /// the call is allowed to succeed.
        send_failures: Mutex<Vec<BotError>>,
        sent: Mutex<Vec<Sent>>,
        offsets: Mutex<Vec<Option<i64>>>,
        edits: Mutex<Vec<(i64, String)>>,
    }

    impl FakeBot {
        fn with_batches(batches: Vec<BotResult<Vec<Update>>>) -> FakeBot {
            FakeBot {
                batches: Mutex::new(batches),
                ..FakeBot::default()
            }
        }

        fn failing_sends(failures: Vec<BotError>) -> FakeBot {
            FakeBot {
                send_failures: Mutex::new(failures),
                ..FakeBot::default()
            }
        }

        fn sent_texts(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.text.clone())
                .collect()
        }
    }

    impl BotApi for FakeBot {
        fn get_updates(
            &self,
            offset: Option<i64>,
            _timeout_s: u64,
        ) -> impl Future<Output = BotResult<Vec<Update>>> + Send {
            self.offsets.lock().unwrap().push(offset);
            let next = {
                let mut b = self.batches.lock().unwrap();
                if b.is_empty() {
                    Ok(Vec::new())
                } else {
                    b.remove(0)
                }
            };
            async move { next }
        }

        fn send_message(
            &self,
            chat_id: i64,
            _thread_id: Option<i64>,
            text: &str,
            _formatting: Formatting,
        ) -> impl Future<Output = BotResult<i64>> + Send {
            let failure = {
                let mut f = self.send_failures.lock().unwrap();
                if f.is_empty() {
                    None
                } else {
                    Some(f.remove(0))
                }
            };
            if failure.is_none() {
                self.sent.lock().unwrap().push(Sent {
                    chat_id,
                    text: text.to_string(),
                });
            }
            let id = self.sent.lock().unwrap().len() as i64;
            async move {
                match failure {
                    Some(e) => Err(e),
                    None => Ok(id),
                }
            }
        }

        fn edit_message_text(
            &self,
            _chat_id: i64,
            message_id: i64,
            text: &str,
            _formatting: Formatting,
        ) -> impl Future<Output = BotResult<()>> + Send {
            self.edits
                .lock()
                .unwrap()
                .push((message_id, text.to_string()));
            async move { Ok(()) }
        }
    }

    fn update(id: i64, user: i64, chat: i64, text: &str) -> Update {
        Update {
            update_id: id,
            message: Some(TgMessage {
                message_id: id * 10,
                from: Some(TgUser {
                    id: user,
                    is_bot: false,
                    username: Some(format!("u{user}")),
                }),
                chat: Some(TgChat {
                    id: chat,
                    kind: "private".to_string(),
                }),
                message_thread_id: None,
                text: Some(text.to_string()),
            }),
            edited_message: None,
        }
    }

    // -- length ------------------------------------------------------------

    #[test]
    fn a_message_of_exactly_4096_utf16_units_is_left_whole() {
        let text = "a".repeat(MAX_MESSAGE_UTF16);
        assert_eq!(utf16_len(&text), 4096);
        assert_eq!(split_for_telegram(&text), vec![text]);
    }

    #[test]
    fn one_unit_past_4096_becomes_two_messages() {
        let text = "a".repeat(MAX_MESSAGE_UTF16 + 1);
        let chunks = split_for_telegram(&text);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| utf16_len(c) <= MAX_MESSAGE_UTF16));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn an_emoji_reply_that_is_legal_by_char_count_is_still_split() {
        // 2,600 astral characters: well under 4096 `char`s, well over 4096
        // UTF-16 units. Counting `char`s here is the bug this test exists for.
        let text = "😀".repeat(2600);
        assert!(text.chars().count() < MAX_MESSAGE_UTF16);
        assert_eq!(utf16_len(&text), 5200);
        let chunks = split_for_telegram(&text);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| utf16_len(c) <= MAX_MESSAGE_UTF16));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn splitting_never_severs_a_surrogate_pair() {
        let text = "😀".repeat(2600);
        for chunk in split_for_telegram(&text) {
            assert!(chunk.chars().all(|c| c == '😀'), "a chunk lost a character");
        }
    }

    #[test]
    fn a_split_inside_a_code_fence_closes_and_reopens_it() {
        let body = (0..400)
            .map(|i| format!("let x{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = format!("here you go\n```rust\n{body}\n```\ndone");
        let chunks = split_at_utf16_limit(&text, 1200);
        assert!(chunks.len() > 2, "the fixture must actually split");
        for chunk in &chunks {
            let fences = chunk.lines().filter(|l| is_fence(l)).count();
            assert_eq!(fences % 2, 0, "a chunk left a fence open:\n{chunk}");
            assert!(utf16_len(chunk) <= 1200);
        }
        assert!(
            chunks[1].starts_with("```rust"),
            "a reopened fence must keep its language: {:?}",
            &chunks[1][..20.min(chunks[1].len())]
        );
    }

    #[test]
    fn text_with_no_fences_is_split_on_line_boundaries() {
        let text = (0..500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = split_at_utf16_limit(&text, 500);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.lines().all(|l| l.starts_with("line ")));
        }
    }

    #[test]
    fn a_split_never_orphans_a_markdown_escape_from_its_character() {
        let escaped = escape_markdown_v2(&".".repeat(3000));
        for chunk in split_for_telegram(&escaped) {
            assert!(
                !ends_with_lone_backslash(&chunk),
                "a chunk ended on a dangling escape"
            );
        }
    }

    // -- formatting --------------------------------------------------------

    #[test]
    fn every_markdown_v2_special_character_is_escaped() {
        for ch in MARKDOWN_V2_SPECIALS {
            let escaped = escape_markdown_v2(&format!("a{ch}b"));
            assert_eq!(escaped, format!("a\\{ch}b"), "{ch} was not escaped");
        }
    }

    #[test]
    fn escaping_the_whole_reserved_set_at_once_leaves_nothing_bare() {
        let raw: String = MARKDOWN_V2_SPECIALS.iter().collect();
        let escaped = escape_markdown_v2(&raw);
        let expected: String = MARKDOWN_V2_SPECIALS
            .iter()
            .map(|c| format!("\\{c}"))
            .collect();
        assert_eq!(escaped, expected);
    }

    #[test]
    fn ordinary_text_survives_escaping_unchanged() {
        assert_eq!(escape_markdown_v2("hello world 42"), "hello world 42");
    }

    #[test]
    fn plain_is_the_default_and_sends_no_parse_mode() {
        assert_eq!(Formatting::default(), Formatting::Plain);
        assert_eq!(Formatting::Plain.parse_mode(), None);
        assert_eq!(Formatting::Plain.render("a.b*c"), "a.b*c");
        assert_eq!(Formatting::MarkdownV2.parse_mode(), Some("MarkdownV2"));
        assert_eq!(Formatting::MarkdownV2.render("a.b"), "a\\.b");
    }

    // -- sessions ----------------------------------------------------------

    #[test]
    fn two_chats_get_two_sessions() {
        let a = session_key(Some(1), ChatKind::Private, None, 99);
        let b = session_key(Some(2), ChatKind::Private, None, 99);
        assert_ne!(a, b);
    }

    #[test]
    fn two_users_in_one_group_get_two_sessions() {
        let a = session_key(Some(-100), ChatKind::Group, None, 1);
        let b = session_key(Some(-100), ChatKind::Group, None, 2);
        assert_ne!(a, b);
    }

    #[test]
    fn a_missing_chat_id_falls_back_to_the_sender_not_a_shared_session() {
        let a = session_key(None, ChatKind::Private, None, 1);
        let b = session_key(None, ChatKind::Private, None, 2);
        assert_ne!(a, b, "two senders collapsed into one session");
        assert!(a.contains(":1:1"));
    }

    #[test]
    fn everyone_in_one_thread_shares_a_session() {
        let a = session_key(Some(-100), ChatKind::Supergroup, Some(7), 1);
        let b = session_key(Some(-100), ChatKind::Supergroup, Some(7), 2);
        assert_eq!(a, b);
    }

    #[test]
    fn two_threads_in_one_group_get_two_sessions() {
        let a = session_key(Some(-100), ChatKind::Supergroup, Some(7), 1);
        let b = session_key(Some(-100), ChatKind::Supergroup, Some(8), 1);
        assert_ne!(a, b);
    }

    #[test]
    fn an_unknown_chat_kind_isolates_rather_than_shares() {
        let a = session_key(Some(-100), ChatKind::Unknown, None, 1);
        let b = session_key(Some(-100), ChatKind::Unknown, None, 2);
        assert_ne!(a, b);
    }

    // -- auth --------------------------------------------------------------

    #[test]
    fn an_allowlisted_user_is_accepted_and_everyone_else_is_refused() {
        let allow = Allowlist::new([42]);
        assert_eq!(allow.decide(42), Access::Allowed);
        assert_eq!(allow.decide(43), Access::Refused);
        assert_eq!(allow.decide(0), Access::Refused);
        assert_eq!(allow.decide(-42), Access::Refused);
    }

    #[test]
    fn an_empty_allowlist_refuses_everyone() {
        let allow = Allowlist::default();
        assert!(allow.is_empty());
        assert_eq!(allow.decide(42), Access::Refused);
    }

    #[test]
    fn an_allowlist_reads_commas_and_whitespace_alike() {
        let allow = Allowlist::parse("1, 2\n3\t4").unwrap();
        for id in [1, 2, 3, 4] {
            assert_eq!(allow.decide(id), Access::Allowed);
        }
        assert_eq!(allow.decide(5), Access::Refused);
    }

    #[test]
    fn an_unparseable_allowlist_entry_is_refused_when_it_is_written() {
        assert!(Allowlist::parse("1, hunter2").is_err());
    }

    #[test]
    fn a_config_with_no_allowlist_will_not_start() {
        let err = Config::from_parts(Some("t".into()), None, PathBuf::from("/tmp"))
            .expect_err("an allowlist-less bot must not start");
        assert!(err.to_string().contains("refuse everyone"));
    }

    #[test]
    fn a_config_with_no_token_will_not_start() {
        assert!(Config::from_parts(None, Some("1".into()), PathBuf::from("/tmp")).is_err());
        assert!(Config::from_parts(Some("  ".into()), Some("1".into()), PathBuf::from("/tmp")).is_err());
    }

    // -- classification ----------------------------------------------------

    #[test]
    fn a_message_from_a_stranger_is_recorded_and_never_answered() {
        let allow = Allowlist::new([42]);
        match classify(&update(1, 999, 999, "rm -rf /"), &allow) {
            Inbound::Refused(r) => {
                assert_eq!(r.user_id, 999);
                assert_eq!(r.preview, "rm -rf /");
            }
            other => panic!("a stranger was not refused: {other:?}"),
        }
    }

    #[test]
    fn a_refusal_keeps_only_a_preview_of_what_was_sent() {
        let allow = Allowlist::new([42]);
        let long = "x".repeat(10_000);
        match classify(&update(1, 999, 999, &long), &allow) {
            Inbound::Refused(r) => assert_eq!(r.preview.chars().count(), PREVIEW_CHARS),
            other => panic!("expected a refusal: {other:?}"),
        }
    }

    #[test]
    fn a_message_from_an_allowlisted_user_carries_its_session_key() {
        let allow = Allowlist::new([42]);
        match classify(&update(1, 42, 7, " hello  "), &allow) {
            Inbound::Handle(m) => {
                assert_eq!(m.text, "hello");
                assert_eq!(m.user_id, 42);
                assert_eq!(m.chat_id, 7);
                assert_eq!(m.session, session_key(Some(7), ChatKind::Private, None, 42));
            }
            other => panic!("an allowlisted user was not handled: {other:?}"),
        }
    }

    #[test]
    fn another_bot_is_ignored_before_the_allowlist_is_consulted() {
        let mut u = update(1, 42, 7, "hi");
        u.message.as_mut().unwrap().from.as_mut().unwrap().is_bot = true;
        assert_eq!(
            classify(&u, &Allowlist::new([42])),
            Inbound::Ignored {
                update_id: 1,
                reason: IgnoreReason::FromBot
            }
        );
    }

    #[test]
    fn a_non_message_update_is_ignored_rather_than_filed_against_user_zero() {
        let u = Update {
            update_id: 5,
            ..Update::default()
        };
        assert_eq!(
            classify(&u, &Allowlist::default()),
            Inbound::Ignored {
                update_id: 5,
                reason: IgnoreReason::NotAMessage
            }
        );
    }

    #[test]
    fn a_sticker_from_an_allowlisted_user_is_ignored_not_run() {
        let mut u = update(1, 42, 7, "");
        u.message.as_mut().unwrap().text = None;
        assert_eq!(
            classify(&u, &Allowlist::new([42])),
            Inbound::Ignored {
                update_id: 1,
                reason: IgnoreReason::NoText
            }
        );
    }

    #[test]
    fn an_edited_message_is_treated_as_a_new_one() {
        let mut u = update(1, 42, 7, "hi");
        u.edited_message = u.message.take();
        assert!(matches!(
            classify(&u, &Allowlist::new([42])),
            Inbound::Handle(_)
        ));
    }

    #[test]
    fn an_update_parses_from_the_json_telegram_actually_sends() {
        let raw = r#"{"update_id":870,"message":{"message_id":12,
            "from":{"id":42,"is_bot":false,"first_name":"R","username":"r"},
            "chat":{"id":42,"first_name":"R","username":"r","type":"private"},
            "date":1730000000,"text":"status?"}}"#;
        let u: Update = serde_json::from_str(raw).unwrap();
        assert_eq!(u.update_id, 870);
        match classify(&u, &Allowlist::new([42])) {
            Inbound::Handle(m) => assert_eq!(m.text, "status?"),
            other => panic!("expected a handle: {other:?}"),
        }
    }

    // -- offset ------------------------------------------------------------

    #[test]
    fn the_offset_acknowledges_the_highest_update_in_the_batch() {
        let batch = vec![update(7, 1, 1, "a"), update(5, 1, 1, "b")];
        assert_eq!(next_offset(None, &batch), Some(8));
    }

    #[test]
    fn an_empty_poll_leaves_the_offset_alone() {
        assert_eq!(next_offset(Some(9), &[]), Some(9));
        assert_eq!(next_offset(None, &[]), None);
    }

    #[test]
    fn the_offset_never_moves_backwards() {
        let batch = vec![update(3, 1, 1, "old")];
        assert_eq!(next_offset(Some(9), &batch), Some(9));
    }

    // -- rate limits -------------------------------------------------------

    #[test]
    fn a_retry_after_is_honoured_verbatim_rather_than_capped() {
        let d = backoff(0, Some(30));
        assert_eq!(d, Duration::from_secs(30));
        // Longer than MAX_BACKOFF on purpose: the server's flood wait wins.
        assert_eq!(backoff(3, Some(3600)), Duration::from_secs(3600));
    }

    #[test]
    fn our_own_backoff_grows_and_then_stops_growing() {
        assert_eq!(backoff(0, None), Duration::from_secs(1));
        assert_eq!(backoff(1, None), Duration::from_secs(2));
        assert_eq!(backoff(2, None), Duration::from_secs(4));
        assert_eq!(backoff(20, None), MAX_BACKOFF);
    }

    #[test]
    fn a_429_is_read_as_a_flood_wait_with_its_retry_after() {
        let params = ResponseParameters {
            retry_after: Some(12),
            migrate_to_chat_id: None,
        };
        match classify_api_error(429, "Too Many Requests", Some(&params)) {
            BotError::RateLimited { retry_after, .. } => assert_eq!(retry_after, Some(12)),
            other => panic!("expected a flood wait: {other:?}"),
        }
    }

    #[test]
    fn a_409_is_fatal_because_only_one_poller_may_hold_a_token() {
        let e = classify_api_error(409, "terminated by other getUpdates request", None);
        assert!(matches!(e, BotError::Conflict(_)));
        assert!(!e.is_retryable());
    }

    #[test]
    fn a_bad_token_is_not_retryable() {
        assert!(!classify_api_error(401, "Unauthorized", None).is_retryable());
        assert!(!classify_api_error(403, "bot was blocked", None).is_retryable());
    }

    #[tokio::test(start_paused = true)]
    async fn a_flood_wait_is_slept_off_and_the_message_still_arrives() {
        let bot = FakeBot::failing_sends(vec![BotError::RateLimited {
            retry_after: Some(30),
            description: "Too Many Requests".into(),
        }]);
        let started = tokio::time::Instant::now();
        deliver(&bot, 7, None, "hello", Formatting::Plain)
            .await
            .unwrap();
        assert_eq!(bot.sent_texts(), vec!["hello".to_string()]);
        assert!(
            started.elapsed() >= Duration::from_secs(30),
            "the flood wait was not waited out"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_send_gives_up_rather_than_retrying_forever() {
        let failures = (0..MAX_SEND_ATTEMPTS)
            .map(|_| BotError::RateLimited {
                retry_after: Some(1),
                description: "Too Many Requests".into(),
            })
            .collect();
        let bot = FakeBot::failing_sends(failures);
        assert!(deliver(&bot, 7, None, "hello", Formatting::Plain)
            .await
            .is_err());
        assert!(bot.sent_texts().is_empty());
    }

    #[tokio::test]
    async fn a_bad_token_is_not_retried() {
        let bot = FakeBot::failing_sends(vec![BotError::Unauthorized("nope".into())]);
        assert!(deliver(&bot, 7, None, "hi", Formatting::Plain)
            .await
            .is_err());
        // One attempt, not four.
        assert_eq!(bot.send_failures.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn a_long_reply_arrives_as_several_messages_in_order() {
        let bot = FakeBot::default();
        let text = format!("{}\n{}", "a".repeat(3000), "b".repeat(3000));
        deliver(&bot, 7, None, &text, Formatting::Plain)
            .await
            .unwrap();
        let sent = bot.sent_texts();
        assert_eq!(sent.len(), 2);
        assert!(sent[0].starts_with('a'));
        assert!(sent[1].starts_with('b'));
        assert!(
            bot.sent.lock().unwrap().iter().all(|s| s.chat_id == 7),
            "a chunk went to the wrong chat"
        );
    }

    // -- progress ----------------------------------------------------------

    #[test]
    fn the_first_heartbeat_says_something_before_a_minute_has_passed() {
        assert_eq!(progress_text(Duration::ZERO), "⏳ Working — under a minute");
        assert_eq!(progress_text(Duration::from_secs(60)), "⏳ Working — 1 min");
        assert_eq!(progress_text(Duration::from_secs(605)), "⏳ Working — 10 min");
    }

    #[test]
    fn the_bubble_is_only_edited_when_the_minute_count_changes() {
        assert_eq!(progress_due(None, Duration::from_secs(10)), Some(0));
        assert_eq!(progress_due(Some(0), Duration::from_secs(30)), None);
        assert_eq!(progress_due(Some(0), Duration::from_secs(61)), Some(1));
        assert_eq!(progress_due(Some(1), Duration::from_secs(119)), None);
    }

    #[test]
    fn a_completion_notice_says_whether_it_worked() {
        assert_eq!(completion_text(true, Duration::from_secs(300)), "✅ Done — 5 min");
        assert_eq!(
            completion_text(false, Duration::from_secs(300)),
            "❌ Failed — 5 min"
        );
    }

    #[test]
    fn only_the_end_of_a_run_is_reported_to_a_phone() {
        assert_eq!(
            step_for(&AgentEvent::ToolCall {
                name: "Bash".into(),
                input: None
            }),
            Step::Quiet
        );
        assert_eq!(
            step_for(&AgentEvent::Message {
                text: "thinking out loud".into()
            }),
            Step::Quiet
        );
        assert_eq!(
            step_for(&AgentEvent::Finished {
                text: Some("done".into()),
                exit_code: Some(0),
                is_error: false,
                usage: Default::default(),
            }),
            Step::Done {
                text: Some("done".into()),
                ok: true
            }
        );
        assert_eq!(
            step_for(&AgentEvent::Error {
                message: "spawn failed".into()
            }),
            Step::Done {
                text: Some("spawn failed".into()),
                ok: false
            }
        );
    }

    // -- the poll loop -----------------------------------------------------

    #[tokio::test]
    async fn a_poll_acknowledges_what_it_read_before_returning_it() {
        let bot = FakeBot::with_batches(vec![Ok(vec![update(4, 42, 7, "hi")])]);
        let poller = Poller::new(bot, Allowlist::new([42]));
        assert_eq!(poller.offset(), None);
        let batch = poller.poll_once().await.unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(poller.offset(), Some(5));
        // The next poll asks from the acknowledged offset.
        poller.poll_once().await.unwrap();
        assert_eq!(
            *poller.bot().offsets.lock().unwrap(),
            vec![None, Some(5)]
        );
    }

    #[tokio::test]
    async fn a_refused_message_is_logged_and_nothing_is_sent_back() {
        let bot = FakeBot::with_batches(vec![Ok(vec![
            update(1, 999, 999, "let me in"),
            update(2, 42, 7, "status?"),
        ])]);
        let poller = Poller::new(bot, Allowlist::new([42]));
        let batch = poller.poll_once().await.unwrap();
        assert!(matches!(batch[0], Inbound::Refused(_)));
        assert!(matches!(batch[1], Inbound::Handle(_)));
        assert_eq!(poller.refusals().len(), 1);
        assert_eq!(poller.refusals()[0].user_id, 999);
        assert!(
            poller.bot().sent_texts().is_empty(),
            "a stranger was answered"
        );
    }

    #[tokio::test]
    async fn a_conflict_reaches_the_caller_rather_than_being_swallowed() {
        let bot = FakeBot::with_batches(vec![Err(BotError::Conflict("other poller".into()))]);
        let poller = Poller::new(bot, Allowlist::new([42]));
        assert!(matches!(
            poller.poll_once().await,
            Err(BotError::Conflict(_))
        ));
        // A failed poll must not acknowledge anything.
        assert_eq!(poller.offset(), None);
    }
}
