//! Fuzzy matching, in-process, with the positions needed to highlight it.
//!
//! Jod builds fzf's *feel* and depends on no picker binary. The target is the
//! interaction: type a few scattered letters, see ranked matches update on
//! every keystroke with the matched characters highlighted, move with the
//! arrows, accept with enter.
//!
//! Shelling out to `fzf` would actively prevent the good version of that.
//! `fzf` owns a whole terminal, so every `@` would tear down and restore the
//! screen, and an inline popup drawn under the cursor is not something an
//! external full-screen program can draw at all. So the matching lives here,
//! over a candidate list enumerated by ripgrep with a walker fallback, and no
//! picker binary is required, preferred, or supported.
//!
//! ## Why this is in core and not in the terminal
//!
//! Ranking is logic, not drawing. Everything here is testable without a
//! terminal, which is the rule that keeps the one-lane-owns-the-TUI split from
//! making the terminal a bottleneck — and it is a better shape regardless.
//!
//! ## The bar this is measured against
//!
//! - results **ranked**, not merely filtered
//! - matched characters highlighted in every row, which is why [`Match`]
//!   carries positions rather than only a score
//! - live on every keystroke with no perceptible lag on a large repository
//! - a deep exact path outranks a scattered-letters coincidence

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::error::Result;

/// One candidate that matched, with everything the renderer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Index into the candidate list that was searched.
    pub index: usize,
    /// Higher is better. Comparable only within one query's results.
    pub score: i32,
    /// Byte offsets in the candidate that the query matched, ascending.
    ///
    /// The reason a score alone is not enough: without these the popup can
    /// filter but cannot show *why* a row matched, and a fuzzy list you cannot
    /// read the match in is a list you stop trusting.
    pub positions: Vec<usize>,
}

// ---- scoring ----------------------------------------------------------
//
// The weights are relative to each other and to nothing else, so a score is
// only ever meaningful against another score for the same query. They are
// tuned to make the orderings the tests assert come out right; if you change
// one, the tests are the specification of what must not move.

/// What one matched character is worth before any bonus.
const SCORE_MATCH: i32 = 16;
/// A character matched immediately after the previous one. This is the whole
/// reason `src/rank` beats a candidate that merely contains those eight
/// letters somewhere.
const SCORE_CONSECUTIVE: i32 = 12;
/// Matching at the start of a path segment — after `/`, or at the very start.
const BONUS_SEGMENT: i32 = 14;
/// Matching after a word break inside a segment: `_`, `-`, `.`, a space.
const BONUS_WORD: i32 = 10;
/// Matching a camelCase hump.
const BONUS_CAMEL: i32 = 8;
/// The first matched character's boundary bonus counts double: where a match
/// *starts* says more about whether it is the match you meant than where it
/// happens to continue.
const FIRST_CHAR_MULTIPLIER: i32 = 2;
/// A character that matched with its case as typed. Small on purpose — case is
/// a tie-breaker between two matches of the same shape, never a reason to
/// prefer a structurally worse one.
const SCORE_CASE: i32 = 3;
/// Opening a gap between two matched characters.
const PENALTY_GAP_START: i32 = 5;
/// Each further character of that gap.
const PENALTY_GAP_EXTENSION: i32 = 1;
/// The whole match lies in the filename rather than the directories above it.
///
/// The single most important heuristic in a file picker: people type the name
/// of the file they want, and a directory that happens to contain those letters
/// is almost never what they meant. Large enough to beat a couple of boundary
/// bonuses, small enough that a long consecutive run in a directory still wins.
const BONUS_FILENAME: i32 = 40;

/// A candidate as an indexable sequence of characters.
///
/// Two implementations, one matcher. The byte implementation is the one that
/// runs: it decodes no UTF-8 and its element index *is* the byte offset, which
/// is what [`Match::positions`] carries. It is only ever selected for an ASCII
/// query, and an ASCII byte never occurs inside a multi-byte UTF-8 sequence, so
/// every position it reports is a character boundary the renderer can slice at.
///
/// Every accessor is `inline(always)` rather than plain `inline`, because these
/// are called several million times per keystroke over a large repository and
/// an unoptimised build — which is what `cargo test` measures, and what a
/// developer running Jod from `target/debug` uses — inlines nothing otherwise.
trait Text {
    fn len(&self) -> usize;
    /// Case-folded, for comparison.
    fn key(&self, i: usize) -> u32;
    /// As written, for the exact-case bonus and the camelCase test.
    fn raw(&self, i: usize) -> u32;
    /// Byte offset of element `i` in the original string.
    fn offset(&self, i: usize) -> usize;
}

struct Bytes<'a>(&'a [u8]);

/// Case folding as a table rather than a branch. `u8::to_ascii_lowercase` is a
/// call and a comparison per byte in an unoptimised build, and this is the
/// innermost thing the matcher does — several million times per keystroke over
/// a large repository.
static FOLD: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = if i >= b'A' as usize && i <= b'Z' as usize {
            (i as u8) + 32
        } else {
            i as u8
        };
        i += 1;
    }
    table
};

impl Text for Bytes<'_> {
    #[inline(always)]
    fn len(&self) -> usize {
        self.0.len()
    }
    #[inline(always)]
    fn key(&self, i: usize) -> u32 {
        FOLD[self.0[i] as usize] as u32
    }
    #[inline(always)]
    fn raw(&self, i: usize) -> u32 {
        self.0[i] as u32
    }
    #[inline(always)]
    fn offset(&self, i: usize) -> usize {
        i
    }
}

/// The path a non-ASCII query takes, where a byte is not a character and the
/// positions handed to the renderer would otherwise land mid-codepoint.
struct Chars<'a>(&'a [(usize, char)]);

impl Text for Chars<'_> {
    #[inline(always)]
    fn len(&self) -> usize {
        self.0.len()
    }
    #[inline(always)]
    fn key(&self, i: usize) -> u32 {
        lower(self.0[i].1)
    }
    #[inline(always)]
    fn raw(&self, i: usize) -> u32 {
        self.0[i].1 as u32
    }
    #[inline(always)]
    fn offset(&self, i: usize) -> usize {
        self.0[i].0
    }
}

fn lower(c: char) -> u32 {
    c.to_lowercase().next().unwrap_or(c) as u32
}

/// A query, folded once rather than once per candidate.
struct Query {
    keys: Vec<u32>,
    raw: Vec<u32>,
    ascii: bool,
}

impl Query {
    fn new(query: &str) -> Query {
        Query {
            keys: query.chars().map(lower).collect(),
            raw: query.chars().map(|c| c as u32).collect(),
            ascii: query.is_ascii(),
        }
    }
}

/// Reused across a whole `rank` call, so matching a hundred thousand candidates
/// allocates once rather than three times per candidate.
#[derive(Default)]
struct Scratch {
    /// One buffer per attempt in [`attempt`], holding element indices.
    attempts: [Vec<usize>; 3],
    /// Which attempt won, so the caller can turn the right one into byte
    /// offsets — and only when it actually wants them.
    winner: usize,
}

/// Score one candidate, and say which of its bytes the query matched.
///
/// `None` means the query is not a subsequence of the candidate at all — the
/// cheap rejection that most candidates take on every keystroke.
///
/// The returned [`Match::index`] is always 0: this function does not know which
/// list the candidate came from. [`rank`] fills it in.
pub fn match_candidate(query: &str, candidate: &str) -> Option<Match> {
    let q = Query::new(query);
    let mut scratch = Scratch::default();
    let mut positions = Vec::new();
    let score = match_one(&q, candidate, &mut scratch, Some(&mut positions))?;
    Some(Match {
        index: 0,
        score,
        positions,
    })
}

/// Rank `candidates` against `query`, best first, at most `limit` of them.
///
/// An empty query is the state the popup opens in, so it costs nothing: every
/// candidate comes back in input order with no matching work done at all.
pub fn rank(query: &str, candidates: &[String], limit: usize) -> Vec<Match> {
    if query.is_empty() {
        return candidates
            .iter()
            .take(limit)
            .enumerate()
            .map(|(index, _)| Match {
                index,
                score: 0,
                positions: Vec::new(),
            })
            .collect();
    }

    if limit == 0 {
        return Vec::new();
    }

    let q = Query::new(query);
    let mut scratch = Scratch::default();
    // Score first, positions later. A hundred thousand candidates yield a
    // hundred thousand `Vec<usize>` if positions are collected on the way past,
    // and the popup shows twenty rows — so the sweep carries three integers per
    // hit and the highlighting is worked out for the handful that survive.
    let mut hits: Vec<Hit> = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if let Some(score) = match_one(&q, candidate, &mut scratch, None) {
            hits.push(Hit {
                score,
                len: candidate.len(),
                index,
            });
        }
    }

    // Length breaks ties rather than entering the score, because a length
    // penalty large enough to separate two equal matches is also large enough
    // to demote a genuinely better match on a long path. Index breaks what is
    // left, which keeps the order a total one — the selection below is
    // unstable, and two runs of the same query must not disagree.
    let better = |a: &Hit, b: &Hit| {
        b.score
            .cmp(&a.score)
            .then(a.len.cmp(&b.len))
            .then(a.index.cmp(&b.index))
    };
    if hits.len() > limit {
        // Partition rather than sort: the popup wants the best twenty of a
        // hundred thousand, and paying `n log n` to order the other 99,980
        // rows is most of the cost of a keystroke.
        hits.select_nth_unstable_by(limit - 1, better);
        hits.truncate(limit);
    }
    hits.sort_by(better);

    hits.into_iter()
        .map(|hit| {
            let mut positions = Vec::new();
            let score = match_one(
                &q,
                &candidates[hit.index],
                &mut scratch,
                Some(&mut positions),
            )
            .unwrap_or(hit.score);
            Match {
                index: hit.index,
                score,
                positions,
            }
        })
        .collect()
}

/// A matched candidate during the sweep, before anyone asks where it matched.
struct Hit {
    score: i32,
    len: usize,
    index: usize,
}

/// Score `candidate`, and fill `positions` with the byte offsets that matched
/// when the caller asks for them.
fn match_one(
    q: &Query,
    candidate: &str,
    scratch: &mut Scratch,
    positions: Option<&mut Vec<usize>>,
) -> Option<i32> {
    if q.keys.is_empty() {
        if let Some(out) = positions {
            out.clear();
        }
        return Some(0);
    }
    // The first byte after the last `/`. `rfind` on the string rather than a
    // scan over the sequence: it is a memchr backwards, and it runs for every
    // candidate on every keystroke.
    let basename_byte = candidate.rfind('/').map_or(0, |i| i + 1);
    if q.ascii {
        let text = Bytes(candidate.as_bytes());
        let score = attempt(q, &text, basename_byte, scratch)?;
        emit(scratch, &text, positions);
        Some(score)
    } else {
        let chars: Vec<(usize, char)> = candidate.char_indices().collect();
        let basename = chars
            .iter()
            .position(|(off, _)| *off >= basename_byte)
            .unwrap_or(chars.len());
        let text = Chars(&chars);
        let score = attempt(q, &text, basename, scratch)?;
        emit(scratch, &text, positions);
        Some(score)
    }
}

/// Turn the winning attempt's element indices into the byte offsets the
/// renderer highlights.
fn emit(scratch: &Scratch, text: &impl Text, positions: Option<&mut Vec<usize>>) {
    let Some(out) = positions else { return };
    out.clear();
    out.extend(
        scratch.attempts[scratch.winner]
            .iter()
            .map(|&i| text.offset(i)),
    );
}

/// Match up to three times and keep the best: over the whole candidate, over
/// the filename alone, and — when the first two anchored in the middle of a
/// word — from the next boundary that could start a match.
///
/// A greedy match takes the leftmost letters it can, and that is regularly not
/// the match a person meant. `rank` against `core/src/rank.rs` would take the
/// `r` of `core` and the `an` of `rank`, scoring the file people obviously want
/// as a scattered coincidence in a directory name. The extra attempts are the
/// cheap half of what an exhaustive search would buy, at one more pass each —
/// and they are what let a match that lies wholly in the filename be found at
/// all, which is the precondition for earning [`BONUS_FILENAME`].
fn attempt(q: &Query, text: &impl Text, basename: usize, scratch: &mut Scratch) -> Option<i32> {
    if !tighten(&q.keys, text, 0, &mut scratch.attempts[0]) {
        return None;
    }
    let mut best = score(q, text, &scratch.attempts[0], basename);
    scratch.winner = 0;

    if basename > 0
        && basename < text.len()
        && tighten(&q.keys, text, basename, &mut scratch.attempts[1])
    {
        let filename = score(q, text, &scratch.attempts[1], basename);
        if filename > best {
            best = filename;
            scratch.winner = 1;
        }
    }

    // A match anchored mid-word is the one case where greedy reliably picks
    // the wrong occurrence: typing `s` at `parseSpec` means the hump, not the
    // `s` of `parse`. One restart, at the next boundary that could begin a
    // match, is enough to cover it — and it costs nothing at all for the
    // overwhelmingly common case where the match already starts at a `/`.
    let anchored_at = scratch.attempts[scratch.winner][0];
    if boundary_bonus(text, anchored_at) == 0 {
        if let Some(from) = next_boundary_start(&q.keys, text, anchored_at + 1) {
            if tighten(&q.keys, text, from, &mut scratch.attempts[2]) {
                let realigned = score(q, text, &scratch.attempts[2], basename);
                if realigned > best {
                    best = realigned;
                    scratch.winner = 2;
                }
            }
        }
    }

    Some(best)
}

/// The next element at or after `from` that both matches the query's first
/// character and sits at a boundary.
fn next_boundary_start(query: &[u32], text: &impl Text, from: usize) -> Option<usize> {
    let want = query[0];
    (from..text.len()).find(|&i| text.key(i) == want && boundary_bonus(text, i) > 0)
}

/// Find the tightest match starting at or after `from`, writing its element
/// indices into `out`. `false` when the query is not a subsequence of that
/// span.
///
/// Forward pass, then backward, then forward again — the classic three-pass
/// greedy. The first forward pass finds where the earliest possible match
/// *ends*; walking backwards from there finds the latest possible start for a
/// match ending at that point, which squeezes out the leading slack a plain
/// greedy leaves behind (`rank` against `core/src/rank.rs` otherwise anchors on
/// the `r` of `core`). It is not an exhaustive search — full dynamic
/// programming would be, and would cost a hundred times as much for an ordering
/// that differs on candidates nobody is choosing between.
fn tighten(query: &[u32], text: &impl Text, from: usize, out: &mut Vec<usize>) -> bool {
    let len = text.len();
    let mut i = from;
    for &want in query {
        loop {
            if i >= len {
                return false;
            }
            let hit = text.key(i) == want;
            i += 1;
            if hit {
                break;
            }
        }
    }
    let end = i;

    let mut j = end;
    for &want in query.iter().rev() {
        loop {
            j -= 1;
            if text.key(j) == want {
                break;
            }
        }
    }

    out.clear();
    let mut k = j;
    for &want in query {
        while text.key(k) != want {
            k += 1;
        }
        out.push(k);
        k += 1;
    }
    true
}

fn score(q: &Query, text: &impl Text, idx: &[usize], basename: usize) -> i32 {
    let mut total = 0;
    let mut prev: Option<usize> = None;
    for (k, &i) in idx.iter().enumerate() {
        let bonus = boundary_bonus(text, i);
        let mut points = SCORE_MATCH;
        match prev {
            None => points += bonus * FIRST_CHAR_MULTIPLIER,
            Some(p) if p + 1 == i => points += SCORE_CONSECUTIVE + bonus,
            Some(p) => {
                let gap = (i - p - 1) as i32;
                points += bonus - PENALTY_GAP_START - (gap - 1) * PENALTY_GAP_EXTENSION;
            }
        }
        if text.raw(i) == q.raw[k] {
            points += SCORE_CASE;
        }
        total += points;
        prev = Some(i);
    }
    if idx.first().is_some_and(|&i| i >= basename) {
        total += BONUS_FILENAME;
    }
    total
}

fn boundary_bonus(text: &impl Text, i: usize) -> i32 {
    if i == 0 {
        return BONUS_SEGMENT;
    }
    let before = text.key(i - 1);
    if before == '/' as u32 {
        return BONUS_SEGMENT;
    }
    if before == '_' as u32 || before == '-' as u32 || before == '.' as u32 || before == ' ' as u32
    {
        return BONUS_WORD;
    }
    // A hump only counts as a boundary when the character before it was
    // lower-case as written — `HTTPServer` has one hump, at the `S`, and the
    // folded key cannot tell that from `httpserver`.
    if is_lower_or_digit(text.raw(i - 1)) && is_upper(text.raw(i)) {
        return BONUS_CAMEL;
    }
    0
}

/// Both of these take the ASCII answer without touching `char`'s Unicode
/// tables, which at `-O0` are a call and a binary search per character.
#[inline(always)]
fn is_lower_or_digit(c: u32) -> bool {
    if c < 128 {
        let b = c as u8;
        return b.is_ascii_lowercase() || b.is_ascii_digit();
    }
    char::from_u32(c).is_some_and(|c| c.is_lowercase() || c.is_numeric())
}

#[inline(always)]
fn is_upper(c: u32) -> bool {
    if c < 128 {
        return (c as u8).is_ascii_uppercase();
    }
    char::from_u32(c).is_some_and(char::is_uppercase)
}

// ---- candidates -------------------------------------------------------

/// How long an enumeration is reused before the tree is read again.
///
/// `@` is typed one character at a time and the popup re-ranks on every
/// keystroke; walking a large repository per keystroke is precisely the thing
/// that makes a picker feel bad. Five seconds is long enough that a burst of
/// typing costs one walk, and short enough that a file you created in another
/// pane is there by the time you go looking for it. The mtime check below
/// usually gets there first anyway.
const CACHE_TTL: Duration = Duration::from_secs(5);

/// Directory names that are never worth offering — the one definition of
/// noise in this program.
///
/// It used to be three copies: this list, the fallback walker's, and the
/// `/add-dir` picker's. They disagreed, and the disagreement was visible —
/// `/add-dir ~/tetris` offered `src`, while `@` in the same tree offered
/// `dist` and `node_modules`. One list, three consumers: the ripgrep call
/// below turns it into `--glob` exclusions, [`walk`] refuses to descend into
/// it, and the picker imports it.
///
/// `.venv` is here as well as `venv` because the mention path passes
/// `--hidden`; without it, `--hidden` is exactly what drags a Python
/// environment into the list.
///
/// Not a security measure. A root can still *point* at any of these — this is
/// only about what gets offered before you have typed anything.
pub const NOISE: [&str; 7] = [
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "venv",
    ".venv",
];

struct Cached {
    at: Instant,
    /// The root directory's own mtime, which changes the moment a file is
    /// added or removed directly inside it. Not a full stamp of the tree —
    /// that would cost the walk this cache exists to avoid — but it catches
    /// the common case (you just created the file you are now looking for)
    /// without waiting out the TTL.
    stamp: Option<SystemTime>,
    entries: Arc<Vec<String>>,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, Cached>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Every file and directory under `root`, as paths relative to it.
///
/// Directories are in the list because `@` mentions them: a folder mention
/// expands to a listing at send time, which is how you hand an agent a
/// subtree without naming twenty files.
pub fn candidates(root: &Path) -> Result<Vec<String>> {
    Ok(candidates_shared(root)?.as_ref().clone())
}

/// [`candidates`] without the copy — the per-keystroke path.
///
/// A hundred thousand paths is a few megabytes of `String`, and cloning that
/// on every keystroke would hand back exactly the stall the cache was added to
/// remove. The list is immutable once built, so callers share one.
pub fn candidates_shared(root: &Path) -> Result<Arc<Vec<String>>> {
    let key = crate::roots::normalise(root);
    let stamp = std::fs::metadata(&key).ok().and_then(|m| m.modified().ok());

    {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(&key) {
            if hit.at.elapsed() < CACHE_TTL && hit.stamp == stamp {
                return Ok(hit.entries.clone());
            }
        }
    }

    let fresh = Arc::new(enumerate(&key)?);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(
        key,
        Cached {
            at: Instant::now(),
            stamp,
            entries: fresh.clone(),
        },
    );
    Ok(fresh)
}

/// Forget every cached enumeration. For an explicit refresh, and for tests that
/// need to see the filesystem as it is rather than as it was.
pub fn clear_candidate_cache() {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

fn enumerate(root: &Path) -> Result<Vec<String>> {
    match ripgrep_files(root) {
        Some(files) => Ok(with_ancestors(files)),
        None => walk(root),
    }
}

/// Ask ripgrep for the files. `None` means it is not installed or could not
/// run, and the caller should walk the tree itself.
///
/// `--hidden` because a dotfile is a file people mention. That flag is also
/// what made [`NOISE`] mandatory here rather than merely nice.
///
/// The original reasoning was that ripgrep leaves `target` and `node_modules`
/// out by reading `.gitignore`. True — **inside a git repository**. A plain
/// directory has no `.gitignore`, so nothing filters at all, and what had been
/// quietly holding the line was that pnpm keeps the real files in the *hidden*
/// `node_modules/.pnpm/` and exposes packages as symlinks: ripgrep skips
/// hidden directories and does not follow links. `--hidden` switches off
/// exactly that accident. In a freshly scaffolded, not-yet-`git init`-ed
/// project — the normal state of the thing you most want to `@` — the list
/// came back 95% dependencies.
///
/// So the exclusions are stated rather than inherited: the guarantee no longer
/// depends on the directory happening to be a repository. `.gitignore` is
/// still read where there is one, which is still why ripgrep beats the walker.
fn ripgrep_files(root: &Path) -> Option<Vec<String>> {
    let rg = crate::discovery::find_binary("JOD_RIPGREP_BIN", &["rg"], &[])?;
    let mut args: Vec<String> = vec!["--files".into(), "--hidden".into()];
    for name in NOISE {
        // A glob with no `/` matches the basename at any depth, and one that
        // matches a directory excludes everything under it — gitignore
        // semantics, which is what the list already meant.
        args.push("--glob".into());
        args.push(format!("!{name}"));
    }
    let out = std::process::Command::new(rg)
        .args(&args)
        .current_dir(root)
        .output()
        .ok()?;
    // ripgrep exits 1 on "no files", which is an answer rather than a failure.
    // Anything else that produced no output is a failure to enumerate — a
    // non-existent directory, a binary that is not ripgrep — and the walker
    // gets its turn rather than the picker silently showing nothing.
    if !out.status.success() && out.stdout.is_empty() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// ripgrep lists files, and the picker offers directories too, so the
/// directories are the ancestors of the files.
///
/// Derived rather than walked for: a `--files` run has already paid for the
/// traversal, and every directory that holds a listable file appears here. An
/// empty directory does not, which is the one thing this misses and the
/// cheapest possible thing to miss.
fn with_ancestors(files: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(files.len() * 2);
    for file in &files {
        for (i, ch) in file.char_indices() {
            if ch == '/' {
                out.push(file[..i].to_string());
            }
        }
    }
    out.extend(files);
    out.sort();
    out.dedup();
    out
}

/// Walk the tree ourselves, for a machine with no ripgrep.
///
/// Deliberately simple: it cannot read `.gitignore`, so it skips [`NOISE`] and
/// nothing else, and a repository with a large ignored directory outside that
/// list will list more than ripgrep would. That is a worse candidate list, not
/// a broken one, and the alternative — reimplementing gitignore semantics — is
/// a project rather than a fallback.
fn walk(root: &Path) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    // An explicit stack rather than recursion: a deep tree should cost memory
    // we can see, not stack we cannot.
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    let mut at_root = true;

    while let Some((dir, prefix)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // The root failing is the caller's answer — they named a directory
            // that is not there. A subdirectory failing is a fact about the
            // tree, usually one root owns, and a picker that returns nothing
            // because of one unreadable directory is worse than one that
            // returns everything else.
            Err(e) if at_root => return Err(e.into()),
            Err(_) => continue,
        };
        at_root = false;

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            // `file_type` does not follow symlinks, so a link to an ancestor
            // is listed as an entry and never descended into. That is the
            // whole cycle protection, and it is enough.
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if NOISE.contains(&name.as_str()) {
                    continue;
                }
                stack.push((entry.path(), rel.clone()));
            }
            out.push(rel);
        }
    }

    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The candidate cache is one map for the whole process, so the two tests
    /// that assert on cache *behaviour* would otherwise race each other's
    /// `clear`. Every other test here touches candidates for its own root and
    /// needs no lock.
    static CACHE_TESTS: Mutex<()> = Mutex::new(());

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jod-rank-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::roots::normalise(&dir)
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn score_of(query: &str, candidate: &str) -> i32 {
        match_candidate(query, candidate)
            .unwrap_or_else(|| panic!("`{query}` should match `{candidate}`"))
            .score
    }

    fn ordered(query: &str, candidates: &[&str]) -> Vec<String> {
        let owned: Vec<String> = candidates.iter().map(|c| c.to_string()).collect();
        rank(query, &owned, 100)
            .into_iter()
            .map(|m| owned[m.index].clone())
            .collect()
    }

    // ---- matching ------------------------------------------------------

    #[test]
    fn scattered_letters_match_as_a_subsequence() {
        assert!(match_candidate("crk", "core/src/rank.rs").is_some());
        assert!(match_candidate("mrs", "src/main.rs").is_some());
    }

    #[test]
    fn a_candidate_missing_one_query_character_does_not_match() {
        assert!(match_candidate("rankz", "core/src/rank.rs").is_none());
        // Order matters: the letters are all there, in the wrong sequence.
        assert!(match_candidate("knar", "rank.rs").is_none());
    }

    #[test]
    fn a_consecutive_run_outscores_the_same_letters_scattered() {
        assert!(
            score_of("abc", "abc.txt") > score_of("abc", "a_b_c.txt"),
            "consecutive {} vs scattered {}",
            score_of("abc", "abc.txt"),
            score_of("abc", "a_b_c.txt")
        );
    }

    #[test]
    fn a_match_at_a_path_segment_boundary_outscores_one_mid_word() {
        assert!(score_of("rank", "core/src/rank.rs") > score_of("rank", "core/src/prank.rs"));
    }

    #[test]
    fn a_match_after_a_word_break_outscores_one_mid_word() {
        assert!(score_of("log", "audit_log.rs") > score_of("log", "catalogue.rs"));
    }

    /// Both spellings contain the query; only one has a hump where it starts.
    /// The one-character query is in here on purpose: it is the case a plain
    /// greedy match gets wrong, because the `s` of `parse` comes first.
    #[test]
    fn a_match_on_a_camel_case_hump_outscores_one_mid_word() {
        assert!(score_of("spec", "parseSpec") > score_of("spec", "parsespec"));
        assert!(score_of("s", "parseSpec") > score_of("s", "parsespec"));
    }

    #[test]
    fn a_match_in_the_filename_outscores_the_same_match_in_a_directory() {
        assert_eq!(
            ordered("rank", &["rank/src/core.rs", "core/src/rank.rs"]),
            ["core/src/rank.rs", "rank/src/core.rs"]
        );
    }

    /// docs/spec-harness.md names this assertion as E1's check: the picker ranks a deep
    /// exact path above a scattered-letters match.
    #[test]
    fn a_deep_exact_path_outranks_a_scattered_letters_coincidence() {
        let ranked = ordered(
            "src/rank",
            &[
                "server/config/rules/apply-now/knob.rs",
                "core/src/rank.rs",
                "supervisor/src/lib.rs",
            ],
        );
        assert_eq!(ranked[0], "core/src/rank.rs");
        assert_eq!(
            ranked.last().unwrap(),
            "server/config/rules/apply-now/knob.rs",
            "a coincidence spread over five segments is the worst match here"
        );
    }

    #[test]
    fn the_shorter_of_two_equally_good_candidates_comes_first() {
        assert_eq!(
            score_of("rs", "a.rs"),
            score_of("rs", "ab.rs"),
            "the two are the same match, so only length can separate them"
        );
        assert_eq!(ordered("rs", &["ab.rs", "a.rs"]), ["a.rs", "ab.rs"]);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(match_candidate("README", "readme.md").is_some());
        assert!(match_candidate("readme", "README.md").is_some());
    }

    #[test]
    fn an_exact_case_match_outranks_a_case_insensitive_one() {
        assert_eq!(
            ordered("readme", &["README.md", "readme.md"]),
            ["readme.md", "README.md"]
        );
    }

    /// Case is a tie-breaker, never a reason to prefer a structurally worse
    /// match — otherwise typing lower case would demote the file you meant.
    #[test]
    fn case_never_outweighs_where_the_match_lands() {
        assert!(score_of("rank", "src/RANK.rs") > score_of("rank", "rank/src/other.rs"));
    }

    #[test]
    fn an_empty_query_returns_every_candidate_in_input_order() {
        let all: Vec<String> = ["z.rs", "a.rs", "m.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ranked = rank("", &all, 10);
        assert_eq!(
            ranked.iter().map(|m| m.index).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(ranked.iter().all(|m| m.positions.is_empty()));
    }

    #[test]
    fn positions_point_at_the_characters_that_matched() {
        let m = match_candidate("rank", "core/src/rank.rs").unwrap();
        assert_eq!(m.positions, [9, 10, 11, 12]);
        let matched: String = m
            .positions
            .iter()
            .map(|&i| "core/src/rank.rs"[i..].chars().next().unwrap())
            .collect();
        assert_eq!(matched, "rank", "this is what the popup highlights");
    }

    #[test]
    fn rank_returns_at_most_the_limit_asked_for() {
        let all: Vec<String> = (0..50).map(|i| format!("src/file{i}.rs")).collect();
        assert_eq!(rank("file", &all, 7).len(), 7);
    }

    /// Positions are byte offsets and the renderer slices at them, so one
    /// landing inside a codepoint would panic the TUI rather than mis-render.
    #[test]
    fn positions_in_a_non_ascii_candidate_land_on_character_boundaries() {
        let candidate = "café/naïve/résumé.txt";
        for query in ["résumé", "txt", "cnr"] {
            let m = match_candidate(query, candidate)
                .unwrap_or_else(|| panic!("`{query}` should match"));
            for &p in &m.positions {
                assert!(
                    candidate.is_char_boundary(p),
                    "byte {p} of `{candidate}` is mid-codepoint for query `{query}`"
                );
            }
        }
    }

    // ---- performance ---------------------------------------------------

    fn time(run: impl FnOnce()) -> Duration {
        let started = Instant::now();
        run();
        started.elapsed()
    }

    /// A hundred thousand files is a large monorepo, and `@` re-ranks the whole
    /// list on every keystroke. The query is deliberately the expensive shape:
    /// seven characters every candidate matches, spanning almost the whole
    /// path, so nothing is rejected early.
    ///
    /// The budget depends on the build, because the two differ by an order of
    /// magnitude and only one of them ships. On an idle machine this measures
    /// 14ms optimised and 175ms not; the sweep is linear in the candidate count
    /// end to end, at 110–175ns each from a thousand candidates to a hundred
    /// thousand.
    ///
    /// The budgets sit an order of magnitude above the idle measurement on
    /// purpose. This test runs inside a suite of nine hundred others, on a
    /// machine that may be building four other branches at the same time, and
    /// under that the same code takes 600ms — so a tight bound would fail for
    /// reasons that have nothing to do with this file, and a perf test that
    /// fails when a colleague starts a build is one people learn to ignore. It
    /// is still the assertion that matters: every way of getting this wrong
    /// that anyone would actually write — a rescan per candidate, a sort inside
    /// the loop, re-enumerating per keystroke — costs seconds here, not
    /// milliseconds.
    ///
    /// The best of three runs, because the fastest sample is the one least
    /// contaminated by somebody else's scheduling.
    #[test]
    fn ranking_a_hundred_thousand_candidates_stays_within_one_frame() {
        let candidates: Vec<String> = (0..100_000)
            .map(|i| format!("crate{}/src/module{}/file{i}.rs", i % 50, i % 997))
            .collect();

        let mut best = Duration::MAX;
        let mut kept = 0;
        for _ in 0..3 {
            best = best.min(time(|| kept = rank("srcfile", &candidates, 100).len()));
        }
        assert_eq!(kept, 100);

        let budget = if cfg!(debug_assertions) {
            Duration::from_millis(1500)
        } else {
            Duration::from_millis(250)
        };
        assert!(
            best < budget,
            "ranking 100k candidates took {best:?}, budget {budget:?}"
        );
    }

    /// D1: Jod builds fzf's *feel* and depends on no picker binary — shelling
    /// out to one would make the inline popup impossible to draw. Asserted
    /// against the source so nobody quietly reintroduces one; the names are
    /// assembled a character at a time so this test does not find itself.
    #[test]
    fn the_picker_depends_on_no_external_picker_binary() {
        let pickers = [
            ['f', 'z', 'f'].iter().collect::<String>(),
            ['f', 'z', 'y'].iter().collect::<String>(),
            ['s', 'k', 'i', 'm'].iter().collect::<String>(),
            ['p', 'e', 'c', 'o'].iter().collect::<String>(),
        ];
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let files = walk(&src).expect("the crate's own source");

        for file in files.iter().filter(|f| f.ends_with(".rs")) {
            let text = std::fs::read_to_string(src.join(file)).unwrap();
            for name in &pickers {
                // Quoted, so the prose above — which talks about fzf at
                // length — is not what this trips over. A binary is spawned by
                // its name in a string literal.
                assert!(
                    !text.contains(&format!("\"{name}\"")),
                    "{file} names the picker binary {name}; the matcher is in-process"
                );
            }
        }
    }

    // ---- candidates ----------------------------------------------------

    #[test]
    fn the_walker_lists_files_and_directories_and_skips_the_noisy_ones() {
        let dir = scratch("walk");
        write(&dir.join("a.txt"), "");
        write(&dir.join("sub/b.txt"), "");
        write(&dir.join(".hidden"), "");
        write(&dir.join(".git/config"), "");
        write(&dir.join("node_modules/left-pad/index.js"), "");
        write(&dir.join("target/debug/binary"), "");

        let found = walk(&dir).unwrap();
        assert!(found.contains(&"a.txt".to_string()));
        assert!(
            found.contains(&"sub".to_string()),
            "folders are mentionable"
        );
        assert!(found.contains(&"sub/b.txt".to_string()));
        assert!(
            found.contains(&".hidden".to_string()),
            "a dotfile is a file people mention"
        );
        for noise in [".git", "node_modules", "target"] {
            assert!(
                !found.iter().any(|f| f.starts_with(noise)),
                "{noise} should not be in the list: {found:?}"
            );
        }
    }

    /// A build directory is noise wherever it is found, and the walker used to
    /// know about three of the seven names the picker knew about.
    #[test]
    fn the_walker_skips_every_name_the_shared_list_holds() {
        let dir = scratch("walk-noise");
        for name in NOISE {
            write(&dir.join(name).join("buried.txt"), "");
        }
        write(&dir.join("src/main.rs"), "");

        let found = walk(&dir).unwrap();
        assert!(found.contains(&"src/main.rs".to_string()), "{found:?}");
        for name in NOISE {
            assert!(
                !found.iter().any(|f| f.starts_with(name)),
                "{name} survived the walk: {found:?}"
            );
        }
    }

    /// BUG-15, at the level the user meets it: a project scaffolded and
    /// `npm install`-ed but not yet `git init`-ed.
    ///
    /// No `.git` and no `.gitignore`, so nothing filters by inheritance — the
    /// state the old comment assumed away. The fixture mirrors what pnpm
    /// actually lays down, real files under the *hidden* `node_modules/.pnpm/`,
    /// because that hidden directory is what `--hidden` un-hid.
    #[test]
    fn a_project_that_is_not_a_git_repo_offers_its_source_not_its_dependencies() {
        let dir = scratch("non-git");
        assert!(!dir.join(".git").exists(), "the fixture must not be a repo");
        write(&dir.join("src/engine.js"), "export const board = [];");
        write(&dir.join("index.html"), "<html>");
        write(&dir.join("package.json"), "{}");
        write(&dir.join("dist/assets/index-CGus7geV.js"), "");
        for n in 0..40 {
            write(
                &dir.join(format!(
                    "node_modules/.pnpm/pkg{n}@1.0.0/node_modules/pkg{n}/index.js"
                )),
                "",
            );
        }

        let found = candidates(&dir).unwrap();
        assert!(
            found.contains(&"src/engine.js".to_string()),
            "the source is not offered at all: {found:?}"
        );
        for noise in ["node_modules", "dist"] {
            assert!(
                !found.iter().any(|f| f.starts_with(noise)),
                "{noise} floods the list: {} of {} paths",
                found.iter().filter(|f| f.starts_with(noise)).count(),
                found.len()
            );
        }
    }

    /// What `--hidden` was added for, and what removing it would have cost.
    #[test]
    fn a_dotfile_is_still_offered_in_a_directory_that_is_not_a_repo() {
        let dir = scratch("non-git-dotfiles");
        write(&dir.join(".env"), "KEY=1");
        write(&dir.join(".github/workflows/ci.yml"), "on: push");
        write(&dir.join("node_modules/.pnpm/left-pad@1/index.js"), "");

        let found = candidates(&dir).unwrap();
        assert!(found.contains(&".env".to_string()), "{found:?}");
        assert!(
            found.contains(&".github/workflows/ci.yml".to_string()),
            "{found:?}"
        );
        assert!(!found.iter().any(|f| f.starts_with("node_modules")));
    }

    #[test]
    fn the_walker_refuses_a_root_that_is_not_there() {
        let missing = std::env::temp_dir().join(format!("jod-rank-gone-{}", std::process::id()));
        assert!(walk(&missing).is_err());
    }

    /// ripgrep's `--files` lists files only, and `@` mentions folders too.
    #[test]
    fn directories_are_derived_from_ripgreps_file_list() {
        let derived = with_ancestors(vec![
            "core/src/rank.rs".to_string(),
            "core/src/roots.rs".to_string(),
            "README.md".to_string(),
        ]);
        assert_eq!(
            derived,
            [
                "README.md",
                "core",
                "core/src",
                "core/src/rank.rs",
                "core/src/roots.rs",
            ],
            "every ancestor once, no duplicates"
        );
    }

    #[test]
    fn candidates_lists_the_tree_relative_to_its_root() {
        let dir = scratch("candidates");
        write(&dir.join("src/main.rs"), "fn main() {}");
        write(&dir.join("README.md"), "# hi");

        let found = candidates(&dir).unwrap();
        assert!(found.contains(&"src/main.rs".to_string()));
        assert!(found.contains(&"README.md".to_string()));
        assert!(found.contains(&"src".to_string()));
    }

    /// The reason the cache exists: `@` is typed a character at a time, and a
    /// keystroke must not cost a walk of the repository.
    #[test]
    fn a_second_look_within_the_ttl_is_served_from_the_cache() {
        let _guard = CACHE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("cache-hit");
        write(&dir.join("src/one.rs"), "");
        clear_candidate_cache();

        let first = candidates(&dir).unwrap();
        assert!(first.contains(&"src/one.rs".to_string()));

        // Written into a subdirectory on purpose: it leaves the root's own
        // mtime alone, which is what isolates the TTL from the mtime check.
        write(&dir.join("src/two.rs"), "");
        let second = candidates(&dir).unwrap();
        assert_eq!(first, second, "the walk was not repeated");

        clear_candidate_cache();
        assert!(candidates(&dir)
            .unwrap()
            .contains(&"src/two.rs".to_string()));
    }

    /// The file you just created is the file you are about to look for, so
    /// waiting out the TTL for it would be the cache's most annoying failure.
    #[test]
    fn a_file_created_in_the_root_shows_up_without_waiting_out_the_ttl() {
        let _guard = CACHE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("cache-mtime");
        write(&dir.join("first.rs"), "");
        clear_candidate_cache();
        assert!(candidates(&dir).unwrap().contains(&"first.rs".to_string()));

        write(&dir.join("second.rs"), "");
        assert!(
            candidates(&dir).unwrap().contains(&"second.rs".to_string()),
            "adding a file changes the root's mtime, which retires the entry"
        );
    }
}
