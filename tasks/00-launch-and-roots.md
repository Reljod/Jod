# Launch, roots and the TUI's working directory

Findings confirmed by running the built binary against a fresh `JOD_HOME` and
by reading the live `~/.jod/jod.db`. Every one of these was observed, not
guessed.

Reproduction used throughout:

```sh
export JOD_HOME=$(mktemp -d)
cd /some/scratch/repo
jod main            # or: jod tui
jod root ls
```

---

## L1. A new main chat is created with no roots at all
Status: open · Owner: — · Severity: high

`Store::main_conversation` (`core/src/orchestrator.rs:1292`) creates the pinned
main chat with a `cwd` and never calls `add_root`. Nothing on the launch path
adds one either.

Observed, fresh `JOD_HOME`, run from a scratch repo:

```
$ jod root ls
no roots on f75ba93a — `jod root add <path>` sets one

sqlite> SELECT * FROM conversation_roots;
(empty)
```

Live evidence that this is not just a fresh-install problem: 34 of the 46
conversations in `~/.jod/jod.db` have no row in `conversation_roots`. The one
root the real main chat does have is `origin = human` — added by hand, which is
exactly the symptom Reljod is describing.

Fix: seed a read-only root from `cwd` when the main chat is created.
`Store::ensure_inherited_root` (`core/src/roots.rs:309`) already does precisely
this, is idempotent, marks the root `Origin::Inherited`, and is **never called
from the launch path**. Wiring it in may be the whole fix.

Check: fresh `JOD_HOME`, `jod main` from a scratch directory, assert
`jod root ls` reports that directory with origin `inherited`.

---

## L2. `jod main` starts the chat in `$HOME`, not the directory you ran it in
Status: open · Owner: — · Severity: high

Observed: run from `/…/tmp/scratch-repo` on a fresh `JOD_HOME`, the pinned
conversation came back with `cwd = /home/reljod`.

Cause: the two entry points disagree about what "here" means.

- `Command::Tui` (`cli/src/main.rs:2006`) uses `console_cwd(cwd)`, which falls
  back to `std::env::current_dir()` — the directory you are in. Correct.
- `main_chat` (`cli/src/main.rs:2331`) uses
  `cwd.unwrap_or_else(jod_core::service::default_cwd)`, and `default_cwd`
  (`core/src/service.rs:1359`) returns `$HOME`. Wrong.

This is why the real main chat is pinned to `/home/reljod`: whichever
`jod main` first created it was standing anywhere, and the chat took `$HOME`.

Fix: `main_chat` should use `console_cwd` too. One function, both entry points,
so they cannot drift again.

Check: fresh `JOD_HOME`, `jod main "hi"` from a scratch directory, assert
`conversations.cwd` is that directory and not `$HOME`.

---

## L3. The main chat is frozen at the first directory it ever saw
Status: open · Owner: — · Severity: high

`main_conversation` is get-or-create and returns on `pinned_conversation()`
before it looks at `cwd` at all (`core/src/orchestrator.rs:1297`). The main
chat is a singleton, so the second `jod tui` you ever run — in a different
repository — reuses a conversation pinned to the first directory and never
learns about the new one.

Note this is **not** fixed by L1 alone. L1 seeds a root at creation time, and
the real main chat was created long ago; it would stay wrong for ever.

Fix: on every console launch, add the launch directory as a read-only root of
the main chat. `add_root` already upserts and keeps its position, so doing it
unconditionally is safe and idempotent. Leave `conversations.cwd` alone — it
means "where the harness process starts" and rewriting it under a running
session would move a live process's feet.

Check: launch in directory X, quit, launch in directory Y, assert `jod root ls`
returns both.

---

## L4. A fresh console cannot open any work at all
Status: open · Owner: — · Severity: high — this is the headline failure

`open_work` with no explicit `checkout` reads the caller's roots and, finding
none, refuses (`core/src/mcp.rs:2149`):

> say which directory this work happens in — `checkout` — because this session
> has no roots of its own to inherit one from

Chained with L1, the *first* instruction a brand-new console ever receives
cannot be routed to `open_work` — and the orchestrator's own preamble calls
`open_work` "the usual answer for anything about code". The usual answer is the
one that fails.

Fix: falls out of L1/L3. Keep the refusal itself — defaulting to whatever
directory the daemon happens to have been started in would be worse — but make
sure a console always has a root to inherit, so the refusal is unreachable in
the ordinary case.

Check: fresh `JOD_HOME`, launch in a repo, send one instruction that should
route to `open_work`, assert a work opens rather than a refusal comes back.

---

## L5. `jod project current` does not exist, though the MCP tool does
Status: open · Owner: — · Severity: low

Observed: `jod project current` exits 2 with "unrecognized subcommand". The MCP
tool `project_current` exists (`core/src/mcp.rs`), and the orchestrator's
preamble tells the model to call it, so the concept is real — only the CLI is
missing it. Anyone debugging why the router picked a project has no way to ask
from a terminal.

Fix: add the subcommand, printing the conversation's current project and how it
was resolved. `project_resolutions` already records the how.

---

## L6. `jod team list` where every other noun uses `ls`
Status: open · Owner: — · Severity: low

Observed: `jod ls`, `jod work ls`, `jod schedule ls`, `jod goal ls` and
`jod project ls` all work; `jod team ls` exits 2 and suggests `list`.

Fix: accept `ls` on `team`, keeping `list` as an alias so nothing breaks.
