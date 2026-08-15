# TASKS

Open work found by testing the orchestrator end to end against the live
`~/.jod/jod.db` and an isolated `JOD_HOME`. Newest findings at the bottom.

One owner per task — claim it by putting your name in `Owner:` before you
start. Each task names the file and line where the behaviour lives, and the
check that should go green when it is fixed.

---

## A. Roots and the launch directory

### A1. `jod tui` in a directory does not make that directory a root
Status: open · Owner: — · Severity: high

`Store::main_conversation` (`core/src/orchestrator.rs:1292`) creates the pinned
main chat with a `cwd` and never calls `add_root`. Nothing else on the launch
path adds one: `Command::Tui` (`cli/src/main.rs:1991`) resolves `console_cwd`
and passes it as the spawn directory only. So a fresh console starts with an
empty root set, and the orchestrator cannot read the directory you launched it
in until you run `/add-dir` by hand.

Evidence: 34 of 46 conversations in the live `~/.jod/jod.db` have no row in
`conversation_roots`. The one root the pinned main chat does have is
`origin = human` — added by hand, which is the symptom.

Fix: seed a read-only root from `cwd` when the main chat is created.
`ensure_inherited_root` already does exactly this and is never called from the
launch path — wiring it in may be the whole fix.

Check: launch a console in a scratch directory under a fresh `JOD_HOME`, then
assert `list_roots` returns that directory with `origin = inherited`.

### A2. The pinned main chat is frozen at the first directory it ever saw
Status: open · Owner: — · Severity: high

`main_conversation` is get-or-create and returns early on
`pinned_conversation()` before it looks at `cwd` at all
(`core/src/orchestrator.rs:1297`). The main chat is a singleton, so the second
`jod tui` you ever run — in a different repository — reuses a conversation
pinned to the first directory and never learns about the new one.

Live evidence: the pinned chat's `cwd` is `/home/reljod`, set whenever it was
first created. Running `jod tui` inside `/home/reljod/repo/Jod` today changes
nothing about it.

Note this is not fixed by A1 alone: A1 seeds a root at *creation*, and this
chat was created long ago.

Fix: on every console launch, add the launch directory as a read-only root of
the main chat — idempotent, since `add_root` already upserts and keeps
position. Leave `conversations.cwd` alone; it means "where the harness process
starts" and should not be rewritten under a running session.

Check: launch a console in directory X, quit, launch in directory Y, assert
`list_roots` returns both X and Y.

### A3. A fresh console cannot open any work at all
Status: open · Owner: — · Severity: high — this is the headline bug

`open_work` with no explicit `checkout` reads the caller's roots and, finding
none, refuses: "say which directory this work happens in — `checkout` —
because this session has no roots of its own to inherit one from"
(`core/src/mcp.rs:2149`).

Chained with A1, that means the *first* instruction a brand-new console ever
receives cannot be routed to `open_work`. The orchestrator's own preamble
calls `open_work` "the usual answer for anything about code", so the usual
answer is the one that fails.

Fix: falls out of A1/A2. Keep the refusal — defaulting to the daemon's
directory would be worse — but make sure the console always has a root to
inherit so the refusal is unreachable in the ordinary case.

Check: fresh `JOD_HOME`, launch a console in a repo, send one instruction that
should route to `open_work`, assert a work is opened rather than refused.

---

## B. Routing — answer it yourself or hand it over

### B1. The orchestrator is forbidden from ever answering anything
Status: open · Owner: — · Severity: high

`orchestrator_preamble` (`core/src/orchestrator.rs:353`) opens with "**You do
not do the work.** You decide who does, hand it over, and come straight back",
and there is no branch anywhere in it for answering directly. Every
instruction — including "what time is it in Manila" or "what does A2A stand
for" — costs a spawned agent, a conversation row and a round-trip.

Wanted behaviour, in Reljod's words: decide by the task. A quick question the
orchestrator can answer in one turn, it answers. Something long-running, it
delegates and the sub-agent reports back. Something that is really a project,
it goes to a project manager.

Fix: give the preamble an explicit first branch — answer directly when the
instruction needs no repository, no tools beyond recall, and no work that
outlasts this turn; otherwise route as it does today. Keep the routing verbs
exactly as they are; this adds a case rather than changing one.

Check: a table of instructions with the expected disposition
(`answer` / `continue_agent` / `open_work` / `delegate` / `schedule_create` /
`goal_create`), asserted against what the router actually did. See B2.

### B2. There is no test that routing decisions are correct
Status: open · Owner: — · Severity: medium

`parse_decision` and `router_prompt` (`core/src/orchestrator.rs:229`, `:300`)
are tested for shape — that a decision parses — but nothing asserts that a
given instruction reaches the right verb. So B1 can be fixed and silently
regress.

Fix: a fixture table of instruction → expected disposition, run against the
router. It needs a model, so it belongs behind the same marker as the other
harness-touching tests rather than in the unit suite.

---

## C. Project managers

### C1. Project managers do not exist
Status: open · Owner: — · Severity: high — this is a feature, not a bug

Reljod wants an instruction that turns out to be a project to be handed to a
project manager agent — an existing suitable one, or a new one created for a
new project. Nothing in the codebase implements this. Grepping `core/`, `cli/`
and `docs/` for "project manager" returns one unrelated hit about Homebrew.

What exists today is the *project catalog* — `project_add`, `project_switch`,
`project_list`, `project_current` (`core/src/mcp.rs:504`) — which names a
repository and remembers which one a conversation is about. That is a label,
not an agent, and nothing owns a project over time.

There is a `docs/spec-ceo-and-managers` branch that may already hold the
design; read it before designing this again.

Fix: needs a spec before code — `/write-spec`. At minimum it has to answer:
is a project manager a long-lived conversation per project, or a role a
session takes? How does the orchestrator find "the suitable one"? What happens
when a project has none yet?

Check: none possible until the spec exists.
