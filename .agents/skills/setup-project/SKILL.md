---
name: setup-project
description: >
  Use when the user wants to bootstrap/scaffold/initialize a repo with an
  agent charter and tooling — set up AGENTS.md (+ CLAUDE.md), pick an agent
  behavior/personality, and copy in reusable skills and their slash commands.
  Triggers on "set up this project", "scaffold a new repo", "add an AGENTS.md",
  "initialize the coding conventions", "give this repo the Jod setup".
  Presents the choices interactively so the user picks their own taste, with
  Jod's opinionated defaults pre-selected.
---

# setup-project

Bootstraps a repository with an **agent charter** (`AGENTS.md`, with
`CLAUDE.md` symlinked to it) and a chosen set of **reusable skills + slash
commands**. It is the richer sibling of Claude Code's built-in `/init`: that
one scans code to draft a `CLAUDE.md`; this one lets the user *choose* an
opinionated setup — behavior preset, ticket/branch conventions, and which
skills to install — and shows those choices at setup time.

The design goal is **opinionated by default, but yours to override**. Jod's
current charter is the default preset; anyone can pick a leaner one, an
OSS-flavored one, or a test-first one instead.

## The two things the user chooses (show these, don't assume)

Always surface both choices before scaffolding — the point of this skill is
that the taste is theirs. Use `AskUserQuestion` (or plain prompts if that's
unavailable), pre-selecting the Jod defaults:

1. **Behavior preset** — which `AGENTS.md` template seeds the charter. One
   of (run `--list` for the live set):

   | Preset | For | Default? |
   |---|---|---|
   | `jod` | The full Jod charter: quality layer-model, `<type>: TICKET` commits, `claude/<desc>-<id>` branches, draft-PR habits. | ✅ default |
   | `minimal` | A lean identity + a couple of principles; grow it as needs get real. | |
   | `team` | Conventional Commits (no ticket key), PR-template + review norms — open-source / multi-contributor repos. | |
   | `tdd-strict` | Test-first enforced, coverage as a required gate. | |

2. **Skills to copy in** — a multi-select over the Jod skill library (run
   `--list` for the live set): `create-pr`, `setup-git-hooks`, `tdd-loop`,
   `test-scenarios`. Each is copied into the target's `.agents/skills/`
   together with its `.claude/commands/<skill>.md` wrapper. Recommended
   default: everything for `jod`/`team`; `tdd-loop` + `test-scenarios` +
   `setup-git-hooks` for `tdd-strict`; none for `minimal`.

Also collect (or infer): **project name** (defaults to the target dir name),
a **one-line description**, the **ticket prefix** (e.g. `JOD`), and the
**branch prefix** (default `claude`).

## How to run it

1. **List what's available**, then present the choices to the user:

   ```
   .agents/skills/setup-project/scripts/setup-project.sh --list
   ```

2. **Scaffold** with the chosen preset and skills:

   ```
   .agents/skills/setup-project/scripts/setup-project.sh \
     --preset jod --skills create-pr,setup-git-hooks,tdd-loop \
     --name "My Project" --desc "One line about it." \
     --ticket JOD --target /path/to/repo
   ```

   Flags: `--preset`, `--skills a,b,c` (or `all`), `--name`, `--desc`,
   `--ticket`, `--branch`, `--target` (defaults to cwd), `--no-symlink`
   (write `CLAUDE.md` as a copy — for filesystems/repos where symlinks are a
   problem), `--force` (overwrite existing `AGENTS.md`/`CLAUDE.md`). Without
   `--force` it refuses to clobber an existing charter, so re-runs are safe.

   The script renders the template (substituting `{{PROJECT_NAME}}`,
   `{{PROJECT_DESC}}`, `{{TICKET_PREFIX}}`, `{{BRANCH_PREFIX}}`), creates the
   `CLAUDE.md` → `AGENTS.md` symlink, and copies each chosen skill + command.
   It never copies `setup-project` itself into a target repo.

3. **Finish the charter with the user.** The template leaves the description
   and any preset-specific assumptions (ticket/branch prefixes) for a human
   to confirm. Read the generated `AGENTS.md` back and adjust anything that
   doesn't fit the actual project — the scaffold is a starting point, not the
   final word.

4. **Wire the local gate** if `setup-git-hooks` was copied: run
   `/setup-git-hooks` in the target so the commit-convention matches the
   charter's stated convention.

5. **Commit the scaffold** (`AGENTS.md`, `CLAUDE.md`, `.agents/`, `.claude/`)
   on a feature branch, per the charter's branching rule.

## Extending the setup

- **New preset** → add `templates/agents/<name>.md` (inside this skill) using
  the `{{PLACEHOLDER}}` tokens above; it appears in `--list` automatically.
- **New selectable skill** → once a skill under `.agents/skills/` has proven
  itself (the charter's "extend by writing it down" rule), it's offered by
  `--list` with no code change; give it a matching `.claude/commands/<skill>.md`
  so its slash command travels with it.

This is how the setup stays opinionated without being rigid: the defaults
encode Jod's taste, and the preset/skill lists are the knobs everyone else
turns.

## Boundaries

- Don't silently overwrite an existing charter — respect the no-`--force`
  refusal and ask the user before clobbering.
- The scaffold seeds conventions; it does not *enforce* them. Enforcement is
  `setup-git-hooks` locally and required checks in CI (the quality-layering
  rationale lives in the charter's **Design choices**).
- **Templates ship inside this skill** (`templates/agents/`), so the whole
  `.agents/` toolkit stays copyable into any repo — it never reaches into
  `domains/`.
- Keep presets thin and identity-focused (the charter's "keep this file
  thin" rule). Deep procedure belongs in a skill or a linked doc, not baked
  into every scaffolded `AGENTS.md`.
