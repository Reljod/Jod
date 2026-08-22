//! Pull requests a run opened.
//!
//! Detected two ways, on purpose. The event stream gives *immediacy* — a URL
//! appears the moment the agent prints it — and polling gives *authority*,
//! because only the forge knows whether it is still open. Neither alone is
//! enough: the stream cannot tell you it was merged an hour later, and polling
//! alone would leave the fleet blank for as long as the poll interval.
//!
//! Jod shows and opens. It never merges — that is `merge_pr.sh`'s job and the
//! charter is explicit that a script decides what merges unread.
//!
//! ## Two detectors, and what each one is for
//!
//! **The stream** is parsed for URLs. It is the only thing that is instant: the
//! moment `gh pr create` prints a URL into a tool result, the fleet can show it.
//! It knows nothing else — a URL is not a status — so a row starts
//! [`State::Unknown`] and stays that way until somebody asks the forge.
//!
//! **The poll** asks the forge. It is the only thing with authority: a pull
//! request merged an hour after the session ended produces no event anywhere,
//! and nothing but a question to GitHub will ever discover it. It also
//! *discovers* — `gh pr list --head <branch>` finds a pull request opened by
//! hand, or by an agent whose output nobody parsed, which is why
//! [`Source::Poll`] exists as a way of first hearing about one rather than only
//! as a way of refreshing.
//!
//! ## One path here has never been run
//!
//! [`run_gh`] actually spawning `gh` is **not exercised by any test**, and that
//! is deliberate rather than an oversight to be tidied up later. Exercising it
//! end to end means opening a pull request, which is externally visible and
//! which docs/spec-harness.md lists as stop-and-ask; it has not been authorised, so it has
//! not been done.
//!
//! What *is* held to reality either side of that gap: the argv, run verbatim
//! against a real `gh` and confirmed accepted; the JSON it prints, pasted into
//! the tests as fixtures rather than invented; the three failure messages it
//! produces, captured from real runs with no host configured, with a stale
//! token, and against a number that does not exist; and the fold from an answer
//! into a row, through [`Store::absorb_view`] and [`Store::absorb_list`], which
//! exist as separate functions precisely so that most of what could go wrong
//! here needs no process. The untested seam is the dozen lines that turn an
//! `Output` into a `String`.
//!
//! Nobody should read the green suite as covering it.
//!
//! ## Absent tooling is a machine, not an error
//!
//! No `gh`, or a `gh` nobody has logged in, is a fact about the box. It makes
//! the state column less useful and breaks nothing else, so it degrades to a
//! single line on stderr and silence afterwards — see [`SaidOnce`]. A poller
//! that complained once a minute would train its reader to ignore it, which is
//! the failure mode that matters here, because the same stream carries the
//! messages about credentials that do need reading.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::event::{AgentEnvelope, AgentEvent};
use crate::store::Store;

/// What the forge says about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Draft,
    Open,
    Merged,
    Closed,
    /// Parsed out of a stream, never yet reconciled. Honest and common — a URL
    /// is not a status, and claiming one before asking would be inventing it.
    Unknown,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Draft => "draft",
            State::Open => "open",
            State::Merged => "merged",
            State::Closed => "closed",
            State::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> State {
        match s {
            "draft" => State::Draft,
            "open" => State::Open,
            "merged" => State::Merged,
            "closed" => State::Closed,
            _ => State::Unknown,
        }
    }
}

/// How Jod first heard about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Stream,
    Poll,
}

/// One pull request, as Jod knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: i64,
    pub work_id: Option<String>,
    pub conversation_id: Option<String>,
    /// Which lease's branch this came off, which is how a PR is attributed to
    /// the session that actually did the work.
    pub lease_id: Option<i64>,
    pub repo: String,
    pub number: Option<i64>,
    pub url: String,
    pub title: String,
    pub branch: String,
    pub state: State,
    pub source: Source,
    pub detected_at_ms: i64,
    pub reconciled_at_ms: Option<i64>,
}

/// Whether Jod opens a pull request by itself when a session's work looks
/// finished.
///
/// Off by default, and it opens a **draft** through the existing skill. Two
/// separate reasons: opening a PR is externally visible, and a draft is the
/// repo's own convention for one that is not asking to be read yet.
pub const AUTO_PR_SETTING: &str = "auto_pr";

// ---- reading the stream ------------------------------------------------

/// A pull request URL found in an agent's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// Canonical form — `https://host/owner/repo/pull/<n>`, with any `/files`
    /// tab, comment anchor or query string cut off.
    ///
    /// Normalised because `url` is the unique key. An agent that links the
    /// files tab in one message and the plain URL in the next has mentioned one
    /// pull request, and two rows for it would show as two on the work's row.
    pub url: String,
    /// `owner/repo`, which is the form `gh --repo` takes.
    pub repo: String,
    pub number: i64,
    /// Whether the line reads as one this run *opened*, rather than one it
    /// merely referred to. Only these are recorded; see [`opened`].
    pub opened: bool,
}

/// Words that, in front of a URL, mean the line is announcing a pull request
/// rather than pointing at one.
///
/// Matched only *before* the URL, which is what makes a veto list unnecessary:
/// "opened … which closes #12" keeps its verb, and "see … for the earlier
/// discussion" never had one.
const OPENING_VERBS: [&str; 7] = [
    "opened",
    "opening",
    "created",
    "creating",
    "raised",
    "submitted",
    "drafted",
];

/// Every pull request URL in a piece of text, in the order they appear.
pub fn references(text: &str) -> Vec<Reference> {
    let mut out: Vec<Reference> = Vec::new();
    for line in text.lines() {
        for (at, token) in tokens(line) {
            let Some((url, repo, number)) = parse_url(token) else {
                continue;
            };
            // Two ways a line announces rather than refers. The first is the
            // shape `gh pr create` prints: the URL, alone, and nothing else —
            // deliberately strict, so a bullet list of existing pull requests
            // stays a list of references.
            let alone = line.trim() == token;
            let opened = alone || has_opening_verb(&line[..at]);
            // A URL repeated inside one text is one reference. The first
            // sighting decides, so an announcement is not demoted by a later
            // mention of the same thing.
            if let Some(seen) = out.iter_mut().find(|r| r.url == url) {
                seen.opened |= opened;
                continue;
            }
            out.push(Reference {
                url,
                repo,
                number,
                opened,
            });
        }
    }
    out
}

/// Only the pull requests the text says this run opened.
pub fn opened(text: &str) -> Vec<Reference> {
    references(text).into_iter().filter(|r| r.opened).collect()
}

/// The words an agent actually says, from whichever kind of event carried them.
///
/// `ToolResult` is in here and is the one that matters most: `gh pr create`
/// prints its URL to a shell tool's output, not into the assistant's prose, so
/// a detector that read only [`AgentEvent::Message`] would miss the common
/// case entirely.
pub fn spoken_text(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::Message { text } | AgentEvent::Thinking { text } => Some(text),
        AgentEvent::Raw { line } => Some(line),
        AgentEvent::ToolResult { summary, .. } => summary.as_deref(),
        AgentEvent::Finished { text, .. } => text.as_deref(),
        _ => None,
    }
}

/// Split a line into whitespace-separated tokens, each with its byte offset,
/// trimming the punctuation that surrounds a URL in prose but is not part of
/// it — `(https://…)`, a trailing full stop, a markdown backtick.
///
/// The offset is carried because the opening-verb test reads everything to the
/// left of the URL, and "left of" has to mean the original line.
fn tokens(line: &str) -> Vec<(usize, &str)> {
    let mut words: Vec<(usize, &str)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, ch) in line.char_indices() {
        match (ch.is_whitespace(), start) {
            (true, Some(s)) => {
                words.push((s, &line[s..i]));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        words.push((s, &line[s..]));
    }

    words
        .into_iter()
        .map(|(at, raw)| {
            let opened = raw.trim_start_matches(['(', '[', '<', '"', '\'', '`']);
            let trimmed = opened
                .trim_end_matches([')', ']', '>', '"', '\'', '`', '.', ',', ';', ':', '!', '?']);
            (at + (raw.len() - opened.len()), trimmed)
        })
        .collect()
}

/// Recognise `https://host/owner/repo/pull/<n>` and nothing else.
///
/// Matched on the *shape of the path* rather than against a list of hosts, so
/// a GitHub Enterprise install at `github.example.com` works without anybody
/// configuring it. An issue URL has `/issues/` and is deliberately not a pull
/// request: they share a numbering space on GitHub, and treating one as the
/// other would put a bug report on a work's row as a branch nobody can find.
fn parse_url(token: &str) -> Option<(String, String, i64)> {
    let rest = token
        .strip_prefix("https://")
        .or_else(|| token.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    if host.is_empty() {
        return None;
    }
    let mut parts = path.split('/');
    let owner = parts.next().filter(|p| !p.is_empty())?;
    let repo = parts.next().filter(|p| !p.is_empty())?;
    if parts.next()? != "pull" {
        return None;
    }
    let number: i64 = parts
        .next()?
        .split(['#', '?'])
        .next()?
        .parse()
        .ok()
        .filter(|n| *n > 0)?;
    let scheme = if token.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    Some((
        format!("{scheme}://{host}/{owner}/{repo}/pull/{number}"),
        format!("{owner}/{repo}"),
        number,
    ))
}

fn has_opening_verb(before: &str) -> bool {
    let lower = before.to_lowercase();
    OPENING_VERBS.iter().any(|verb| lower.contains(verb))
}

// ---- the store ---------------------------------------------------------

/// Who a pull request belongs to, as far as the caller knows.
///
/// All optional, because the stream knows different things at different
/// moments: a URL in a tool result comes with a conversation and no branch,
/// and one found by polling a lease comes with a branch and no conversation.
/// What is missing here is filled in later from the lease.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attribution {
    pub work_id: Option<String>,
    pub conversation_id: Option<String>,
    pub lease_id: Option<i64>,
    pub branch: Option<String>,
}

/// A pull request about to be written down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPullRequest {
    pub url: String,
    pub repo: String,
    pub number: Option<i64>,
    pub title: String,
    pub branch: String,
    pub state: State,
    pub source: Source,
    pub attribution: Attribution,
}

impl NewPullRequest {
    /// What the stream can honestly say: a URL, a repository, a number, and no
    /// opinion whatsoever about the state.
    pub fn from_stream(reference: &Reference, attribution: &Attribution) -> NewPullRequest {
        NewPullRequest {
            url: reference.url.clone(),
            repo: reference.repo.clone(),
            number: Some(reference.number),
            title: String::new(),
            branch: attribution.branch.clone().unwrap_or_default(),
            state: State::Unknown,
            source: Source::Stream,
            attribution: attribution.clone(),
        }
    }
}

const PR_COLUMNS: &str = "id, work_id, conversation_id, lease_id, repo, number, url, title,
     branch, state, source, detected_at_ms, reconciled_at_ms";

fn read_pr(r: &rusqlite::Row<'_>) -> rusqlite::Result<PullRequest> {
    Ok(PullRequest {
        id: r.get(0)?,
        work_id: r.get(1)?,
        conversation_id: r.get(2)?,
        lease_id: r.get(3)?,
        repo: r.get(4)?,
        number: r.get(5)?,
        url: r.get(6)?,
        title: r.get(7)?,
        branch: r.get(8)?,
        state: State::parse(&r.get::<_, String>(9)?),
        source: match r.get::<_, String>(10)?.as_str() {
            "poll" => Source::Poll,
            _ => Source::Stream,
        },
        detected_at_ms: r.get(11)?,
        reconciled_at_ms: r.get(12)?,
    })
}

fn source_str(source: Source) -> &'static str {
    match source {
        Source::Stream => "stream",
        Source::Poll => "poll",
    }
}

/// Which lease's branch this is, so the pull request lands on the session that
/// did the work rather than on whoever happened to be printing.
///
/// **Integration point.** `leases` owns this table and may grow a query of its
/// own; when it does, this should call it instead. The rule it would have to
/// keep: a branch can have been leased more than once, and the live lease wins
/// over a released one — reusing a branch name after releasing it is ordinary,
/// and attributing to the dead lease would file the pull request under a
/// session that finished last week.
fn lease_for_branch(tx: &rusqlite::Transaction<'_>, branch: &str) -> Result<Option<i64>> {
    if branch.is_empty() {
        return Ok(None);
    }
    Ok(tx
        .query_row(
            "SELECT id FROM leases WHERE branch = ?1
              ORDER BY (state = 'held') DESC, created_at_ms DESC LIMIT 1",
            params![branch],
            |r| r.get(0),
        )
        .optional()?)
}

impl Store {
    /// Write down a pull request, or fold what is now known into the row that
    /// is already there.
    ///
    /// The merge is the whole of this method, and it exists because the two
    /// detectors disagree by design. A sighting in the stream carries
    /// [`State::Unknown`] and no title; a poll carries both. Whichever arrives
    /// second must not undo the other, so:
    ///
    /// - `state` is never overwritten *with* `Unknown` — not knowing is not
    ///   news, and a stream re-sighting after a poll would otherwise blank a
    ///   perfectly good "merged".
    /// - a title, branch or number that is already there survives an empty one.
    /// - `source` and `detected_at_ms` record the *first* time Jod heard about
    ///   it and never move. That is what makes "the stream found this one and
    ///   the poll found that one" answerable weeks later.
    pub fn record_pull_request(&self, new: NewPullRequest) -> Result<PullRequest> {
        let at = now_ms();
        self.write(|tx| {
            let existing: Option<PullRequest> = tx
                .query_row(
                    &format!("SELECT {PR_COLUMNS} FROM pull_requests WHERE url = ?1"),
                    params![new.url],
                    read_pr,
                )
                .optional()?;

            // Attribution by branch, falling back to what the caller passed.
            // Done here rather than at the call site because the branch is
            // often learnt later than the URL.
            let branch = pick(&new.branch, existing.as_ref().map(|p| p.branch.as_str()));
            let lease_id = new
                .attribution
                .lease_id
                .or_else(|| existing.as_ref().and_then(|p| p.lease_id))
                .or(lease_for_branch(tx, &branch)?);

            match existing {
                Some(old) => {
                    let state = if new.state == State::Unknown {
                        old.state
                    } else {
                        new.state
                    };
                    tx.execute(
                        "UPDATE pull_requests
                            SET work_id = COALESCE(?2, work_id),
                                conversation_id = COALESCE(?3, conversation_id),
                                lease_id = ?4,
                                repo = ?5,
                                number = ?6,
                                title = ?7,
                                branch = ?8,
                                state = ?9
                          WHERE url = ?1",
                        params![
                            new.url,
                            new.attribution.work_id,
                            new.attribution.conversation_id,
                            lease_id,
                            pick(&new.repo, Some(&old.repo)),
                            new.number.or(old.number),
                            pick(&new.title, Some(&old.title)),
                            branch,
                            state.as_str(),
                        ],
                    )?;
                }
                None => {
                    tx.execute(
                        "INSERT INTO pull_requests
                           (work_id, conversation_id, lease_id, repo, number, url, title,
                            branch, state, source, detected_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        params![
                            new.attribution.work_id,
                            new.attribution.conversation_id,
                            lease_id,
                            new.repo,
                            new.number,
                            new.url,
                            new.title,
                            branch,
                            new.state.as_str(),
                            source_str(new.source),
                            at,
                        ],
                    )?;
                }
            }

            // A pull request found on a lease's branch belongs to that lease's
            // work and session, and saying so here means the fleet can group it
            // without every caller having to know the join.
            tx.execute(
                "UPDATE pull_requests
                    SET work_id = COALESCE(work_id, (SELECT work_id FROM leases WHERE id = lease_id)),
                        conversation_id = COALESCE(
                            conversation_id, (SELECT conversation_id FROM leases WHERE id = lease_id))
                  WHERE url = ?1 AND lease_id IS NOT NULL",
                params![new.url],
            )?;

            let saved = tx.query_row(
                &format!("SELECT {PR_COLUMNS} FROM pull_requests WHERE url = ?1"),
                params![new.url],
                read_pr,
            )?;
            Ok(saved)
        })
    }

    /// Record every pull request a piece of agent output says it opened.
    ///
    /// **Integration point** for whoever owns the event pipeline: call this
    /// with [`spoken_text`] of each event as it lands. It is deliberately
    /// idempotent — replaying a run's whole stream produces the same rows.
    pub fn note_pull_requests(
        &self,
        text: &str,
        attribution: &Attribution,
    ) -> Result<Vec<PullRequest>> {
        let mut out = Vec::new();
        for reference in opened(text) {
            out.push(
                self.record_pull_request(NewPullRequest::from_stream(&reference, attribution))?,
            );
        }
        Ok(out)
    }

    /// The same, straight from an event envelope.
    pub fn note_pull_requests_in_event(
        &self,
        envelope: &AgentEnvelope,
        attribution: &Attribution,
    ) -> Result<Vec<PullRequest>> {
        match spoken_text(&envelope.event) {
            Some(text) => self.note_pull_requests(text, attribution),
            None => Ok(Vec::new()),
        }
    }

    pub fn pull_request(&self, url: &str) -> Result<Option<PullRequest>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                &format!("SELECT {PR_COLUMNS} FROM pull_requests WHERE url = ?1"),
                params![url],
                read_pr,
            )
            .optional()?)
    }

    /// Every pull request a work has, newest first.
    pub fn work_pull_requests(&self, work_id: &str) -> Result<Vec<PullRequest>> {
        self.pull_requests_where("work_id = ?1", work_id)
    }

    /// Every pull request one session opened, newest first.
    pub fn conversation_pull_requests(&self, conversation_id: &str) -> Result<Vec<PullRequest>> {
        self.pull_requests_where("conversation_id = ?1", conversation_id)
    }

    fn pull_requests_where(&self, clause: &str, value: &str) -> Result<Vec<PullRequest>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {PR_COLUMNS} FROM pull_requests
              WHERE {clause} ORDER BY detected_at_ms DESC, id DESC"
        ))?;
        let rows = stmt.query_map(params![value], read_pr)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// What the poller should ask about next: least recently reconciled first,
    /// never asked at all before that.
    ///
    /// Merged and closed rows are left out permanently. Those two states are
    /// terminal on every forge, so re-asking spends a network call to be told
    /// the same thing — and the reason to bound the sweep at all is that the
    /// poll runs on a timer against a rate-limited API.
    pub fn stale_pull_requests(&self, limit: usize) -> Result<Vec<PullRequest>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {PR_COLUMNS} FROM pull_requests
              WHERE state NOT IN ('merged', 'closed')
              ORDER BY reconciled_at_ms IS NOT NULL, reconciled_at_ms, id
              LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit as i64], read_pr)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// Prefer a non-empty new value, otherwise keep what was there.
fn pick(fresh: &str, existing: Option<&str>) -> String {
    if !fresh.is_empty() {
        return fresh.to_string();
    }
    existing.unwrap_or_default().to_string()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---- asking the forge --------------------------------------------------

/// The fields [`parse_view`] reads. One constant, so the request and the parse
/// cannot drift apart — asking for a field nobody reads is waste, and reading
/// one nobody asked for is a `None` that looks like a closed pull request.
pub const GH_FIELDS: &str = "number,title,state,isDraft,headRefName,baseRefName,url";

/// What the forge says a pull request is right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub url: String,
    pub number: i64,
    pub title: String,
    pub branch: String,
    pub base: String,
    pub state: State,
}

/// Why the forge could not be asked.
///
/// Three cases and not one, because the first two are the machine and the
/// third might be Jod's fault — and the message a person needs is different in
/// each. None of them is an error: a pull request whose state is stale is a
/// less useful row, not a broken one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No `gh` on this box.
    NotInstalled,
    /// `gh` is there and nobody has logged it in.
    NotAuthenticated,
    /// It ran and failed for some other reason — offline, rate-limited, a
    /// repository this account cannot see.
    Failed(String),
}

impl Unavailable {
    /// The one line this is worth saying, once.
    pub fn why(&self) -> String {
        match self {
            Unavailable::NotInstalled => {
                "`gh` is not installed, so pull request states will stay as they were last seen \
                 — install the GitHub CLI to have them kept up to date"
                    .into()
            }
            Unavailable::NotAuthenticated => {
                "`gh` is installed but not logged in, so pull request states will stay as they \
                 were last seen — run `gh auth login`"
                    .into()
            }
            Unavailable::Failed(why) => {
                format!("could not ask GitHub about a pull request, leaving it as it was: {why}")
            }
        }
    }
}

/// What one reconciliation attempt came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// The forge answered and the row now says what it says.
    Updated(PullRequest),
    /// There was nothing to ask about, or the forge does not know it. A URL
    /// parsed out of prose that points at a private repository, a deleted
    /// branch's pull request, or a number an agent invented — recorded as seen
    /// and left alone, because deleting a row on one negative answer would lose
    /// the evidence that an agent claimed to have opened something it had not.
    Unknown,
    /// Nobody could be asked. See [`Unavailable`].
    Unavailable(Unavailable),
}

/// Something worth saying exactly once per process.
///
/// A poller that says "gh is not installed" every minute is a poller whose
/// output stops being read, and the same stream carries the messages about
/// credentials that do need reading. Not `std::sync::Once`, because this has
/// to report *whether* it said anything so a test can hold it to that.
#[derive(Debug, Default)]
pub struct SaidOnce(AtomicBool);

impl SaidOnce {
    pub const fn new() -> SaidOnce {
        SaidOnce(AtomicBool::new(false))
    }

    /// Print `message` if it has not been printed before. Returns whether it
    /// printed.
    pub fn say(&self, message: &str) -> bool {
        if self.0.swap(true, Ordering::Relaxed) {
            return false;
        }
        eprintln!("[jod/prs] {message}");
        true
    }
}

/// The process-wide one, so every poller shares the same silence.
static GH_SILENCE: SaidOnce = SaidOnce::new();

/// Where `gh` is, if anywhere.
///
/// The env override exists for the same reason every other one here does: Jod
/// runs as a daemon and from a GUI app, and neither inherits the `PATH` a
/// login shell has.
pub fn gh_binary() -> Option<PathBuf> {
    crate::discovery::find_binary(
        "JOD_GH_BIN",
        &["gh"],
        &["/opt/homebrew/bin/gh", "/usr/local/bin/gh"],
    )
}

/// `gh pr view` for one pull request.
pub fn view_args(repo: &str, number: i64) -> Vec<String> {
    vec![
        "pr".into(),
        "view".into(),
        number.to_string(),
        "--repo".into(),
        repo.into(),
        "--json".into(),
        GH_FIELDS.into(),
    ]
}

/// `gh pr list` for everything on one branch.
///
/// `--state all` on purpose: this is how a pull request that was opened and
/// merged while nobody was watching is discovered at all, and asking only for
/// open ones would find nothing in exactly that case.
pub fn list_args(branch: &str) -> Vec<String> {
    vec![
        "pr".into(),
        "list".into(),
        "--head".into(),
        branch.into(),
        "--state".into(),
        "all".into(),
        "--json".into(),
        GH_FIELDS.into(),
    ]
}

/// Turn `gh`'s exit into either its stdout or a reason nobody could be asked.
///
/// The two authentication messages are matched on the advice `gh` itself
/// prints — "gh auth login" — which is the one string common to both of the
/// shapes it actually produces: `To get started with GitHub CLI, please run:
/// gh auth login` when no host is configured, and `HTTP 401: Bad credentials
/// … Try authenticating with: gh auth login` when a token has gone stale.
fn run_gh(
    gh: &Path,
    dir: Option<&Path>,
    args: &[String],
) -> std::result::Result<String, Unavailable> {
    let mut command = Command::new(gh);
    command.args(args);
    if let Some(dir) = dir {
        // Checked before spawning, because a missing working directory and a
        // missing binary both come back as `NotFound` and the two send whoever
        // reads the message looking in completely different places. This one is
        // ordinary: a lease's worktree removed by hand.
        if !dir.is_dir() {
            return Err(Unavailable::Failed(format!(
                "`{}` is not there any more, so there is no checkout to ask about",
                dir.display()
            )));
        }
        command.current_dir(dir);
    }
    let out = match command.output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Unavailable::NotInstalled)
        }
        Err(e) => return Err(Unavailable::Failed(e.to_string())),
    };
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).to_string());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(classify(&stderr))
}

fn classify(stderr: &str) -> Unavailable {
    if stderr.contains("gh auth login") {
        return Unavailable::NotAuthenticated;
    }
    Unavailable::Failed(stderr.lines().next().unwrap_or("no output").to_string())
}

/// Whether a failure means "there is no such pull request" rather than "I
/// could not ask".
///
/// Matched on what GitHub's GraphQL API actually replies, captured from a real
/// `gh pr view` against a number that does not exist. A missing pull request
/// is an answer — it is the difference between leaving a row alone and
/// believing a URL an agent invented.
fn reads_as_missing(why: &Unavailable) -> bool {
    match why {
        Unavailable::Failed(text) => {
            let lower = text.to_lowercase();
            lower.contains("could not resolve to a pullrequest")
                || lower.contains("no pull requests found")
                || lower.contains("not found")
        }
        _ => false,
    }
}

/// Read one `gh pr view --json` object.
pub fn parse_view(json: &str) -> Option<Reconciled> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    reconciled_from(&value)
}

/// Read a `gh pr list --json` array.
pub fn parse_list(json: &str) -> Vec<Reconciled> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json)
    else {
        return Vec::new();
    };
    items.iter().filter_map(reconciled_from).collect()
}

/// `state` and `isDraft` are two fields and one fact: `gh` reports a draft as
/// `OPEN` with `isDraft` true, and a draft that shows as open is a pull request
/// the fleet says is ready when it is not.
fn reconciled_from(value: &serde_json::Value) -> Option<Reconciled> {
    let number = value.get("number")?.as_i64()?;
    let raw_state = value.get("state")?.as_str()?;
    let draft = value
        .get("isDraft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let state = match raw_state.to_ascii_lowercase().as_str() {
        "merged" => State::Merged,
        "closed" => State::Closed,
        "open" if draft => State::Draft,
        "open" => State::Open,
        _ => State::Unknown,
    };
    Some(Reconciled {
        url: text(value, "url"),
        number,
        title: text(value, "title"),
        branch: text(value, "headRefName"),
        base: text(value, "baseRefName"),
        state,
    })
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

impl Store {
    /// Ask the forge about one pull request and write down what it says.
    pub fn reconcile_pull_request(&self, url: &str) -> Result<Reconciliation> {
        self.reconcile_pull_request_with(url, gh_binary().as_deref())
    }

    /// Sweep the rows whose state could still change.
    pub fn reconcile_pull_requests(&self, limit: usize) -> Result<Vec<Reconciliation>> {
        self.reconcile_pull_requests_with(limit, gh_binary().as_deref())
    }

    /// The two above, with the binary passed in.
    ///
    /// Split out so the absent-tooling path can be exercised for real. Faking
    /// `gh` itself is out of bounds — docs/spec-harness.md says so — but a machine that has
    /// none is not a fake, it is the case this degrades for, and `None` is
    /// exactly what [`gh_binary`] returns there.
    pub fn reconcile_pull_request_with(
        &self,
        url: &str,
        gh: Option<&Path>,
    ) -> Result<Reconciliation> {
        let Some(existing) = self.pull_request(url)? else {
            return Ok(Reconciliation::Unknown);
        };
        let Some(number) = existing.number else {
            // A row with no number cannot be asked about. It should not exist —
            // every path that writes one parses the number out of the URL — so
            // this is a guard rather than a case.
            return Ok(Reconciliation::Unknown);
        };
        self.apply_view(&existing, gh, &view_args(&existing.repo, number))
    }

    /// Stops at the first [`Unavailable`], because every remaining call would
    /// fail the same way — there is no `gh` halfway through a sweep — and a
    /// hundred identical failures cost a hundred process spawns to learn
    /// nothing.
    pub fn reconcile_pull_requests_with(
        &self,
        limit: usize,
        gh: Option<&Path>,
    ) -> Result<Vec<Reconciliation>> {
        let mut out = Vec::new();
        for pr in self.stale_pull_requests(limit)? {
            let outcome = self.reconcile_pull_request_with(&pr.url, gh)?;
            let stop = matches!(outcome, Reconciliation::Unavailable(_));
            out.push(outcome);
            if stop {
                break;
            }
        }
        Ok(out)
    }

    /// Find pull requests on a branch that nobody parsed out of a stream.
    ///
    /// The other half of "detected two ways". Run against a lease's checkout,
    /// so `gh` infers the repository from the remote the person actually
    /// pushed to rather than from anything Jod guessed.
    pub fn discover_pull_requests(
        &self,
        repo_path: &Path,
        branch: &str,
        attribution: &Attribution,
    ) -> Result<Vec<Reconciliation>> {
        self.discover_pull_requests_with(repo_path, branch, attribution, gh_binary().as_deref())
    }

    /// The same, with the binary passed in — see
    /// [`Store::reconcile_pull_request_with`] for why that is the shape.
    pub fn discover_pull_requests_with(
        &self,
        repo_path: &Path,
        branch: &str,
        attribution: &Attribution,
        gh: Option<&Path>,
    ) -> Result<Vec<Reconciliation>> {
        let Some(gh) = gh else {
            return Ok(vec![unavailable(Unavailable::NotInstalled)]);
        };
        let stdout = match run_gh(gh, Some(repo_path), &list_args(branch)) {
            Ok(stdout) => stdout,
            Err(why) => return Ok(vec![unavailable(why)]),
        };
        Ok(self
            .absorb_list(&stdout, attribution)?
            .into_iter()
            .map(Reconciliation::Updated)
            .collect())
    }

    /// Fold a `gh pr list` answer into rows. Split out for the same reason as
    /// [`Store::absorb_view`].
    fn absorb_list(&self, json: &str, attribution: &Attribution) -> Result<Vec<PullRequest>> {
        let mut out = Vec::new();
        for pr in parse_list(json) {
            // `gh pr list` does not name the repository it listed, and the URL
            // is the only place the answer is. Parsing it back is also the
            // check that the URL is the shape everything else here assumes.
            let repo = parse_url(&pr.url)
                .map(|(_, repo, _)| repo)
                .unwrap_or_default();
            let saved = self.record_pull_request(NewPullRequest {
                url: pr.url.clone(),
                repo,
                number: Some(pr.number),
                title: pr.title.clone(),
                branch: pr.branch.clone(),
                state: pr.state,
                // Poll, because this is how Jod first heard about it. A row
                // the stream already has keeps `stream`, which is the point of
                // the column.
                source: Source::Poll,
                attribution: Attribution {
                    branch: Some(pr.branch.clone()),
                    ..attribution.clone()
                },
            })?;
            self.stamp_reconciled(&saved.url)?;
            out.push(self.pull_request(&saved.url)?.unwrap_or(saved));
        }
        Ok(out)
    }

    /// Run one `gh pr view` and fold the answer in. Split out so a caller can
    /// pass `None` for the binary and exercise the degradation.
    fn apply_view(
        &self,
        existing: &PullRequest,
        gh: Option<&Path>,
        args: &[String],
    ) -> Result<Reconciliation> {
        let Some(gh) = gh else {
            return Ok(unavailable(Unavailable::NotInstalled));
        };
        let stdout = match run_gh(gh, None, args) {
            Ok(stdout) => stdout,
            Err(why) if reads_as_missing(&why) => return Ok(Reconciliation::Unknown),
            Err(why) => return Ok(unavailable(why)),
        };
        match self.absorb_view(existing, &stdout)? {
            Some(updated) => Ok(Reconciliation::Updated(updated)),
            None => Ok(unavailable(Unavailable::Failed(
                "`gh` answered with something that is not a pull request".into(),
            ))),
        }
    }

    /// Fold one `gh pr view` answer into the row.
    ///
    /// Separate from running `gh` so that what the forge says and what the row
    /// then says can be checked against each other without a network call —
    /// which is most of what could go wrong here, and none of it needs a
    /// process.
    fn absorb_view(&self, existing: &PullRequest, json: &str) -> Result<Option<PullRequest>> {
        let Some(fresh) = parse_view(json) else {
            return Ok(None);
        };
        let saved = self.record_pull_request(NewPullRequest {
            url: existing.url.clone(),
            repo: existing.repo.clone(),
            number: Some(fresh.number),
            title: fresh.title,
            branch: fresh.branch.clone(),
            state: fresh.state,
            source: Source::Poll,
            attribution: Attribution {
                branch: Some(fresh.branch),
                ..Default::default()
            },
        })?;
        self.stamp_reconciled(&saved.url)?;
        Ok(Some(self.pull_request(&saved.url)?.unwrap_or(saved)))
    }

    /// Note that the forge was asked, whatever it said.
    ///
    /// Separate from the state it reported, because "asked a minute ago and it
    /// is still open" and "nobody has ever asked" are the two things the fleet
    /// most needs to tell apart.
    fn stamp_reconciled(&self, url: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE pull_requests SET reconciled_at_ms = ?2 WHERE url = ?1",
                params![url, now_ms()],
            )?;
            Ok(())
        })
    }
}

/// Say why once, and hand back the reason.
fn unavailable(why: Unavailable) -> Reconciliation {
    GH_SILENCE.say(&why.why());
    Reconciliation::Unavailable(why)
}

// ---- auto-PR -----------------------------------------------------------

/// Whether Jod asks a finished session to open a pull request by itself.
impl Store {
    /// Off unless it has been turned on, and off if the setting says anything
    /// this does not recognise. A toggle whose broken value means *on* is a
    /// toggle that opens pull requests nobody asked for.
    pub fn auto_pr(&self) -> Result<bool> {
        Ok(matches!(
            self.setting(AUTO_PR_SETTING)?.as_deref(),
            Some("true" | "1" | "on" | "yes")
        ))
    }

    pub fn set_auto_pr(&self, on: bool) -> Result<()> {
        self.set_setting(AUTO_PR_SETTING, if on { "true" } else { "false" })
    }
}

// ---- the two callers --------------------------------------------------
//
// Everything above is inert until something calls it, and a subsystem that
// nothing calls is one whose tests are green for ever. These two functions are
// the shapes the two live callers want: one per event, one per tick.

/// Record any pull request an event says this run opened.
///
/// **The stream half's entry point**, called from the service's event loop
/// beside the card lifter. Deliberately cheap on the ordinary event, because it
/// runs on *every* event of every run: an event with no text at all returns
/// immediately, text without `/pull/` in it costs one substring scan, and the
/// conversation is only looked up once there is something to attribute.
///
/// Nothing here is worth failing a run over — a pull request nobody recorded is
/// a row missing from a panel — so the caller logs and carries on.
pub fn note_from_stream(
    store: &Store,
    conversation_id: Option<&str>,
    event: &AgentEvent,
) -> Result<Vec<PullRequest>> {
    let Some(text) = spoken_text(event) else {
        return Ok(Vec::new());
    };
    // The rejection that makes this affordable per event. Every pull request
    // URL contains `/pull/`, and almost nothing else an agent prints does.
    if !text.contains("/pull/") {
        return Ok(Vec::new());
    }
    let attribution = match conversation_id {
        Some(id) => store.attribution_for(id)?,
        None => Attribution::default(),
    };
    store.note_pull_requests(text, &attribution)
}

/// What one poll of the forge did, for the tick's log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Swept {
    /// Rows whose state was refreshed.
    pub reconciled: usize,
    /// Pull requests nobody had parsed out of a stream.
    pub discovered: usize,
    /// Why the sweep stopped early, when it did. Said once by whoever produced
    /// it; the caller does not print this again.
    pub quiet: Option<Unavailable>,
}

/// **The poll half's entry point**, called once per tick.
///
/// Two passes, in this order: refresh what is already known, then ask each held
/// lease's branch for anything Jod has never seen. Refreshing first because a
/// stale "open" on a row somebody is looking at is worse than a pull request
/// nobody has noticed yet.
///
/// Bounded by `limit` on both halves, because this runs on a timer against a
/// rate-limited API, and it stops at the first sign that nobody can be asked —
/// there is no `gh` halfway through a sweep.
pub fn sweep(store: &Store, limit: usize) -> Result<Swept> {
    sweep_with(store, limit, gh_binary().as_deref())
}

/// [`sweep`] with the binary passed in, so the degradation can be exercised.
pub fn sweep_with(store: &Store, limit: usize, gh: Option<&Path>) -> Result<Swept> {
    let mut swept = Swept::default();

    for outcome in store.reconcile_pull_requests_with(limit, gh)? {
        match outcome {
            Reconciliation::Updated(_) => swept.reconciled += 1,
            Reconciliation::Unavailable(why) => {
                swept.quiet = Some(why);
                return Ok(swept);
            }
            Reconciliation::Unknown => {}
        }
    }

    for lease in store.leases_to_ask(limit)? {
        // A worktree removed by hand is ordinary and is not a reason to stop
        // the sweep; `run_gh` says so plainly and the next lease still gets
        // its turn.
        for outcome in store.discover_pull_requests_with(
            &lease.worktree_path,
            &lease.branch,
            &lease.attribution,
            gh,
        )? {
            match outcome {
                Reconciliation::Updated(_) => swept.discovered += 1,
                Reconciliation::Unavailable(
                    why @ (Unavailable::NotInstalled | Unavailable::NotAuthenticated),
                ) => {
                    swept.quiet = Some(why);
                    return Ok(swept);
                }
                Reconciliation::Unavailable(why) => swept.quiet = Some(why),
                Reconciliation::Unknown => {}
            }
        }
    }

    Ok(swept)
}

/// A held lease, reduced to what the poller needs from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Askable {
    pub worktree_path: PathBuf,
    pub branch: String,
    pub attribution: Attribution,
}

impl Store {
    /// Who a pull request found in this conversation's output belongs to.
    ///
    /// The work comes from the conversation row and the branch from the lease
    /// the session is holding, which is what lets a URL printed in prose be
    /// attributed to the worktree the work was actually done in.
    pub fn attribution_for(&self, conversation_id: &str) -> Result<Attribution> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let row: Option<(Option<String>, Option<String>, Option<i64>)> = conn
            .query_row(
                "SELECT c.work_id, l.branch, l.id
                   FROM conversations c
                   LEFT JOIN leases l
                     ON l.conversation_id = c.id AND l.state = 'held'
                  WHERE c.id = ?1
                  ORDER BY l.created_at_ms DESC
                  LIMIT 1",
                params![conversation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (work_id, branch, lease_id) = row.unwrap_or((None, None, None));
        Ok(Attribution {
            work_id,
            conversation_id: Some(conversation_id.to_string()),
            lease_id,
            branch,
        })
    }

    /// The held leases the poller should ask about, newest first.
    ///
    /// **Integration point**, like [`lease_for_branch`]: `leases` owns this
    /// table and has no "every held lease" query yet. Only held ones, because a
    /// released lease's branch is somebody else's business now, and asking
    /// about it every minute for ever is how a poller becomes the reason for a
    /// rate limit.
    pub fn leases_to_ask(&self, limit: usize) -> Result<Vec<Askable>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT worktree_path, branch, work_id, conversation_id, id
               FROM leases WHERE state = 'held' AND branch <> ''
              ORDER BY created_at_ms DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(Askable {
                worktree_path: PathBuf::from(r.get::<_, String>(0)?),
                branch: r.get(1)?,
                attribution: Attribution {
                    work_id: r.get(2)?,
                    conversation_id: r.get(3)?,
                    lease_id: Some(r.get(4)?),
                    branch: Some(r.get(1)?),
                },
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// What Jod says to a session when auto-PR is on and its work looks finished.
///
/// An instruction to the agent rather than a `gh pr create` Jod runs itself,
/// and that is the design rather than a shortcut. The charter's rule is that a
/// pull request carries evidence — real output, diff-derived deltas — and the
/// `create-pr` skill is what produces it. Jod shelling out to `gh pr create`
/// with a title and an empty body would open exactly the pull request that rule
/// exists to prevent, and it would do it from the one process that never saw
/// the work happen. The session has the context; it is asked to use the skill.
///
/// **Integration point** for whoever owns delivery: this is the text to inject
/// at a turn boundary once [`Store::auto_pr`] is on and the board is empty.
///
/// `stacked_on` is the branch another engineer on the same job is working in,
/// when this session's work was cut on top of it. Passing it changes two
/// things and nothing else: the pull request is opened against that branch
/// rather than against `base`, and a sentence says why. Both matter for the
/// same reason — a pull request opened against `main` when its branch actually
/// starts from a colleague's unmerged branch shows that colleague's diff as
/// well as its own, and a reviewer then reads a change nobody in that pull
/// request wrote.
///
/// `None` is the ordinary case and produces exactly the text it always has.
/// That is not a coincidence to be maintained by hand; it is held by
/// `the_instruction_for_an_unstacked_pull_request_is_the_one_it_has_always_been`.
pub fn auto_pr_instruction(branch: &str, base: &str, stacked_on: Option<&str>) -> String {
    // What the pull request is actually opened against. A stacked one is based
    // on the branch below it, which is the whole of what stacking is.
    let against = stacked_on.unwrap_or(base);
    let stacked = match stacked_on {
        None => String::new(),
        Some(parent) => format!(
            "This branch was cut on top of `{parent}`, which belongs to another engineer on \
             the same job and has not landed yet. That is why the base is `{parent}` and not \
             `{base}`: based that way, the diff is only the part you added, and the reviewer \
             is not asked to read somebody else's change as well as yours.\n\n"
        ),
    };
    format!(
        "Your work on `{branch}` looks finished. Open a pull request against `{against}` by \
         running the `create-pr` skill — it builds the body and the evidence bundle.\n\n\
         {stacked}\
         Open it as a **draft**. Do not merge it, do not mark it ready for review, and do not \
         run `gh pr merge`: whether this merges is decided by `merge_pr.sh` and by a person, \
         not here.\n\n\
         If anything blocks the pull request, write BLOCKED.md and stop — that is a successful \
         ending."
    )
}

// ---- stacking ----------------------------------------------------------

/// One work's pull requests, in the order they should sit on each other:
/// `prs[0]` is the bottom of the stack and the last one is the top.
///
/// Only ever built with two or more in it. One pull request is a pull request,
/// not a stack — see [`Stacking::TooFew`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    pub prs: Vec<PullRequest>,
}

/// Whether a work has something worth linking into a stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stacking {
    Ready(Stack),
    /// Fewer than two pull requests, carrying how many there were. Zero and one
    /// are different situations for whoever reads the refusal — nobody has
    /// opened one yet, against only one engineer having finished — and a
    /// refusal that does not say which leaves the manager guessing.
    TooFew { found: usize },
}

impl Store {
    /// A work's pull requests, ordered bottom to top by the order their tasks
    /// were planned.
    ///
    /// ## Where the order comes from
    ///
    /// The plan is the only ordering anybody has a stated reason to trust. The
    /// manager wrote the tasks in the order the work has to happen in, so the
    /// task at the top of the board is the one everything else is built on,
    /// and that is the bottom of the stack.
    ///
    /// Plan order is `tasks` sorted by `rowid`, and nothing else. **That has to
    /// stay character for character the same as [`Store::work_tasks`]**, which
    /// is the other reader of the same order; the whole design rests on the
    /// board and the stack agreeing about what the plan said, and two queries
    /// that sort a board differently would disagree silently.
    ///
    /// Not `id`, because `Store::plan_work` takes `now_ms()` once and reuses it
    /// for every task in the plan, inside one transaction. Every task in a plan
    /// therefore carries an identical `created_at_ms`, so the tiebreaker is the
    /// only thing separating them — and `tasks.id` is a uuid v4, which would
    /// shuffle the plan into a random order while looking perfectly
    /// deterministic.
    ///
    /// Not `created_at_ms, rowid` either, which is what this used to be. That
    /// spelling assumed the clock only moves forwards, and `now_ms()` is
    /// `chrono::Utc::now()` — a wall clock. An NTP correction or a laptop waking
    /// from sleep steps it backwards, and then a plan written second carries a
    /// smaller timestamp than one written first and sorts underneath it: the
    /// board silently reorders and the stack bases earlier work on top of later
    /// work. `rowid` alone is immune, and it is the more faithful expression of
    /// "the order they were written" in any case. `works.rs` holds a test of
    /// that property, `a_backwards_clock_does_not_reorder_the_board`, which
    /// covers this query too — the invariant is one thing and both halves have
    /// to spell it the same way.
    ///
    /// A pull request reaches its task through the engineer that opened it:
    /// `pull_requests.conversation_id` to `conversations.task_id`. Two other
    /// conversations count as well, and both matter because of
    /// `Placement::Share`. A shared worktree is one branch and therefore one
    /// pull request covering the work of everybody standing in it, so the
    /// conversation that holds the lease and every conversation in
    /// `lease_sharers` are candidates too. The pull request takes the
    /// **earliest** task among them: it contains that task's work, and putting
    /// it any higher would make the tasks underneath appear to depend on a
    /// pull request that already includes them.
    ///
    /// ## Pull requests with no task
    ///
    /// Every pull request written before `conversations.task_id` existed has a
    /// null task, as does one opened by a session nobody spawned onto a task.
    /// Those fall back to `detected_at_ms`, oldest first, which is the order
    /// they came back in before any of this — and `detected_at_ms` stays the
    /// tiebreaker inside a single rank, so a shared worktree's two pull
    /// requests do not swap places between calls.
    ///
    /// They sort **above** every pull request that does have a task. Slotting
    /// one into the middle would be a guess, and a wrong guess there rewrites a
    /// planned pull request's base to point at something nobody planned. At the
    /// top it is the only one whose position is uncertain and everything below
    /// it is still ordered by the plan.
    pub fn stack_for_work(&self, work_id: &str) -> Result<Stacking> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(&format!(
            "WITH planned AS (
               SELECT id AS task_id,
                      ROW_NUMBER() OVER (ORDER BY rowid) AS position
                 FROM tasks WHERE work_id = ?1
             ),
             -- Every task a pull request can be said to contain: the one its
             -- opener was spawned onto, plus the ones belonging to anybody
             -- else standing in the same worktree.
             holders AS (
               SELECT p.id AS pr_id, c.task_id AS task_id
                 FROM pull_requests p
                 JOIN conversations c ON c.id = p.conversation_id
                WHERE p.work_id = ?1
               UNION
               SELECT p.id, c.task_id
                 FROM pull_requests p
                 JOIN leases l ON l.id = p.lease_id
                 JOIN conversations c ON c.id = l.conversation_id
                WHERE p.work_id = ?1
               UNION
               SELECT p.id, c.task_id
                 FROM pull_requests p
                 JOIN lease_sharers s ON s.lease_id = p.lease_id
                 JOIN conversations c ON c.id = s.conversation_id
                WHERE p.work_id = ?1
             ),
             ranked AS (
               SELECT h.pr_id, MIN(planned.position) AS position
                 FROM holders h JOIN planned ON planned.task_id = h.task_id
                GROUP BY h.pr_id
             )
             SELECT {PR_COLUMNS}
               FROM pull_requests
               LEFT JOIN ranked ON ranked.pr_id = pull_requests.id
              WHERE pull_requests.work_id = ?1
              ORDER BY ranked.position IS NULL, ranked.position,
                       pull_requests.detected_at_ms, pull_requests.id"
        ))?;
        let rows = stmt.query_map(params![work_id], read_pr)?;
        let prs = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        if prs.len() < 2 {
            return Ok(Stacking::TooFew { found: prs.len() });
        }
        Ok(Stacking::Ready(Stack { prs }))
    }
}

/// The `gh stack link` command line for an ordered list, bottom to top.
///
/// A free function taking the list rather than a method taking a work, so the
/// rendering can be checked without a database — the argument order is the
/// whole point of this command and it is worth testing on its own.
///
/// `gh stack link` accepts a pull request number, a branch name or a pull
/// request URL for each argument, which is why each one is rendered from
/// whichever of the three this row actually has. A number is preferred because
/// it is what a person reading the line will recognise; a branch name is the
/// fallback for a row polling found before it had a number, and the URL is the
/// last resort that is always present, since it is the table's unique key.
pub fn stack_link_command(prs: &[PullRequest]) -> String {
    let args: Vec<String> = prs.iter().map(stack_argument).collect();
    format!("gh stack link {}", args.join(" "))
}

fn stack_argument(pr: &PullRequest) -> String {
    if let Some(number) = pr.number {
        return number.to_string();
    }
    if !pr.branch.is_empty() {
        return pr.branch.clone();
    }
    pr.url.clone()
}

/// What Jod says to a manager that asked for its work's pull requests to be
/// stacked.
///
/// An instruction and an ordered list, not a `gh` call Jod makes itself, for
/// the same reason [`auto_pr_instruction`] is one: linking rewrites the base
/// branch of every pull request named on that line, which is a visible change
/// to somebody else's open work, and the session asking for it is the one that
/// can see whether the list is still right.
pub fn stack_instruction(prs: &[PullRequest]) -> String {
    let command = stack_link_command(prs);
    let count = prs.len();
    let repo = prs.first().map(|pr| pr.repo.as_str()).unwrap_or_default();
    // Every pull request in a stack has to live in one repository, because a
    // stack is a GitHub object belonging to a repository. A work whose
    // engineers opened pull requests in two of them cannot be one stack, and
    // saying so is more use than a command line that will be rejected.
    let spread: Vec<&str> = {
        let mut seen: Vec<&str> = Vec::new();
        for pr in prs {
            if !seen.contains(&pr.repo.as_str()) {
                seen.push(pr.repo.as_str());
            }
        }
        seen
    };
    let where_to_run = if spread.len() > 1 {
        format!(
            "These pull requests are not all in one repository — they are spread across {}. A \
             stack belongs to a single repository, so link the ones that share a repository and \
             leave the rest alone.\n\n",
            spread.join(", ")
        )
    } else if repo.is_empty() {
        String::new()
    } else {
        format!("Run it in a checkout of `{repo}`.\n\n")
    };

    format!(
        "This job produced {count} pull requests. They were cut from the same starting point \
         and each one is only part of the job, so they read as a stack rather than as {count} \
         independent changes. Link them on GitHub, bottom to top:\n\n\
         ```\n{command}\n```\n\n\
         {where_to_run}\
         The order above is Jod's best reading of which pull request sits on which — check it \
         before you run anything, because the command takes it literally.\n\n\
         Linking rewrites each pull request's base branch. A pull request whose branch has \
         already landed must be left out of the command line: pointing a stack at a branch that \
         no longer exists breaks every pull request above it. Look at each one first and drop \
         the ones that are done.\n\n\
         Do not pass `--open`. These are drafts deliberately, and marking them ready for review \
         is not yours to do here. Nothing about a stack changes who decides what lands: that is \
         `merge_pr.sh` and a person, as it is for every other pull request."
    )
}

/// The refusal for a work that has fewer than two pull requests.
///
/// Its own function so the tool and this module cannot end up saying different
/// things, and so the count is always in the sentence. Running `gh stack link`
/// on a single pull request churns a base branch to produce a stack of one,
/// which is worth nothing and is visible to everyone watching that pull
/// request.
pub fn stack_refusal(found: usize) -> String {
    match found {
        0 => "This work has no pull requests yet, so there is nothing to stack. An engineer \
              opens one when its branch is finished; wait for at least two."
            .to_string(),
        1 => "This work has one pull request, and one pull request is not a stack. Linking it \
              would rewrite its base branch to produce a stack of one, which changes an open \
              pull request for no gain. Wait until a second engineer has opened theirs."
            .to_string(),
        found => format!(
            "This work has {found} pull requests, which is not enough to stack. Two is the \
             minimum."
        ),
    }
}

// ---- asking a session to open its pull request -------------------------
//
// The half that was missing. `auto_pr_instruction` wrote the words and
// `AUTO_PR_SETTING` held the switch, and between them there was nothing that
// ever decided a session should be asked — which made the whole of auto-PR a
// subsystem whose tests were green for ever because nothing ran it.
//
// What is here is the decision, and only the decision. Rendering the words is
// `auto_pr_instruction`'s job and getting them into a session is the tick's,
// through the delivery queue that already knows how to resume a conversation
// without trampling one mid-turn. Jod still never runs `gh pr create`.

/// A held lease that the database says is ready to be asked.
///
/// Everything here is answerable without leaving the process. Whether the
/// branch has anything on it is not, and is asked separately — see
/// [`branch_is_ahead`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub lease_id: i64,
    pub work_id: String,
    pub conversation_id: String,
    pub worktree_path: PathBuf,
    pub branch: String,
    /// What the branch was cut from, from `leases.base_ref`.
    pub base: String,
    /// The branch below this one in its work's stack, when there is one.
    ///
    /// Computed over **every** held lease of the work, not only the ones being
    /// asked. A lease whose pull request is already open is not a candidate,
    /// but it is still what the next branch up sits on, and leaving it out
    /// would base that pull request on the trunk and show somebody else's diff
    /// inside it.
    pub stacked_on: Option<String>,
}

/// One session, the instruction it should be given, and what it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub candidate: Candidate,
    pub instruction: String,
}

impl Store {
    /// Every held lease whose session should be asked to open a pull request.
    ///
    /// The conditions, all of them necessary and none of them a matter of
    /// judgement:
    ///
    /// - The lease is still **held** and has a branch. A released lease's
    ///   branch is somebody else's business, and a lease with no branch has
    ///   nothing to open a pull request from.
    /// - Its work's board has at least one task and **no open one**. A board
    ///   with work left on it is not finished whatever the session is doing,
    ///   and requiring a task to exist keeps a work whose board was never
    ///   written from reading as a finished one.
    /// - **No pull request is recorded** for that lease or that branch. Asking
    ///   for a second one is asking a session to duplicate its own work.
    /// - **It has not been asked before** — `leases.auto_pr_asked_at_ms` is
    ///   null. This is the one that matters on a loop that runs every minute:
    ///   without it the ask is re-sent every tick for ever, and each repeat
    ///   spends the session a turn.
    ///
    /// That last fact lives on the lease rather than in `settings` or in the
    /// delivered `pending_deliveries` row, and both alternatives were
    /// considered. The delivery ledger is durable today, because nothing prunes
    /// `pending_deliveries`, but resting this loop's idempotence on that staying
    /// true means the day somebody adds a retention policy Jod starts asking
    /// every finished session again — silently, months later, with no visible
    /// connection to the change that caused it. A `settings` key named after a
    /// lease id is durable too, and outlives its lease for ever with nothing
    /// ever looking for it again. On the lease, it is a property of the thing it
    /// describes and it goes when the lease goes.
    ///
    /// [`Store::auto_pr`] is deliberately **not** checked here. This function
    /// answers "which leases are ready", the setting answers "may Jod speak at
    /// all", and keeping them apart means the readiness logic can be tested
    /// without the switch and the switch cannot be forgotten in only one of
    /// two callers — there is one caller, [`ask_for_pull_requests`], and it
    /// checks it first.
    ///
    /// Ordering is plan order within each work, by the same ranking
    /// [`Store::stack_for_work`] uses, because that is what makes
    /// [`Candidate::stacked_on`] the branch actually below this one.
    pub fn leases_ready_for_a_pull_request(&self) -> Result<Vec<Candidate>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        let mut stmt = conn.prepare(
            "WITH planned AS (
               SELECT work_id, id AS task_id,
                      ROW_NUMBER() OVER (PARTITION BY work_id ORDER BY rowid) AS position
                 FROM tasks WHERE work_id IS NOT NULL
             ),
             holders AS (
               SELECT l.id AS lease_id, c.task_id AS task_id
                 FROM leases l JOIN conversations c ON c.id = l.conversation_id
               UNION
               SELECT s.lease_id, c.task_id
                 FROM lease_sharers s JOIN conversations c ON c.id = s.conversation_id
             ),
             ranked AS (
               SELECT h.lease_id, MIN(p.position) AS position
                 FROM holders h JOIN planned p ON p.task_id = h.task_id
                GROUP BY h.lease_id
             )
             SELECT l.id, l.work_id, l.conversation_id, l.worktree_path, l.branch, l.base_ref,
                    -- Whether this one is asked, as opposed to merely counted
                    -- for the ordering. Every held lease comes back, because a
                    -- lease that already has its pull request is still what the
                    -- branch above it stands on.
                    (EXISTS (SELECT 1 FROM tasks t WHERE t.work_id = l.work_id)
                     AND NOT EXISTS (SELECT 1 FROM tasks t
                                      WHERE t.work_id = l.work_id AND t.status != 'done')
                     AND NOT EXISTS (SELECT 1 FROM pull_requests p
                                      WHERE p.lease_id = l.id
                                         OR (p.branch = l.branch AND p.branch <> ''))
                     AND l.auto_pr_asked_at_ms IS NULL) AS ready
               FROM leases l
               LEFT JOIN ranked ON ranked.lease_id = l.id
              WHERE l.state = 'held'
                AND l.branch <> ''
                AND l.work_id IS NOT NULL
                AND l.conversation_id IS NOT NULL
              ORDER BY l.work_id,
                       ranked.position IS NULL, ranked.position,
                       l.created_at_ms, l.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                Candidate {
                    lease_id: r.get(0)?,
                    work_id: r.get(1)?,
                    conversation_id: r.get(2)?,
                    worktree_path: PathBuf::from(r.get::<_, String>(3)?),
                    branch: r.get(4)?,
                    base: r.get(5)?,
                    stacked_on: None,
                },
                r.get::<_, bool>(6)?,
            ))
        })?;

        // The predecessor is filled in here rather than in SQL because it is a
        // property of the row *before* this one within its work, and the rows
        // arrive in exactly that order.
        let mut out = Vec::new();
        let mut below: Option<(String, String)> = None;
        for row in rows {
            let (mut candidate, ready) = row?;
            candidate.stacked_on = match &below {
                Some((work, branch)) if *work == candidate.work_id => Some(branch.clone()),
                _ => None,
            };
            below = Some((candidate.work_id.clone(), candidate.branch.clone()));
            if ready {
                out.push(candidate);
            }
        }
        Ok(out)
    }
}

impl Store {
    /// Write down that this lease's session has been asked, so it never is
    /// again.
    ///
    /// **Integration point**, like [`Store::leases_to_ask`] and
    /// [`lease_for_branch`]: `leases` is `crate::leases`' table and this is a
    /// column only the auto-PR loop reads or writes, so the statement lives
    /// beside the loop that needs it rather than in a module that would never
    /// call it.
    pub fn note_pull_request_asked(&self, lease_id: i64, at_ms: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE leases SET auto_pr_asked_at_ms = ?2 WHERE id = ?1",
                params![lease_id, at_ms],
            )?;
            Ok(())
        })
    }

    /// When this lease's session was asked, or `None` for one that never was.
    pub fn pull_request_asked_at(&self, lease_id: i64) -> Result<Option<i64>> {
        let conn = self.conn.lock().expect("store lock poisoned");
        Ok(conn
            .query_row(
                "SELECT auto_pr_asked_at_ms FROM leases WHERE id = ?1",
                params![lease_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }
}

/// Whether `branch` has any commit `base` does not.
///
/// A branch with nothing on it is a session that claimed a worktree and then
/// did the work somewhere else, or did none — and asking it for a pull request
/// spends a turn to be told there is nothing to open.
///
/// **Every failure answers "no".** No git, a worktree somebody deleted by hand,
/// a `base_ref` that no longer resolves: none of those are reasons to ask, and
/// an ask Jod cannot justify is worse than one it misses, because the ask is
/// recorded and never repeated. The missed one is recoverable by a person; the
/// wrong one has already spent the turn.
///
/// This spawns `git` and so is not exercised by the fast tests. One test does
/// run it against a real repository, because the question it answers is a fact
/// about git rather than about Jod, and a stub would test the stub.
pub fn branch_is_ahead(worktree: &Path, base: &str, branch: &str) -> bool {
    if base.is_empty() || branch.is_empty() || !worktree.is_dir() {
        return false;
    }
    let out = Command::new("git")
        .current_dir(worktree)
        .args(["rev-list", "--count", &format!("{base}..{branch}")])
        .output();
    match out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<u64>()
            .is_ok_and(|commits| commits > 0),
        _ => false,
    }
}

/// Every session that should be asked right now, and what to say to each.
///
/// Decides and renders; delivers nothing and writes nothing down. That split is
/// what makes the whole of the judgement testable without a queue, a harness or
/// a spawned process.
pub fn asks(store: &Store) -> Result<Vec<Ask>> {
    asks_with(store, |c| {
        branch_is_ahead(&c.worktree_path, &c.base, &c.branch)
    })
}

/// [`asks`] with the "has this branch got anything on it" question passed in,
/// so the rest can be exercised without a git repository per case.
pub fn asks_with(store: &Store, is_ahead: impl Fn(&Candidate) -> bool) -> Result<Vec<Ask>> {
    if !store.auto_pr()? {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for candidate in store.leases_ready_for_a_pull_request()? {
        if !is_ahead(&candidate) {
            continue;
        }
        let instruction = auto_pr_instruction(
            &candidate.branch,
            &candidate.base,
            candidate.stacked_on.as_deref(),
        );
        out.push(Ask {
            candidate,
            instruction,
        });
    }
    Ok(out)
}

/// Ask each ready session, once, and remember that it was asked.
///
/// `deliver` is how the words reach the session, and it is a parameter rather
/// than a call to the queue because *what* to say and *how to say it* are
/// different jobs owned by different parts of the system. The tick passes the
/// delivery queue; a test passes a closure that records what it was handed.
///
/// **The ask is written down before it is delivered**, which is the same order
/// [`crate::ticker`] stamps its poll and its ledger trim in, and for the same
/// reason: a delivery that fails and is retried every tick is the nagging this
/// record exists to prevent, and it would nag hardest exactly when something
/// is already wrong. The cost is that a failed delivery is a pull request
/// nobody is asked for — recoverable, because a person or a manager can still
/// ask, and visible, because the error is returned rather than swallowed.
///
/// Returns what it asked, so the caller can say so without asking again.
pub fn ask_for_pull_requests(
    store: &Store,
    deliver: impl Fn(&Ask) -> Result<()>,
) -> Result<Vec<Ask>> {
    ask_for_pull_requests_with(
        store,
        |c| branch_is_ahead(&c.worktree_path, &c.base, &c.branch),
        deliver,
    )
}

/// [`ask_for_pull_requests`] with the branch check passed in, so the recording
/// half can be exercised without a git repository per case.
pub fn ask_for_pull_requests_with(
    store: &Store,
    is_ahead: impl Fn(&Candidate) -> bool,
    deliver: impl Fn(&Ask) -> Result<()>,
) -> Result<Vec<Ask>> {
    let mut done = Vec::new();
    for ask in asks_with(store, is_ahead)? {
        store.note_pull_request_asked(ask.candidate.lease_id, now_ms())?;
        deliver(&ask)?;
        done.push(ask);
    }
    Ok(done)
}

/// A session that has finished its only task, holding a lease on a real git
/// repository whose branch has a commit the base does not.
///
/// Returns the lease id and the conversation id, and panics when git is not
/// installed — the same contract [`crate::leases::fixture_repo`] keeps, and for
/// the same reason: a caller that bailed out early still reported as a pass, so
/// the suite claimed to have checked something it never ran.
///
/// Lives here rather than in the test module because [`crate::ticker`]'s guard
/// on the auto-PR wiring needs it, and it is this module that knows what
/// "ready to be asked" is made of. A real repository because
/// [`branch_is_ahead`] runs real `git`, and the whole point of the guard is
/// that the tick reaches it.
#[cfg(test)]
pub(crate) fn a_finished_session(store: &Store) -> (i64, String) {
    let dir = std::env::temp_dir().join(format!(
        "jod-prs-ask-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    std::fs::write(dir.join("README.md"), "fixture\n").expect("a file to commit");
    // Hermetic, and identity given per invocation: a machine with no
    // `user.email` configured must not fail this for a reason that has nothing
    // to do with what is being tested.
    let who = [
        "-c",
        "user.name=Jod Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
    ];
    let steps: Vec<Vec<&str>> = vec![
        vec!["init", "--quiet"],
        vec!["add", "README.md"],
        [&who[..], &["commit", "--quiet", "-m", "init"]].concat(),
    ];
    for args in steps {
        let run = Command::new("git")
            .current_dir(&dir)
            .args(&args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output();
        match run {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => panic!(
                "`git` is not installed on this machine, and whether a branch has anything \
                 on it is a fact about git, so this test cannot run. Install git and run \
                 the suite again."
            ),
            Err(e) => panic!("could not run `git {}`: {e}", args.join(" ")),
            Ok(out) if !out.status.success() => panic!(
                "`git {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            ),
            Ok(_) => {}
        }
    }
    // Whatever `git init` called the default branch on this machine — it is
    // `master` with no global config and `main` on plenty of real boxes, and
    // the base has to be the real name or `rev-list base..branch` says nothing.
    // Read before the checkout, because afterwards HEAD is the new branch.
    let base = String::from_utf8_lossy(
        &Command::new("git")
            .current_dir(&dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs")
            .stdout,
    )
    .trim()
    .to_string();

    // The branch needs something on it, or the ask is right to skip it.
    std::fs::write(dir.join("work.md"), "the work\n").expect("a file to commit");
    for args in [
        vec!["checkout", "--quiet", "-b", "jod/first"],
        vec!["add", "work.md"],
        [&who[..], &["commit", "--quiet", "-m", "the work"]].concat(),
    ] {
        let out = Command::new("git")
            .current_dir(&dir)
            .args(&args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(out.status.success(), "`git {}` failed", args.join(" "));
    }
    let conversation = store
        .new_conversation(crate::harness::HarnessKind::ClaudeCode, "/tmp/repo", None)
        .expect("a session")
        .id;
    store
        .write(|tx| {
            tx.execute(
                "INSERT INTO works (id, title, created_at_ms, updated_at_ms)
                 VALUES ('w-ask', 'a work', 1, 1)",
                [],
            )?;
            Ok(())
        })
        .expect("a work");
    let task = store
        .plan_work(
            "w-ask",
            &crate::works::Plan {
                tasks: vec![crate::works::PlannedTask {
                    title: "the only task".into(),
                    paths: Vec::new(),
                }],
            },
        )
        .expect("a board")
        .remove(0)
        .id;
    let lease = store
        .write(|tx| {
            tx.execute(
                "UPDATE conversations SET task_id = ?2 WHERE id = ?1",
                params![conversation, task],
            )?;
            tx.execute(
                "INSERT INTO leases
                   (work_id, work_title, conversation_id, repo_path, worktree_path, branch,
                    base_ref, state, created_at_ms)
                 VALUES ('w-ask', 'a work', ?1, '/tmp/repo/ask', ?2, 'jod/first', ?3, 'held', 1)",
                params![conversation, dir.to_string_lossy(), base],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .expect("a lease");
    store.complete_work_task(&task).expect("a finished board");
    (lease, conversation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessKind;
    use crate::works::{Plan, PlannedTask};

    fn store() -> Store {
        Store::in_memory().expect("in-memory store")
    }

    fn urls(found: &[Reference]) -> Vec<&str> {
        found.iter().map(|r| r.url.as_str()).collect()
    }

    /// A lease on `branch`, so attribution has something real to attach to.
    fn lease(s: &Store, work_id: &str, conversation_id: &str, branch: &str) -> i64 {
        s.write(|tx| {
            tx.execute(
                "INSERT INTO leases
                   (work_id, work_title, conversation_id, repo_path, worktree_path, branch,
                    base_ref, state, created_at_ms)
                 VALUES (?1, 'a work', ?2, ?5, ?3, ?4, 'main', 'held', 1)",
                params![
                    work_id,
                    conversation_id,
                    // Unique per row: one branch name can have been leased
                    // twice, and `worktree_path` is the column that may not
                    // repeat.
                    format!("/tmp/wt/{}", uuid::Uuid::new_v4()),
                    branch,
                    // One *held* lease per work and repository, so a test that
                    // wants two of them needs two repositories. That index is
                    // what makes a sibling session reuse a lease rather than
                    // cut a second branch for the same job.
                    format!("/tmp/repo/{branch}"),
                ],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap()
    }

    fn work(s: &Store, id: &str) {
        s.write(|tx| {
            tx.execute(
                "INSERT INTO works (id, title, created_at_ms, updated_at_ms)
                 VALUES (?1, 'a work', 1, 1)",
                params![id],
            )?;
            Ok(())
        })
        .unwrap()
    }

    fn conversation(s: &Store) -> String {
        s.new_conversation(HarnessKind::ClaudeCode, "/tmp/repo", None)
            .unwrap()
            .id
    }

    // ---- reading the stream --------------------------------------------

    /// The shape `gh pr create` prints: the URL, on its own, with nothing else
    /// on the line. This is the common case and the whole reason the stream is
    /// parsed at all.
    #[test]
    fn a_url_alone_on_a_line_reads_as_one_this_run_opened() {
        let out = opened("Creating pull request for feat/x into main in Reljod/Jod\n\nhttps://github.com/Reljod/Jod/pull/54\n");
        assert_eq!(urls(&out), ["https://github.com/Reljod/Jod/pull/54"]);
        assert_eq!(out[0].repo, "Reljod/Jod");
        assert_eq!(out[0].number, 54);
    }

    #[test]
    fn a_url_after_an_opening_verb_reads_as_opened() {
        for line in [
            "Opened https://github.com/Reljod/Jod/pull/61 as a draft.",
            "I created the pull request: https://github.com/Reljod/Jod/pull/61",
            "Raised https://github.com/Reljod/Jod/pull/61 against main",
            "Submitted https://github.com/Reljod/Jod/pull/61.",
        ] {
            assert_eq!(
                urls(&opened(line)),
                ["https://github.com/Reljod/Jod/pull/61"],
                "should read as opened: {line}"
            );
        }
    }

    /// The distinction the whole parser exists for. An agent that reads a pull
    /// request, or compares against one, has not opened it — and recording it
    /// would put somebody else's work on this work's row.
    #[test]
    fn a_url_merely_referred_to_is_not_recorded_as_opened() {
        for line in [
            "See https://github.com/Reljod/Jod/pull/12 for the earlier discussion.",
            "This is similar to https://github.com/Reljod/Jod/pull/12",
            "Reviewing https://github.com/Reljod/Jod/pull/12 now",
            "The approach in https://github.com/Reljod/Jod/pull/12 is the one to copy.",
            "- https://github.com/Reljod/Jod/pull/12 (already merged)",
        ] {
            assert!(
                opened(line).is_empty(),
                "should read as a mention, not an opening: {line}"
            );
            assert_eq!(references(line).len(), 1, "it is still a reference: {line}");
        }
    }

    /// An agent links the files tab in one breath and the plain URL in the
    /// next. That is one pull request, and `url` is the unique key, so two
    /// spellings would be two rows on the work's row.
    #[test]
    fn a_link_into_a_tab_or_an_anchor_is_the_same_pull_request() {
        let text = "Opened https://github.com/Reljod/Jod/pull/54/files\n\
                    Opened https://github.com/Reljod/Jod/pull/54#issuecomment-9\n\
                    Opened https://github.com/Reljod/Jod/pull/54?w=1\n";
        let found = references(text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].url, "https://github.com/Reljod/Jod/pull/54");
    }

    /// Matched on the shape of the path, not against a list of hosts, so a
    /// GitHub Enterprise install works without anybody configuring one.
    #[test]
    fn an_enterprise_host_is_recognised_by_the_shape_of_the_path() {
        let found = references("https://github.corp.example.com/team/service/pull/8");
        assert_eq!(
            urls(&found),
            ["https://github.corp.example.com/team/service/pull/8"]
        );
        assert_eq!(found[0].repo, "team/service");
    }

    /// Issues and pull requests share a numbering space on GitHub. Treating one
    /// as the other would put a bug report on a work's row as a branch nobody
    /// can find.
    #[test]
    fn an_issue_url_is_not_a_pull_request() {
        assert!(references("https://github.com/Reljod/Jod/issues/54").is_empty());
        assert!(references("https://github.com/Reljod/Jod/pull/").is_empty());
        assert!(references("https://github.com/Reljod/Jod/pull/abc").is_empty());
        assert!(references("https://github.com/Reljod/pull/54").is_empty());
    }

    #[test]
    fn the_punctuation_around_a_url_in_prose_is_not_part_of_it() {
        let found = references("Opened (https://github.com/Reljod/Jod/pull/7), then waited.");
        assert_eq!(urls(&found), ["https://github.com/Reljod/Jod/pull/7"]);
    }

    /// Real-shaped Claude Code output: a tool result carrying `gh pr create`'s
    /// answer, prose either side of it, and a reference to an unrelated pull
    /// request. Exactly one of the two is this run's.
    #[test]
    fn a_realistic_transcript_yields_the_one_pull_request_it_opened() {
        let transcript = "\
I have pushed the branch and opened the pull request.

Creating pull request for feat/harness-spec into main in Reljod/Jod
https://github.com/Reljod/Jod/pull/61

The approach follows https://github.com/Reljod/Jod/pull/54, which did the same \
thing for the MCP server.
";
        assert_eq!(
            urls(&opened(transcript)),
            ["https://github.com/Reljod/Jod/pull/61"]
        );
        assert_eq!(references(transcript).len(), 2, "both are still references");
    }

    /// `gh pr create` prints to a shell tool's output, not into the assistant's
    /// prose, so a detector that read only `Message` would miss the common case.
    #[test]
    fn a_pull_request_is_found_in_a_tool_result_as_well_as_in_prose() {
        let result = AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some("https://github.com/Reljod/Jod/pull/61".into()),
            is_error: false,
        };
        assert_eq!(
            urls(&opened(spoken_text(&result).unwrap())),
            ["https://github.com/Reljod/Jod/pull/61"]
        );
        assert!(
            spoken_text(&AgentEvent::ToolCall {
                name: "Bash".into(),
                input: None
            })
            .is_none(),
            "the command going out is not the agent saying anything"
        );
    }

    // ---- the store -----------------------------------------------------

    #[test]
    fn a_pull_request_parsed_from_the_stream_starts_out_unknown() {
        let s = store();
        let saved = s
            .note_pull_requests(
                "https://github.com/Reljod/Jod/pull/61",
                &Attribution::default(),
            )
            .unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].state, State::Unknown, "a URL is not a status");
        assert_eq!(saved[0].source, Source::Stream);
        assert_eq!(saved[0].number, Some(61));
        assert!(
            saved[0].reconciled_at_ms.is_none(),
            "nobody has asked the forge yet"
        );
    }

    /// Replaying a run's stream is ordinary — a reconnecting UI, a resumed
    /// supervisor — and it must not multiply the row.
    #[test]
    fn the_same_url_seen_twice_is_one_row() {
        let s = store();
        let text = "https://github.com/Reljod/Jod/pull/61";
        s.note_pull_requests(text, &Attribution::default()).unwrap();
        s.note_pull_requests(text, &Attribution::default()).unwrap();
        assert_eq!(
            s.stale_pull_requests(10).unwrap().len(),
            1,
            "the unique index is on the canonical URL"
        );
    }

    /// The two detectors disagree by design, and the one that knows less must
    /// not win. This is the regression the merge in `record_pull_request`
    /// exists to prevent.
    #[test]
    fn a_second_sighting_in_the_stream_does_not_undo_what_the_poll_learned() {
        let s = store();
        let url = "https://github.com/Reljod/Jod/pull/61";
        s.note_pull_requests(url, &Attribution::default()).unwrap();

        s.record_pull_request(NewPullRequest {
            url: url.into(),
            repo: "Reljod/Jod".into(),
            number: Some(61),
            title: "feat: something".into(),
            branch: "feat/x".into(),
            state: State::Merged,
            source: Source::Poll,
            attribution: Attribution::default(),
        })
        .unwrap();

        s.note_pull_requests(url, &Attribution::default()).unwrap();

        let after = s.pull_request(url).unwrap().unwrap();
        assert_eq!(after.state, State::Merged, "not knowing is not news");
        assert_eq!(after.title, "feat: something");
        assert_eq!(after.branch, "feat/x");
        assert_eq!(
            after.source,
            Source::Stream,
            "`source` is how Jod first heard about it, and that does not change"
        );
    }

    /// A pull request belongs to the session that did the work, and the branch
    /// is the only thing that says which one that was.
    #[test]
    fn a_pull_request_is_attributed_to_the_lease_whose_branch_it_came_off() {
        let s = store();
        let c = conversation(&s);
        work(&s, "w1");
        let id = lease(&s, "w1", &c, "feat/x");

        let saved = s
            .record_pull_request(NewPullRequest {
                url: "https://github.com/Reljod/Jod/pull/61".into(),
                repo: "Reljod/Jod".into(),
                number: Some(61),
                title: String::new(),
                branch: "feat/x".into(),
                state: State::Open,
                source: Source::Poll,
                attribution: Attribution::default(),
            })
            .unwrap();

        assert_eq!(saved.lease_id, Some(id));
        assert_eq!(
            saved.work_id.as_deref(),
            Some("w1"),
            "the lease says which work it was for"
        );
        assert_eq!(saved.conversation_id.as_deref(), Some(c.as_str()));
    }

    /// Branch names get reused after a lease is released. Attributing to the
    /// dead one would file the pull request under a session that finished last
    /// week.
    #[test]
    fn the_live_lease_wins_when_a_branch_name_has_been_used_twice() {
        let s = store();
        let old_c = conversation(&s);
        let new_c = conversation(&s);
        work(&s, "w1");
        work(&s, "w2");
        let released = lease(&s, "w1", &old_c, "feat/x");
        s.write(|tx| {
            tx.execute(
                "UPDATE leases SET state = 'released' WHERE id = ?1",
                params![released],
            )?;
            Ok(())
        })
        .unwrap();
        let live = lease(&s, "w2", &new_c, "feat/x");

        let saved = s
            .record_pull_request(NewPullRequest {
                url: "https://github.com/Reljod/Jod/pull/62".into(),
                repo: "Reljod/Jod".into(),
                number: Some(62),
                title: String::new(),
                branch: "feat/x".into(),
                state: State::Open,
                source: Source::Poll,
                attribution: Attribution::default(),
            })
            .unwrap();
        assert_eq!(saved.lease_id, Some(live));
        assert_eq!(saved.work_id.as_deref(), Some("w2"));
    }

    #[test]
    fn a_works_pull_requests_come_back_newest_first() {
        let s = store();
        work(&s, "w1");
        let attribution = Attribution {
            work_id: Some("w1".into()),
            ..Default::default()
        };
        for n in [1, 2, 3] {
            s.note_pull_requests(
                &format!("https://github.com/Reljod/Jod/pull/{n}"),
                &attribution,
            )
            .unwrap();
        }
        let numbers: Vec<Option<i64>> = s
            .work_pull_requests("w1")
            .unwrap()
            .iter()
            .map(|p| p.number)
            .collect();
        assert_eq!(numbers, [Some(3), Some(2), Some(1)]);
    }

    /// Merged and closed are terminal on every forge, so re-asking spends a
    /// rate-limited call to be told the same thing.
    #[test]
    fn merged_and_closed_pull_requests_are_never_polled_again() {
        let s = store();
        for (n, state) in [
            (1, State::Merged),
            (2, State::Closed),
            (3, State::Open),
            (4, State::Unknown),
        ] {
            s.record_pull_request(NewPullRequest {
                url: format!("https://github.com/Reljod/Jod/pull/{n}"),
                repo: "Reljod/Jod".into(),
                number: Some(n),
                title: String::new(),
                branch: String::new(),
                state,
                source: Source::Stream,
                attribution: Attribution::default(),
            })
            .unwrap();
        }
        let stale: Vec<Option<i64>> = s
            .stale_pull_requests(10)
            .unwrap()
            .iter()
            .map(|p| p.number)
            .collect();
        assert_eq!(stale, [Some(3), Some(4)]);
    }

    /// "Asked a minute ago and it is still open" and "nobody has ever asked"
    /// are the two things the fleet most needs to tell apart, so the row that
    /// has never been asked about goes first.
    #[test]
    fn the_never_asked_are_polled_before_the_least_recently_asked() {
        let s = store();
        for n in [1, 2] {
            s.note_pull_requests(
                &format!("https://github.com/Reljod/Jod/pull/{n}"),
                &Attribution::default(),
            )
            .unwrap();
        }
        s.write(|tx| {
            tx.execute(
                "UPDATE pull_requests SET reconciled_at_ms = 5 WHERE number = 1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let order: Vec<Option<i64>> = s
            .stale_pull_requests(10)
            .unwrap()
            .iter()
            .map(|p| p.number)
            .collect();
        assert_eq!(order, [Some(2), Some(1)]);
    }

    // ---- asking the forge ----------------------------------------------

    /// The request and the parse share one constant, so a field can never be
    /// asked for and not read, or read and not asked for — which reads back as
    /// a pull request with no title rather than as a bug.
    #[test]
    fn the_argv_asks_for_exactly_the_fields_that_are_parsed() {
        let args = view_args("Reljod/Jod", 54);
        assert_eq!(
            args,
            [
                "pr",
                "view",
                "54",
                "--repo",
                "Reljod/Jod",
                "--json",
                GH_FIELDS
            ]
        );
        for field in ["number", "title", "state", "isDraft", "headRefName"] {
            assert!(GH_FIELDS.contains(field), "{field} is parsed and unasked");
        }
    }

    /// `--state all`, because a pull request opened and merged while nobody was
    /// watching is exactly the one this is meant to discover.
    #[test]
    fn discovery_asks_for_every_state_of_a_branch_not_only_the_open_ones() {
        let args = list_args("feat/x");
        assert_eq!(
            args,
            ["pr", "list", "--head", "feat/x", "--state", "all", "--json", GH_FIELDS]
        );
    }

    /// Captured verbatim from `gh pr view 54 --repo Reljod/Jod --json …` on a
    /// real pull request of this repository, so the parse is held to what `gh`
    /// prints rather than to what this file assumed it prints.
    #[test]
    fn real_gh_output_for_a_merged_pull_request_parses() {
        let json = r#"{"baseRefName":"main","headRefName":"feat/mcp-install","isDraft":false,"number":54,"state":"MERGED","title":"feat: register Jod's MCP server with the harnesses you launch","url":"https://github.com/Reljod/Jod/pull/54"}"#;
        let parsed = parse_view(json).expect("real gh output parses");
        assert_eq!(parsed.number, 54);
        assert_eq!(parsed.state, State::Merged);
        assert_eq!(parsed.branch, "feat/mcp-install");
        assert_eq!(parsed.base, "main");
        assert_eq!(parsed.url, "https://github.com/Reljod/Jod/pull/54");
    }

    /// Two fields, one fact. A draft shown as open is a pull request the fleet
    /// says is ready when it is not.
    #[test]
    fn a_draft_is_not_reported_as_open() {
        let draft = r#"{"number":7,"state":"OPEN","isDraft":true,"headRefName":"feat/x","baseRefName":"main","title":"t","url":"u"}"#;
        let ready = r#"{"number":7,"state":"OPEN","isDraft":false,"headRefName":"feat/x","baseRefName":"main","title":"t","url":"u"}"#;
        assert_eq!(parse_view(draft).unwrap().state, State::Draft);
        assert_eq!(parse_view(ready).unwrap().state, State::Open);
    }

    /// The first fixture is verbatim from `gh pr list --head feat/mcp-install
    /// --state all --json …` against this repository. An empty list is the
    /// ordinary answer for a branch nobody has opened anything from, and it is
    /// not a failure.
    #[test]
    fn a_list_of_pull_requests_parses_and_an_empty_one_is_not_a_failure() {
        let real = r#"[{"baseRefName":"main","headRefName":"feat/mcp-install","isDraft":false,"number":54,"state":"MERGED","title":"feat: register Jod's MCP server with the harnesses you launch","url":"https://github.com/Reljod/Jod/pull/54"}]"#;
        let parsed = parse_list(real);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].number, 54);
        assert_eq!(parsed[0].state, State::Merged);

        let two = r#"[{"number":1,"state":"OPEN","isDraft":false,"headRefName":"a","baseRefName":"main","title":"one","url":"https://github.com/o/r/pull/1"},{"number":2,"state":"CLOSED","isDraft":false,"headRefName":"a","baseRefName":"main","title":"two","url":"https://github.com/o/r/pull/2"}]"#;
        assert_eq!(parse_list(two).len(), 2);
        assert_eq!(parse_list(two)[1].state, State::Closed);

        assert!(parse_list("[]").is_empty());
    }

    /// A lease's worktree removed by hand is ordinary, and it must not read as
    /// "the GitHub CLI is not installed" — the two send whoever gets the
    /// message looking in completely different places.
    #[test]
    fn a_checkout_that_is_gone_does_not_read_as_a_missing_gh() {
        let s = store();
        let missing = std::env::temp_dir().join(format!("jod-prs-gone-{}", std::process::id()));
        let outcome = s
            .discover_pull_requests_with(
                &missing,
                "feat/x",
                &Attribution::default(),
                Some(Path::new("/bin/sh")),
            )
            .unwrap();
        match &outcome[..] {
            [Reconciliation::Unavailable(Unavailable::Failed(why))] => {
                assert!(why.contains("not there any more"), "{why}")
            }
            other => panic!("expected a plain failure, got {other:?}"),
        }
    }

    /// Both messages captured from a real `gh` — one with no host configured,
    /// one with a token that no longer works. The advice `gh` prints is the
    /// single string common to both, which is why it is what this matches.
    #[test]
    fn an_unauthenticated_gh_is_recognised_by_what_it_actually_prints() {
        let no_host = "To get started with GitHub CLI, please run:  gh auth login\n\
                       Alternatively, populate the GH_TOKEN environment variable with a GitHub \
                       API authentication token.";
        let stale_token = "HTTP 401: Bad credentials (https://api.github.com/graphql)\n\
                           Try authenticating with:  gh auth login -h github.com";
        assert_eq!(classify(no_host), Unavailable::NotAuthenticated);
        assert_eq!(classify(stale_token), Unavailable::NotAuthenticated);
        assert!(classify(no_host).why().contains("gh auth login"));
    }

    /// Also captured from a real `gh`, against a number that does not exist.
    /// A missing pull request is an *answer*: it is the difference between
    /// leaving a row alone and believing a URL an agent invented.
    #[test]
    fn a_pull_request_the_forge_cannot_resolve_is_an_answer_not_a_failure() {
        let missing = "GraphQL: Could not resolve to a PullRequest with the number of 99999. \
                       (repository.pullRequest)";
        assert!(reads_as_missing(&classify(missing)));
        assert!(!reads_as_missing(&Unavailable::NotAuthenticated));
        assert!(!reads_as_missing(&Unavailable::NotInstalled));
        assert!(
            !reads_as_missing(&classify("error connecting to api.github.com")),
            "being offline must not read as a pull request that is gone"
        );
    }

    /// A poller that says this every minute is a poller nobody reads — and the
    /// same stream carries the messages about credentials that do need reading.
    #[test]
    fn absent_tooling_says_why_once_and_then_stays_quiet() {
        let silence = SaidOnce::new();
        let why = Unavailable::NotInstalled.why();
        assert!(silence.say(&why), "the first time is worth saying");
        assert!(!silence.say(&why), "and no time after that");
        assert!(
            why.contains("install"),
            "the one line has to say what to do about it: {why}"
        );
    }

    /// The degradation itself, through the real path: no binary, so nothing is
    /// asked, nothing is changed, and the row keeps what it had.
    #[test]
    fn with_no_gh_a_pull_request_keeps_what_it_last_said() {
        let s = store();
        let url = "https://github.com/Reljod/Jod/pull/61";
        s.note_pull_requests(url, &Attribution::default()).unwrap();
        let before = s.pull_request(url).unwrap().unwrap();

        let outcome = s
            .apply_view(&before, None, &view_args("Reljod/Jod", 61))
            .unwrap();

        assert_eq!(
            outcome,
            Reconciliation::Unavailable(Unavailable::NotInstalled)
        );
        let after = s.pull_request(url).unwrap().unwrap();
        assert_eq!(after, before, "an unasked question changes nothing");
        assert!(after.reconciled_at_ms.is_none());
    }

    /// Every remaining call in a sweep would fail the same way, and a hundred
    /// identical failures cost a hundred process spawns to learn nothing.
    #[test]
    fn a_sweep_stops_at_the_first_sign_that_nobody_can_be_asked() {
        let s = store();
        for n in [1, 2, 3] {
            s.note_pull_requests(
                &format!("https://github.com/Reljod/Jod/pull/{n}"),
                &Attribution::default(),
            )
            .unwrap();
        }
        let outcomes = s.reconcile_pull_requests_with(3, None).unwrap();
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert!(matches!(outcomes[0], Reconciliation::Unavailable(_)));
    }

    /// The whole point of the poll: the forge said `MERGED`, so the row does
    /// too, and it now knows when it was asked.
    #[test]
    fn what_the_forge_says_is_written_down_and_stamped() {
        let s = store();
        let url = "https://github.com/Reljod/Jod/pull/54";
        s.note_pull_requests(url, &Attribution::default()).unwrap();
        let before = s.pull_request(url).unwrap().unwrap();

        let answer = r#"{"baseRefName":"main","headRefName":"feat/mcp-install","isDraft":false,"number":54,"state":"MERGED","title":"feat: register Jod's MCP server","url":"https://github.com/Reljod/Jod/pull/54"}"#;
        let updated = s
            .absorb_view(&before, answer)
            .unwrap()
            .expect("a real answer parses");

        assert_eq!(updated.state, State::Merged);
        assert_eq!(updated.title, "feat: register Jod's MCP server");
        assert_eq!(updated.branch, "feat/mcp-install");
        assert!(updated.reconciled_at_ms.is_some());
        assert_eq!(
            updated.detected_at_ms, before.detected_at_ms,
            "when it was first seen does not move"
        );
    }

    /// The other half of "detected two ways": a pull request opened by hand, or
    /// by an agent whose output nobody parsed, exists only if somebody asks the
    /// branch about it.
    #[test]
    fn a_pull_request_nobody_parsed_is_discovered_by_asking_the_branch() {
        let s = store();
        let c = conversation(&s);
        work(&s, "w1");
        let id = lease(&s, "w1", &c, "feat/x");

        let answer = r#"[{"number":61,"state":"OPEN","isDraft":true,"headRefName":"feat/x","baseRefName":"main","title":"feat: something","url":"https://github.com/Reljod/Jod/pull/61"}]"#;
        let found = s.absorb_list(answer, &Attribution::default()).unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, Source::Poll, "this is how Jod first heard");
        assert_eq!(found[0].state, State::Draft);
        assert_eq!(found[0].repo, "Reljod/Jod", "read back out of the URL");
        assert_eq!(found[0].lease_id, Some(id));
        assert_eq!(found[0].work_id.as_deref(), Some("w1"));
        assert!(found[0].reconciled_at_ms.is_some());
    }

    /// A row the stream found first keeps `stream`, even when a poll is what
    /// filled in everything else about it.
    #[test]
    fn discovery_does_not_rewrite_how_jod_first_heard_about_a_pull_request() {
        let s = store();
        let url = "https://github.com/Reljod/Jod/pull/61";
        s.note_pull_requests(url, &Attribution::default()).unwrap();

        let answer = r#"[{"number":61,"state":"OPEN","isDraft":false,"headRefName":"feat/x","baseRefName":"main","title":"feat: something","url":"https://github.com/Reljod/Jod/pull/61"}]"#;
        s.absorb_list(answer, &Attribution::default()).unwrap();

        let after = s.pull_request(url).unwrap().unwrap();
        assert_eq!(after.source, Source::Stream);
        assert_eq!(after.state, State::Open, "and the poll's authority stands");
    }

    #[test]
    fn discovering_with_no_gh_degrades_quietly_and_writes_nothing() {
        let s = store();
        let outcome = s
            .discover_pull_requests_with(
                Path::new("/tmp/repo"),
                "feat/x",
                &Attribution::default(),
                None,
            )
            .unwrap();
        assert_eq!(
            outcome,
            [Reconciliation::Unavailable(Unavailable::NotInstalled)]
        );
        assert!(s.stale_pull_requests(10).unwrap().is_empty());
    }

    // ---- the two callers ------------------------------------------------

    /// This runs on every event of every run, so the common case has to be
    /// nearly free — and, more importantly, has to record nothing.
    #[test]
    fn an_event_that_mentions_no_pull_request_records_nothing() {
        let s = store();
        for event in [
            AgentEvent::Message {
                text: "I have finished the refactor and pushed the branch.".into(),
            },
            AgentEvent::ToolCall {
                name: "Bash".into(),
                input: None,
            },
            AgentEvent::Started {
                session_id: Some("s".into()),
                model: None,
            },
        ] {
            assert!(note_from_stream(&s, None, &event).unwrap().is_empty());
        }
        assert!(s.stale_pull_requests(10).unwrap().is_empty());
    }

    /// The stream half, end to end through the entry point the service calls:
    /// a `gh pr create` URL in a tool result, attributed to the work and the
    /// worktree the session was actually writing in.
    #[test]
    fn a_pull_request_in_the_stream_lands_on_the_sessions_work_and_lease() {
        let s = store();
        let c = conversation(&s);
        work(&s, "w1");
        s.write(|tx| {
            tx.execute(
                "UPDATE conversations SET work_id = 'w1' WHERE id = ?1",
                params![c],
            )?;
            Ok(())
        })
        .unwrap();
        let id = lease(&s, "w1", &c, "feat/x");

        let event = AgentEvent::ToolResult {
            name: "Bash".into(),
            summary: Some("https://github.com/Reljod/Jod/pull/61".into()),
            is_error: false,
        };
        let saved = note_from_stream(&s, Some(&c), &event).unwrap();

        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].conversation_id.as_deref(), Some(c.as_str()));
        assert_eq!(saved[0].work_id.as_deref(), Some("w1"));
        assert_eq!(
            saved[0].lease_id,
            Some(id),
            "the branch the session holds is how it reaches the worktree"
        );
        assert_eq!(saved[0].state, State::Unknown);
    }

    #[test]
    fn attribution_survives_a_conversation_with_no_work_and_no_lease() {
        let s = store();
        let c = conversation(&s);
        let found = s.attribution_for(&c).unwrap();
        assert_eq!(found.conversation_id.as_deref(), Some(c.as_str()));
        assert_eq!(found.work_id, None);
        assert_eq!(found.branch, None);
    }

    /// A released lease's branch is somebody else's business now, and asking
    /// about it every minute for ever is how a poller becomes the reason for a
    /// rate limit.
    #[test]
    fn only_held_leases_are_asked_about() {
        let s = store();
        let c = conversation(&s);
        work(&s, "w1");
        lease(&s, "w1", &c, "feat/live");
        let done = lease(&s, "w1", &c, "feat/released");
        s.write(|tx| {
            tx.execute(
                "UPDATE leases SET state = 'released' WHERE id = ?1",
                params![done],
            )?;
            Ok(())
        })
        .unwrap();

        let asking: Vec<String> = s
            .leases_to_ask(10)
            .unwrap()
            .into_iter()
            .map(|a| a.branch)
            .collect();
        assert_eq!(asking, ["feat/live"]);
    }

    /// The poll half's entry point on a machine with no `gh`: it stops at the
    /// first sign nobody can be asked, writes nothing, and does not go on to
    /// spawn a process per lease to be told the same thing.
    #[test]
    fn a_sweep_with_no_gh_stops_at_the_first_answer_and_writes_nothing() {
        let s = store();
        let c = conversation(&s);
        work(&s, "w1");
        lease(&s, "w1", &c, "feat/x");
        s.note_pull_requests(
            "https://github.com/Reljod/Jod/pull/61",
            &Attribution::default(),
        )
        .unwrap();
        let before = s
            .pull_request("https://github.com/Reljod/Jod/pull/61")
            .unwrap();

        let swept = sweep_with(&s, 10, None).unwrap();

        assert_eq!(swept.reconciled, 0);
        assert_eq!(swept.discovered, 0);
        assert_eq!(swept.quiet, Some(Unavailable::NotInstalled));
        assert_eq!(
            s.pull_request("https://github.com/Reljod/Jod/pull/61")
                .unwrap(),
            before,
            "an unasked question changes nothing"
        );
    }

    /// With nothing to refresh, the sweep still gets as far as the leases —
    /// which is the half that discovers a pull request nobody parsed.
    #[test]
    fn a_sweep_with_nothing_to_refresh_still_asks_the_leases() {
        let s = store();
        let c = conversation(&s);
        work(&s, "w1");
        lease(&s, "w1", &c, "feat/x");

        let swept = sweep_with(&s, 10, None).unwrap();
        assert_eq!(
            swept.quiet,
            Some(Unavailable::NotInstalled),
            "it reached the lease pass and stopped there, rather than returning \
             empty-handed with nothing attempted"
        );
    }

    // ---- auto-PR --------------------------------------------------------

    #[test]
    fn auto_pr_is_off_until_it_is_turned_on() {
        let s = store();
        assert!(!s.auto_pr().unwrap(), "opening a PR is externally visible");
        s.set_auto_pr(true).unwrap();
        assert!(s.auto_pr().unwrap());
        s.set_auto_pr(false).unwrap();
        assert!(!s.auto_pr().unwrap());
    }

    /// A toggle whose unreadable value means *on* is a toggle that opens pull
    /// requests nobody asked for.
    #[test]
    fn an_unreadable_auto_pr_setting_means_off() {
        let s = store();
        s.set_setting(AUTO_PR_SETTING, "perhaps").unwrap();
        assert!(!s.auto_pr().unwrap());
    }

    #[test]
    fn the_auto_pr_instruction_asks_for_a_draft_through_the_skill() {
        let text = auto_pr_instruction("feat/x", "main", None);
        assert!(text.contains("create-pr"), "the skill builds the evidence");
        assert!(text.contains("draft"));
        assert!(text.contains("feat/x") && text.contains("main"));
        assert!(
            text.contains("merge_pr.sh"),
            "and it says who does decide: {text}"
        );
    }

    /// The regression guard on the third argument. Every caller that existed
    /// before stacking passes `None`, and for those the text must not have
    /// moved by a single character — this is the whole of what makes the new
    /// argument safe to add. Compared against the string written out in full
    /// rather than against another call, because a test that renders the thing
    /// it is checking would pass through any edit at all.
    #[test]
    fn the_instruction_for_an_unstacked_pull_request_is_the_one_it_has_always_been() {
        let before = "Your work on `feat/x` looks finished. Open a pull request against `main` \
                      by running the `create-pr` skill — it builds the body and the evidence \
                      bundle.\n\nOpen it as a **draft**. Do not merge it, do not mark it ready \
                      for review, and do not run `gh pr merge`: whether this merges is decided \
                      by `merge_pr.sh` and by a person, not here.\n\nIf anything blocks the \
                      pull request, write BLOCKED.md and stop — that is a successful ending.";
        assert_eq!(auto_pr_instruction("feat/x", "main", None), before);
    }

    /// The stacked form changes the base and says why. Both halves are checked
    /// because either one alone is a trap: the right base with no explanation
    /// leaves the session wondering whether Jod meant `main`, and the sentence
    /// with the wrong base opens a pull request showing a colleague's diff.
    #[test]
    fn a_stacked_instruction_bases_the_pull_request_on_the_branch_below_it() {
        let text = auto_pr_instruction("jod/second", "main", Some("jod/first"));
        assert!(
            text.contains("Open a pull request against `jod/first`"),
            "the base is the branch below it, not the trunk: {text}"
        );
        assert!(
            text.contains("jod/first`, which belongs to another engineer"),
            "and it says whose branch that is: {text}"
        );
        assert!(
            text.contains("the diff is only the part you added"),
            "which is the reason the base is not `main`: {text}"
        );
        assert!(
            text.contains("create-pr") && text.contains("draft"),
            "everything the unstacked instruction says, it still says: {text}"
        );
    }

    // ---- stacking -------------------------------------------------------

    /// A board written the way a manager actually writes one, through
    /// [`Store::plan_work`]. Returns the task ids in plan order.
    ///
    /// The real writer rather than hand-placed rows, and the distinction is the
    /// whole reason this helper exists. `plan_work` stamps one `now_ms()` across
    /// the entire plan and inserts it in a single transaction, so every task in
    /// a plan shares a millisecond and the tiebreaker is the only thing that
    /// orders them. A test that picks distinct timestamps by hand exercises a
    /// board production cannot produce and never touches the one it always
    /// does.
    fn plan(s: &Store, work_id: &str, titles: &[&str]) -> Vec<String> {
        let plan = Plan {
            tasks: titles
                .iter()
                .map(|title| PlannedTask {
                    title: (*title).to_string(),
                    paths: Vec::new(),
                })
                .collect(),
        };
        s.plan_work(work_id, &plan)
            .unwrap()
            .into_iter()
            .map(|task| task.id)
            .collect()
    }

    /// An engineer session, spawned onto one task. `conversations.task_id` is
    /// the link the stack ordering is built on.
    fn engineer(s: &Store, task_id: &str) -> String {
        let id = conversation(s);
        s.write(|tx| {
            tx.execute(
                "UPDATE conversations SET task_id = ?2 WHERE id = ?1",
                params![id, task_id],
            )?;
            Ok(())
        })
        .unwrap();
        id
    }

    /// A pull request this session opened, recorded the way the stream detector
    /// records one.
    ///
    /// Through `attribution_for`, which is what `note_from_stream` calls, so a
    /// session holding a lease produces a row carrying that lease's branch —
    /// the same row production would write. Building the attribution by hand
    /// would leave `branch` empty and quietly stop matching anything that looks
    /// a pull request up by its branch.
    fn opened_by(s: &Store, work_id: &str, conversation_id: &str, number: i64) {
        let mut attribution = s.attribution_for(conversation_id).unwrap();
        attribution.work_id = Some(work_id.to_string());
        s.note_pull_requests(
            &format!("https://github.com/Reljod/Jod/pull/{number}"),
            &attribution,
        )
        .unwrap();
    }

    /// One pull request, as the renderers see it — no database involved.
    fn pr(number: i64, branch: &str) -> PullRequest {
        PullRequest {
            id: number,
            work_id: None,
            conversation_id: None,
            lease_id: None,
            repo: "Reljod/Jod".into(),
            number: Some(number),
            url: format!("https://github.com/Reljod/Jod/pull/{number}"),
            title: String::new(),
            branch: branch.into(),
            state: State::Open,
            source: Source::Stream,
            detected_at_ms: number,
            reconciled_at_ms: None,
        }
    }

    /// A stack of one is a pull request. Linking it rewrites its base branch to
    /// produce nothing, in public, so the refusal has to say what it found
    /// rather than only that it refused.
    #[test]
    fn a_work_with_one_pull_request_is_not_a_stack_and_the_refusal_says_so() {
        let s = store();
        work(&s, "w1");
        let attribution = Attribution {
            work_id: Some("w1".into()),
            ..Default::default()
        };
        s.note_pull_requests("https://github.com/Reljod/Jod/pull/41", &attribution)
            .unwrap();

        let found = match s.stack_for_work("w1").unwrap() {
            Stacking::TooFew { found } => found,
            Stacking::Ready(stack) => panic!("one pull request is not a stack: {stack:?}"),
        };
        assert_eq!(found, 1);
        let refusal = stack_refusal(found);
        assert!(
            refusal.contains("one pull request"),
            "the refusal names how many it found: {refusal}"
        );
    }

    /// A work nobody has opened a pull request on refuses differently, because
    /// "wait for the engineers to finish" and "you only have one" are different
    /// things for the manager to do next.
    #[test]
    fn a_work_with_no_pull_requests_at_all_is_refused_by_the_same_door() {
        let s = store();
        work(&s, "w1");
        assert_eq!(
            s.stack_for_work("w1").unwrap(),
            Stacking::TooFew { found: 0 }
        );
        assert!(stack_refusal(0).contains("no pull requests"));
    }

    /// Bottom to top is plan order, and plan order is neither finish order nor
    /// task id order.
    ///
    /// This is the test that decides whether the stack is right at all, and it
    /// is built on the real writer for a reason. `plan_work` stamps one
    /// timestamp across a whole plan, so every task on this board shares a
    /// millisecond and nothing but the tiebreaker separates them. A task id is
    /// a uuid v4, so a tiebreaker of `id` is a shuffle — which is exactly the
    /// bug this guards, and it is invisible to any test that writes tasks with
    /// hand-picked timestamps.
    ///
    /// Five tasks rather than two, because the failure is probabilistic: one
    /// arrangement in a hundred and twenty is the right one by luck, and at two
    /// tasks a broken implementation would pass half the time.
    ///
    /// The pull requests are opened out of plan order as well, so finish order
    /// is wrong too. Both are ordinary — a small task at the top of a plan
    /// finishes before a large one underneath it all the time.
    #[test]
    fn a_works_pull_requests_stack_in_the_order_their_tasks_were_planned() {
        let s = store();
        work(&s, "w1");
        let tasks = plan(
            &s,
            "w1",
            &["the board", "placement", "stacking", "the tools", "the briefs"],
        );
        let engineers: Vec<String> = tasks.iter().map(|task| engineer(&s, task)).collect();

        // Finished 5, 3, 1, 4, 2 — nothing like the order they were planned in.
        for (number, who) in [(45, 4), (43, 2), (41, 0), (44, 3), (42, 1)] {
            opened_by(&s, "w1", &engineers[who], number);
        }

        let stack = match s.stack_for_work("w1").unwrap() {
            Stacking::Ready(stack) => stack,
            other => panic!("five pull requests are a stack: {other:?}"),
        };
        let numbers: Vec<Option<i64>> = stack.prs.iter().map(|p| p.number).collect();
        assert_eq!(
            numbers,
            [Some(41), Some(42), Some(43), Some(44), Some(45)],
            "the plan's order, not the order they were finished in and not the order \
             their uuids happen to sort in"
        );
        assert_eq!(stack_link_command(&stack.prs), "gh stack link 41 42 43 44 45");
    }

    /// Every pull request already in a database on this box was opened before
    /// there was a task to attach it to, and one with no task still has to come
    /// back in a sensible order rather than an arbitrary one. Oldest first, the
    /// same order it had before any of this existed.
    #[test]
    fn pull_requests_with_no_task_fall_back_to_the_order_they_were_opened_in() {
        let s = store();
        work(&s, "w1");
        let attribution = Attribution {
            work_id: Some("w1".into()),
            ..Default::default()
        };
        for n in [41, 42, 43] {
            s.note_pull_requests(
                &format!("https://github.com/Reljod/Jod/pull/{n}"),
                &attribution,
            )
            .unwrap();
        }

        let stack = match s.stack_for_work("w1").unwrap() {
            Stacking::Ready(stack) => stack,
            other => panic!("three pull requests are a stack: {other:?}"),
        };
        let numbers: Vec<Option<i64>> = stack.prs.iter().map(|p| p.number).collect();
        assert_eq!(
            numbers,
            [Some(41), Some(42), Some(43)],
            "oldest at the bottom, and the reverse of what a panel shows"
        );
    }

    /// A pull request the plan does not account for goes above every one it
    /// does, never among them.
    ///
    /// Somewhere in the middle would be a guess, and a wrong guess there
    /// rewrites the base of a planned pull request to point at something
    /// nobody planned. At the top it is the only one whose base is uncertain,
    /// and everything below it is still ordered by the plan.
    #[test]
    fn a_pull_request_with_no_task_sits_above_every_one_that_has_a_task() {
        let s = store();
        work(&s, "w1");
        let tasks = plan(&s, "w1", &["the board"]);
        let planned = engineer(&s, &tasks[0]);

        // The stray one is opened first, so time order would put it at the
        // bottom.
        s.note_pull_requests(
            "https://github.com/Reljod/Jod/pull/99",
            &Attribution {
                work_id: Some("w1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        opened_by(&s, "w1", &planned, 41);

        let stack = match s.stack_for_work("w1").unwrap() {
            Stacking::Ready(stack) => stack,
            other => panic!("two pull requests are a stack: {other:?}"),
        };
        let numbers: Vec<Option<i64>> = stack.prs.iter().map(|p| p.number).collect();
        assert_eq!(numbers, [Some(41), Some(99)]);
    }

    /// Two engineers sharing one worktree share one branch and so open one
    /// pull request between them. It carries both their tasks, and it belongs
    /// at the position of the earlier one — anywhere lower and a task that is
    /// inside it would appear to depend on it.
    ///
    /// Written so that two simpler implementations both get it wrong. Reading
    /// only the conversation that opened it gives the last task, because the
    /// borrower is the one that opened it, and the lease under it belongs to
    /// the engineer on the first task. Reading the clock gives the other order
    /// again, because the shared pull request is opened second.
    #[test]
    fn a_shared_worktrees_pull_request_sits_at_its_earliest_task() {
        let s = store();
        work(&s, "w1");
        let tasks = plan(&s, "w1", &["the board", "placement", "stacking"]);
        let lender = engineer(&s, &tasks[0]);
        let borrower = engineer(&s, &tasks[2]);
        let alone = engineer(&s, &tasks[1]);

        // The lender cut the worktree; the borrower joined it and is the one
        // that opened the pull request off their shared branch.
        let shared = lease(&s, "w1", &lender, "jod/shared");
        s.write(|tx| {
            tx.execute(
                "INSERT INTO lease_sharers (lease_id, conversation_id, work_id, shared_at_ms)
                 VALUES (?1, ?2, 'w1', 1)",
                params![shared, borrower],
            )?;
            Ok(())
        })
        .unwrap();
        opened_by(&s, "w1", &alone, 51);
        opened_by(&s, "w1", &borrower, 50);
        s.write(|tx| {
            tx.execute(
                "UPDATE pull_requests SET lease_id = ?1 WHERE number = 50",
                params![shared],
            )?;
            Ok(())
        })
        .unwrap();

        let stack = match s.stack_for_work("w1").unwrap() {
            Stacking::Ready(stack) => stack,
            other => panic!("two pull requests are a stack: {other:?}"),
        };
        let numbers: Vec<Option<i64>> = stack.prs.iter().map(|p| p.number).collect();
        assert_eq!(
            numbers,
            [Some(50), Some(51)],
            "the shared pull request holds the first task as well as the last, so it is \
             the bottom"
        );
    }

    /// The command line is the part a person will read before they run it, so
    /// the arguments are pull request numbers when there are numbers to use.
    #[test]
    fn the_stack_command_names_each_pull_request_in_stack_order() {
        let prs = [pr(41, "jod/board"), pr(42, "jod/placement")];
        assert_eq!(stack_link_command(&prs), "gh stack link 41 42");
    }

    /// A row the poller found before it had a number still has a branch, and
    /// `gh stack link` takes a branch name in the same position. Falling back
    /// keeps one unnumbered row from making the whole command unusable.
    #[test]
    fn a_pull_request_with_no_number_is_named_by_its_branch_instead() {
        let mut second = pr(42, "jod/placement");
        second.number = None;
        assert_eq!(
            stack_link_command(&[pr(41, "jod/board"), second]),
            "gh stack link 41 jod/placement"
        );
    }

    /// Linking rewrites base branches, so the one thing the instruction cannot
    /// leave out is that a branch which has already landed must be dropped from
    /// the command line. And it must not tell anybody to land anything: that is
    /// `merge_pr.sh` and a person, stack or no stack.
    #[test]
    fn the_stack_instruction_warns_about_rewritten_bases_and_never_asks_for_a_landing() {
        let text = stack_instruction(&[pr(41, "jod/board"), pr(42, "jod/placement")]);
        assert!(
            text.contains("rewrites each pull request's base branch"),
            "the warning is the point of the instruction: {text}"
        );
        assert!(
            text.contains("already landed must be left out"),
            "and what to do about it: {text}"
        );
        assert!(
            text.contains("merge_pr.sh") && text.contains("a person"),
            "who decides what lands has not changed: {text}"
        );
        assert!(
            text.contains("Do not pass `--open`"),
            "`--open` marks every pull request in the stack ready for review: {text}"
        );
        assert!(
            text.contains("Run it in a checkout of `Reljod/Jod`"),
            "the command needs a repository to be run in: {text}"
        );
    }

    /// A stack is an object belonging to one repository. A work whose engineers
    /// opened pull requests in two of them cannot be linked into one, and a
    /// command line that will be rejected is worse than being told why.
    #[test]
    fn pull_requests_spread_across_two_repositories_are_not_one_stack() {
        let mut elsewhere = pr(7, "jod/api");
        elsewhere.repo = "Reljod/Jod-Apps".into();
        let text = stack_instruction(&[pr(41, "jod/board"), elsewhere]);
        assert!(
            text.contains("not all in one repository"),
            "it says so plainly: {text}"
        );
        assert!(
            text.contains("Reljod/Jod, Reljod/Jod-Apps"),
            "and names both: {text}"
        );
    }

    /// A pull request an agent merely printed reaches its lease, exactly as a
    /// polled one does.
    ///
    /// Worth a test of its own because the ordering in [`Store::stack_for_work`]
    /// reaches a task through the lease as well as through the opener, and if
    /// the two detection routes disagreed about `lease_id` then two pull
    /// requests on one job would sort by different rules depending on how each
    /// happened to be noticed.
    ///
    /// They agree twice over, which is the thing to know before touching
    /// either. `attribution_for` now carries the lease id straight out of the
    /// join it was already doing — and even when it did not, the row still
    /// ended up with one, because `record_pull_request` falls back to
    /// `lease_for_branch` and the attribution carries the branch. The direct
    /// route is better only in the narrow case the fallback gets wrong: two
    /// leases on one branch name, where a lookup by branch can pick the other
    /// session's.
    #[test]
    fn a_pull_request_printed_by_a_session_holding_a_lease_reaches_that_lease() {
        let s = store();
        work(&s, "w1");
        let session = conversation(&s);
        let held = lease(&s, "w1", &session, "jod/first");

        let attribution = s.attribution_for(&session).unwrap();
        assert_eq!(
            attribution.lease_id,
            Some(held),
            "the attribution carries the lease it already joined to"
        );
        assert_eq!(attribution.branch.as_deref(), Some("jod/first"));

        opened_by(&s, "w1", &session, 41);
        let saved = s
            .pull_request("https://github.com/Reljod/Jod/pull/41")
            .unwrap()
            .expect("the row");
        assert_eq!(saved.lease_id, Some(held));
    }

    // ---- asking for a pull request --------------------------------------

    /// Complete every task on a board, which is what makes a work look
    /// finished to the ask.
    fn finished(s: &Store, tasks: &[String]) {
        for task in tasks {
            s.complete_work_task(task).unwrap();
        }
    }

    /// Ask, with the branch check answered yes for everything, recording what
    /// each session would have been told.
    fn ask_all(s: &Store) -> Vec<Ask> {
        let asks = asks_with(s, |_| true).unwrap();
        // The recording delivery a test wants is the identity: `asks_with` has
        // already done the deciding, and this is the write-it-down half.
        let mut done = Vec::new();
        for ask in asks {
            s.note_pull_request_asked(ask.candidate.lease_id, 1).unwrap();
            done.push(ask);
        }
        done
    }

    /// One session, one finished board, one branch with something on it.
    fn ready_session(s: &Store, work_id: &str, branch: &str) -> (String, i64) {
        let tasks = plan(s, work_id, &["the only task"]);
        let session = engineer(s, &tasks[0]);
        let lease = lease(s, work_id, &session, branch);
        s.complete_work_task(&tasks[0]).unwrap();
        (session, lease)
    }

    /// Opt-in, and it stays opt-in. Opening a pull request is externally
    /// visible and costs a turn, so a box whose owner never asked for this must
    /// never have it happen.
    #[test]
    fn nothing_is_asked_while_auto_pr_is_off() {
        let s = store();
        work(&s, "w1");
        ready_session(&s, "w1", "jod/first");

        assert!(
            asks_with(&s, |_| true).unwrap().is_empty(),
            "the setting defaults to off and nothing may be sent before it is turned on"
        );
        s.set_auto_pr(true).unwrap();
        assert_eq!(asks_with(&s, |_| true).unwrap().len(), 1, "and on, it asks");
    }

    /// A board with work left on it is not finished, whatever the session
    /// looks like it is doing.
    #[test]
    fn nothing_is_asked_while_a_task_is_still_open() {
        let s = store();
        s.set_auto_pr(true).unwrap();
        work(&s, "w1");
        let tasks = plan(&s, "w1", &["the board", "stacking"]);
        let session = engineer(&s, &tasks[0]);
        lease(&s, "w1", &session, "jod/first");

        s.complete_work_task(&tasks[0]).unwrap();
        assert!(
            asks_with(&s, |_| true).unwrap().is_empty(),
            "one of the two tasks is still open, so the job is not done"
        );

        s.complete_work_task(&tasks[1]).unwrap();
        assert_eq!(asks_with(&s, |_| true).unwrap().len(), 1);
    }

    /// **The one that matters on a loop that runs every minute.** Without the
    /// record, the same instruction is re-sent every tick for ever and each
    /// repeat spends a turn — an agent nagged hourly about a pull request it
    /// opened an hour ago.
    #[test]
    fn a_session_is_asked_once_and_not_again_on_the_next_tick() {
        let s = store();
        s.set_auto_pr(true).unwrap();
        work(&s, "w1");
        let (_, lease_id) = ready_session(&s, "w1", "jod/first");

        let first = ask_all(&s);
        assert_eq!(first.len(), 1, "the first tick asks");
        assert!(
            s.pull_request_asked_at(lease_id).unwrap().is_some(),
            "and writes down on the lease that it did"
        );

        assert!(
            ask_all(&s).is_empty(),
            "the second tick says nothing, and so does every tick after it"
        );
        assert!(ask_all(&s).is_empty());
    }

    /// A session that already opened one does not need asking, however it came
    /// to be recorded — parsed out of the stream or found by the poller.
    #[test]
    fn nothing_is_asked_when_the_branch_already_has_a_pull_request() {
        let s = store();
        s.set_auto_pr(true).unwrap();
        work(&s, "w1");
        let (session, _) = ready_session(&s, "w1", "jod/first");
        opened_by(&s, "w1", &session, 41);

        assert!(asks_with(&s, |_| true).unwrap().is_empty());
    }

    /// Each branch is based on the one below it, so its diff is only the part
    /// its own engineer wrote. The order is the plan's, the same one
    /// `stack_for_work` uses.
    #[test]
    fn a_second_lease_in_a_work_is_asked_to_stack_on_the_one_below_it() {
        let s = store();
        s.set_auto_pr(true).unwrap();
        work(&s, "w1");
        let tasks = plan(&s, "w1", &["the board", "stacking"]);
        let first = engineer(&s, &tasks[0]);
        let second = engineer(&s, &tasks[1]);
        lease(&s, "w1", &first, "jod/first");
        lease(&s, "w1", &second, "jod/second");
        finished(&s, &tasks);

        let asks = asks_with(&s, |_| true).unwrap();
        let stacked: Vec<(&str, Option<&str>)> = asks
            .iter()
            .map(|a| {
                (
                    a.candidate.branch.as_str(),
                    a.candidate.stacked_on.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            stacked,
            [("jod/first", None), ("jod/second", Some("jod/first"))],
            "the bottom of the stack is based on the trunk and nothing else is"
        );
        assert!(
            asks[1].instruction.contains("against `jod/first`"),
            "and the instruction says so: {}",
            asks[1].instruction
        );
    }

    /// The branch below is whatever is below it, not whatever is below it
    /// *and still being asked*. A lease whose pull request is already open is
    /// no longer a candidate, and basing the next one on the trunk instead
    /// would put that engineer's diff inside somebody else's pull request.
    #[test]
    fn a_lease_that_already_has_its_pull_request_is_still_the_branch_below() {
        let s = store();
        s.set_auto_pr(true).unwrap();
        work(&s, "w1");
        let tasks = plan(&s, "w1", &["the board", "stacking"]);
        let first = engineer(&s, &tasks[0]);
        let second = engineer(&s, &tasks[1]);
        lease(&s, "w1", &first, "jod/first");
        lease(&s, "w1", &second, "jod/second");
        finished(&s, &tasks);
        opened_by(&s, "w1", &first, 41);

        let asks = asks_with(&s, |_| true).unwrap();
        assert_eq!(asks.len(), 1, "only the second one still needs asking");
        assert_eq!(asks[0].candidate.branch, "jod/second");
        assert_eq!(
            asks[0].candidate.stacked_on.as_deref(),
            Some("jod/first"),
            "the branch below it has not moved just because it is no longer being asked"
        );
    }

    /// The write-down happens whether or not the delivery does, and the ask is
    /// handed out exactly as `asks` rendered it.
    #[test]
    fn asking_records_the_ask_and_hands_the_instruction_to_the_deliverer() {
        let s = store();
        s.set_auto_pr(true).unwrap();
        work(&s, "w1");
        let (_, lease_id) = ready_session(&s, "w1", "jod/first");

        let seen = std::cell::RefCell::new(Vec::new());
        let asked = ask_for_pull_requests_with(
            &s,
            |_| true,
            |ask| {
                seen.borrow_mut().push(ask.instruction.clone());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(asked.len(), 1);
        assert_eq!(seen.borrow().len(), 1);
        assert_eq!(seen.borrow()[0], asked[0].instruction);
        assert!(seen.borrow()[0].contains("create-pr"));
        assert!(s.pull_request_asked_at(lease_id).unwrap().is_some());
    }

    /// Against a real repository, because whether a branch has anything on it
    /// is a fact about git and a stub would test the stub.
    #[test]
    fn a_branch_with_nothing_on_it_is_not_worth_a_pull_request() {
        let (_guard, dir) = crate::leases::scratch("prs-ahead");
        let repo = crate::leases::fixture_repo(&dir.join("repo"));
        let base = git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
        git_ok(&repo, &["checkout", "--quiet", "-b", "jod/feature"]);

        assert!(
            !branch_is_ahead(&repo, &base, "jod/feature"),
            "a branch cut and left alone has nothing to open a pull request from"
        );

        std::fs::write(repo.join("new.md"), "work\n").expect("a file to commit");
        git_ok(&repo, &["add", "new.md"]);
        git_ok(
            &repo,
            &[
                "-c",
                "user.name=Jod Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "a commit",
            ],
        );

        assert!(
            branch_is_ahead(&repo, &base, "jod/feature"),
            "one commit the base does not have is a pull request worth asking for"
        );
    }

    /// Every way of failing to find out answers no, because an ask is recorded
    /// and never repeated: a wrong one has spent the turn for good.
    #[test]
    fn a_branch_check_that_cannot_be_answered_does_not_ask() {
        assert!(!branch_is_ahead(Path::new("/nonexistent/worktree"), "main", "jod/x"));
        assert!(!branch_is_ahead(Path::new("/tmp"), "", "jod/x"));
        assert!(!branch_is_ahead(Path::new("/tmp"), "main", ""));
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The charter is emphatic that a script decides what merges unread, and
    /// docs/spec-harness.md puts merging out of scope entirely. This is the guard that
    /// nobody quietly adds it here — the word is assembled a character at a
    /// time so the test does not find itself.
    #[test]
    fn nothing_in_this_module_can_merge_a_pull_request() {
        let source =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/prs.rs"))
                .expect("this module's own source");
        let argument = format!(
            "\"{}\"",
            ['m', 'e', 'r', 'g', 'e'].iter().collect::<String>()
        );
        assert!(
            !source.contains(&argument),
            "an argv element that would make `gh` merge something"
        );
    }
}
