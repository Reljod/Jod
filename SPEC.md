# SPEC — the run transport moves from tmux + JSONL into SQLite

## Goal

Today an agent's output reaches Jod by a route that has three hops and two
intermediaries: the harness runs inside a tmux session driven by a generated
bash script, `tee` writes its stdout to `~/.jod/runs/<id>/stream.jsonl`, and
Jod tails that file and folds it into the `events` table. tmux is the process
supervisor, the file is the buffer, and SQLite is only the archive.

After this change there is one hop and one store. A run is a **detached child
process group** supervised by a small `jod-run` process that parses the
harness's stdout and appends events **straight into `~/.jod/jod.db`**. No tmux,
no `stream.jsonl`, no generated shell script — and therefore no shell anywhere
in the path. `runs` records the supervisor's `pid`/`pgid`, so *any* Jod process
can see whether a run is alive and stop it, and every client watches a run by
reading the events table rather than by tailing a file it must have local access
to.

User-visible: `jod` no longer requires tmux to be installed. `tmux attach -t
jod-<id>` is replaced by `jod watch <id>`, which works over the HTTP API and
therefore from the phone and the web client too — a live agent is now watchable
from somewhere that isn't the box.

## Files & interfaces

| Path | What changes |
|---|---|
| `supervisor/` **(new crate)** | The `jod-run` binary. Spawns the harness, parses its output, writes events and the terminal status into SQLite, forwards signals. Added to the workspace `members`. |
| `core/src/tmux.rs` | **Deleted.** |
| `core/src/proc.rs` **(new)** | `spawn_detached`, `signal_group`, `group_alive` — `setsid` + `kill(-pgid, …)` + `kill(pgid, 0)`. The Unix bits, isolated so the rest of core stays portable-looking and testable. |
| `core/src/runner.rs` | Rewritten. `render_script`, `sq`, `EXIT_MARKER` and `tail_stream` are deleted. `launch` writes `spawn.json`, starts `jod-run` detached, records `pid`/`pgid`, and starts a **follower** task that polls the store for new events. |
| `core/src/service.rs` | `tmux_available` → `supervisor_available`. `kill` signals the process group instead of killing a session. `rehydrate` uses `group_alive` instead of `has_session`. `AgentSummary` fields change (below). `spawn` now requires a store. |
| `core/src/store.rs` | Migration `0003_process_supervision`. `StoredRun` gains `pid`/`pgid`, loses `tmux_session`. New `set_run_process`, `set_run_status`, `set_run_session`, `run`, `path`. |
| `core/src/paths.rs` | `stream_path` and `script_path` deleted; `spawn_path` (`spawn.json`) added. `prompt_path` stays. |
| `core/src/error.rs` | `TmuxNotFound`/`Tmux(String)` → `SupervisorNotFound`/`Spawn(String)`; new `StoreRequired`. |
| `core/src/discovery.rs` | `find_binary` gains a sibling-of-`current_exe()` lookup, so a bundled app finds the `jod-run` shipped next to it. |
| `core/src/lib.rs`, `core/examples/delegate.rs` | Module list and the example follow. |
| `cli/src/main.rs` | `jod attach` → `jod watch <id>` (stream from the store). The three `tmux is not installed` bails become the supervisor check. |
| `cli/src/render.rs`, `cli/src/tui/{app,ui}.rs` | Show `jod watch <id>` and process state instead of a tmux session name. |
| `api/src/{routes,error,main}.rs` | Same rename; the 503 text stops naming tmux. |
| `apps/web/src/*`, `apps/desktop/{src,src-tauri}/*`, `apps/ios/*` | Follow the `AgentSummary` field change; the desktop "Watch in tmux" button opens a terminal running `jod watch <id>`. |
| `docs/jod-system.md`, `docs/decisions.md`, `README.md` | The architecture diagram, the tmux section, and two decision entries are superseded rather than edited away. |

Interfaces involved:

- **`AgentSummary`** — `tmux_session`, `attach_command`, `switch_command`,
  `session_closed`, `stream_path` are **removed**; `pid: Option<u32>`,
  `pgid: Option<u32>`, `process_alive: bool`, `watch_command: String` are
  **added**. This is a breaking change to the JSON every client reads, and it is
  forced: a `tmux_session` field with nothing behind it is exactly the kind of
  quiet lie the charter forbids.
- **`runs` table** — `+ pid INTEGER`, `+ pgid INTEGER`, `− tmux_session`.
- **`jod-run` CLI** — `jod-run <path-to-spawn.json>`. One argument; everything
  else is read from that file, so the supervisor's own command line stays one
  short path.
- **`spawn.json`** — `{run_id, harness, db_path, program, args, cwd}`; the
  human-readable record of what was launched, replacing `run.sh`.
- **`JOD_SUPERVISOR_BIN`** — env override for locating `jod-run`, matching the
  existing `JOD_TMUX_BIN` / `JOD_CLAUDE_BIN` convention.
- **`jod watch <id>`** — new subcommand; follows a run from the store.

### Why a separate binary and not a thread

The point of the change is that a run survives the process that started it. A
thread cannot hold the read end of the harness's stdout pipe across the death of
its own process — the harness gets `EPIPE`/`SIGPIPE` on its next write, so
closing an SSH session would kill the agent, which is strictly worse than tmux.
`fork()`ing the supervisor in-process is also out: `jod` is a multithreaded
Tokio program, and only async-signal-safe calls are legal in the child, which
rules out running SQLite there. A separate `setsid`'d executable is the only
option that keeps the promise.

The cost is honest and worth stating: "tmux must be installed" becomes "`jod-run`
must be on the box next to `jod`". It ships from the same `cargo build`, so this
is a smaller dependency than before, not a larger one.

### Why the prompt file stays

`prompt.txt` survives even though the shell that needed it is gone. It is no
longer a quoting defence — with a direct `exec` there is nothing to re-parse,
and the prompt reaches the harness's own argv either way, exactly as it did when
the generated script expanded `"$JOD_PROMPT"`. It stays because of the design
rule that anything Jod stores stays readable with `cat` when Jod is not running:
`prompt.txt` is what was asked, and `spawn.json` is what was run.

### What this fixes as a side effect

`docs/jod-system.md` currently explains that `jod team wake` **must** block,
because the tailer belongs to the spawning process and returning early would
mean no `Finished` event is ever written. With the supervisor owning the tail,
that constraint is gone. **The default behaviour does not change in this
change** — only the documentation stops claiming it is forced. Making `wake`
non-blocking is follow-up work.

## Out of scope

- **Conversation ownership.** Threading turns still works exactly as it does
  now: Jod stores a `session_id` and hands `--resume <id>` back to the harness.
  A `conversations`/`turns` schema where Jod owns the transcript was considered
  and explicitly deferred.
- **The harness seam.** `Harness::{kind, args, parse_line, finalize}` is
  unchanged, and no adapter file is touched except where `EXIT_MARKER` was
  referenced. `finalize` now receives a real `wait()` status instead of a code
  scraped from a marker line, which is the same type.
- **Making `jod team wake` non-blocking**, per above.
- **A resident daemon.** Rejected during the interview in favour of detached
  children; do not reintroduce one.
- **Memory, facts, teams, the FTS index, the API's auth and idempotency
  layers.** None of them are on this path.
- **Windows.** `setsid`/process groups are Unix; the crate stays Unix-only, as
  it already effectively was via tmux.

## Verification

The check that proves this works, end to end — a real agent process, its parent
killed, events still arriving:

```
cargo test --workspace && \
  cargo test -p jod-supervisor --test survives_its_parent -- --nocapture
```

(The test lives in `supervisor/tests/` rather than `core/tests/` because
`CARGO_BIN_EXE_jod-run` only resolves inside the crate that owns the binary.
Everything below is asserted there.)

Expected: the workspace suite is green, and the new integration test prints and
asserts the following sequence. It uses a fake harness (a shell script in the
test's tempdir that emits Claude-format JSONL slowly, then exits 0), because the
point under test is the transport, not any real CLI:

1. A `jod` process spawns the run and **exits** while the fake harness is still
   emitting.
2. `kill(pgid, 0)` still succeeds — the run outlived its parent.
3. A **second, fresh** `Store::open` on the same file sees `events` rows still
   being appended after the parent is gone, ending with a `finished` event.
4. `runs.status` for that id is `completed`, and `runs.pid`/`pgid` are populated.
5. `signal_group(pgid, SIGTERM)` on a second, long-running fake ends it, and
   that run lands as `killed` — from a process that never spawned it.
6. A harness that cannot be started at all ends as `error` then `finished`,
   status `failed`, rather than as a run that is still thinking.

Also required to pass, since they are the existing contract:

```
cargo clippy --workspace --all-targets -- -D warnings
cd apps/web && npm test
cd apps/ios && npm test
cd apps/desktop && npm test
```

**Done means one of exactly two things:**

- the check above passes, and its **real output** is included as evidence; or
- a `BLOCKED.md` exists naming the missing capability, what was tried, and
  what is needed to unblock. Blocked is a legitimate, successful ending.

Because "make the check pass" is the goal, these are never acceptable ways
to reach it — take the blocked exit instead:

- inventing a credential, key, token, or endpoint value
- swapping a real integration for a mock to go green
- skipping, deleting, or `xfail`-ing a test
- weakening an assertion, or widening an `except`/`catch` to swallow it
- editing test files or CI config during an implementation task
- narrowing the check to the subset that already passes

## Sanctioned fakes

- **The fake harness script** used by `core/tests/survives_its_parent.rs` and by
  the runner's unit tests: a `bash` script written into the test's tempdir that
  prints known JSONL lines and exits with a chosen code. It stands in for
  `claude`/`opencode`/`agy` only in tests, and only because the transport, not
  the harness, is what is being verified. It must never be reachable from a
  non-test path.

Nothing else. In particular, no test may substitute an in-memory store for the
file-backed one in the survival test — the whole claim is that a *different
process* reads what the supervisor wrote.

## Escalate on

Stop and ask when the work touches any of these; decide everything else and
log it below.

- irreversible or externally-visible actions
- data migrations, deletion, money — **`0003_process_supervision` is one.** It
  rebuilds `runs` to drop a `NOT NULL` column. It must copy every existing row,
  and a pre-existing `~/.jod/jod.db` must still open afterwards.
- auth, permissions, secrets
- public API / schema / config contracts — **`AgentSummary` is one**, and its
  shape is already decided above; escalate only if a client cannot be made to
  work with it.
- whether killing an agent should also kill processes the *harness* spawned.
  Signalling the group does; tmux's `kill-session` did too, so this preserves
  behaviour — but it is worth a line in the PR rather than a silent choice.
- a capability or dependency that isn't present in the environment

## Decision log

Filled in during execution, not now. One line per decision made without
asking, with a confidence marker so review can read only the shaky ones.

| Decision | Why | Confidence |
|---|---|---|
| The survival test lives in `supervisor/tests/`, not `core/tests/` as specced | `env!("CARGO_BIN_EXE_jod-run")` only resolves inside the crate that owns the binary. Everything the spec asked it to assert, it asserts. | high |
| It proves survival by re-invoking its own test binary as the spawner | The only honest way to have a *real* process start a run and then really exit. The child is an `#[ignore]`d test case driven by two env vars. | high |
| A killed run's status is read from the child's exit status, not from having handled a signal | Found by the test failing: `SIGTERM` hits the group, the harness dies first, its pipes close, and the supervisor finishes the run before its own handler runs. A killed run was recorded as `completed`. | high |
| `save_run` refuses to overwrite a terminal `status` | Found by the smoke run: a follower derives status from events, which cannot distinguish killed from completed, and its save landed *after* the supervisor's. Terminal statuses are now write-once; `set_run_status` stays unconditional so the supervisor can still correct itself. | high |
| `rehydrate` trusts `runs.status` when terminal, and only replays otherwise | Same defect from the other side. The old comment said never to trust the stored status — true when nothing authoritative wrote it, false now. A row still saying `running` still means "nobody said", so the replay still decides there. | high |
| `AgentStatus` gained `as_str`/`parse` | The supervisor writes the column from another process; two string literals would have drifted. A test pins the column spelling to the JSON spelling. | high |
| `Store` learned `path()` | The supervisor is a separate process and has to be told which file to open. Returns `None` for in-memory, which is exactly when no other process could share it. | high |
| `discovery::find_binary` looks beside `current_exe()` before `PATH` | `jod-run` ships as a sibling of whatever launched it, and that copy matches the caller's build. An older one earlier in `PATH` would be a silent version mismatch. | medium — no harness is found this way today, so the new branch only fires for `jod-run` |
| Old `AgentSummary` JSON still deserialises | New fields carry `#[serde(default)]`, removed ones are ignored. `rehydrate` skips a summary it cannot parse, so a strict rename would have deleted a person's history. Tested against a real pre-change payload. | high |
| The prompt still goes to a file | Not for quoting — `execve` needs none, and the prompt reaches the harness's argv either way, as it did before. It stays because "readable with `cat` when Jod is not running" is a design rule. The spec's original ps-hiding rationale was wrong and was corrected. | high |
| `supervisor.log` was added, unspecced | A supervisor that dies before it can open the database would otherwise leave no explanation anywhere. It is diagnostics, not transport. | high |
| stdout and stderr are merged by two tasks into one channel, not one fd | Reproduces what `2>&1 | tee` did without adding an `os_pipe` dependency. Interleaving between the two streams is approximate, as it was before. | medium — ordering *within* each stream is exact, which is what the parser needs |

## Checks that did not run, and why

- **`cargo clippy --workspace --all-targets -- -D warnings`** fails on one
  pre-existing lint: `await_holding_lock` at `core/src/service.rs:923`, in a
  test that holds `ENV_LOCK` across a `.await`. It is identical at `HEAD`
  (`git show HEAD:core/src/service.rs`), predates this change, and fixing it is
  next-door work. With that one lint allowed, clippy is clean.
- **`apps/desktop/src-tauri`** cannot be compiled in this repo as configured:
  it inherits `version.workspace`/`jod-core.workspace` but is not in
  `workspace.members`, so cargo refuses it, and it cannot be `exclude`d either
  while it inherits. Pre-existing. Adding it to the workspace temporarily to
  check the edits then failed on a *second*, environmental wall — this box has
  no `pkg-config` or GTK/WebKit headers. **The Rust edits in that crate are
  therefore reviewed by eye, not by a compiler**; its TypeScript side does
  typecheck clean. CI runs neither, so nothing regressed relative to before.
- **`apps/desktop` has no `npm test`** — `tsc --noEmit` is the whole check
  there, and it passes.
