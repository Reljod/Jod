# Hermes Agent — source-verified feature audit, and the gap against Jod

**Date:** 2026-08-10 · **Analyst:** Jod · **Subject:**
[NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) at commit
`03fa32c92dd445eb64c7f67434dd91b32c40701d` (2026-08-10) ·
**Supersedes in part:** [`../harness-agents-research/HERMES.md`](../harness-agents-research/HERMES.md)
(2026-08-09, docs-only)

> **Provenance of this file.** The audit was carried out by a subagent whose
> `Write` to this path was refused by a harness hook ("subagents should return
> findings as text"). It filed [`BLOCKED.md`](BLOCKED.md) rather than routing
> around the check, and delivered the report as text. This file is that text,
> written by the lead session. The measurements and citations are the
> subagent's; nothing was added to them.

**Scope, as asked:** Telegram, Memory, Chat, Cron/scheduling. Everything else is
one short list at the end.

**Method.** The prior study was docs-only and said so. This one reads the
repository: a `--filter=blob:none` clone pinned at the commit above, 8,713
files, sparse-checked-out over `plugins/`, `gateway/`, `cron/`, `tools/`,
`agent/`, `hermes_cli/`, `website/docs/`. Every claim is tagged **[src]** (read
in source at a cited path:line), **[doc]** (repo documentation only), or
**[doc, src]**. Where something was not read, it says *not verified* rather than
guessing.

## The answer in one paragraph

The prior study's picture of memory holds up and gets sharper, but its framing
of Hermes as a harness with "no cron" is wrong — and that was the single most
important thing to get right, because scheduling is Jod's roadmap item 7. Hermes
ships a **durable, plug-in-backed cron scheduler of 11,406 lines** with cron
expressions, intervals, one-shots, job chaining, change-detection monitors, and
LLM-free script jobs, plus **two further autonomous loops** the prior study never
saw: `/goal` (a Ralph-style judge loop with quality gates) and `/heartbeat` (a
recurring prompt inside a live session). Telegram is not a thin bot but a
**10,271-line adapter** under a 7,256-line `BasePlatformAdapter` ABC shared by
~22 platforms. The most transferable ideas for Jod are not the memory caps
everyone quotes — they are the **cron job record**, the **durable delivery
ledger**, and the **monitor-hash suppression** pattern.

---

## 1. Hypotheses and grades

Written before the evidence was gathered, graded after.

| # | Hypothesis | Grade | Decided by |
|---|---|---|---|
| **H1** | Hermes has no durable cron; scheduling is inactivity-triggered (curator) only | **REFUTED** — decisively | `cron/` is a top-level package, 11,406 lines across 10 modules; `cron/__init__.py:1-18` [src] |
| **H2** | Telegram is a first-class adapter behind a shared multi-platform abstraction | **CONFIRMED** | `gateway/platforms/base.py:2878` `class BasePlatformAdapter(ABC)`, 7,256 lines; Telegram at `plugins/platforms/telegram/adapter.py`, 10,271 lines [src] |
| **H3** | Per-chat session mapping is a deterministic key over (platform, chat, thread, user) | **CONFIRMED** | `gateway/session.py:1087` `build_session_key(...)` — "the single source of truth for session key construction" [src] |
| **H4** | Auth is allowlist + DM pairing, denying by default | **CONFIRMED** | `plugins/platforms/telegram/plugin.yaml` (`TELEGRAM_ALLOWED_USERS`, `TELEGRAM_ALLOW_ALL_USERS`) [src]; "the gateway denies all users who are not in an allowlist or paired via DM" [doc] |
| **H5** | Streaming is progressive message edits with a typing indicator | **CONFIRMED, and richer than assumed** | Four transports `auto\|draft\|edit\|off`; Bot API 9.5 native `sendMessageDraft` [doc]; `REQUIRES_EDIT_FINALIZE`, `FALLBACK_ON_FINAL_EDIT_FLOOD` at `adapter.py:665-673` [src] |
| **H6** | Chunking splits at Telegram's 4096 limit | **CONFIRMED, with a UTF-16 subtlety** | `MAX_MESSAGE_LENGTH = 4096` measured in **UTF-16 code units** via `utf16_len`, `_SPLIT_THRESHOLD = 4000`, `RICH_MESSAGE_MAX_CHARS = 32768` — `adapter.py:645-656`, `4573`, `4900` [src] |
| **H7** | Memory caps are 2,200 / 1,375 chars and configurable | **CONFIRMED** | `hermes_cli/config_defaults.py:1709-1710`; `tools/memory_tool.py:165` [src] |
| **H8** | Memory overflow is a hard error, not truncation | **CONFIRMED, with an escape hatch the docs omit** | `memory_tool.py:430-441` returns `success: False`; but `_MAX_CONSOLIDATION_FAILURES_PER_TURN = 3` (`:163`) then returns a *terminal* "stop retrying" result so a failed write can't starve the user's reply [src] |
| **H9** | The `memory` tool has exactly three actions: add / replace / remove | **CONFIRMED but incomplete** | Three actions, yes — but the schema also has a required `target` (`memory`\|`user`) and a batched **`operations` array** applied atomically against the *final* budget. `memory_tool.py:1160-1224` [src] |
| **H10** | `session_search` returns raw messages with no LLM summarisation | **CONFIRMED** — README is stale | `tools/session_search_tool.py:22` "No LLM calls anywhere — every shape returns actual messages from the DB", and `:25-28` records that the summary path was *removed* [src] |
| **H11** | `state.db` schema is version 23 | **REFUTED (superseded)** | `hermes_state_common.py:167` `SCHEMA_VERSION = 25` [src] |
| **H12** | `parent_session_id` implements compaction-triggered session forking | **CONFIRMED** | self-referential FK (`hermes_state_common.py:207-263`); used at `agent/conversation_compression.py:1210,1282,3350,3570` [src] |
| **H13** | The memory-provider interface is a documented ABC a graph provider could implement | **CONFIRMED** | `agent/memory_provider.py:81` `class MemoryProvider(ABC)`, 4 abstract + 14 optional hooks [src] |
| **H14** | The "periodic nudge" is a system-prompt instruction, not a real mechanism | **REFUTED** | A real iteration counter: `agent/agent_init.py:1698` `agent._memory_nudge_interval = 10`, config `memory.nudge_interval`; twin `_skill_nudge_interval` at `:1797-1801` [src] |
| **H15** | Hermes has no "goals" / autonomous-loop concept | **REFUTED** | `hermes_cli/goals.py` (2,143 lines) and `hermes_cli/heartbeat.py` (332 lines) [src] |
| **H16** | Cron jobs live in `state.db` alongside sessions | **REFUTED** | `~/.hermes/cron/jobs.json`, atomic temp-file rename, `flock` advisory locking — `cron/jobs.py:4,85` [src] |

Two of the prior study's three open questions are now closed (H10, H14).

---

## 2. Iteration log

**Honest sequencing note.** The 10-pass requirement arrived after evidence
gathering had begun. The rubric below was *not* fixed before passes 1–8; those
are graded **retrospectively**, reconstructed from the actual tool record.
Passes 9–12 were run with the rubric in hand and deliberately aimed at the
weakest claims in the draft. This is recorded rather than presented as twelve
uniformly live-scored passes.

### Rubric (fixed before passes 9–12)

| Criterion | Weight | Test |
|---|---|---|
| **C1 Coverage** | 25 | Every core feature in the four areas found, not just those the prior report named |
| **C2 Source-verification rate** | 30 | Share of *load-bearing* claims read at a path:line vs docs-only. Highest weight — it was the point of the task |
| **C3 Open questions resolved** | 15 | The prior report's four, closed with a citation |
| **C4 Actionability** | 15 | "What Jod should build" ordered, sized, each with a rationale |
| **C5 Jod-side correctness** | 15 | The gap matrix's Jod column verified, not assumed |

### The twelve passes

| # | What changed | What I learned | C1 | C2 | C3 | C4 | C5 | **Total** |
|---|---|---|---|---|---|---|---|---|
| 1 | Read charter, `jod-system.md`, prior `HERMES.md` | Baseline; open questions enumerated | 1 | 0 | 0 | 1 | 6 | **13** |
| 2 | GitHub API tree + WebSearch | First crack in H1: a snippet said the gateway "runs the cron scheduler, ticking every 60 seconds" | 3 | 1 | 1 | 1 | 6 | **23** |
| 3 | Directory listings: `cron/`, `gateway/platforms/`, `tools/` | `cron/` is a **top-level package**. H1 in serious doubt | 5 | 2 | 1 | 2 | 6 | **32** |
| 4 | **Method shift**: found Bash network access → blobless clone | No new findings, but every later pass becomes source-grade. Highest-leverage pass, lowest immediate score | 5 | 3 | 1 | 2 | 6 | **35** |
| 5 | `cron/__init__.py`, `jobs.py`, `parse_schedule`, `cronjob` tool schema | **H1 refuted in source.** Job record captured incl. `monitor_*`, `no_agent`, `context_from` | 7 | 6 | 2 | 5 | 6 | **55** |
| 6 | `goals.md`, `heartbeat.md`; `suggestions.py`, `blueprint_catalog.py` | **H15 refuted** — but from *docs only*. Weakness introduced here | 8 | 6 | 2 | 6 | 6 | **59** |
| 7 | Telegram adapter, `base.py`, `build_session_key` | H2/H3/H5/H6 confirmed in source; UTF-16 chunking found | 9 | 7 | 3 | 7 | 6 | **68** |
| 8 | `memory_tool.py`, `session_search_tool.py`, `memory_provider.py`, `hermes_state_common.py` | H7–H14 resolved; four corrections to the prior report | 10 | 8 | 10 | 8 | 6 | **85** |
| 9 | **Re-probed the Jod side**: `deploy/`, repo-wide cron grep | **Refuted my own filed claim** — see reversals | 10 | 8 | 10 | 8 | 10 | **91** |
| 10 | Read `hermes_cli/goals.py` (2,143 ln), `heartbeat.py` | H15 upgraded docs→source; two invariants that change the recommendation | 10 | 9 | 10 | 9 | 10 | **96** |
| 11 | `gateway/delivery_ledger.py` constants | Ledger claims upgraded docs→source with exact values | 10 | 9.5 | 10 | 9 | 10 | **97** |
| 12 | `scheduler_provider.py`, `scheduler.py` monitor path | Tick interval and `no_change` suppression upgraded docs→source | 10 | 10 | 10 | 9 | 10 | **98.5** |

### Which pass produced the final answer

**Pass 12** — and that is a boring answer rather than a vindication of "last pass
wins." This is an evidence-accumulation task: no pass discards a prior finding,
it only verifies or corrects it, so the ranking is near-monotonic **by
construction**. A rubric weighted 30% on source-verification rate can
essentially only climb.

That makes the ranking table the least interesting output here. The real signal
is **pass 4** — the method shift from WebFetch summaries to reading source,
which scored only +3 immediately but made every subsequent pass possible — and
the reversals below. If the task had been choosing among competing designs
rather than accumulating evidence, a mid-run peak would be meaningful; here,
claiming one would be manufactured.

### Reversals — where a later pass refuted an earlier one

1. **P3/P5 refuted the prior report's central scheduling claim** (H1). "Hermes
   has no durable cron" → 11,406 lines of it. The largest correction in the
   audit. Note the trail: P2's *search snippet* already said so, but it was
   correctly not graded decisive until P5 read `cron/__init__.py`.
2. **P9 refuted this report's own draft, not the prior one.** The draft claimed
   "Jod has no scheduling — zero `cron|schedule` matches in `core/src` or
   `cli/src`." That grep was too narrow: `.github/workflows/pr-shepherd.yml:25-26`
   runs `cron: "0 * * * *"`. **Corrected claim: `jod-core` has no scheduler; the
   repo has one hourly GitHub Actions cron.** It matters beyond pedantry — it is
   an existence proof that an hourly-sweep pattern is already accepted here.
3. **P10 refuted the docs-derived reading of `/goal` gates.** Source
   (`goals.py:427-433`) inverts the order: *"Gates run at turn boundary BEFORE
   the LLM judge. A failing gate short-circuits — its output IS the evidence the
   agent must repair against. Deterministic — no judge involved. Only when every
   gate passes does the judge get to decide DONE."* The deterministic check is a
   **precondition for consulting the judge at all**, not a peer of it.
4. **P10 also inverted a natural assumption about failure.** `goals.py:18` —
   *"Judge failures are fail-OPEN: `continue`. A broken judge must not wedge
   progress; the turn budget is the backstop."*
5. **P8 refuted the prior report on four counts** — the `memory` tool's shape,
   `session_search` summarisation, schema version, and the nudge.

---

## 3. Memory

### 3.1 The caps, confirmed

`hermes_cli/config_defaults.py:1709-1710` [src]:

```python
"memory_char_limit": 2200,   # ~800 tokens at 2.75 chars/token
"user_char_limit": 1375,     # ~500 tokens at 2.75 chars/token
```

Both user-overridable. The prior study's numbers were right.

### 3.2 Overflow — and the part the docs don't tell you

An `add` that would exceed the limit returns `success: False` with the current
entries inlined and an instruction to consolidate *in this turn*
(`memory_tool.py:430-441`) [src].

**But there is a circuit breaker the docs omit** (`memory_tool.py:159-163`) [src]:

```python
_MAX_CONSOLIDATION_FAILURES_PER_TURN = 3
```

After three failed consolidation attempts in one turn the tool returns a
*terminal* `{"success": False, "done": True}` telling the model to leave memory
alone and answer the user. The comment names the failure it prevents: a fragile
replace/add looping the turn to budget exhaustion and **suppressing the user's
reply**. The design rule is worth stating plainly: *a failed memory side effect
must never block the turn's reply.*

### 3.3 The exact tool schema — corrected

The real schema (`memory_tool.py:1160-1224`) [src] has **two shapes**, and
`target` is the only required field:

```json
{
  "name": "memory",
  "parameters": {
    "type": "object",
    "properties": {
      "action":   {"enum": ["add", "replace", "remove"]},
      "target":   {"enum": ["memory", "user"]},
      "content":  {"type": "string"},
      "old_text": {"type": "string"},
      "operations": {"type": "array", "items": {"properties": {
        "action": {"enum": ["add", "replace", "remove"]},
        "content": {"type": "string"}, "old_text": {"type": "string"}
      }, "required": ["action"]}}
    },
    "required": ["target"]
  }
}
```

The **batch shape is the recommended one**, for a token reason
(docstring at `:565-573`) [src]:

> All operations are validated and applied against the FINAL budget —
> intermediate overflow is irrelevant. This lets the model free space
> (remove/replace) and add new entries in a SINGLE tool call instead of the
> multi-turn consolidate-then-retry dance that re-sends the whole conversation
> context several times.

Semantics are all-or-nothing. **The cap forces consolidation, and the batch
shape makes consolidation cost one call instead of four.** Without the batch,
the hard cap would be a token tax.

### 3.4 The frozen snapshot, confirmed in source

`memory_tool.py:171` [src]: `self._system_prompt_snapshot` is "set once at
`load_from_disk()`". Writes hit disk immediately; the prompt copy does not move
until the next session.

### 3.5 The nudge — open question resolved

`agent/agent_init.py:1698` [src]:

```python
agent._memory_nudge_interval = 10          # config: memory.nudge_interval
agent._skill_nudge_interval  = 10          # config: skills.creation_nudge_interval
```

An **iteration counter**, not a clock and not a prompt line.
`agent/background_review.py:841-842` sets both to `0` inside the background
review fork, so the reviewer is never nudged to review [src].

### 3.6 `state.db`

`SCHEMA_VERSION = 25` (`hermes_state_common.py:167`) [src] — the docs' 23 is
stale. Tables (`:198-334`) [src]: `schema_version`, `system_prompts`,
`sessions`, `messages`, `session_model_usage`, `state_meta`, `gateway_routing`,
`compression_locks`, `async_delegations` — plus FTS5 `messages_fts` and
`messages_fts_trigram`.

`sessions` is 50 columns, now carrying `session_key`, `title`/`title_source`,
`archived`, `pinned`, `last_read_at`, `rewind_count`, `handoff_state`, and four
compression-failure columns. **The compression columns are a tell: they exist
because compaction can fail repeatedly and the system needs to stop trying.**

`messages` carries `active`, `compacted`, `observed`, `api_content`. The
`active`/`compacted` pair is how a compacted message stays searchable while
leaving the live context.

Auto-maintenance (`config_defaults.py:2733-2775`) [src]: prune after
`retention_days: 90`, auto-archive after `auto_archive_days: 3`, `VACUUM` no
more than once per 30 days, coordinated through `state_meta` **in the database
itself** so the cadence is shared across processes. Both default to **off** —
"silently deleting it could surprise users."

### 3.7 `session_search` — open question resolved

**No summarisation.** `tools/session_search_tool.py:22` [src]:

> All three modes operate on the SQLite session DB via the FTS5 index … **No LLM
> calls anywhere — every shape returns actual messages from the DB.**

`:25-28` records that PR #20238's summary mode was *removed*. The README line
the prior study flagged is stale copy.

There are **four** shapes, not three (`:982-1060`) [src]:

1. **Discovery** (`query`) — FTS5, deduped by session lineage. Each hit carries
   `snippet`, `bookend_start` (first 3 messages — the goal), `messages` (±5
   around the match), and `bookend_end` (last 3 — the resolution). *"Bookends +
   window together let you reconstruct goal → match → resolution without paying
   for the whole transcript."*
2. **Scroll** (`session_id` + `around_message_id`) — with the boundary message
   deliberately repeated in both windows as an orientation marker.
3. **Read** (`session_id` alone) — first 20 + last 10 when large.
4. **Browse** (no args) — recent sessions chronologically.

The description also carries a **source-first guardrail** telling the model that
session history is not evidence about the current state of an external source
[src]. Prompt-level epistemics inside a tool schema; cheap and smart.

### 3.8 The provider interface — what a graph provider would need

`agent/memory_provider.py:81` [src]. **Four abstract members:** `name`,
`is_available()` (**"should not make network calls"**), `initialize(session_id,
**kwargs)`, `get_tool_schemas()`.

`initialize` may receive `agent_context` (`primary`|`subagent`|`cron`|`flush`),
with a warning worth heeding: *"Providers should skip writes for non-primary
contexts (cron system prompts would corrupt user representations)"* [src]. **A
graph memory that indexed its own cron runs would poison itself.**

**Fourteen optional hooks**, all no-op by default. The load-bearing pair:

| Hook | When | Note |
|---|---|---|
| `prefetch(query, session_id)` | before **each** API call | must be fast; "use background threads … and return cached results here" |
| `queue_prefetch(query, session_id)` | after each turn | queues the recall the *next* `prefetch` consumes |
| `on_pre_compress(messages)` | before compaction | the hook for salvaging facts before they're summarised away |
| `sync_turn(...)`, `handle_tool_call(...)`, `on_turn_start(...)`, `on_session_end(...)`, `on_delegation(...)`, `on_memory_write(...)`, `backup_paths()`, `shutdown()` | various | |

**The prefetch/queue_prefetch split is the load-bearing design**: recall is never
on the critical path. Any graph provider Jod builds should adopt this two-phase
shape rather than querying synchronously.

Governing constraint [doc]: **exactly one external provider may be active, and
the built-in memory is always active alongside it** — a provider augments, never
replaces.

*Not verified:* the nine providers' internal algorithms, **including Hindsight**
(the knowledge-graph one, and the closest analogue to a Jod graph memory).

---

## 4. Cron and scheduling — the section the prior study got wrong

**Direct answer to the question asked: yes, Hermes has real durable, time-based
scheduling.** The curator's inactivity trigger is *not* the only time-based
mechanism — it is one of four, and the least capable.

- A **`hermes cron` command** and a top-level `cron/` package of **11,406 lines**.
- **Absolute wall-clock scheduling**: 5-field cron via `croniter`, fixed
  intervals, relative one-shots, ISO timestamps — all parsed by `cron/jobs.py:612`
  `parse_schedule` [src].
- **Durability across restart**: jobs persist to `~/.hermes/cron/jobs.json`; the
  docs state cron "survives process restart: **yes — fully durable**" [doc].
- A **60-second tick** at `cron/scheduler_provider.py:58` (`interval: int = 60`) [src].

So Jod is **matching, not exceeding**, Hermes on the existence of scheduling —
and Hermes is ahead on job semantics. Jod would exceed it by putting jobs in
SQLite rather than a JSON file, which is the one place Hermes' design is weaker
than Jod's existing store.

### 4.1 Schedule grammar

`parse_schedule` [src] — four formats, one string field:

| Input | Kind |
|---|---|
| `30m`, `2h`, `1d` | `once`, relative |
| `every 30m`, `every 2h` | `interval` |
| `0 9 * * *` | `cron` (5-field, validated at parse time) |
| `2026-02-03T14:00` | `once`, absolute |

Naive timestamps are anchored to the **configured Hermes timezone, not the
server's** — a naive `20:07` read as server-local UTC while `now()` runs in
`Asia/Kolkata` lands hours off, far enough that one-shots never become due
(issue #51021) [src]. **Jod will hit this exact bug.**

### 4.2 The job record — the most transferable artefact here

`create_job`, `jobs.py:1569-1650` [src]:

| Field | What it buys |
|---|---|
| `prompt`, `schedule`, `name`, `repeat` | the basics; `repeat=None` = forever |
| `deliver` | `origin`\|`local`\|`all`\|`platform:chat:thread`, comma-combinable; resolved **at fire time**, so a job created before a channel existed picks it up later |
| `skills` | ordered skills loaded before the prompt |
| `model`/`provider`/`base_url` | per-job inference override |
| `script` | stdout injected into the prompt as context |
| **`no_agent`** | skip the LLM entirely — the script *is* the job. **Empty stdout = silent**; non-zero exit = error alert |
| **`monitor_script`/`monitor_url`** | run first, hash the exact bytes: unchanged ⇒ **suppress the agent run entirely**; changed ⇒ inject a `MONITOR CHANGE DETECTED` diff. `cron/scheduler.py:3347-3358` [src] |
| **`context_from`** | job ids whose latest output is injected — job chaining |
| `enabled_toolsets` | restrict tools to cut input tokens |
| `workdir` | run as if launched there; jobs with `workdir` run **sequentially** |
| **`attach_to_session`** | makes delivery **continuable** — opens a thread so replying works |

`no_agent` + `monitor_*` together are the important idea: **most scheduled work
should not wake a model.** A watchdog is a script and a hash. For an agent
Reljod pays per token to run 24/7, this is the difference between a scheduler and
a bill.

**Concurrency:** `claim_dispatch()`, `claim_job_for_fire(claim_ttl_seconds=300)`
keyed on `_machine_id()`, `heartbeat_run_claim(expected_owner=…)`,
`advance_next_runs()` [src] — a claim-with-TTL protocol, plus ticker heartbeat
and catch-up counters for observability.

**Storage:** `~/.hermes/cron/jobs.json` with `flock` [src]. **This is the one
place not to copy Hermes**: Jod already has SQLite with `BEGIN IMMEDIATE` and a
benchmark saying so. There is code commenting on a root-owned `jobs.json` that
"failed every tick for ~14h".

There is a documented **recursion guard** — cron-run sessions must not
recursively schedule more cron jobs [src, doc].

### 4.3 Suggestions and blueprints

Two modules exist purely so users never write cron expressions.

**Suggestions** (`cron/suggestions.py:1-26`) [src] — four origins: `catalog`,
`blueprint`, **`usage` (the background self-improvement review noticed a
recurring ask a scheduled job would serve)**, and `integration`. Two rules
stated in source: *"Accepting a suggestion just calls the existing
`cron.jobs.create_job` … there is NO second job engine"*, and *"Suggestions never
auto-create jobs; acceptance is always explicit (consent-first)."*

**Blueprints** (`cron/blueprint_catalog.py:1-22`) [src] — one parameterised
definition with typed slots that every surface renders natively. *"Design
choice: users never type raw cron."*

The self-improvement loop noticing a repeated ask and *offering* a job — rather
than creating one — is the sharpest instance of Hermes' consent-first pattern.

### 4.4 `/goal` — the Ralph loop

`hermes_cli/goals.py`, 2,143 lines [src]. After every turn a **judge call** asks
an auxiliary model whether the goal is satisfied; on `continue` a continuation
prompt is fed back into the *same* session. State persists in
`SessionDB.state_meta` keyed `goal:<session_id>`.

**Constants** [src]: `DEFAULT_MAX_TURNS = 20`, `DEFAULT_JUDGE_TIMEOUT = 30.0`,
`DEFAULT_MAX_CONSECUTIVE_PARSE_FAILURES = 3`,
`DEFAULT_MAX_CONSECUTIVE_TRANSPORT_FAILURES = 5`,
`DEFAULT_GATE_TIMEOUT_SECONDS = 300`, `DEFAULT_GATE_MAX_RETRIES = 3`.

Two invariants only source shows:

1. **Gates run before the judge, not beside it** (`goals.py:427-433`): a failing
   gate short-circuits, and *its output IS the evidence the agent must repair
   against*. Only when every gate passes does the judge get to decide DONE.
2. **Judge failures are fail-OPEN** (`goals.py:18`): *"A broken judge must not
   wedge progress; the turn budget is the backstop."*

Commands: `/goal <text>`, `/goal draft` (a structured **completion contract** —
`outcome`, `verification`, `constraints`, `boundaries`, `stop_when`), `show`,
`status`, `pause`, `resume`, `clear`, `/goal gate add <command>`,
`/goal wait <pid>`.

### 4.5 `/heartbeat`

`hermes_cli/heartbeat.py`, 332 lines [src]. One recurring instruction per
session: `/heartbeat every 10m Check the deployment and report meaningful changes`.

| | `/heartbeat` | `hermes cron` |
|---|---|---|
| Runs in | **this conversation** — full context, same prompt cache | a fresh isolated session per tick |
| Survives restart | state survives; firing needs the owning process | **yes — fully durable** |
| How many | one per session | unlimited |

Five behaviours worth copying verbatim: **idle-only**; **missed ticks coalesce**
into one fire with the timer re-anchored; **user messages win**; **cache-safe**
(an ordinary user-role message, no system-prompt mutation); and a
**don't-invent-work guard**. That last one is the difference between a heartbeat
and a machine that generates busywork forever.

### 4.6 The delivery ledger

`gateway/delivery_ledger.py` [src]. A durable row per outbound final response,
with three checkpoints: `record_obligation()` → `pending`, `mark_attempting()` →
`attempting`, then `mark_delivered()` / `mark_failed()`. On startup
`sweep_recoverable()` claims rows whose owning process is dead.

Constants (`:58-72`), deliberately **not** configurable [src]:

```python
MAX_ATTEMPTS = 3
STALE_AFTER_SECONDS  = 24 * 60 * 60
_RETENTION_SECONDS   = 7 * 24 * 60 * 60
_MAX_ROWS = 500
RECOVERED_MARKER = ("♻️ Recovered reply — the gateway restarted during delivery, "
                    "so this may be a duplicate:\n\n")
```

Crash semantics are explicitly honest at-least-once: a `pending` row never
started sending, so it is redelivered plainly; an `attempting` row may or may not
have arrived, so it is redelivered **with the visible marker**. Ambiguity is
labelled, never silently resent.

---

## 5. Telegram

### 5.1 Shape of the abstraction

- **`gateway/platforms/base.py`** — `BasePlatformAdapter(ABC)`, 7,256 lines
  [src]. Carries *shared behaviour*: media extraction from `MEDIA:` tags,
  image/video/voice/document sending, TTS, ephemeral replies, exec-approval
  rendering, typing start/stop, per-chat max-length policy.
- **`plugins/platforms/telegram/`** — the adapter (10,271 lines) plus
  `telegram_network.py` (direct-IP DNS-over-HTTPS failover) [src]. A **plugin**
  with `plugin.yaml`, so a platform is installable rather than compiled in.

~22 platforms hang off this.

### 5.2 Per-chat session mapping

`build_session_key()` (`gateway/session.py:1087`) [src]:

```
<profile-namespace>:<platform>:<chat_type>[:scope_id][:chat_id][:user_id][:thread_id]
```

- **DMs** include `chat_id`; when an adapter supplies none it falls back to the
  *sender's* id rather than collapsing every user into one shared `…:dm` session
  — the comment names this as a cross-user history-bleed bug it exists to prevent.
- **Group threads are shared across participants by default**; group *non-thread*
  messages are per-user by default.

### 5.3 Auth

Default-deny. `TELEGRAM_ALLOWED_USERS` is the allowlist; `GATEWAY_ALLOW_ALL_USERS=true`
is the documented foot-gun ("NOT recommended for bots with terminal access")
[doc]. Unknown DMs get a one-time pairing code, expiring after 1 hour,
rate-limited [doc].

Above that sits an **admin/user tier split, per scope**. Multi-profile gateways
read the allowlist through `_platform_gate_env` rather than `os.getenv`, because
the process env is first-writer-wins and a raw getenv returns *another profile's
allowlist* (`adapter.py:38-56`) [src]. That is the kind of bug that only shows up
in production.

### 5.4 Streaming, typing, chunking

**Streaming**: four transports, `auto`|`draft`|`edit`|`off` [doc]. Three class
flags encode hard-won failure handling (`adapter.py:665-673`) [src]:
`REQUIRES_EDIT_FINALIZE` (MarkdownV2 conversion only happens on finalize, so an
unchanged-text short-circuit would ship raw markdown),
`FALLBACK_ON_FINAL_EDIT_FLOOD`, and `RESEND_FINAL_ON_EMPTY_STREAM_FALLBACK`.

**Chunking**: `MAX_MESSAGE_LENGTH = 4096` measured with `utf16_len` — Telegram
counts **UTF-16 code units, not Python characters**, so an emoji-heavy reply that
looks legal by `len()` is not [src]. `_SPLIT_THRESHOLD = 4000`. Chunk indicators
are separated from code fences so a split never lands inside a fence [src]. Rich
Messages raise the ceiling to `RICH_MESSAGE_MAX_CHARS = 32768`.

**Ingress batching** is adaptive: ≤320 codepoints settle in 180 ms, ≤1024 in
240 ms — "tuned for *feels instant*" (`adapter.py:684-690`) [src].

### 5.5 How a long run reports back

The part most relevant to Jod; Hermes answers it four ways:

1. **`long_running_notifications`** — a single edit-in-place "⏳ Working — N min"
   bubble, "so you have a heartbeat instead of staring at `typing…` for half an
   hour" [doc]. On by default for Telegram.
2. **`tool_progress`** — per-tool breadcrumbs, defaulting to **`off`** on
   Telegram because "Telegram is usually a mobile inbox" [doc].
3. **`/background <prompt>`** — separate agent instance, isolated session,
   returns a task id immediately, delivers "✅ Background task complete" [doc].
   Near-exactly Jod's `/delegate`.
4. **The delivery ledger** — §4.6.

Two smaller touches: **intentional silence tokens** (`[SILENT]`, `NO_REPLY`) —
delivery suppressed but the turn still stored [doc] — and **observe mode**.

---

## 6. Chat

### 6.1 Session model

Sessions persist until reset, and **by default never auto-reset** [doc]. Opt-in
`session_reset.mode` = `none`|`idle`|`daily`|`both`.

A live background process normally protects its session from resetting — but a
process older than `bg_process_max_age_hours` (default 24) stops blocking reset.
The process is **not killed, only ignored by the reset guard** [doc].

### 6.2 Compaction and forking

Compaction **forks rather than destroys**. `parent_session_id` is a
self-referential FK written by the compression path
(`agent/conversation_compression.py:1282`, `:3350`) [src]. Messages summarised
out of live context keep `active=0, compacted=1` rather than being deleted,
which is why `session_search` can still find them — and `_is_compaction_summary()`
filters the *machine-generated* summaries back out of results so the agent reads
original messages, not its own compression artefacts [src].

The four `compression_*` columns implement giving up gracefully: a cooldown
timestamp, a fallback streak, and an "ineffective count" for compressions that
ran but freed nothing.

### 6.3 Resume

`/sessions` lists; `/sessions <name>` resumes; `/sessions search <query>` filters.
**`/sessions all` is admin-only — regular users only see sessions from their own
chat origin** [doc]. Sessions are addressable as `@session:<profile>/<id>` links.

A `/model` override **survives gateway restart**, persisted to the session store,
credentials re-resolved at load and never written to disk [doc].

After an unclean shutdown, sessions with an in-flight tool call are flagged
`restart_interrupted` and auto-resume is scheduled [doc].

### 6.4 Streaming UX and interruption

Three documented busy-input modes — **queue**, **interrupt**, **steer** — where a
redirect restarts model generation with context, retaining already-shown
reasoning as "an ordinary assistant checkpoint" [doc]. Jod queues; Hermes queues,
interrupts, *or* steers.

---

## 7. Gap matrix — Hermes vs Jod today

Jod's state verified in this worktree: no `cron`/`schedule` match in `core/src/`
or `cli/src/`; no `telegram` or `webhook` match in any `.rs` file; `deploy/`
contains only `jod-api.service`. **Caveat from pass 9:**
`.github/workflows/pr-shepherd.yml:25-26` runs `cron: "0 * * * *"`.

| Feature | Hermes | Jod today | Verdict | Why |
|---|---|---|---|---|
| **Durable scheduler** | `cron/`, 11.4k lines, 60 s tick, claim-with-TTL | none in `jod-core` | **steal** | Jod's supervisor + store already do the hard part; a scheduler is a due-query and a spawn |
| **Job record shape** | 18 fields | n/a | **adapt** | Take `deliver`, `context_from`, `workdir`, `repeat`. Drop `skills`/`enabled_toolsets` — harness-owned |
| **`no_agent` script jobs** | script runs, stdout verbatim, empty = silent | none | **steal** | A watchdog that costs zero tokens |
| **`monitor_*` hash suppression** | unchanged bytes ⇒ no agent run | none | **steal** | Highest-value idea here. Converts "poll with an LLM" into "wake the LLM when reality moved" |
| **Jobs in JSON + flock** | `jobs.json` | n/a | **skip** | `agent-db-2026` already settled this. Jobs go in `jod.db` |
| **Timezone anchoring** | naive stamps anchored to configured TZ | n/a | **steal** | One-line rule preventing a whole class of "never fired" bug |
| **`attach_to_session`** | delivery becomes continuable | n/a | **adapt** | A digest you can reply to beats one you can only read |
| **Delivery ledger** | pending→attempting→delivered, ambiguity **labelled** | none | **steal** | Jod's thesis is a failed run must never look successful. An undelivered digest is exactly that |
| **`/goal` + quality gates** | gates run **before** the judge; fail-open judge; turn budget | none | **adapt** | Charter already says *every task needs one runnable check*. The gate **is** that check |
| **`/heartbeat`** | recurring prompt, idle-only, coalescing | none | **adapt** | Cheap once a scheduler exists |
| **Suggestions (consent-first)** | proposes, never auto-creates | none | **adapt — later** | Fits *reversible by default*; needs a scheduler and a usage signal first |
| **Blueprints** | typed slots, multi-surface | none | **skip — for now** | Solves a consumer-UX problem Jod doesn't have with one user |
| **Telegram** | ~22 platforms | none | **steal — Telegram only** | Reaches the same phone this week with no Apple certificate |
| **Multi-platform ABC** | 7.2k-line base, ~22 adapters | n/a | **skip** | One user, one phone. Build a transport, not a framework |
| **Session key derivation** | single source of truth | run ids | **adapt** | Copy the *rule*, not the 22-platform generality |
| **Default-deny allowlist** | allowlist, pairing, tiers | API auth exists | **steal** | A bot with a shell is the most exposed surface Jod would have |
| **4096 UTF-16 chunking** | `utf16_len`, fence-aware | n/a | **steal** | Non-obvious; silently corrupts output otherwise |
| **Progressive edit streaming** | 4 transports | n/a | **adapt** | Start with `edit`; skip the fallback matrix until it hurts |
| **Long-run heartbeat bubble** | "⏳ Working — N min" | TUI spinner | **steal** | On a phone this is "working" vs "dead" |
| **Silence tokens** | `[SILENT]` suppresses delivery | n/a | **steal** | Trivial, and the enabler for any watchdog job |
| **Memory cap + overflow error** | 2200/1375, hard error | uncapped | **adapt** | Jod's facts are triples, not prose. Cap the *injected* set, not the store |
| **Batched atomic `operations`** | one call frees and adds | n/a | **steal — if a cap lands** | The cap without the batch is a token tax |
| **Circuit breaker on memory failure** | 3/turn ⇒ terminal | n/a | **steal** | "A failed side effect must never block the reply" |
| **`session_search`** | FTS5, 4 shapes, no LLM | transcripts in SQLite; **no search tool** | **steal** | The missing bottom tier. Jod has the data and FTS5 already |
| **Bookend + window shape** | goal → match → resolution | n/a | **steal** | Pure win, costs no model call |
| **Source-first guardrail** | "don't conclude *not found* from history" | n/a | **steal** | One paragraph, prevents a real failure mode |
| **`parent_session_id` forking** | self-FK | runs are flat | **skip** | Compaction is the harness's context to manage |
| **Memory provider ABC** | 4 abstract + 14 hooks | none | **adapt** | Copy the **prefetch/queue_prefetch** split and `on_pre_compress`. Skip the 14-hook surface |
| **Nudge every N iterations** | `= 10` | none | **skip** | Jod doesn't own an agent loop to nudge |
| **Curator two-phase GC** | deterministic on, LLM off | none | **adapt — later** | Only matters once something authors artefacts automatically |
| **Session auto-prune** | opt-in, cadence in `state_meta`, off by default | none | **adapt** | `jod.db` grows forever too |

---

## 8. What Jod should build, in value order

1. **A durable scheduler in `jod.db`** — `schedules` table, a due-query on a 60 s
   tick, spawning through the existing supervisor. *Every other item depends on
   it.* **M** — the spawn path, store and supervision all exist; this is a table,
   a tick, and a claim.
2. **`no_agent` script jobs + `monitor_*` hash suppression.** Makes a 24/7
   scheduler nearly free. **S** on top of #1.
3. **A Telegram transport.** Delivers "Jod in your pocket" without the Apple
   certificate blocking `apps/ios`; a scheduler with nowhere to deliver is a cron
   job writing to a database. **M**. Explicitly **not** a platform abstraction.
4. **A durable delivery ledger.** An unsent digest is a failed run that looks
   successful. **S** — one table, three checkpoints, reusing the `rehydrate`
   sweep Jod already runs at boot.
5. **`session_search` over the transcript store.** The bottom memory tier;
   events are already in SQLite with FTS5. **S–M**.
6. **Delivery targets and `attach_to_session`.** **S**.
7. **Quality gates on long-running work.** Copy the source ordering: gates first
   and short-circuiting, judge fails open, budget is the backstop. **M**.
8. **`/heartbeat`-style recurring prompt on a live run.** **S**.
9. **Memory hygiene: cap the injected set, batch the edits, never block the
   reply.** **S**.
10. **Consent-first job suggestions.** **M**, and last for a reason.

---

## 9. Other Hermes features Jod lacks (noted, out of scope)

Kanban board with worker lanes · multi-profile isolation · skills as installable
bundles with trust tiers and a non-overridable `dangerous` verdict · checkpoints
and rollback with a shared content store · DSPy+GEPA offline skill evolution ·
MCP server and ACP adapter · web dashboard and desktop app · TTS/STT and
wake-word voice · computer-use and browser tooling · mixture-of-agents ·
trajectory compression for training data · LSP integration · provider routing
with fallback chains.

---

## 10. Corrections to the prior `HERMES.md`

| Prior claim | Correction |
|---|---|
| Scheduling is inactivity-triggered only; no cron | Wrong. A durable 11.4k-line scheduler, plus `/goal` and `/heartbeat` |
| The `memory` tool takes three actions | Incomplete. Also a required `target` and a batched atomic `operations` array |
| Overflow is a hard error | True, but capped at 3 failures/turn, then terminal so it can't block the reply |
| `state.db` schema version 23 | Now 25 |
| `session_search` has three modes; summarisation unresolved | Four shapes; no LLM path anywhere. README is stale |
| Nudge "probably a system-prompt instruction" | An iteration counter, `nudge_interval: 10` |
| "Seven backends, CLI + 5 platforms" | ~22 platforms; backend count not re-verified |

---

## 11. What is explicitly not verified

- The nine memory providers' internal algorithms, **including Hindsight**.
- The memory-provider threading contract and profile-isolation sections.
- The full slash-command set (`gateway/slash_commands.py`, 275 KB — sampled only).
- Hermes' execution-backend count.
- `gateway/pairing.py` — existence and size confirmed, implementation unread; the
  pairing TTL and rate-limit figures are **[doc]**, not source.

---

## Sources

**Repository** — [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent)
@ `03fa32c92dd445eb64c7f67434dd91b32c40701d`, read from a blobless clone:
`cron/{__init__,jobs,scheduler,scheduler_provider,suggestions,blueprint_catalog}.py` ·
`tools/{cronjob_tools,memory_tool,session_search_tool}.py` ·
`agent/{memory_provider,agent_init,background_review,conversation_compression,codex_runtime}.py` ·
`gateway/{platforms/base,session,delivery_ledger,run}.py` ·
`plugins/platforms/telegram/{adapter.py,plugin.yaml}` ·
`hermes_cli/{goals,heartbeat,config_defaults}.py` ·
`hermes_state_common.py`, `hermes_state.py`

**Repository documentation** (same commit) —
`website/docs/user-guide/messaging/{index,telegram}.md` ·
`website/docs/user-guide/features/{goals,heartbeat,memory,curator,memory-providers,kanban}.md` ·
`website/docs/developer-guide/{cron-internals,memory-provider-plugin,session-storage}.md`

**External** — [Hermes Agent docs](https://hermes-agent.nousresearch.com/docs/) ·
[hermes-agent-self-evolution](https://github.com/NousResearch/hermes-agent-self-evolution) ·
[Codex CLI](https://github.com/openai/codex) (origin of the `/goal` Ralph loop, credited in Hermes' own docs)

**Jod, for the gap matrix** — this worktree: `core/src/store.rs`, `core/src/`,
`cli/src/`, `api/src/`, `deploy/`, `.github/workflows/pr-shepherd.yml`,
[`docs/jod-system.md`](../../docs/jod-system.md), [`AGENTS.md`](../../AGENTS.md).
