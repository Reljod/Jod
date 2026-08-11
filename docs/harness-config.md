# Configuring the harnesses

Jod does not do the work — it delegates to a harness and normalises what comes
back. So there are two places a setting can live, and telling them apart is the
whole of this page.

**Jod's settings** are the ones Jod passes on the command line at every spawn:
which harness, which model, how much it may do without asking. These are
per-conversation, they change from inside the TUI, and they win.

**The harness's own settings** are everything else: which provider, which
credentials, which tools exist, MCP servers, themes, hooks. Jod never writes
these files and never reads them. Configure them where the harness expects, and
Jod inherits the result.

## What Jod controls, and how you change it

| Setting | In the TUI | On the command line |
|---|---|---|
| Harness | `/harness claude\|opencode\|agy` | `jod tui -H <name>` |
| Model | `/model <name>`, `/model` to reset | `jod tui -m <name>` |
| Permission mode | `/mode <name>`, or **Tab** to cycle | `jod tui --permission <name>` |

`/model` is generic on purpose. All three harnesses take `--model` and Jod
forwards the name verbatim; what a valid name looks like is the harness's
business, and Jod does not keep a table of model names that would be wrong the
week a model ships. Ask each harness what it has:

```sh
agy models          # gemini-3.6-flash-high, claude-sonnet-4-6, gpt-oss-120b-medium, …
opencode models     # provider/model form, e.g. anthropic/claude-sonnet-4-5
claude --help       # --model takes an alias or a full name
```

Model names do not survive `/harness`. `claude-sonnet-4-5` means nothing to
OpenCode or AGY, so switching harness drops the requested model back to `None`
and lets the new harness pick its own default — otherwise the switch would look
like it simply had not worked.

The permission mode is four levels: `plan` (read and reason, change nothing),
`ask` (check first), `edits` (file edits go through), `auto` (everything
auto-approved). `auto` is the default. What each maps to per harness is in
[`decisions.md`](decisions.md), including the two OpenCode genuinely cannot
express.

The `--permission` flag you launch with is a **ceiling**, not a starting point:
Tab can move down from it and never up.

## Where each harness keeps its own configuration

Verified on this machine; paths are the usual ones but check yours.

### Claude Code

- `~/.claude/settings.json` — user settings: permissions, hooks, env, status
  line. Project-level equivalents are `.claude/settings.json` and
  `.claude/settings.local.json` in the repo.
- `~/.claude.json` — account and MCP server registration.
- `~/.claude/` also holds agents, commands and skills as directories.

Note that Jod passes `--strict-mcp-config` whenever it grants its own tools.
That is deliberate: without it Claude Code would *also* load whatever MCP
servers your own configuration names, so a run Jod meant to hold read-only tools
could quietly inherit a filesystem server from `~/.claude.json`. The grant has
to be exactly what Jod granted. If you add an MCP server for interactive use, it
will not appear in a Jod-spawned run, and that is the intended behaviour.

### OpenCode

- `~/.config/opencode/opencode.jsonc` — global config. It takes a `$schema` of
  `https://opencode.ai/config.json`, so an editor will complete the keys.
- `opencode.json` in a project root — per-project overrides.

OpenCode is the harness with the least to say about permissions: one `--auto`
switch and no mode flag. If you want finer control there, it has to come from
its own config rather than from Jod.

### AGY (Antigravity)

- `~/.gemini/antigravity-cli/settings.json` — settings.
- `~/.gemini/antigravity-cli/` also holds the OAuth token, conversation history
  and its own summaries database.
- `agy plugin list` manages plugins; `agy agents` lists the agents `--agent`
  accepts.

AGY has `--effort low|medium|high` for reasoning effort, which Jod does not
currently pass. Its models encode the effort in the name instead
(`gemini-3.6-flash-high`), so `/model gemini-3.6-flash-high` reaches the same
place.

## Reasoning output

Claude Code and OpenCode both surface reasoning, and Jod stores it as a
`thinking` message distinct from the assistant's text. OpenCode needs
`--thinking` to emit any, which Jod now always passes — what gets *recorded* is
not a display preference, and a conversation read back tomorrow should not be
missing its reasoning because a toggle was off yesterday. Whether it is *shown*
is `/thinking`.

AGY's stream has no reasoning message type at all, only `agent_response` and
`tool` steps, so there is nothing to show there and Jod does not pretend
otherwise.
