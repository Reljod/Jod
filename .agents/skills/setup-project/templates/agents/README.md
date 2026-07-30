# AGENTS.md presets

Behavior presets the [`setup-project`](../../SKILL.md) skill offers when
scaffolding a repo. They live inside the skill so the whole `.agents/` toolkit
stays copyable into any repo. Each `<name>.md` is a full `AGENTS.md` charter;
`<name>` is what you pass to `--preset` (and what shows in `--list`).

| Preset | For |
|---|---|
| `jod` | The full Jod charter (default). |
| `minimal` | Lean identity + a couple of principles. |
| `team` | Conventional Commits + PR/review norms, for OSS / multi-contributor repos. |
| `tdd-strict` | Test-first enforced, coverage as a required gate. |

## Placeholders

The scaffolder substitutes these tokens when it renders a preset:

| Token | Filled with |
|---|---|
| `{{PROJECT_NAME}}` | `--name`, or the target directory name |
| `{{PROJECT_DESC}}` | `--desc`, or a "replace me" stub |
| `{{BRANCH_PREFIX}}` | `--branch`, default `claude` |
| `{{TICKET_PREFIX}}` | `--ticket` (e.g. `JOD`); empty by default |
| `{{TICKET_RULE}}` | a one-line "reference the issue key" rule — **only when `--ticket` is given**; otherwise the whole line holding the token is dropped |

Issue keys are opt-in, so a preset must not hard-code `<TICKET>` into its
commit convention. Write the convention without one and put the optional
rule on its own `{{TICKET_RULE}}` line (with the list marker, if any, on
that same line so it disappears with it).

## House style — bullets, not prose

A charter is read on every turn, so length is a cost paid continuously. A
bloated one is the usual reason a rule you wrote keeps getting violated. Every
preset here follows the same shape:

- **Guidelines in bullet form.** One rule per bullet, one or two lines. If a
  rule needs a paragraph, the paragraph goes elsewhere.
- **Detail lives behind a pointer** — the skill that owns the procedure
  (`→ **`create-pr`**`), or `docs/decisions.md` for the WHY. Point at
  `docs/decisions.md` as the place to *write* reasoning; don't hyperlink it,
  since a freshly scaffolded repo doesn't have it yet.
- **The test for a line:** would removing it cause a mistake? If not, cut it.
  That kills meta-commentary about the file's own form, "keep this file thin"
  principles, and a `## Skills` section restating where skills live — the
  runtime already surfaces them.
- **One exception to "keep it short":** the never-work-around-a-blocked-check
  list stays enumerated inline in every preset. "Don't cheat" is too abstract to
  bind behavior, and that rule has to be in context at the moment of temptation
  rather than one link away.
- **Keep presets identity-focused.** Deep procedure belongs in a skill, not
  baked into every scaffolded charter.

Current sizes are the budget: 35–70 lines per preset. If a new one lands well
past that, it's carrying detail that belongs in a skill.

## Adding a preset

Drop a new `<name>.md` here using the tokens above, in the house style. No code
change needed — `setup-project.sh --list` picks it up automatically.
