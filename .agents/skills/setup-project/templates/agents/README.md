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

## Adding a preset

Drop a new `<name>.md` here using the tokens above. No code change needed —
`setup-project.sh --list` picks it up automatically. Keep presets thin and
identity-focused (the charter's "keep this file thin" rule); deep procedure
belongs in a skill, not baked into every scaffolded charter.
