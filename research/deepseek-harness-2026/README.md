# DeepSeek Harness: how its plugin system works

This is a research note written on 15 August 2026. The question asked was how
DeepSeek Harness handles plugin setup, and which of its design ideas are worth
applying to Jod.

This document records what the tool does. The opinions about what Jod should do
in response are kept separate, in [`RECOMMENDATION.md`](RECOMMENDATION.md).

## What the tool is

DeepSeek Harness is an open-source program from DeepSeek. Its command is `dsh`,
and it is released under the MIT licence. It is not a ready-made coding
assistant in the way that Claude Code is. It is a kit from which you assemble
your own assistant. The project describes itself with the phrase "everything is
a plugin", and its product page pairs that with "every run is traceable".

The following facts establish its maturity, and they matter when deciding
whether to depend on it:

- The version in the repository is `0.1.0-rc.5`. There are no git tags and no
  published releases, so the "v0.1" in the announcement is a description rather
  than a released version.
- The README contains a warning in capital letters that there will be changes
  which break compatibility.
- The program offers three ways to use it. There is a web interface, started
  with `npx @deepseek-ai/dsh web` and served at `http://127.0.0.1:3080`. There
  is a command-line interface that runs named profiles and one-off jobs without
  a user interface. There is also a Python library that embeds the whole runtime
  inside a Python program.
- It is built on a separate plugin framework called Cordis. Rather than
  depending on Cordis as a library, DeepSeek copied its source code into the
  repository under `vendor/` and maintains a procedure for keeping the copy in
  step. Cordis is designed so that a component can be removed while the program
  is running and its effects are undone, and so that components can declare
  which other components they require.

The architectural claim that matters is stated in the documentation as follows:
"there is no privileged core to patch: you extend dsh by mounting a plugin
beside the others." Models, tools, skills, sessions, sandboxes, storage, the
agent loop, scheduling and the user interface are all supplied by plugins.

## How the plugin setup actually works

The description below is taken from the real configuration files in the
repository's `examples/` directory rather than from the project's prose
documentation, because the examples are more precise and their comments record
the reasoning.

### A configuration is a flat, ordered list

Each entry in the list is one piece of the program. Every entry has three
fields, and each field does exactly one job.

```yaml
- id: bash
  name: '@deepseek-ai/dsh-bash-local'
  config:
    timeoutMs: 60000
```

The `name` field identifies the npm package, which is the code that will run.
The `id` field is the name this particular instance is given within this
configuration. The `config` field holds settings, and the plugin declares a
schema that those settings are checked against.

There is no separate registry file, no manifest that has to be kept in step with
the list, and no installation step that is distinct from configuration. Adding a
capability to the program means adding an entry to the list.

### The instance name is separate from the package name

Because `id` and `name` are different fields, the same package can appear in the
list more than once. The example for the headless agent uses this to create two
different tools from one package. The first entry produces a tool named
`subagent`, which runs in the background and can be sent further instructions.
The second entry produces a tool named `subagent_fork`, which runs once and then
stops.

```yaml
- id: tool-subagent
  name: '@deepseek-ai/dsh-tool-subagent'
  config:
    provider: spawn
    toolName: subagent
    backgroundMode: continuable

- id: tool-subagent-fork
  name: '@deepseek-ai/dsh-tool-subagent'
  config:
    provider: fork
    toolName: subagent_fork
    backgroundMode: one-shot
```

This is the most useful idea in the whole design. In most programs, offering two
variations of the same feature requires a branch in the code. Here it requires a
second entry in a list.

### The order of the list carries meaning

The order in which entries appear determines the order in which they are loaded,
and that order changes behaviour. The example files state this directly in their
comments. One reads: "Policy loads before the model-facing filesystem tools so
writes and edits require an observed file."

In other words, a policy is placed between a service and the tool that exposes
it, so that the policy takes effect first. In that particular case, the policy
requires the assistant to read a file before it is allowed to edit it.

### Live values are allowed at marked points

Configuration files are data, but some settings have to be worked out when the
program starts. DeepSeek allows this through a marker called `!!js`, which
indicates that the value is an expression to be evaluated rather than a literal.

```yaml
cwd: !!js process.cwd()
mode: !!js "process.env.DSH_PERMISSION_MODE ?? 'workspace-write'"
```

This is a deliberate compromise. Rather than requiring a new package every time
a setting varies between deployments, the format admits expressions, but only at
points where they have been written explicitly.

### Configurations are changed by inclusion and patching, not by editing

To change a configuration, you do not edit the original file. You write a second
file that includes the first and then lists the changes. The mechanism that
performs the inclusion is itself a plugin, named `cordis-plugin-include`.

```yaml
- id: base
  name: '@deepseek-ai/cordis-plugin-include'
  config:
    path: ./cordis.yml
    patches:
      - id: llm-deepseek
        name: '@deepseek-ai/dsh-llm-deepseek'
        config:
          retryPolicy: { mode: normal, maxRetries: 2 }
```

Three operations are available. Naming an existing entry by its `id` replaces
that entry's settings. Adding `disabled: true` removes the entry. Using
`insert:` adds new entries.

The examples record two limitations of this mechanism in their own comments,
which is a practice worth noting in itself, because the limitations are
documented where a reader will meet them rather than in a separate reference:

- A patch replaces an entry's settings completely and does not merge them. This
  is why the example that adds a retry policy has to restate every unrelated
  setting alongside it. The result is verbose, but there is never any doubt
  about which settings are in effect.
- A patch can only reach entries one level down. It cannot change an entry that
  is itself behind a nested inclusion.

### The example that demonstrates the benefit

The file `examples/headless-agent/e2b.cordis.yml` moves an assistant's file
access and command execution from the local machine into a short-lived remote
sandbox provided by E2B. The entire change consists of disabling two entries and
inserting their remote equivalents.

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

Nothing else changes. The shell tool, the terminal, the language-server support
and every tool the model can call continue to work without modification, because
they were written against the general file service rather than against the local
implementation of it. The same technique appears in another example, where a
sandboxed file service replaces the local one in order to restrict writes
according to a permission mode.

The comment on that overlay is worth quoting, because it is candid about what
this approach does not solve: "One-world invariant: e2b.cwd,
sandbox-policy.workspaceRoot, and bash-local's default workdir must all name the
same remote directory." Three separate settings have to agree, and nothing
enforces that agreement automatically. Making a program out of interchangeable
parts does not remove the connections between those parts. It moves them into
the configuration file, where they have to be written down and maintained.

### Profiles and bundles assemble a running instance

A profile is a named configuration stored in the program's home directory, and
it lists the bundles that it stacks together. A bundle is a distribution format
that carries both configuration entries and the code those entries refer to.

The layers are applied in a fixed order. Each bundle is applied first, then the
profile's own changes, then any changes made at the level of the home directory,
and finally any overlay. Because working out the result by reading four layers is
difficult, the program can print the configuration it actually started with:

```sh
dsh --profile web --dump-config
```

### Settings and credentials are supplied by plugins as well

Two entries in the headless example provide user settings and credentials
respectively.

```yaml
- id: settings
  name: '@deepseek-ai/dsh-settings-file'
- id: credentials
  name: '@deepseek-ai/dsh-credentials-local'
```

The comments in the file define the behaviour precisely. A section written into
`$DSH_HOME/settings.yaml` overrides the corresponding entry in the configuration
without requiring a restart. Credentials are read from the running process's
environment first, and from an owner-only file named `.credentials.yaml` second.
That file is re-read when it changes, and the credential is looked up at the time
of each request. The stated consequence is that no key is written into the
configuration file itself.

### Four preset configurations are supplied

The four modes the program ships with are not special code paths. Each is simply
a different set of plugins.

| Mode | What it provides |
| --- | --- |
| Standard | A complete assistant with file editing, a shell, search, skills, planning and sub-agents. |
| Code | The same tools, but exposed through a TypeScript library, so that the model writes a script instead of calling tools individually. Only one tool, `run_code`, is visible to the model. |
| Minimal | A persistent shell and a string-replacement editor, and nothing else. This is the configuration DeepSeek uses to publish benchmark results. |
| Creator | The standard set, plus the ability to inspect the running program, try plugins in memory and write new presets. |

One small inconsistency is worth recording: the directory for Creator mode is
named `cordis` on disk rather than `creator`.

### Plugins are found through a GitHub topic

Third-party plugins are discovered by tagging a repository with the topic
`dsh-plugin`. There is no central registry, no submission process and no index
for DeepSeek to operate.

## The rules that keep the system predictable

A program in which everything can be replaced risks becoming impossible to
reason about. Much of the repository's `AGENTS.md` file consists of rules that
prevent this, and those rules are the part of the design that transfers most
readily to a project written in a different language.

- **Every registration can be undone.** All contributions are made through
  `ctx.effect()` or `ctx.on()`, and a registry's `register()` method returns the
  function that removes the registration. This is what makes it genuinely
  possible to unload a plugin rather than merely intending to.
- **A replaceable part consists of exactly three pieces**: the definition of the
  service, an implementation of it, and the code that consumes it. Partial
  implementations are not permitted. Each such part is a separate workspace in
  the repository, and it is split into separate packages only when the three
  pieces begin to change independently of one another.
- **Plugins contain no hardcoded settings.** Any choice that varies between
  deployments must be a validated configuration field that can be changed from
  the configuration file. Where a plugin can detect that it has been
  misconfigured on its own, it is required to fail immediately at load time
  rather than later.
- **Anything the model sees must be written to the log.** The rule is stated as:
  "anything that reaches a model request must be reconstructable from the session
  log." The log is append-only, and transcripts, telemetry, storage, resuming,
  branching and search are all produced from that single record.
- **Events are the intended way to extend the program.** There are three kinds:
  session events, which record durable facts; agent events, which allow live work
  to be observed; and capability events, which allow a policy to be attached
  without modifying the main loop. Choosing which kind to use is described as the
  first decision in most changes.
- **A listener in a chain must call `next()`** in order to pass control on.
  Returning without calling it stops the chain.
- **Checks are made against real state, not against metadata.** The rule is to
  examine the event stream or the current data, rather than testing whether a
  service or a method happens to exist.
- **Input is validated only at the edges** of the program: when parsing, when
  reading configuration, when receiving JSON from a model or a tool, when reading
  stored files, and when crossing a process, worker or network boundary. Within
  the program, where types are already checked at compile time, no further
  validation is performed.
- **Identifiers that cross boundaries are given distinct types** rather than
  being passed as plain strings.
- **The test requirement is unusually strict.** The gate is a coverage run rather
  than a plain test run, and it requires every source file to be fully covered.
  In addition, "every non-trivial model- or product-user-visible behavior change
  adds or updates a keyless snapshot through a real runnable example in the same
  PR". The `examples/` directory therefore serves as documentation and as the
  test fixtures at the same time.

## Weaknesses and limitations

The following points should be weighed against the strengths described above.

- The project is an early preview, and the README states plainly that
  compatibility will be broken.
- The configuration is long. The headless example requires roughly twenty-five
  entries before there is an assistant that can edit files and run commands.
  Removing the privileged core also removes the built-in defaults, and nothing
  works until a preset supplies them.
- The patching rules are coarse. Settings are replaced rather than merged, and
  patches reach only one level of inclusion. The project's own examples work
  around both restrictions by restating fields.
- The plugin framework has been copied into the repository rather than depended
  upon, so the project now maintains its own copy of Cordis together with a
  procedure for synchronising it. The flexibility described above has an ongoing
  maintenance cost attached to it.

## Sources

- [github.com/deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness),
  specifically the README, `AGENTS.md`, `docs/architecture.md`,
  `docs/development.md` and the configuration files under `examples/`, which
  were read through the GitHub contents API.
- [DeepSeek Harness developer preview](https://deepseek.com/harness/en/)
- [VentureBeat: DeepSeek Harness launches as open source rival to Claude Code](https://venturebeat.com/technology/deepseek-harness-launches-as-open-source-rival-to-claude-code-alongside-v4-pro-on-api-with-higher-prices)
- [The New Stack: DeepSeek open sources an agent harness where everything is a plugin](https://thenewstack.io/deepseek-harness-open-source-plugins/)
- [The Register: DeepSeek's innovative harness treats everything as a plug-in](https://www.theregister.com/ai-and-ml/2026/08/14/deepseeks-innovative-harness-treats-everything-as-a-plug-in/5288095)
- [Digital Applied: DeepSeek Harness, everything is a plugin](https://www.digitalapplied.com/blog/deepseek-harness-open-source-agent-framework-2026)
