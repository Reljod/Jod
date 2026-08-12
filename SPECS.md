# SPEC — Jod as the coding harness

> Six epics. Each one ships on its own branch, behind its own check, in the
> order given — later epics read tables earlier ones create.
>
> Task ids are stable (`E2.S1.T3`). Quote them in branch names, commits and PR
> bodies so a half-finished epic is legible to the next session.

## Goal

Make `jod tui` the surface Reljod codes in, instead of `claude` — without
becoming a harness. Six user-visible changes:

1. **A session has working directories** (plural). `@` fuzzy-picks a file or
   folder across all of them; content search goes through ripgrep. With no
   roots set, `@` says so instead of silently searching `$PWD`.
2. **A left rail carries the agent's decisions and its open questions.** An
   autonomous choice arrives as a small card ("chat DB: chose SQLite — switch?");
   a real blocker arrives with a coloured border and the word `blocked`. Expand
   a card to pick an option or answer in prose. Answered cards leave the stack
   and stay findable — filter by text, sort by importance or age.
3. **Credentials come in through that same rail and never reach the model.**
   The value lands in a 0600 file outside every repo, is injected as an
   environment variable at spawn, and is scrubbed out of every line the harness
   prints before it is stored. The agent is told a *name*, never a value — so a
   missing key blocks one test, not the session.
4. **The main chat is an orchestrator over a tree of sessions.** "Work on
   @some-repo, do X" opens a *work*: a titled group of sessions, each pointed at
   a git worktree it owns, none of them able to touch the original checkout.
   The orchestrator never blocks; it delegates and comes back.
5. **Fleet becomes that tree.** Arrow keys walk it, `→`/`←`/`space` expand and
   collapse, `⏎` opens. Every node shows whether it is running and what it is
   doing. Cards from every descendant cascade up to the orchestrator's rail,
   colour-coded per work.
6. **The experience is the same on all three harnesses**, because everything
   above is Jod's, not Claude Code's. Slash commands and skills found in the
   repo are offered in Jod's palette; pull requests opened by a run are shown,
   and auto-PR is a toggle.

## Vocabulary

Fixed here because six epics use these words and a drifting noun is a bug.

| Word | Means |
|---|---|
| **root** | An absolute directory a conversation may read. A conversation has zero or more. |
| **work** | One intent, spanning several conversations. Titled and summarised by a throwaway model call. Owns a colour. |
| **project-session** | A conversation belonging to a work. Not a new type — a `conversations` row with `work_id` set. |
| **card** | One row in the left rail: a `decision`, a `question`, or a `secret` request. |
| **lease** | A git worktree bound to one conversation, tracked so siblings can reuse it. |
| **redactor** | The supervisor filter that replaces a live secret's value with `«redacted:NAME»` in every line before it is parsed. |

## The shape

```
    ┌──────────────── jod tui ─────────────────────────────────────┐
    │ rail (left)         chat / fleet-tree        panel (right)   │
    │ ┌───────────┐   ┌──────────────────────┐   ┌──────────────┐  │
    │ │ ▸ decision│   │  main = orchestrator  │   │ sessions     │  │
    │ │ ! blocked │   │   ├ work: jod-api     │   │ context      │  │
    │ │ ⚿ secret  │   │   │  ├ session A ●    │   └──────────────┘  │
    │ └───────────┘   │   │  └ session B      │                     │
    │      ▲          │   └ work: apps/web    │                     │
    │      │ cascade  └──────────────────────┘                      │
    └──────┼───────────────────────────────────────────────────────┘
           │  cards from every descendant
    ┌──────┴────────────────────────────────────────────────────────┐
    │ jod.db :: cards · works · roots · worktrees · pull_requests    │
    └───────────────────────────────────────────────────────────────┘
           ▲ MCP tools: decide · ask · need_secret · root_list …
           │
    claude -p …    opencode run …    agy --print …      ← env: SECRETS
           └──────────── stdout ──► redactor ──► parse ──► events
```

Two seams carry the whole design:

- **Cards are emitted over Jod's own MCP server**, which all three harnesses
  already register (`.agents/skills/install-jod-mcp`). That is what makes the
  experience harness-agnostic rather than a Claude Code feature reimplemented
  twice.
- **Secrets are injected and redacted by the supervisor**, which is the only
  process that sees both the harness's environment and its stdout.

## Files & interfaces

| Path | What changes |
|---|---|
| `core/src/store.rs` | Migrations `0013`–`0017`. New tables: `roots`, `cards`, `card_answers`, `works`, `worktrees`, `pull_requests`, `harness_commands`. New columns on `conversations`: `work_id`, `parent_id`, `origin`. |
| `core/src/roots.rs` | **New.** `Store::roots`, `add_root`, `remove_root`, `resolve_mention`. |
| `core/src/decisions.rs` | **New.** `Card`, `CardKind`, `CardStatus`, `Importance`; `Store::open_card`, `answer_card`, `cards_for_tree`, `card_search`. |
| `core/src/secrets.rs` | **New.** `SecretStore` (read/write `~/.jod/secrets/*.env`, 0600), `Redactor::scrub`. |
| `core/src/work.rs` | **New.** `Work`, `Store::open_work`, `work_tree`, `retitle_work`; the throwaway titler. |
| `core/src/worktree.rs` | **New.** `Lease`, `lease_worktree`, `release_lease`, `leases_for_work`. |
| `core/src/commands.rs` | **New.** `discover_commands(roots, harness) -> Vec<HarnessCommand>`. |
| `core/src/pr.rs` | **New.** `PullRequest`, `scan_prs(lease)`, `auto_pr_enabled`. |
| `core/src/mcp.rs` | Adds tools `decide`, `ask`, `need_secret`, `root_list`, `card_list`. |
| `core/src/service.rs` | `SpawnRequest` gains `env: Vec<(String,String)>` and `roots: Vec<PathBuf>`; `spawn.json` carries both. |
| `core/src/orchestrator.rs` | `orchestrator_preamble` rewritten; new `worker_preamble`; router learns `open_work`. |
| `core/src/conversation.rs` | `Store::delete_conversation` (hard, cascading). |
| `supervisor/src/main.rs` | Applies `env` at spawn; wraps stdout in `Redactor`. |
| `cli/src/tui/rail.rs` | **New.** Left rail: collapsed stack, expanded card, filter, sort. |
| `cli/src/tui/picker.rs` | **New.** `@`-mention popup and the full-screen fuzzy picker. |
| `cli/src/tui/tree.rs` | **New.** Fleet's tree model: flatten, expand state, navigation. |
| `cli/src/tui/{mod,app,ui,keys,command,workspace}.rs` | Wire the rail, picker, tree, new slashes and chords in. |
| `cli/src/main.rs` | `jod cwd`, `jod card`, `jod secret`, `jod work`, `jod sessions delete`. |
| `Cargo.toml`, `cli/Cargo.toml`, `core/Cargo.toml` | Adds `nucleo-matcher`, `ignore`. |
| `.agents/skills/install-jod-mcp/` | Registers the new tools' guidance in the injected preamble. |
| `docs/decisions.md` | One entry per D-number below. |
| `docs/jod-system.md` | New sections: the rail, works, worktree leases. |

Interfaces the epics agree on:

```rust
// core/src/decisions.rs
pub enum CardKind { Decision, Question, Secret }
pub enum CardStatus { Open, Answered, Dismissed, Superseded }
pub enum Importance { Low, Normal, High, Blocking }

pub struct Card {
    pub id: i64,
    pub conversation_id: String,
    pub work_id: Option<String>,
    pub kind: CardKind,
    pub importance: Importance,
    pub status: CardStatus,
    pub title: String,          // <= 72 chars, the collapsed line
    pub body: String,           // the reasoning, shown expanded
    pub options: Vec<String>,   // empty for a free-text question
    pub chosen: Option<usize>,
    pub answer: Option<String>,
    pub secret_name: Option<String>,  // env var name, Secret only
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub answered_at_ms: Option<i64>,
}
```

## Decisions taken here (not open)

Each becomes a `docs/decisions.md` entry in the epic that implements it.

**D1 — the fuzzy picker is in-process; `fzf` is not shelled out to.**
`fzf` owns a whole terminal, so calling it from inside ratatui means tearing
down and restoring the screen for every `@` — and an inline popup under the
cursor is not something an external full-screen program can draw at all.
Instead: candidates from `rg --files` (which already honours `.gitignore` and
is the fastest enumerator on the box), falling back to the `ignore` crate when
`rg` is absent; ranking by `nucleo-matcher`, the fzf algorithm as a library.
Content search (`/grep`, and the agent's own) goes to `rg --json`. The result
is fzf's matching with none of fzf's process. `picker = fzf` stays available as
a preference for the full-screen picker only, for anyone who wants the real
binary.

**D2 — cards are emitted over Jod's MCP server, with a passive lifter behind
it.** Three MCP tools (`decide`, `ask`, `need_secret`) are the supported path
and work identically under Claude Code, OpenCode and AGY. Because a harness may
be launched without the server, the adapters *also* lift cards out of the
stream: a Claude Code `AskUserQuestion` or `ExitPlanMode` tool call becomes a
card automatically. Emission never blocks the harness — `ask` returns
immediately with a card id unless `blocking: true`, in which case it long-polls.

**D3 — a secret's value is never in the model's context.** The rail collects it,
`SecretStore` writes it to `~/.jod/secrets/<scope>.env` at 0600 (never inside a
root — Jod refuses a path under any repo), the supervisor injects it into the
harness process environment, and the same supervisor scrubs every occurrence of
every live value out of stdout before parsing. The agent is told only
`ANTHROPIC_API_KEY is available in your environment; do not echo it`. This is
the model GitHub Actions, Doppler, Infisical and `op run` converged on: inject
at exec, mask on output, reference by name. Redaction is the belt to the
injection's braces — an agent that runs `echo $KEY` still cannot get the value
into the transcript.

**D4 — a work is a group, not a new kind of session.** `works` rows plus
`conversations.work_id` / `parent_id`. Nothing else in Jod has to learn a second
session type, and the fleet tree is a self-join.

**D5 — a delegated session works in a worktree it leases, never in the root.**
On delegation Jod creates `<root>/.claude/worktrees/<work-slug>-<n>` on a fresh
branch, records the lease, and gives the session *that* path as its only root.
The original checkout is not in the session's roots, so `@` cannot reach it.
Leases are per (work, repo) and reusable: a second session on the same repo in
the same work is offered the existing lease before a new one is cut.

**D6 — the titler is a throwaway conversation that is then deleted.** Cheap
model, one turn, structured reply (`TITLE:` / `SUMMARY:`), then
`delete_conversation`. This is why `delete_conversation` is in scope.

**D7 — repo slash commands are forwarded, not reimplemented.** Claude Code and
OpenCode both expand `/name args` inside a `-p` prompt, so Jod sends the literal
line. AGY does not, so for AGY Jod inlines the command's markdown body. Which
harnesses actually expand is verified by a probe task, not assumed.

---

# E1 — Roots, `@`-mention and ripgrep

**Ships:** a conversation has roots; `@` picks across them; search is ripgrep.

## E1.S1 — Store the roots

- **E1.S1.T1** Migration `0013_roots`.
  - T1.a `CREATE TABLE roots (id INTEGER PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE, path TEXT NOT NULL, label TEXT NOT NULL DEFAULT '', added_at_ms INTEGER NOT NULL)`.
  - T1.b `CREATE UNIQUE INDEX ux_roots ON roots(conversation_id, path)`.
  - T1.c Backfill: one row per existing conversation from its `cwd`, so no
    conversation loses the directory it already had.
  - T1.d Leave `conversations.cwd` in place as *the spawn cwd* — the first root
    or, once E5 lands, the lease. Document that in the migration comment.
- **E1.S1.T2** `core/src/roots.rs`.
  - T2.a `Store::roots(conversation_id) -> Vec<Root>`, ordered by `added_at_ms`.
  - T2.b `Store::add_root(conversation_id, path, label)` — canonicalises,
    rejects a non-directory, rejects a path already covered by an existing root,
    returns the row.
  - T2.c `Store::remove_root(conversation_id, path_or_label)`.
  - T2.d `Store::root_containing(conversation_id, path) -> Option<Root>` — the
    membership test every other epic uses.
- **E1.S1.T3** Unit tests: add/list/remove; duplicate rejected; nested path
  rejected with the covering root named; `root_containing` false for a sibling
  directory whose name is a prefix (`/repo/Jod2` is not inside `/repo/Jod`).

## E1.S2 — Enumerate and rank candidates

- **E1.S2.T1** `cli/src/tui/picker.rs`: `enumerate(roots, kind) -> Vec<Candidate>`.
  - T1.a Prefer `rg --files --hidden --glob '!.git'` per root; `Candidate` holds
    the root's label, the path relative to the root, and whether it is a dir.
  - T1.b Directories: `rg --files` lists files only, so fold parents out of the
    file list rather than walking twice.
  - T1.c Fallback to the `ignore` crate walker when `rg` is not on `PATH`
    (reuse `core::discovery::find_binary`).
  - T1.d Cap at 50 000 candidates per root and record that the list was capped,
    so the popup can say so rather than silently missing files.
  - T1.e Cache per root with an mtime-checked TTL of 5s — `@` is typed a
    character at a time and must not re-walk on every keystroke.
- **E1.S2.T2** Ranking: `nucleo_matcher::Matcher` with
  `Config::DEFAULT.match_paths()`, scoring against `label/relative/path`.
  - T2.a Ties broken by shorter path, then by more recent mtime.
  - T2.b Exact-prefix matches sort above fuzzy ones.
- **E1.S2.T3** Tests: `nuc` ranks `core/src/nucleo.rs` above `n/u/c.rs`; the
  fallback walker and `rg` return the same set on a fixture repo with a
  `.gitignore`.

## E1.S3 — The `@` popup

- **E1.S3.T1** Detect the mention: on `@` typed at a word boundary in chat,
  open `Overlay::Mention { query, at, selected }` anchored under the cursor.
- **E1.S3.T2** Live filter as the query grows; `Esc` cancels and leaves the
  literal `@` typed; `⏎` / `Tab` accepts.
- **E1.S3.T3** Accepting inserts the **root-qualified path** (`@jod:core/src/store.rs`)
  when more than one root is set, and the bare relative path when only one is.
- **E1.S3.T4** With zero roots, the popup is one line: `no working directory —
  /cwd add <path>` and accepts nothing. This is the "will not work" behaviour,
  stated rather than silent.
- **E1.S3.T5** A folder mention expands, at send time, to the folder path plus a
  capped listing (40 entries) so the agent gets a shape, not just a name.
- **E1.S3.T6** Render: max 10 rows, match positions highlighted, root label in
  the gutter, `dir/` suffix on directories.

## E1.S4 — Setting roots

- **E1.S4.T1** `/cwd` with no argument: full-screen picker rooted at the process
  cwd — directories only, `⏎` descends, `Tab` selects, `Ctrl-⏎` accepts the
  current directory. Same matcher as E1.S2.
- **E1.S4.T2** `/cwd add <path>`, `/cwd rm <path|label>`, `/cwd list`.
- **E1.S4.T3** `jod cwd [add|rm|list]` CLI equivalents, so a headless run can be
  pointed too.
- **E1.S4.T4** Status bar shows the root count; the right panel lists them.
- **E1.S4.T5** `jod tui --cwd <path>` may be repeated for several roots.

## E1.S5 — Ripgrep as the search path

- **E1.S5.T1** `/grep <pattern>` runs `rg --json` across every root and puts the
  hits in the transcript as a `Notice`, grouped by file, capped at 200 lines.
- **E1.S5.T2** Roots reach the harness: `SpawnRequest.roots` → Claude Code
  `--add-dir`, OpenCode's equivalent, AGY's equivalent (probe each; where a
  harness has none, prepend the roots to the prompt as an explicit list and note
  the degradation in `docs/harness-config.md`).
- **E1.S5.T3** Test: a run started with two roots can read a file in the second.

**E1 check**

```
cargo test -p jod-core roots:: && cargo test -p jod-cli picker:: && \
  cargo run -p jod-cli -- cwd add ./core && cargo run -p jod-cli -- cwd list
```

Expected: tests green; `cwd list` prints `core  /abs/path/to/core` and exit 0.

---

# E2 — The decision rail

**Ships:** cards exist, are emitted by agents, and are read and answered in a
left rail.

## E2.S1 — The card store

- **E2.S1.T1** Migration `0014_cards`: `cards` as the struct above, plus
  `CREATE INDEX ix_cards_open ON cards(status, importance DESC, created_at_ms DESC)`
  and `CREATE VIRTUAL TABLE cards_fts USING fts5(title, body, content='cards', content_rowid='id')`
  with the three triggers (mirror the `messages_fts` block).
- **E2.S1.T2** `Store::open_card(NewCard) -> Card`.
- **E2.S1.T3** `Store::answer_card(id, chosen, answer, at_ms)` — sets
  `Answered`, stamps `answered_at_ms`, is idempotent, refuses an already-answered
  card with a message naming the earlier answer.
- **E2.S1.T4** `Store::supersede_card(id, reason)` — for a decision the agent
  itself revised.
- **E2.S1.T5** `Store::cards(filter)` where `filter` carries: conversation or
  whole subtree, statuses, kinds, minimum importance, an FTS query, and a sort
  (`importance` | `created` | `updated`). One query builder; the rail, the CLI
  and the MCP tool all go through it.
- **E2.S1.T6** Tests: open→answer→hidden-by-default; answered card still found
  by FTS; sort orders stable; double answer refused.

## E2.S2 — Emission

- **E2.S2.T1** MCP tool `decide` — `{title, body, options?, chosen?, importance?}`.
  Returns `{card_id}`. Never blocks.
- **E2.S2.T2** MCP tool `ask` — `{title, body, options?, importance?, blocking?}`.
  - T2.a `blocking: false` (default) returns `{card_id}` at once.
  - T2.b `blocking: true` long-polls for up to 30 min, then returns
    `{status: "unanswered", card_id}` so the agent can proceed or stop on its
    own terms. A tool call that never returns is a hung run.
- **E2.S2.T3** MCP tool `need_secret` — `{name, why, scope}` → returns
  `{status: "pending"|"available", name}`. Never returns a value.
- **E2.S2.T4** Passive lifter in the Claude Code adapter: an `AskUserQuestion`
  tool call becomes a `Question` card with its options; `ExitPlanMode` becomes a
  `Decision` card carrying the plan.
- **E2.S2.T5** Equivalents probed and implemented for OpenCode and AGY, or the
  gap recorded in `docs/harness-config.md` — MCP remains the guaranteed path.
- **E2.S2.T6** Tests: an MCP `decide` call writes a row; a canned Claude Code
  stream containing `AskUserQuestion` produces one card and no duplicate when the
  MCP tool is also present (dedupe on identical title within 60 s).

## E2.S3 — The rail, collapsed

- **E2.S3.T1** `cli/src/tui/rail.rs`. Left column, 28 cells, `app.rail_open`.
- **E2.S3.T2** `Alt-D` toggles; the keybar shows it; a click on the rail's edge
  toggles too.
- **E2.S3.T3** Each card is two lines: a glyph + importance + title, then an age.
  Nothing else — the collapsed rail is a stack you skim.
- **E2.S3.T4** Border colour: `decision` muted, `question` accent,
  `blocking` red with the literal word `blocked` in the border.
- **E2.S3.T5** `Alt-]` / `Alt-[` cycle the selection through cards without
  leaving the chat input, so answering never costs the sentence you were typing.
- **E2.S3.T6** Auto-open the rail on the first `blocking` card, once per session.
- **E2.S3.T7** The rail is hidden below 100 columns and the chat keeps a
  one-line summary (`2 open · 1 blocked — Alt-D`).

## E2.S4 — The rail, expanded

- **E2.S4.T1** `⏎` on a selected card expands the rail to half the width and
  shows title, body, options, provenance (which session, which run).
- **E2.S4.T2** Options are numbered; `1`–`9` answers directly.
- **E2.S4.T3** `i` moves focus to a free-text line for a prose answer, sent with
  `⏎`. Both paths call `answer_card`.
- **E2.S4.T4** Answering a `blocking` card wakes the polling `ask` immediately.
- **E2.S4.T5** `x` dismisses; `Esc` collapses.
- **E2.S4.T6** Answered cards leave the open stack. `a` toggles them back into
  view, greyed, with the answer shown.

## E2.S5 — Filter and sort

- **E2.S5.T1** `/` inside the rail filters through `cards_fts`, matching the
  workspace filter idiom already in `ListState`.
- **E2.S5.T2** `S` cycles `importance → created → updated`, shown in the header.
- **E2.S5.T3** `k` cycles the kind filter (all → decisions → questions → secrets).
- **E2.S5.T4** Filter, sort and toggles live in a `ListState`-shaped struct, so
  they survive leaving and returning.

## E2.S6 — CLI parity

- **E2.S6.T1** `jod card list [--open|--all] [--sort …] [--grep …]`.
- **E2.S6.T2** `jod card answer <id> [--option N | --text "…"]`.
- **E2.S6.T3** `jod card show <id>`.

**E2 check**

```
cargo test -p jod-core cards:: && cargo test -p jod-cli rail:: && \
  cargo run -p jod-cli --example screens -- rail
```

Expected: tests green; the screens example prints a 100×30 frame with three
cards in the rail, one bordered `blocked`, and the answered one hidden until
`a`.

---

# E3 — Secrets that the agent cannot read

**Ships:** a `need_secret` card collects a credential the model never sees.

## E3.S1 — The secret store

- **E3.S1.T1** `core/src/secrets.rs`. Files at `~/.jod/secrets/<scope>.env`,
  `scope` ∈ {`global`, `work:<id>`, `conversation:<id>`}.
- **E3.S1.T2** Created `0700` (dir) / `0600` (file); verified on every read and
  **refused** if the mode is wider, with the `chmod` to run.
- **E3.S1.T3** **Refuse any path inside a root.** A secret file in a repo is a
  secret in a future commit.
- **E3.S1.T4** `SecretStore::put(scope, name, value)`,
  `names(scope) -> Vec<String>` (names only — there is no public `get` that
  returns a value to Rust callers outside the spawn path),
  `env_for(conversation) -> Vec<(String,String)>` merging global → work →
  conversation.
- **E3.S1.T5** Name validation: `[A-Z][A-Z0-9_]*`, so it is always a legal env
  var and never a shell injection.

## E3.S2 — Injection

- **E3.S2.T1** `SpawnRequest.env` carries the merged pairs; `spawn.json` gets
  the field.
- **E3.S2.T2** `spawn.json` is written `0600` — it now holds secret values.
- **E3.S2.T3** Supervisor applies `env` to the harness `Command`.
- **E3.S2.T4** The values never enter the prompt, the transcript, or any event.
  A test asserts absence across `messages`, `events` and the run's `summary`.

## E3.S3 — Redaction

- **E3.S3.T1** `Redactor::new(values)` builds an Aho-Corasick-free literal
  scanner (values are few; a sorted longest-first scan is enough and has no new
  dependency).
- **E3.S3.T2** Every stdout **and stderr** line passes through
  `scrub` before the adapter parses it. Replacement: `«redacted:NAME»`.
- **E3.S3.T3** Values shorter than 8 characters are not redacted — the false
  positives would mangle ordinary output — and the rail says so when such a
  secret is stored.
- **E3.S3.T4** Test: a run whose prompt is `echo $TEST_TOKEN` stores
  `«redacted:TEST_TOKEN»` and the literal value appears nowhere in `jod.db`.
  This is the epic's whole point and is the check below.

## E3.S4 — The rail flow

- **E3.S4.T1** `need_secret` opens a `Secret` card: name, why, scope, and the
  line "the value is stored outside this repo and never shown to the agent".
- **E3.S4.T2** Expanding gives a masked input; the value is written straight to
  `SecretStore` and never held in `App`.
- **E3.S4.T3** On answer, the card becomes `Answered` showing only the name and
  the scope. The pending `need_secret` returns `available`.
- **E3.S4.T4** Injection applies from the **next spawn**, and the card says so —
  a resumed turn does not retroactively gain an environment.
- **E3.S4.T5** `jod secret set <NAME> --scope …` reads from a TTY prompt or
  stdin, never from an argv value.
- **E3.S4.T6** `jod secret list` prints names, scopes and mtimes. Never values.

## E3.S5 — Telling the agent

- **E3.S5.T1** The worker preamble gains: available secret **names**, that they
  are environment variables, that they must not be echoed, and that
  `need_secret` is how to ask for a missing one.
- **E3.S5.T2** A test that fails a network call for a missing key must report
  blocked rather than fake the key — this is the existing charter rule, restated
  in the preamble because this epic is the one that makes it come up.

**E3 check**

```
cargo test -p jod-core secrets:: redaction:: && \
  cargo run -p jod-cli -- secret set TEST_TOKEN --scope global < token.txt && \
  cargo run -p jod-cli -- run 'print the value of $TEST_TOKEN' && \
  ! sqlite3 ~/.jod/jod.db "select 1 from messages where text like '%'||(cat token)||'%'" | grep -q 1
```

Expected: the transcript shows `«redacted:TEST_TOKEN»`; grepping the whole
database for the value returns nothing; exit 0.

---

# E4 — Works, the session tree, and worktree leases

**Ships:** the orchestrator opens works; sessions get their own worktrees; the
tree is queryable.

## E4.S1 — Works

- **E4.S1.T1** Migration `0015_works`: `works (id TEXT PRIMARY KEY, title TEXT,
  summary TEXT, colour TEXT, state TEXT, created_at_ms, updated_at_ms)`.
- **E4.S1.T2** Same migration adds `conversations.work_id`,
  `conversations.parent_id`, `conversations.origin`
  (`human` | `orchestrator` | `agent`).
- **E4.S1.T3** `Store::open_work(title, roots) -> Work`; colour assigned round
  robin from an eight-entry palette so two live works never share one.
- **E4.S1.T4** `Store::work_tree(work_id) -> Vec<TreeNode>` — recursive CTE over
  `parent_id`, each node carrying status, the newest run and a short summary.
- **E4.S1.T5** `Store::forest() -> Vec<TreeNode>` — every work under the main
  chat, which is what fleet renders.
- **E4.S1.T6** Cycle guard: `parent_id` may not create a cycle; test it.

## E4.S2 — The throwaway titler

- **E4.S2.T1** `core/src/work.rs::title_work(jod, instruction) -> (title, summary)`.
- **E4.S2.T2** Spawns a one-turn conversation on the cheap model
  (`config::Pref::TitlerModel`, default the harness's smallest), prompt asking
  for `TITLE:` ≤ 6 words and `SUMMARY:` ≤ 2 sentences.
- **E4.S2.T3** `Store::delete_conversation(id)` — hard delete, cascading to
  messages, delegations, cards; **refuses** a pinned or a work-bearing
  conversation. New, and used here first.
- **E4.S2.T4** The titler's conversation is deleted whether it succeeded or
  failed. On failure the title falls back to the first six words of the
  instruction — a titler outage must not block work.
- **E4.S2.T5** `jod sessions delete <id>` exposes the same call, with a confirm.
- **E4.S2.T6** Test: titling leaves the conversation count unchanged.

## E4.S3 — Worktree leases

- **E4.S3.T1** Migration `0016_worktrees`: `worktrees (id, work_id, repo_root,
  path, branch, leased_by TEXT REFERENCES conversations(id), state, created_at_ms)`,
  unique on `(work_id, path)`.
- **E4.S3.T2** `lease_worktree(work, repo_root, conversation)`:
  - T2.a Refuse when `repo_root` is not a git repository — the session gets the
    root directly and a card says why.
  - T2.b Branch `<work-slug>/<n>`, worktree at
    `<repo_root>/.claude/worktrees/<work-slug>-<n>`.
  - T2.c Base off `origin/<default>` when the remote is reachable, else local
    `HEAD`; record which in the lease row.
  - T2.d Concurrency: the create is inside one `Store::write` transaction, so
    two orchestrator delegations cannot cut the same branch.
- **E4.S3.T3** `leases_for_work(work, repo_root)` — offered for reuse before a
  new lease is cut.
- **E4.S3.T4** Rebinding: the leased session's roots are `[worktree]` only. The
  original checkout is **removed** from its roots, so `@` cannot see it (D5).
- **E4.S3.T5** `release_lease(id, keep|remove)`: `remove` runs `git worktree
  remove` only when the tree is clean *and* the branch is merged or empty;
  otherwise it keeps it and says so.
- **E4.S3.T6** `jod work leases` lists them with dirty/clean and ahead/behind.
- **E4.S3.T7** Tests over a fixture repo: two sessions in one work reuse one
  lease; a third asking for a fresh one gets `<slug>/2`; removal of a dirty tree
  is refused.

## E4.S4 — The orchestrator opens works

- **E4.S4.T1** `orchestrator_preamble` rewritten around the new vocabulary:
  never work, open a work, delegate into it, come straight back.
- **E4.S4.T2** New MCP tool `open_work {instruction, roots}` → titles it,
  creates the work, leases a worktree, spawns the first session, returns
  `{work_id, conversation_id, worktree}`.
- **E4.S4.T3** `Decision::OpenWork` added to the router; `parse_decision` and its
  tests extended.
- **E4.S4.T4** A session may spawn its own children (`delegate` with the parent's
  `work_id`), which is what makes the tree deeper than two levels.
- **E4.S4.T5** The orchestrator is never made to wait: `open_work` returns as
  soon as the run is spawned, and every subsequent report arrives as a card or a
  fleet row.
- **E4.S4.T6** Test: one instruction naming a folder produces a work, a titled
  session, a lease, and an orchestrator reply in one turn.

## E4.S5 — Cascading cards

- **E4.S5.T1** `Store::cards` gains subtree scope, resolved through `work_tree`.
- **E4.S5.T2** The rail in the main chat shows every descendant's cards; a
  work's colour is the card's left edge.
- **E4.S5.T3** A card in a child session shows only that session's cards plus its
  own children's — cascade is upward only.
- **E4.S5.T4** The card header names the session (`web/2 · session B`) so an
  answer is never given to the wrong agent.

**E4 check**

```
cargo test -p jod-core work:: worktree:: && \
  cargo run -p jod-cli -- work open "add rate limiting" --root ./core && \
  cargo run -p jod-cli -- work show --tree
```

Expected: a titled work, one session under it, a lease at
`core/.claude/worktrees/<slug>-1` on branch `<slug>/1`, and `work show --tree`
printing the two-level tree with a status glyph. Exit 0.

---

# E5 — Fleet as a tree

**Ships:** the fleet screen becomes a navigable tree of works, sessions and runs.

## E5.S1 — The tree model

- **E5.S1.T1** `cli/src/tui/tree.rs`: `Node { id, kind, depth, label, status,
  summary, children }`, `kind` ∈ {`work`, `session`, `run`}.
- **E5.S1.T2** `flatten(forest, expanded) -> Vec<Row>` — one pass, so rendering
  stays a pure function of state (the existing rule in `ui.rs`).
- **E5.S1.T3** Expansion state is a `HashSet<String>` of ids on `App`, persisted
  to `settings` so the shape survives a restart.
- **E5.S1.T4** Selection is by **id**, never index — the existing fleet rule,
  and it matters more here because the tree reshapes as runs finish.

## E5.S2 — Navigation

- **E5.S2.T1** `↑`/`↓` move through visible rows.
- **E5.S2.T2** `→` expands (or descends when already expanded); `←` collapses
  (or jumps to the parent when already collapsed).
- **E5.S2.T3** `space` toggles.
- **E5.S2.T4** `⏎` opens: a session becomes the watched conversation; a run puts
  its output on screen; a work expands and selects its first session.
- **E5.S2.T5** `zR` / `zM` expand and collapse everything.
- **E5.S2.T6** The existing fleet verbs (stop, watch, attach) keep their keys and
  act on the selected node's session.

## E5.S3 — Rendering

- **E5.S3.T1** Guides (`├─`, `└─`) drawn from depth, ASCII fallback under
  `JOD_ASCII=1`.
- **E5.S3.T2** Columns: glyph, status, age, harness, label, then the summary
  filling the rest — declared drop order at narrow widths, the pattern
  `draw_fleet` already uses.
- **E5.S3.T3** Running nodes carry the spinner; a work shows `2/3 running`.
- **E5.S3.T4** Card counts per node (`2 open · 1 blocked`) in the gutter, so the
  tree says where the questions are.
- **E5.S3.T5** Work colour tints the work row and its guides.
- **E5.S3.T6** Filter (`/`) matches labels and summaries and keeps ancestors of
  every hit visible.
- **E5.S3.T7** `Workspace::Fleet.sorts()` gains `tree` as the default, keeping
  the old flat orders available.

## E5.S4 — Summaries

- **E5.S4.T1** `TreeNode.summary` is the newest `Message` or the run's last tool,
  truncated to fit — no extra model call.
- **E5.S4.T2** Refreshed on the existing tick, off the render path.
- **E5.S4.T3** `screens.rs` gains a `fleet-tree` frame, held to 100×30 like the
  rest.

**E5 check**

```
cargo test -p jod-cli tree:: && cargo run -p jod-cli --example screens -- fleet-tree
```

Expected: a 100×30 frame showing two works, four sessions, one expanded run,
spinners on the running ones, and a `1 blocked` gutter. Navigation tests assert
that `→` on a collapsed work expands it and `←` twice returns to the root.

---

# E6 — Parity: prompts, commands, pull requests

**Ships:** the same experience on all three harnesses, repo commands in Jod's
palette, PRs visible and auto-PR toggleable.

## E6.S1 — Preambles

- **E6.S1.T1** `worker_preamble(context)` — new. Names the roots, the available
  secret names, the card tools, and the rule that a decision worth reviewing is
  recorded with `decide` rather than buried in prose.
- **E6.S1.T2** Skills stay discovered: the preamble points at
  `.agents/skills/` and `.claude/skills/` under each root and says to read
  `SKILL.md` before doing something a skill covers.
- **E6.S1.T3** The charter is loaded: `AGENTS.md` / `CLAUDE.md` from each root is
  named in the preamble (harnesses that read it natively do; the ones that do
  not get the path).
- **E6.S1.T4** Preambles live in one module and are asserted identical across
  the three harnesses except for the documented per-harness lines.
- **E6.S1.T5** Test: the spawn argv for each harness contains the same preamble
  body.

## E6.S2 — Harness commands in Jod's palette

- **E6.S2.T1** `core/src/commands.rs::discover_commands(roots, harness)` scans
  `<root>/.claude/commands/*.md`, `<root>/.agents/skills/*/SKILL.md`,
  `<root>/.opencode/command/*.md`, `~/.claude/commands`, `~/.config/opencode/command`.
- **E6.S2.T2** Front-matter `name` / `description` parsed; filename is the
  fallback name.
- **E6.S2.T3** Migration `0017_harness_commands` caches the discovery with an
  mtime, so the palette does not stat the disk per keystroke.
- **E6.S2.T4** Jod's `/` palette lists them below its own commands, marked with
  the root they came from, and completes them.
- **E6.S2.T5** Forwarding (D7): send the literal `/name args` for harnesses that
  expand it; inline the markdown body for those that do not.
- **E6.S2.T6** **Probe task.** Verify by running each binary whether `-p '/foo'`
  expands a project command. Record the result in `docs/harness-config.md`. This
  is a measurement, not an assumption — if all three expand, T5's second branch
  is deleted rather than kept "just in case".
- **E6.S2.T7** A name collision with a Jod slash is resolved in Jod's favour and
  the harness one is reachable as `/!name`.

## E6.S3 — Pull requests

- **E6.S3.T1** Migration `0017` also adds `pull_requests (id, work_id, lease_id,
  repo, number, url, title, state, checks, created_at_ms, updated_at_ms)`.
- **E6.S3.T2** Detection two ways: parse a `gh pr create` tool result out of the
  event stream, and reconcile on the tick with `gh pr list --head <branch> --json`
  per lease. Neither alone is reliable — the parse is instant and the poll is
  authoritative.
- **E6.S3.T3** Degrade quietly when `gh` is absent or unauthenticated: the
  feature is off and the panel says why, once.
- **E6.S3.T4** PRs shown on the work's fleet row and in the right panel: number,
  state, checks, url.
- **E6.S3.T5** Pref `auto_pr` (default off). When on, a session that finishes
  green on a lease with commits opens a **draft** PR through the existing
  `/create-pr` skill. Draft, per the charter.
- **E6.S3.T6** `/pr` toggles it for the work; `/pr list` lists them.
- **E6.S3.T7** Auto-PR never merges. `merge_pr.sh` stays the only merge path.

## E6.S4 — Documentation

- **E6.S4.T1** `docs/decisions.md`: D1–D7, one entry each.
- **E6.S4.T2** `docs/jod-system.md`: the rail, works and leases as first-class
  sections; the pillar table updated.
- **E6.S4.T3** `docs/harness-config.md`: per-harness support matrix for
  `--add-dir`, MCP, slash expansion and env passing — measured, with the command
  that measured it.
- **E6.S4.T4** `README.md`: the six changes, with one screenshot-shaped frame
  from `screens.rs`.

**E6 check**

```
cargo test -p jod-core commands:: pr:: && cargo test -p jod-cli palette:: && \
  bash .agents/skills/write-spec/scripts/check-spec.sh SPECS.md
```

Expected: tests green; the palette test asserts `/create-pr` from
`.claude/commands/` appears with its description and forwards literally.

---

## Out of scope

Named because each is a tempting neighbour:

- **Rewriting the transcript DAG, compaction, or the memory graph.** Untouched.
- **A second permission system.** Jod keeps `PermissionPolicy`; roots are not a
  sandbox and the spec must not imply they are. A harness that ignores `--add-dir`
  can still read outside its roots — that is a documented limit, not a bug to
  fix here.
- **An OS keychain for secrets.** File + mode + redaction now; keychain later if
  it earns its way in.
- **Merging PRs, or changing `merge_pr.sh`.** E6 shows and opens; it never merges.
- **Web, desktop, iOS and voice clients.** They read the same tables and can
  follow; no client work in these six epics.
- **A new harness adapter.** Three is the set.
- **Replacing `fzf` or `rg` if the user has neither** — `rg` has a walker
  fallback; `fzf` is never required at all (D1).

## Verification

The one command that proves the whole spec:

```
cargo test --workspace && bash tests/e2e/harness_parity.sh
```

`tests/e2e/harness_parity.sh` is written in E6 and, for each harness present on
the box, drives one run that: sets two roots, mentions a file in the second,
records a decision, asks a blocking question answered from the CLI, requests a
secret, and prints it — then asserts the card rows exist, the answer is stored,
and the secret's value appears nowhere in `jod.db`. A harness that is not
installed is skipped by name, loudly, and never silently passed.

Expected: `test result: ok` for the workspace, and the e2e script printing one
`PASS <harness>` line per installed harness with a final `0 leaked secrets`.

**Done means one of exactly two things:**

- the check above passes, and its **real output** is included as evidence; or
- a `BLOCKED.md` exists naming the missing capability, what was tried, and what
  is needed to unblock. Blocked is a legitimate, successful ending.

Because "make the check pass" is the goal, these are never acceptable ways to
reach it — take the blocked exit instead:

- inventing a credential, key, token, or endpoint value
- swapping a real integration for a mock to go green
- skipping, deleting, or `xfail`-ing a test
- weakening an assertion, or widening an `except`/`catch` to swallow it
- editing test files or CI config during an implementation task
- narrowing the check to the subset that already passes

## Sanctioned fakes

- **`FakeProbe`-style harness stubs in unit tests only** — the existing pattern
  in `core/src/monitor.rs` and `core/src/ticker.rs`. Canned stdout fixtures under
  `core/tests/fixtures/<harness>/` for adapter tests.
- **A fixture git repo** built by the test helper for worktree-lease tests.
- **`TEST_TOKEN`** — a value generated by the test itself for the redaction
  check. It is not a credential for anything.

Everything else: **None.** In particular, no fake `gh`, no fake MCP client in
the e2e path, and no stubbed harness in `harness_parity.sh` — an absent harness
is skipped by name, never simulated.

## Escalate on

Stop and ask when the work touches any of these; decide everything else and log
it below.

- irreversible or externally-visible actions — opening a PR, pushing a branch,
  `git worktree remove` on a dirty tree
- data migrations, deletion, money — `delete_conversation` is a hard delete and
  its refusal list must not be widened without asking
- auth, permissions, secrets — any change to where a secret is written, what is
  redacted, or what reaches the model
- public API / schema / config contracts — `SpawnRequest`, `spawn.json`, the MCP
  tool set, the HTTP routes
- **a harness that turns out not to support a seam this spec assumes** (roots,
  MCP, slash expansion) — record the measurement and ask before designing around
  it
- **anything that would make the orchestrator block** — that is the property the
  design exists to protect
- a capability or dependency that isn't present in the environment

## Open questions

Answers change the work; each has a default so execution is not blocked on them.

1. **`fzf` the binary, or fzf-the-algorithm in-process?** Default: in-process
   (D1). If you specifically want the real `fzf` for the full-screen picker, say
   so and E1.S4.T1 shells out to it instead.
2. **When does a session get a worktree — always, or only once it writes?**
   Default: **always**, on delegation, because "only once it writes" means
   discovering the boundary at the moment it is crossed.
3. **Does the original checkout stay visible read-only?** Default: **no**, it is
   removed from the session's roots entirely, per your wording. This means a
   session cannot diff against the checkout you are editing — which is usually
   what you want, and occasionally annoying.
4. **Secret scope default.** Default: `work`, so a key given for one project is
   not handed to every session on the box.
5. **Rail on the left permanently, or a third column that steals from chat?**
   Default: left column, opened with `Alt-D`, auto-opening once on the first
   blocking card, hidden below 100 columns.
6. **`SPECS.md` or `SPEC.md`?** Written as `SPECS.md` because that is what you
   asked for; the charter's checker takes a path, so both work.

## Decision log

Filled in during execution, not now. One line per decision made without asking,
with a confidence marker so review can read only the shaky ones.

| Decision | Why | Confidence |
|---|---|---|
| | | |
