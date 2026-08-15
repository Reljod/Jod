# DeepSeek Harness (`dsh`) — how "everything is a plugin" actually works

Research note, 15 August 2026. Question asked: *how does DeepSeek Harness do
plugin setup, and what design patterns transfer to Jod?*

Findings here are facts with citations. The recommendations live in
[`RECOMMENDATION.md`](RECOMMENDATION.md).

## What it is

`dsh` is DeepSeek's open-source agent harness, MIT-licensed, announced alongside
DeepSeek-V4-Pro. It is **not** a coding agent in the Claude Code sense — it is
the substrate you assemble one from. The README's own framing is *"Everything is
a plugin"*, and the marketing line pairs it with *"Every run is traceable."*

- Repo: `deepseek-ai/deepseek-harness`, version `0.1.0-rc.5` at time of writing.
  No git tags, no GitHub releases — "v0.1" is announcement wording, not a cut.
- README carries a literal all-caps warning: **"THERE WILL BE
  COMPATIBILITY-BREAKING CHANGES."**
- Entry point: `npx @deepseek-ai/dsh web`, serving `http://127.0.0.1:3080`.
- Three surfaces: Web UI, a CLI that runs named profiles and headless jobs, and
  a Python SDK that embeds the runtime.
- Built on **Cordis**, a general-purpose plugin kernel, *vendored* into
  `vendor/` rather than depended on. Cordis's stated design goal is
  spatiotemporal composability: **temporal** — removing a component reverts its
  effects; **spatial** — components declare dependencies on other components.

The claim that matters architecturally: *"there is no privileged core to patch:
you extend dsh by mounting a plugin beside the others."* Models, tools, skills,
sessions, sandboxes, storage, loops, scheduling and the UI are all plugins.

## The plugin setup, concretely

This is the part worth copying, so it is documented from the real
`examples/*/cordis.yml` files rather than from the prose.

### A config is a flat, ordered list of plugin rows

```yaml
- id: bash
  name: '@deepseek-ai/dsh-bash-local'
  config:
    timeoutMs: 60000
```

Three fields, and each does exactly one job:

- **`name`** is the npm package — the *code*.
- **`id`** is the mount instance — the *identity within this composition*.
- **`config`** is a schema-validated payload the plugin declares.

There is no separate registry file, no plugin manifest to keep in sync, and no
install step distinct from configuration. Adding a capability is adding a row.

### `id` ≠ `name`, so one package mounts many times

The headless example mounts the *same* subagent package twice, producing two
differently-named model-facing tools with different semantics:

```yaml
- id: tool-subagent
  name: '@deepseek-ai/dsh-tool-subagent'
  config:
    provider: spawn
    toolName: subagent
    backgroundMode: continuable
    maxDepth: 1

- id: tool-subagent-fork
  name: '@deepseek-ai/dsh-tool-subagent'
  config:
    provider: fork
    toolName: subagent_fork
    backgroundMode: one-shot
    enableRunInBackground: false
    maxDepth: 1
```

This is the single highest-leverage detail of the whole design. Because
instance identity is separate from package identity, "two flavours of the same
capability" costs a config row instead of a code branch.

### Load order is semantic, not cosmetic

The comments say so explicitly:

> `# Policy loads before the model-facing filesystem tools so writes and edits require an observed file.`

Ordering is how policy wraps capability. `fs-observation-policy` (read-before-edit)
is mounted between the filesystem provider and the tool that exposes it.

### Config carries live values via a `!!js` tag

```yaml
    cwd: !!js process.cwd()
    mode: !!js "process.env.DSH_PERMISSION_MODE ?? (process.env.DSH_SNAPSHOT === undefined ? 'workspace-write' : 'danger-full-access')"
```

A deliberate escape hatch: the config is data, but expressions are admitted at
named points rather than forcing every deployment difference into a new package.

### Composition is include + patch, not inheritance

An overlay is itself a plugin — `cordis-plugin-include` — that takes a base
path and a patch list:

```yaml
- id: base
  name: '@deepseek-ai/cordis-plugin-include'
  config:
    path: ./cordis.yml
    patches:
      - id: llm-deepseek
        name: '@deepseek-ai/dsh-llm-deepseek'
        config:
          retryPolicy: { mode: normal, maxRetries: 2, ... }
```

Three patch operations exist: **re-declare a row by `id`** to replace its
config, **`disabled: true`** to remove it, and **`insert:`** to add rows.

Two stated limits, both recorded in the example comments rather than discovered
later:

- *"Config patches replace whole plugin configs"* — replacement, not deep merge,
  which is why the retry overlay restates every unrelated field around
  `retryPolicy`. Verbose, but there is never a question of what the effective
  config is.
- *"a config patch cannot target an entry behind a nested include"* — patching
  reaches one level, not arbitrarily deep.

### The payoff case: swapping the substrate under a whole agent

`examples/headless-agent/e2b.cordis.yml` moves an agent's filesystem and
processes into a remote E2B sandbox. The entire change is: disable two rows,
insert their remote twins, and everything above them composes unchanged.

```yaml
    patches:
      - id: subprocess
        name: '@deepseek-ai/dsh-subprocess-local'
        disabled: true
      - id: fs-local
        name: '@deepseek-ai/dsh-fs-local'
        disabled: true
      - insert:
          - id: e2b
            name: '@deepseek-ai/dsh-e2b'
          - id: subprocess-e2b
            name: '@deepseek-ai/dsh-subprocess-e2b'
          - id: fs-e2b
            name: '@deepseek-ai/dsh-fs-e2b'
```

The bash tool, the terminal, the LSP stack and every model-facing tool are
untouched — they consume `ctx.fs` and the subprocess service, not an
implementation. The same trick appears in the ACP example, where
`dsh-fs-sandbox` replaces `dsh-fs-local` *behind `ctx.fs`* to fence writes by
permission mode.

The comment on that overlay is also worth stealing — it names the invariant the
composition must preserve:

> `# One-world invariant: e2b.cwd, sandbox-policy.workspaceRoot, and bash-local's default workdir … must all name the same remote directory.`

Composability does not remove coupling; it relocates it into config, where it
has to be written down.

### Profiles and bundles assemble the running instance

- A **profile** is a named composition stored in the Harness home; it lists the
  bundles it stacks.
- A **bundle** is a distribution format for config rows *and the code they mount*.
- Layers apply in order: each bundle → profile patches → home-level patches →
  overlay patches.
- `dsh --profile web --dump-config` prints the configuration actually booted.

### Settings and credentials are themselves plugins, hot-reloaded

```yaml
- id: settings
  name: '@deepseek-ai/dsh-settings-file'
- id: credentials
  name: '@deepseek-ai/dsh-credentials-local'
```

The comments define the contract precisely: a `llm-deepseek:` section in
`$DSH_HOME/settings.yaml` "overrides the adapter entry below **without a
restart**", and the credential store is "the live process environment over
`$DSH_HOME/.credentials.yaml` (owner-only file, hot-reloaded)", resolved
**per request** — "so no key is inlined in this file."

### Presets ship as modes

Four compositions ship, and they are just different plugin sets:

| Mode | What it is |
|---|---|
| Standard | full agent: edit, shell, search, skills, planning, subagents |
| Code | tools exposed via a TypeScript SDK behind a single `run_code` wire tool |
| Minimal | persistent bash + `str_replace_editor`, nothing else — the published benchmark harness |
| Creator | standard plus runtime inspection, in-memory plugin experiments, preset authoring |

Creator mode's on-disk directory is named `cordis`, not `creator`.

### Discovery is a GitHub topic

Third-party plugins are found by tagging a repo with the **`dsh-plugin`** topic.
No registry, no approval queue, no central index to operate.

## The rules that stop it collapsing

A plugin-everything system fails by becoming unpredictable. `AGENTS.md` is
mostly a set of constraints holding that off, and these are the transferable
part:

- **Registrations are effects.** Every contribution goes through `ctx.effect()`
  or `ctx.on()`, and a registry's `register()` **returns the disposer**. This is
  what makes unmounting real rather than aspirational.
- **A capability seam is exactly three roles** — Service Definition, Service
  Provider, Consumer — never a partial implementation. Each seam is its own
  workspace under `packages/`. They split into separate packages only when they
  evolve independently.
- **No hardcoded tunables in plugins.** "Deployment-varying choices are
  validated `Config` fields changeable from `cordis.yml`." Misconfiguration
  fails loudly at load time where it is self-contained.
- **Model-visible means logged.** "Anything that reaches a model request must be
  reconstructable from the session log." The append-only log is the source of
  the model's context; transcripts, telemetry, persistence, resume, fork and
  search all derive from that one stream.
- **Events are the extension points**, in three domains: session events
  (durable facts), `agent/*` events (observe live work), and capability events
  (attach policy without importing the loop). Picking the domain is described as
  "the first decision in most changes."
- **Waterfall listeners must call `next()`.** Returning without it
  short-circuits the chain.
- **Runtime invariants over metadata.** Check authoritative event streams and
  mutable data — not whether a service or method is present.
- **Validate at boundaries only** — parser, config, model/tool JSON, durable
  files, workers, processes, wire. Inside typed same-process boundaries, trust
  the compiler.
- **Brand opaque cross-boundary IDs** (`Branded<B>`), never bare strings.
- **The test gate is `test:coverage`, not `test`**, at 100% per-file coverage,
  and *"every non-trivial model- or product-user-visible behavior change adds or
  updates a keyless snapshot through a real runnable example in the same PR."*
  The `examples/` tree is simultaneously the documentation and the test corpus.

## Honest limits

- **Developer preview.** Breaking changes are promised in the README.
- **Verbosity is real.** The headless example needs ~25 plugin rows to produce
  one agent that can edit files and run bash. "No privileged core" means no
  defaults either, until a preset supplies them.
- **Patch semantics are blunt** — whole-config replacement, one include level
  deep — and the examples work around both by restating fields.
- **The kernel is vendored**, so dsh owns a copy of Cordis with a sync
  procedure. The composability story has a maintenance bill attached.

## Sources

- [github.com/deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) — README, `AGENTS.md`, `docs/architecture.md`, `docs/development.md`, and the `examples/*/cordis.yml` tree (read via the GitHub contents API)
- [DeepSeek Harness developer preview](https://deepseek.com/harness/en/)
- [VentureBeat — DeepSeek Harness launches as open source rival to Claude Code](https://venturebeat.com/technology/deepseek-harness-launches-as-open-source-rival-to-claude-code-alongside-v4-pro-on-api-with-higher-prices)
- [The New Stack — DeepSeek open sources an agent harness where everything is a plugin](https://thenewstack.io/deepseek-harness-open-source-plugins/)
- [The Register — DeepSeek's innovative harness treats everything as a plug-in](https://www.theregister.com/ai-and-ml/2026/08/14/deepseeks-innovative-harness-treats-everything-as-a-plug-in/5288095)
- [Digital Applied — DeepSeek Harness: Everything Is a Plugin](https://www.digitalapplied.com/blog/deepseek-harness-open-source-agent-framework-2026)
