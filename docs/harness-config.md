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

`/model` forwards the name verbatim, because no two harnesses spell a model the
same way — `opus`, `opencode/claude-opus-5`, `claude-opus-4-6-thinking` are all
the same model. So the completion list comes from the harness itself: OpenCode
and AGY are asked (`opencode models`, `agy models`) and their stdout parsed;
Claude Code has no such subcommand — `claude models` is read as a *prompt* and
hangs — so its list is the static catalogue in `core/src/harness/models.rs`.

The list is an aid, not a gate. A name that is not on it is still passed
through, and a harness that is missing, slow or has changed its output format
just offers nothing.
→ [why](decisions.md#the-model-list-comes-from-the-harness-except-where-it-cannot)

`/model` with no argument — or `default`, or `clear` — hands the choice back to
the harness.

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

## Directories and commands: what was measured

Two things a conversation needs from a harness — the directories it may reach
beyond its working directory, and whether it will expand a repository's own
slash commands — differ enough between the three that Jod measured them against
the real binaries rather than reading their documentation. The full write-up,
with every command run and its actual output, is
[`harness-support.md`](harness-support.md). The short version:

| Harness | Extra directories | `/name` in a print-mode prompt |
| --- | --- | --- |
| Claude Code | `--add-dir`, repeatable | Expands — commands and skills both |
| OpenCode | `--dir`, exactly one; a second aborts the run | Does **not** expand; `run --command <name>` does |
| AGY | `--add-dir`, repeatable | Expands its skills; unknown names are refused |

Two consequences you can see from the outside.

**An OpenCode conversation's extra roots do not reach the binary.** Its one
directory flag holds the working directory, and a second one crashes the process
outright, so Jod passes the roots as prose in the run's preamble instead of
pretending to have granted them. The agent can still read those directories; it
simply has not been handed them. Under Claude Code and AGY each root arrives as
its own `--add-dir`.

**Roots are not a sandbox, under any of the three.** Passing a directory puts it
in the agent's context and in whatever allowlist the harness keeps. Withholding
one does not stop the agent reading it — measured directly, and worth stating
here because the palette's wording could otherwise be read as a permission
boundary. If you need a real boundary, it has to come from the machine: a
container, a user account, filesystem permissions. Not from this list.

## Why the chords avoid six Ctrl letters

Jod's global chords are on Ctrl — `Ctrl-G` for the workspace menu, `Ctrl-B` to
delegate, `Ctrl-F` for the fleet. They were briefly on Alt, to get out of the
way of a multiplexer that takes Ctrl chords before Jod sees them, and that made
things worse rather than better: macOS composes Option into accented characters
unless the terminal is specially configured, so `Alt-K` typed `˚` into the prompt
and no chord arrived at all. A binding nobody can press beats a binding
something else eats, but not by enough to be worth it.

So Ctrl came back, minus the letters that are genuinely spoken for:

| letter | who has it |
|---|---|
| `Ctrl-A` `Ctrl-S` `Ctrl-H` `Ctrl-J` `Ctrl-K` `Ctrl-L` | tmux — prefix, sessions, panes |
| `Ctrl-Q` `Ctrl-Z` `Ctrl-I` `Ctrl-M` | the terminal — flow control, job control, Tab, Enter |
| `Ctrl-C` `Ctrl-D` `Ctrl-E` `Ctrl-U` `Ctrl-W` | readline — quit, end of line, kill line, kill word |

That leaves eleven letters for sixteen verbs, which is why five of them are a
letter past the leader rather than a chord of their own: `Ctrl-G j` for
background shells, `Ctrl-G u` for the oldest unread, `Ctrl-G l` to clear the
transcript, `Ctrl-G /` to search every transcript, `Ctrl-G e` for `$EDITOR`.
`Ctrl-G` on its own draws the whole menu, so none of them has to be memorised.

**If your tmux is prefixed somewhere other than `Ctrl-A`,** the six above are
the wrong six — check `tmux list-keys` and expect a collision. `cli/src/tui/keys.rs`
is the one place the map lives, and `no_verb_sits_on_a_chord_a_multiplexer_takes`
is the test that pins it.

The Alt spelling of every verb still fires and is deliberately never printed, so
a terminal configured with Option-as-Meta keeps working and one without it loses
nothing.
