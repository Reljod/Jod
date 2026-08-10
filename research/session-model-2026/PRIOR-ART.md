# Prior art: how other systems branch, revert and compact a conversation

**Date:** 2026-08-10 · **Method:** docs + reading real on-disk transcripts on
this machine + live experiments against the installed binaries.
**Status of claims:** everything below is marked verified-by-experiment,
verified-in-source, or *unverified*. Nothing is assumed.

> Why this document exists: Jod must add *list conversations, recover, revert,
> fork, branch out*, and *compaction when switching harnesses*. Every one of
> those is a solved problem somewhere. This is what the solutions actually look
> like underneath, so Jod copies a working model rather than inventing one.

---

## The three patterns that decide Jod's design

**1. Pointer, not deletion — with deferred destruction.**
OpenCode's revert and its compaction both work by *narrowing a query window*
(`revert.messageID`, `latestCompaction.seq`) rather than removing rows, and only
destroy on the **next prompt**. That buys a free undo window with no extra
bookkeeping. Claude Code goes further and never destroys at all.

**2. Compaction is continue-as-new.**
Claude Code's `compact_boundary` (a new root plus a `logicalParentUuid` back-link)
and Temporal's continue-as-new (new RunId plus `continuedExecutionRunId`) are the
same move: truncate the replayable history, carry a hand-compacted payload
forward, keep a backward pointer for audit. All three systems make the summary an
explicit **first-class node**, not a flag — OpenCode V2 likewise has
`{type:"compaction"}` as a message type.

**3. There are exactly two branch representations, and they are incompatible.**

| | Representation | Cost | What you lose |
|---|---|---|---|
| Claude Code, OpenCode | **Copy a prefix into a new container** (new file / new rows), no explicit parent edge | Cheap to read; no linearisation needed | Branch topology is only recoverable by ID intersection. You cannot render a sibling pager. |
| ChatGPT, LangGraph, git | **One shared DAG with a moving head pointer** (`current_node`, `checkpoint_id`, `HEAD`) | Every reader must linearise by walking parents | Nothing — this is the richer model |

Claude Code's `leafUuid`, ChatGPT's `current_node` and git's `HEAD` are the same
construct. **Only the shared-DAG designs can show a "‹ 2/3 ›" sibling pager at
all.** If Jod wants a real conversation graph — and the goal says *branch out* —
it must own the DAG, because two of its three harnesses do not keep one.

---

## 1. Claude Code

### What a checkpoint is
**A user-message boundary.** "Every user prompt creates a new checkpoint."
Confirmed three ways: the docs, the SDK (`message.uuid` of a *user* message is
the checkpoint handle), and the on-disk transcript, where `file-history-snapshot`
entries key on `messageId`. **Tool calls do not create checkpoints.**

### File checkpointing is not a shadow git repo
It is a content-addressed backup store. Two entry types do the work:

```json
{"type":"file-history-snapshot","messageId":…,"isSnapshotUpdate":false,
 "snapshot":{"messageId","trackedFileBackups":{"<relpath>":{"backupFileName","version","backupTime","realParentDir"}},"timestamp"}}
{"type":"file-history-delta","messageId","snapshotMessageId","trackingPath","backup":{…},"timestamp"}
```

Backups live at `~/.claude/file-history/<session-id>/<hash>@v<N>`.
`backupFileName: null` means the file was new at that point — rewind *deletes* it
rather than restoring content. Retention: file snapshots for the **100 most
recent** checkpoints per session; checkpoints die with sessions after **30 days**
(`cleanupPeriodDays`).

**Not tracked:** Bash-command edits, background-subagent edits, external edits,
symlink/hardlink paths (skipped, reported as `Restored the code, but skipped N files`).

### Restore is three separable things
`/rewind` (or `Esc Esc` on an empty prompt) offers *Restore code and
conversation*, *Restore conversation*, *Restore code*, plus *Summarize from here*
/ *Summarize up to here*. In the SDK the two axes are fully decoupled:
`rewindFiles(uuid)` restores **files only and explicitly does not rewind the
conversation**; conversation rewind is `resumeSessionAt`.

> **Design consequence for Jod:** code-state and conversation-state are
> independent axes. A UI that offers only "revert" conflates them. Jod's TUI
> should ask which.

### The transcript is a tree
`~/.claude/projects/<slug>/<uuid>.jsonl`, slug = cwd with every non-alphanumeric
replaced by `-`. Every content line carries `uuid` + `parentUuid` (null at root).

- **`last-prompt` carries `leafUuid`** — the HEAD pointer, rewritten as the
  conversation advances.
- **`isSidechain`** — marks a subagent branch off the main spine.
- **`isMeta: true`** — system-injected user-role messages that are not real
  prompts (hook feedback, cross-session messages, session-start context).
- Multiple children per `parentUuid` arise routinely from **parallel tool
  results**, not only from rewinds. So "has siblings" ≠ "was branched".
- `attachment` nodes interleave, so an assistant's `parentUuid` often points at
  an attachment rather than the user message.
- Line types: `user`, `assistant`, `system`, `attachment`, plus non-tree
  metadata (`file-history-snapshot`, `file-history-delta`, `last-prompt`,
  `queue-operation`, `ai-title`, `mode`, `permission-mode`, `pr-link`,
  `relocated`, `worktree-state`).
- Other observed fields: `logicalParentUuid`, `isCompactSummary`,
  `isVisibleInTranscriptOnly`, `interruptedMessageId`, `promptId`,
  `sourceToolAssistantUUID`, `slug`, `gitBranch`, `version`, `sessionKind`,
  `entrypoint`.

### Compaction is a continue-as-new — verified in a real transcript
A `system` line with `subtype: "compact_boundary"`, **`parentUuid: null`** (it
starts a *new root*; the physical chain is severed) but **`logicalParentUuid`**
pointing back at the pre-compaction tail.

```
compactMetadata = {trigger:"manual", preTokens:516172, postTokens:11046,
  cumulativeDroppedTokens:505126, durationMs, preCompactDiscoveredTools:[…],
  preservedSegment:{headUuid,anchorUuid,tailUuid},
  preservedMessages:{anchorUuid,uuids[],allUuids[]}}
```

The summary itself is a `user` line with `isCompactSummary:true,
isVisibleInTranscriptOnly:true`, child of the boundary. **Old messages stay on
disk.**

### How a fork is represented — verified by experiment
Built a 2-turn session (codeword FALCON, then changed to WALRUS), then ran
`--resume <id> --resume-session-at <first-assistant-uuid> --fork-session`.
It answered **FALCON**, proving truncation. On disk:

- The fork is a **new file / new session id**; the original is byte-for-byte
  untouched.
- Messages up to the cut are **copied verbatim, preserving their original `uuid`
  and `parentUuid`**; only `sessionId` is rewritten.
- The post-cut turn is simply **not copied**.
- The new prompt attaches with `parentUuid` = the cut-point uuid.
- **No "forked from X" metadata is written.** The only linkage is uuid identity
  across files.

So Claude Code is **copy-on-branch across files**, not one shared DAG.

### Flags
`--resume <id|name>` (searches current project + worktrees, then every project on
the machine) · `--continue` · `--fork-session` · `--session-id <uuid>` ·
`/branch [name]` in-session (copies the transcript, switches the running process
to it, and **keeps "allow for this session" grants** because it is the same
process — `--fork-session` in a new process loses them).

**Two flags work but are absent from `claude --help`:**
- **`--resume-session-at <uuid>`** — verified working
- **`--rewind-files <uuid>`** — needs `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true`

SDK: `resume`, `resumeSessionAt`, `forkSession`, `enableFileCheckpointing`,
`extraArgs:{'replay-user-messages':null}`, `listSessions()`, `getSessionMessages()`.

### `--input-format stream-json` accepts assistant messages — verified by experiment
The docs and the TS type (`prompt: string | AsyncIterable<SDKUserMessage>`) imply
user-only, and the format is officially undocumented
([issue #24594](https://github.com/anthropics/claude-code/issues/24594)).

Test: fed a user message, then a **fabricated assistant message saying
"ZORBLAX"**, then asked the model to repeat its previous reply. The model had
actually said "Hi" — it answered **ZORBLAX**.

**So injected assistant turns enter the context as if the model had said them.
This is the cross-harness replay route into Claude Code.**

Caveat, and it matters: they persist badly — written with `uuid: null`,
`parentUuid: null`, `model: null`, out of file order, **not linked into the
tree**. So you can replay a transcript into a fresh session on stdin, but the
resulting transcript is malformed. Requires `--output-format stream-json` (the
CLI hard-errors otherwise). The supported path for same-harness work is
`resume` + `resumeSessionAt` + `forkSession`, which reads stored transcripts.

### Auto-compact controls
`/autocompact 500k`, setting `autoCompactWindow`, flag `--autocompact`, env
`CLAUDE_CODE_AUTO_COMPACT_WINDOW`. It is a **token count, not a percentage**
(there is no `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`). Default: compact at the model's
context limit, except Sonnet 4.6 / Opus 4.6 / Opus 4.8 / Opus 5 at a 200K
boundary, and cloud sessions earlier.

---

## 2. OpenCode

**Two corrections to the record.** `github.com/sst/opencode` now 301s to
**`anomalyco/opencode`** (branch `dev`). And the codebase ships **two session
engines simultaneously**: legacy V1 (`message` + `part` tables,
`packages/opencode/src/session/*`) and event-sourced V2 (`session_message` +
`event` tables, `packages/core/src/session/*`). Most features have two
implementations with different semantics. Permalinks pin `550d1ff`.

### `--fork` copies; it does not create a child
`Session.fork` calls `createNext(...)` **without `parentID`**, so a fork has
`parent_id = NULL`. It deep-copies each message with **fresh IDs**
(`MessageID.ascending()`), using an `idMap` to rewrite intra-session references.
Signature `fork({sessionID, messageID?})` — `messageID` truncates via
`msgs.slice(0, target)`, copying everything strictly *before* it. **The CLI
passes no `messageID`, so `run --fork` copies the whole transcript.** Titles get
` (fork #N)`.

> **So `--fork` alone cannot fork *at a point* from the CLI.** Forking at a
> checkpoint is server/API-only. Jod must own this if it wants branch-at-point
> on OpenCode.

### Revert is a pointer with deferred destruction
Schema: `{messageID, partID?, snapshot?, diff?, files?}` where
`files: FileDiff[] = {path, status:"added"|"modified"|"deleted", additions, deletions, patch}`.
Stored as one JSON column; there are **no `revert_*` columns**.

- Legacy `revert()` rolls files back and persists the pointer — **zero rows
  deleted**. It reuses any pre-existing snapshot, so repeated reverts always
  measure against the *original* file state.
- **`unrevert()` exists** — restores files from the snapshot and clears the
  pointer, making hidden messages visible again. `POST /session/:id/unrevert`.
- **`cleanup()` is the destructive step, and it fires on the next prompt.** So
  revert is reversible until you type again.
- V2 renames these `stage`/`clear`/`commit` with events
  `session.next.revert.staged|cleared|committed`. Commit does
  `DELETE FROM session_message WHERE seq > boundary.seq`. Note `>` not `>=`, so
  the boundary message survives (unlike legacy).
- *Unverified:* no in-tree caller of the V2 `commit` at this SHA — the V1 path is
  the one demonstrably live.

### Compaction moves a read cursor; it never deletes
`DEFAULT_BUFFER = 20_000`, `DEFAULT_KEEP_TOKENS = 8_000`,
`SUMMARY_OUTPUT_TOKENS = 4_096`. Fires when
`estimate(...) > context - max(output, buffer)`.

V2 produces a first-class message: `{type:"compaction", reason:"auto"|"manual",
summary, recent}`. (Legacy V1 uses `summary: true` on an assistant message.) The
summary is **schema-constrained to fixed Markdown sections** (`## Objective`,
`## Work State`, …), and repeat compactions **merge in place** via
`<previous-summary>` rather than stacking.

`history.ts` finds the latest `type='compaction'` seq, then loads
`seq >= compaction.seq OR (type='system' AND seq > baselineSeq)`. Everything
before stays on disk and is still served to the UI.

> **Steal this:** a fixed-section summary schema, and merge-in-place on repeat
> compaction so summaries never stack into a summary-of-summaries.

### Snapshots are a shadow git repo sharing objects with yours
At `~/.local/share/opencode/snapshot/<projectID>/<fastHash(worktree)>`, driven by
`--git-dir`/`--work-tree` against your real worktree — your `.git` is never
touched, no branches, no commits, no stash. `objects/info/alternates` points at
your real repo's object store, so snapshotting an unchanged file costs nothing.
**A snapshot ID is a tree hash** (`git write-tree`), not a commit. Untracked
files capped at 2 MB. Restore is *selective per-path*, so revert only touches
files the agent changed.

### `parent_id` is for subagents, not forks
Children are real independent session rows created by the `task` tool, nesting
capped at depth 1 by default, hidden from top-level listings.
**child = `parent_id` set + empty message list; fork = `parent_id` NULL +
messages copied.**

### Export / import
`opencode export [id] [--sanitize]` → stdout, exactly
`{info: Session.Info, messages: [{info, parts: […]}]}`. `--sanitize` replaces
content with `[redacted:<kind>:<id>]`.

**`opencode import` preserves the original session ID** — it does *not* mint a
new one; it rebinds `project_id`/`directory`/`path` with `onConflictDoUpdate`,
and messages insert `onConflictDoNothing`, so **re-import is idempotent** and the
result is resumable under the same id.

Share uploads the **full transcript with no redaction** (`--sanitize` is
export-only). *Unverified:* docs advertise `opncd.ai/s/<id>` but `parseShareUrl`
only matches `/share/<id>`.

### Two vestigial things not to model against
- **`session_context_epoch` is not about reverts or snapshots.** It versions the
  **system-prompt preamble** (`baseline` = rendered context text, `snapshot` =
  provider map, `baseline_seq`). A full re-baseline is licensed only when a
  compaction is newer than the baseline.
- **`time_compacting` is vestigial** — declared, round-tripped, read by the TUI
  status indicator, and **no code path sets it**. Every local row is NULL.

---

## 3. ChatGPT — edit creates a sibling

Export `conversations.json`: each conversation has `mapping` (node-id → node) and
**`current_node`**. Each node: `id`, `parent` (null only at the synthetic root),
`children[]`, and `message` — **null for structural/root nodes**, so parsers must
handle it.

Editing a user message appends a **new sibling under the same parent** rather
than mutating; regenerating does the same for assistant nodes. The export
**retains every branch**. The UI linearises by walking back from `current_node`
via `parent`; the "‹ 2/3 ›" pager appears when a node's parent has >1 children,
and navigating just moves the active path — the abandoned branch is not deleted.
Separately, **"Branch in new chat"** forks a point into a wholly new
conversation.

*Unverified:* that server-side "only `current_node` moves and nothing is deleted"
— inferred from export contents plus product behaviour.

---

## 4. Git

Commits immutable and content-addressed; branches are **mutable pointers**
(`refs/heads/*`); `HEAD` is a symbolic ref, or a SHA when detached.

`git reflog` records when ref tips moved. **Verified locally in this repo:**
storage is `.git/logs/HEAD` and `.git/logs/refs/heads/<branch>`, including
per-worktree entries. Expiry: `gc.reflogExpire` **90 days**,
`gc.reflogExpireUnreachable` **30 days**. That window is the recovery net after a
bad reset.

`git revert` records *new* commits that invert the target — history preserved, no
SHA changes. `git reset --hard` moves the ref backward and overwrites the working
directory; Pro Git calls it "the only way to make the `reset` command dangerous,
and one of the very few cases where Git will actually destroy data."

> **The rule Jod should adopt:** revert for shared/durable history, reset only
> for local. And keep a reflog — an append-only log of where the head pointer has
> been is what makes "recover" possible at all.

---

## 5. Temporal — continue-as-new

Event History is capped because a Worker must **replay the whole history** on a
cold start. Hard limit **51,200 events or 50 MB**; warning at **10,240 events or
10 MB**.

Continue-as-new **atomically completes the current Execution and starts a new one
with the same `WorkflowId`, a new `RunId`, and a fresh empty Event History**,
passing hand-picked state as the new run's input — explicitly *manual*
compaction, not automatic snapshotting. The old run emits
`WorkflowExecutionContinuedAsNew` carrying `newExecutionRunId`; the new run
carries **`continuedExecutionRunId`** (immediate predecessor) and
**`firstExecutionRunId`** (origin of the whole chain), so the chain walks
backward.

Suggestion accessors exist in every SDK (`GetContinueAsNewSuggested()`,
`isContinueAsNewSuggested()`, `continueAsNewSuggested`).

**Gotcha worth stealing for Jod's goal loops:** Temporal **rejects incoming
signals** when a workflow tries to close/continue-as-new with buffered signals
pending, and signals can be silently dropped around the boundary. Drain them into
state and fold them into the carried input *before* continuing, and never
continue-as-new from inside a signal handler.

*Unverified:* the exact proto field carrying `SuggestContinueAsNew`, and a
per-field carry-over table for memo/retry policy
([issue #8097](https://github.com/temporalio/temporal/issues/8097)).

---

## 6. Jupyter / Observable

**Observable:** a fork is an independent copy recording its origin; upstream edits
**do not propagate**. It has a PR-like path — **Fork → Suggest → Merge** — which
emails the author a diff with Parent / Fork / Diff toggles, and the author can
merge wholesale or per-cell. "Compare reverse" lets a fork diff against an
updated parent.

**Jupyter:** `.ipynb` is JSON with a flat `cells` array — **no native branching
at all**. Forking is filesystem/git copying; `nbdime` exists because raw git
diffs of notebook JSON are unreadable. Checkpoints live in `.ipynb_checkpoints/`
as `<notebook>-checkpoint.ipynb`. *Unverified:* the cited ~120s autosave interval.

---

## 7. LangGraph and Letta

### LangGraph — the closest thing to a proper checkpoint DAG
Config `{"configurable": {"thread_id", "checkpoint_id", "checkpoint_ns"}}`.
`StateSnapshot` = `values`, `next`, `config`, `metadata`, `created_at`,
**`parent_config`**, `tasks` — `parent_config` is what makes it a chain/tree.

Invoking with a past `checkpoint_id` **replays without re-executing nodes before
the checkpoint** (results are persisted) and re-executes those after, producing a
**new fork**, leaving the original chain intact. `update_state(config, values,
as_node=...)` writes a **forked checkpoint**; `as_node` disambiguates parallel
branches, seeds an empty thread, or *skips* a node by making the graph believe it
already ran. Nodes containing `interrupt()` always re-execute.

Savers: `InMemorySaver`, `SqliteSaver`, `PostgresSaver`. Pending-writes buffering
per super-step lets a partial failure resume without re-running siblings that
succeeded (*the literal `.writes` attribute name is unverified*).

### Letta/MemGPT — the tier model the others lack
- **core memory** — in-context blocks, edited by `core_memory_append` / `core_memory_replace`
- **recall memory** — full history in a DB, `conversation_search`
- **archival memory** — vector store, `archival_memory_insert` / `_search`

A memory-pressure warning is injected as context fills; overflow triggers
**recursive summarization** — the first queue slot always holds a summary of
previously evicted data, merged with newly evicted messages on each flush.
Evicted messages remain queryable via recall, not deleted. A `Fork` operation
exists on conversations in the V1 API.

*Unverified:* the "70%" pressure threshold, and whether `Fork` covers all three
memory tiers.

### Claude Agent SDK compaction, at the stream level
`type:"system", subtype:"compact_boundary"` with `compact_metadata.trigger`
(`"manual"|"auto"`) and `pre_tokens` — both confirmed against real transcript
data (camelCase on disk, snake_case in the SDK stream).

Customisation: the `PreCompact` hook (payload includes `trigger`; runs outside
the agent's context), `/compact <instructions>` as a real SDK input, and
CLAUDE.md guidance. What survives: CLAUDE.md and skill bodies are re-injected
(skills truncated to a per-skill cap, keeping the *start* of the file);
path-scoped rules and nested CLAUDE.md get summarised away and reload on next
matching read.

**Not to be conflated:** Anthropic's *Messages API* compaction is a separate
server-side surface — beta header `anthropic-beta: compact-2026-01-12`,
`context_management.edits` with
`{"type":"compact_20260112","trigger":{"type":"input_tokens","value":150000}}`
(min 50,000) — triggered by an absolute input-token count, unlike the CLI's
window setting.

---

## Sources

- Claude Code: [checkpointing](https://code.claude.com/docs/en/checkpointing) ·
  [sessions](https://code.claude.com/docs/en/sessions) ·
  [cli-reference](https://code.claude.com/docs/en/cli-reference) ·
  [agent-sdk/sessions](https://code.claude.com/docs/en/agent-sdk/sessions) ·
  [file-checkpointing](https://code.claude.com/docs/en/agent-sdk/file-checkpointing) ·
  [context-window](https://code.claude.com/docs/en/context-window) ·
  [streaming-input](https://code.claude.com/docs/en/agent-sdk/streaming-input) ·
  [agent-loop](https://code.claude.com/docs/en/agent-sdk/agent-loop) ·
  [API compaction](https://platform.claude.com/docs/en/build-with-claude/compaction)
- OpenCode: `anomalyco/opencode` @ `550d1ff`
- ChatGPT: [export format](https://ai-chat-importer.com/guides/chatgpt-export-format-explained) ·
  [sanand0/openai-conversations](https://github.com/sanand0/openai-conversations) ·
  [pionxzh/chatgpt-exporter](https://github.com/pionxzh/chatgpt-exporter) ·
  [OpenAI help: export](https://help.openai.com/en/articles/7260999-how-do-i-export-my-chatgpt-history-and-data)
- Git: [git-reflog](https://git-scm.com/docs/git-reflog) ·
  [git-revert](https://git-scm.com/docs/git-revert) ·
  [git-gc](https://git-scm.com/docs/git-gc) ·
  [Reset Demystified](https://git-scm.com/book/en/v2/Git-Tools-Reset-Demystified)
- Temporal: [limits](https://docs.temporal.io/workflow-execution/limits) ·
  [continue-as-new](https://docs.temporal.io/workflow-execution/continue-as-new)
- Observable: [forking](https://observablehq.com/documentation/notebooks/forking) ·
  [suggestions](https://observablehq.com/documentation/collaboration/suggestions) ·
  [nbdime](https://github.com/jupyter/nbdime)
- LangGraph: [time travel](https://docs.langchain.com/oss/python/langgraph/use-time-travel) ·
  [persistence](https://docs.langchain.com/oss/python/langgraph/persistence)
- Letta: [memory management](https://docs.letta.com/concepts/memory-management/)
