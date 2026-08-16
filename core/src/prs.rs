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
        let row: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT c.work_id, l.branch
                   FROM conversations c
                   LEFT JOIN leases l
                     ON l.conversation_id = c.id AND l.state = 'held'
                  WHERE c.id = ?1
                  ORDER BY l.created_at_ms DESC
                  LIMIT 1",
                params![conversation_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (work_id, branch) = row.unwrap_or((None, None));
        Ok(Attribution {
            work_id,
            conversation_id: Some(conversation_id.to_string()),
            lease_id: None,
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
pub fn auto_pr_instruction(branch: &str, base: &str) -> String {
    format!(
        "Your work on `{branch}` looks finished. Open a pull request against `{base}` by \
         running the `create-pr` skill — it builds the body and the evidence bundle.\n\n\
         Open it as a **draft**. Do not merge it, do not mark it ready for review, and do not \
         run `gh pr merge`: whether this merges is decided by `merge_pr.sh` and by a person, \
         not here.\n\n\
         If anything blocks the pull request, write BLOCKED.md and stop — that is a successful \
         ending."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessKind;

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
        let text = auto_pr_instruction("feat/x", "main");
        assert!(text.contains("create-pr"), "the skill builds the evidence");
        assert!(text.contains("draft"));
        assert!(text.contains("feat/x") && text.contains("main"));
        assert!(
            text.contains("merge_pr.sh"),
            "and it says who does decide: {text}"
        );
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
