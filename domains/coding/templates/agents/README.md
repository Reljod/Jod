# AGENTS.md presets

Behavior presets the [`setup-project`](../../../../.agents/skills/setup-project/SKILL.md)
skill offers when scaffolding a repo. Each `<name>.md` is a full `AGENTS.md`
charter; `<name>` is what you pass to `--preset` (and what shows in
`--list`).

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
| `{{TICKET_PREFIX}}` | `--ticket` (e.g. `JOD`), default `PROJ` |
| `{{BRANCH_PREFIX}}` | `--branch`, default `claude` |

## Adding a preset

Drop a new `<name>.md` here using the tokens above. No code change needed —
`setup-project.sh --list` picks it up automatically. Keep presets thin and
identity-focused (the charter's "keep this file thin" rule); deep procedure
belongs in a skill, not baked into every scaffolded charter.
