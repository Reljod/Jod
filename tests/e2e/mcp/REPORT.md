# The MCP seam, proven against a real harness

**Date:** 2026-08-10 · **Method:** the release binary, real JSON-RPC on stdin,
and a real Claude Code run wired to it. No mocks, no fixtures.

> Why this file exists: `core/src/mcp.rs` has 42 passing unit tests, and unit
> tests prove a function returns what its test says. They cannot prove a harness
> can *reach* Jod, which is the only claim that matters — this project has
> already shipped four things that were complete, tested, and connected to
> nothing.

Reproduce with `tests/e2e/mcp/probe.py` and `tests/e2e/mcp/config.json`.

---

## 1. The wire protocol

`jod mcp` speaks JSON-RPC 2.0 over stdio. Sent by hand, read back verbatim:

```
=== initialize ===
  protocol: 2024-11-05
  server:   {'name': 'jod', 'title': 'Jod (read_only access)', 'version': '0.1.0'}
  caps:     ['tools']
```

## 2. The tool set is a function of access

```
  read-only     7 tools: conversation_search, conversations, goal_list,
                         list_agents, recall, related, schedule_list
  delegate     10 tools: … + continue_agent, delegate, stop_agent
  orchestrate  15 tools: … + goal_create, remember, schedule_create,
                             schedule_pause, schedule_run_now
```

And the levels **nest**, which is the property that makes them a hierarchy
rather than three unrelated sets:

```
  read-only ⊆ delegate:    yes
  delegate  ⊆ orchestrate: yes
```

## 3. Refusals, errors, and surviving garbage

```
=== a tool above the caller's level is refused ===
  refused: `delegate` needs `delegate` access and this agent has `read_only`

=== an unknown method is a proper JSON-RPC error ===
  {'code': -32601, 'message': 'unknown method `no/such/method`'}

=== a malformed line does not kill the server ===
  sent 3 lines (one garbage), got 3 replies back
  server answered the request AFTER the garbage: True
```

That last one matters more than it looks: a crashed MCP server takes an agent's
tools away mid-task with no explanation, and the agent then improvises.

---

## 4. The claim that actually matters: a real harness uses it

Everything above is Jod talking to itself. This is Claude Code, the real binary,
pointed at Jod with `--mcp-config --strict-mcp-config`.

**Memory was seeded through the ordinary CLI first**, so the agent had to read
Jod's real store rather than anything staged for it:

```
$ jod remember reljod prefers "a spec before a plan"
$ jod remember jod runs-on jod-cloud
```

### It calls the tool and reports Jod's data

```
$ claude -p "Use the jod MCP tools. Call recall to find what Jod remembers
             about reljod, then say exactly what you found in one sentence."
          --mcp-config config.json --strict-mcp-config

Jod remembers exactly one fact about reljod: that he prefers a spec before a
plan (an owner-origin, accepted belief in the `default` scope).
```

It reported the **origin and the scope** — so what came back was Jod's
structured belief, with its provenance, not a string.

### A read-only agent cannot even see the privileged tools

```
$ claude -p "Try to call a tool named 'delegate' to start a new agent.
             Report exactly what happened."
          --mcp-config config.json --strict-mcp-config

No `delegate` tool exists in this session — ToolSearch for
`select:mcp__jod__delegate` and `+delegate` both returned "No matching deferred
tools found", and the only jod MCP tools exposed are the read-only ones
(list_agents, recall, related, conversations, conversation_search, goal_list,
schedule_list), so no agent was started.
```

The agent *looked for it twice* and could not find it. The gate is not a runtime
refusal the agent could retry around — the tool is absent from its world.

---

## What this establishes

| Claim | Status |
|---|---|
| `jod mcp` speaks MCP over stdio | **verified on the wire** |
| A harness can reach Jod's capabilities | **verified with Claude Code** |
| The tool set narrows with access | **verified, and the levels nest** |
| A low-privilege agent cannot reach a high-privilege tool | **verified — absent, not merely refused** |
| Malformed input does not kill the server | **verified** |

## What this does not establish

- **OpenCode is untested here.** It has `opencode mcp add`, so the mechanism
  exists, but nobody has run it against Jod. AGY exposes no MCP flag at all, so
  for AGY the JSON-decision fallback in `orchestrator.rs` remains the only route.
- **`orchestrate` was not exercised end to end with a harness** — only
  `read-only`. Granting a real agent the ability to create schedules is a live
  effect on a live database and deserves its own run, deliberately.
- The tools were called in a scratch `JOD_HOME`, not against a populated store.
