# Phase 1 — Survey of approaches to the "one main agent" pattern

The pattern the user described has a name in the 2026 literature. It is most
often called the **orchestrator-worker** or **supervisor** pattern, and when it
is packaged as a product it is called an **agent harness** or a **deep agent**.
The defining properties are: a single conversational surface the human talks to;
that agent owns the plan and the state; it manages its own context window; and
it spawns short-lived workers that return summaries rather than transcripts.

Below are 32 approaches, grouped into six families. Each entry records the
mechanism, the evidence behind it, and its cost.

---

## Family A — Control topology: who decides, and who may write

### A1. Orchestrator-worker (lead agent + parallel subagents)
A lead agent plans, spawns 3-5 specialised subagents in parallel, and synthesises
their findings. Anthropic's Research system is the reference implementation.
Evidence: beat single-agent Opus 4 by 90.2% on their internal research eval.
Cost: roughly 15x the tokens of a normal chat. Only pays for breadth-first
questions where the total information exceeds one context window.

### A2. Agent-as-tool (delegation that returns)
The specialist is exposed to the main agent as an ordinary tool. The main agent
keeps the conversation and the decision authority; the specialist answers a
bounded question and control returns. This is the OpenAI Agents SDK
"agents as tools" mode and Claude Code's Task tool.

### A3. Handoff (delegation that transfers)
Control moves permanently to the specialist, carrying conversation state with it.
OpenAI Agents SDK handoffs, LangGraph swarm edges. Good for routing by domain
(billing vs. technical support), bad for anything where the main agent must
retain the thread, because nobody is left holding the plan.

### A4. Single-threaded linear agent (the anti-pattern position)
Cognition's original position: do not build multi-agents. Dispersed
decision-making plus incomplete context sharing makes systems fragile. Their
example: one subagent built a Super Mario background while a sibling built a
bird that was not a game asset. The fix they proposed was context engineering,
not more agents.

### A5. Single-writer, multi-reader (Cognition's revised position)
Their 2026 update: the setups that work all share one property — multiple agents
contribute intelligence, but **writes stay single-threaded**. One main loop
carries state; subagents are stateless workers with narrow scope. This is the
single most important structural finding in the corpus, because it reconciles
A1 and A4.

### A6. Fan-out / scatter-gather
Parallel independent workers over a partitioned input, then a merge step. Pure
parallelism, no inter-worker communication. Cheapest multi-agent shape and the
only one whose speedup is predictable.

### A7. Pipeline / sequential chain
Each stage transforms the previous stage's output. Predictable, debuggable,
no coordination cost, but latency is the sum of stages and an early error
propagates.

### A8. Debate / multi-perspective critique
N agents argue to consensus. Improves calibration on ambiguous questions;
expensive and prone to converging on a confident wrong answer.

### A9. Swarm / peer handoff network
Agents hand control to each other dynamically with no supervisor. LangGraph
swarm. The survey literature reports this as the least reliable topology in
production; it is the shape MAST failures cluster in.

### A10. Hierarchical / nested supervisors
Supervisors of supervisors. Claude Code deliberately restricts subagents to one
level deep. Deeper nesting multiplies the 15x token cost and is the documented
runaway-cost failure (a subagent recursively spawning subagents).

### A11. Capability router ("smart friend")
The primary agent escalates a hard sub-problem to a stronger model, then
continues. Cognition reports this works when **both** models are capable — it is
a capability router, not a difficulty escalator. Weak-primary-delegates-to-strong
remains unsolved because weak models do not know when they are stuck.

### A12. Blackboard architecture (classical)
Shared workspace; specialists read state, contribute, and a control component
picks who acts next. Predates LLMs by 50 years. The modern reinvention is a
shared scratchpad file or a task database — which is exactly what a filesystem-
backed agent does.

### A13. Contract Net Protocol (classical, Smith 1980)
Task announcement, bidding, awarding. The ancestor of every "which agent should
take this" router. Mostly of historical interest; LLM systems route by prompt,
not by bid.

---

## Family B — Context management: how the main agent stays coherent

### B1. Compaction / summarise-and-reinitialise
When the window nears its limit, summarise the conversation and restart from the
summary. Anthropic's documented approach. The risk is fidelity loss at the seam.

### B2. Learned compaction (CompactionRL)
Train the agent to decide *when and what* to compact as an RL action rather than
firing on a fixed threshold. Reported findings: compaction at task-relevant
decision points beats fixed intervals, and learned strategies beat heuristics.

### B3. Compaction validation (Slipstream)
Verify a compaction against the original trajectory before trusting it, instead
of assuming the summary is faithful. Treats the summary as a claim to be checked.

### B4. Sub-agent context isolation
The delegated task runs in a fresh window with its own system prompt and tools;
only a small summary returns. Reported ratio: a subagent burns 10,000+ tokens and
returns 1,000-2,000. The orchestrator's context stays bounded. **This is the
mechanism that makes the main-agent pattern work at all.**

### B5. Filesystem as context (Manus)
Treat the file system as unbounded, persistent context the agent can page in and
out. Notes, artefacts and intermediate results live on disk, not in the window.

### B6. Todo recitation / attention re-anchoring (Manus)
The agent continually rewrites a `todo.md`, pushing its objectives to the *end*
of the context where attention is strongest. Directly counteracts
lost-in-the-middle over the ~50-tool-call average task.

### B7. KV-cache prefix stability (Manus)
Never mutate the prefix. A single changed token near the start invalidates the
cache; the reported spread is about 10x on cost. Implies: append-only context,
deterministic serialisation, no timestamps in system prompts.

### B8. Tool masking rather than tool removal (Manus)
Keep tool definitions constant and mask logits to restrict the available action
set by state. Removing a tool mid-run breaks the cache and orphans past calls.

### B9. Tool-result clearing
Drop stale large tool outputs from the window while keeping the fact that the
call happened. Cheaper and safer than summarising, because nothing is
paraphrased.

### B10. Just-in-time context retrieval
Load identifiers, not contents; fetch the body only when needed. Anthropic's
"smallest set of high-signal tokens" framing. The 2026 hybrid default reported
in the literature: retrieve 50K-200K relevant tokens, then reason over them.

### B11. Progressive disclosure (skills)
Capability is described in one line and its full instructions are loaded only on
use. Keeps a large capability surface at a small resting context cost.

### B12. Hierarchical / sliding-window summarisation
Old turns compress at increasing ratios with distance. Standard, cheap, lossy in
a predictable way.

### B13. Lookahead context engineering (SmoothAgent)
Anticipate what the agent will need next and stage it, rather than reacting when
the window is already full. Serving-efficiency framing rather than quality.

---

## Family C — Asynchrony, scheduling and durability

### C1. Ambient agents
The agent is not summoned by a chat turn. It runs continuously, watches event
streams, and acts. Latency budget in minutes, not milliseconds. The user's stated
goal — "run asynchronously for hours" — is this category.

### C2. Durable execution / checkpoint-and-replay
Persist the full execution history so a crash, restart or API failure resumes
rather than restarts. Temporal, Inngest, and the agent-runtime literature all
converge here. Necessary the moment a task outlives a process.

### C3. Event-driven backbone
Agents subscribe to a durable event log (Kafka-class) instead of being called.
Decouples submission from completion and survives timeouts and approvals.

### C4. Agent inbox / priority queue steering
A queue in front of the main loop lets a human enqueue, interrupt or redirect
work mid-run. Distinguishes **human-in-the-loop** (agent initiates the pause)
from **steering** (human initiates the correction).

### C5. Interrupt-and-resume
`interrupt()` inside the loop pauses execution, collects input, and folds it back
in. Requires C2 to be safe.

### C6. Turn-boundary queueing vs. mid-batch injection
A practical finding worth recording: injecting steering text into the *tool
channel* while a batch is in flight can be flagged as prompt injection by
injection-resistant models. Delivering it as a user-role message at the turn
boundary avoids the collision.

### C7. Sleep-time compute
A background agent reorganises memory while the primary agent is idle,
pre-computing what the next turn will need. Letta's framing. Splits memory
maintenance out of the hot path.

### C8. DAG planner with parallel dispatch (LLMCompiler)
The planner emits a task DAG with dependencies; a fetching unit dispatches ready
tasks in parallel. Reported: up to 3.7x latency speedup, 6.7x cost saving, ~9%
accuracy improvement over ReAct.

### C9. Plan-and-execute / ReWOO
Plan the whole sequence up front with variable substitution between steps, so the
LLM is not re-invoked to route every step. Cuts LLM calls; brittle when the plan
must change.

### C10. Supervision trees / let-it-crash
Erlang/OTP heritage: a supervisor restarts failed children under a declared
strategy rather than trying to prevent all failure. Maps cleanly onto process-
group-per-agent designs.

---

## Family D — Memory

### D1. Self-editing memory blocks (MemGPT / Letta)
The agent edits labelled in-context memory blocks through ordinary tool calls.
OS-inspired hierarchy: main context, recall storage, archival storage.

### D2. Memory-stream retrieval by recency + importance + relevance
Generative Agents' scoring function, all three weights equal in the original.
Retrieval that ignores importance surfaces trivia; ignoring recency surfaces
stale facts.

### D3. Reflection / insight synthesis
When accumulated importance crosses a threshold, synthesise higher-level
insights from recent observations and write them back as first-class memories.
Turns a log into knowledge.

### D4. Externalised structured note-taking
The agent writes durable notes outside the window and re-reads them. The
low-tech version of D1 and the one that survives a harness change.

### D5. Temporal knowledge graph memory
Facts with validity intervals, so superseded beliefs are invalidated rather than
duplicated. Answers "what is related to this" — which a flat fact list cannot.

---

## Family E — Verification and quality

### E1. Evaluator-optimiser loop
Generator plus critic, iterate. Documented gain curve: ~62% to ~70% on the first
correction, ~75% by attempt 3, saturating near 79% by attempt 5. Most of the
value is in passes 1-2.

### E2. Clean-context reviewer
The reviewing agent sees the artefact but **not** the history that produced it.
Cognition reports ~2 bugs caught per PR, 58% of them severe. The clean context is
the whole point: a reviewer that shares the author's context inherits its blind
spots.

### E3. Shared-context self-evaluation is a trap
When generator and evaluator are the same model sharing context, they jointly
exploit the scoring proxy. Reward hacking intensifies precisely in the cheap
configuration everyone reaches for first.

### E4. Veto-only review
Reviewers may block but never approve. Anything that is not an exact clear signal
counts as a block. Removes the failure mode where a hedging reviewer is read as
assent.

### E5. LLM-as-judge with a taxonomy
MAST's judge pipeline reached 94% accuracy and 0.77 Cohen's kappa against expert
annotation by scoring against a fixed 14-mode taxonomy rather than a vague
rubric.

---

## Family F — Interfaces and protocols

### F1. MCP — agent to tools
Vertical integration, client-server. Spec dated November 2025; now the general
standard for context provision.

### F2. A2A — agent to agent
Google, contributed to the Linux Foundation, v1.0 April 2026, 150+ organisations.
Agent Cards (JSON-LD capability descriptors) for discovery, task delegation,
streaming results. ACP folded into it in 2025.

### F3. Layered protocol stack
The observed 2026 arrangement: A2A for high-level orchestration, MCP for
low-level tool execution.

### F4. Agent-Computer Interface design
Treat the tool surface as a UI designed for a model: few tools, unambiguous
names, errors that teach. Consistently reported as higher-leverage than prompt
tuning.

---

## Cross-cutting evidence: how these systems fail

MAST (Multi-Agent System Failure Taxonomy) analysed 150 traces with high
inter-annotator agreement (kappa = 0.88) and produced 14 failure modes in three
clusters: **system design issues**, **inter-agent misalignment**, and **task
verification failure**. The headline finding is that failures come mostly from
design, not model capability — which means the choice of pattern matters more
than the choice of model.

Context rot is the other cross-cutting constraint. Degradation is non-uniform
rather than linear, with a clearly observable effect on 1M-token models around
300,000-400,000 tokens, and a U-shaped retrieval curve where the middle fades.
The safety-relevant version is stark: frontier models miss dangerous actions
2x to 30x more often when those actions occur after 800K tokens of benign
activity.
