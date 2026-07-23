---
description: Scaffold a repo with an AGENTS.md charter, chosen behavior preset, and skills.
argument-hint: "[target repo dir, defaults to current repo]"
---

Bootstrap the project using the **setup-project** skill at
`.agents/skills/setup-project/SKILL.md`. Read that skill and follow it.

Target: $ARGUMENTS (if empty, the current repository).

Steps:
1. Run `.agents/skills/setup-project/scripts/setup-project.sh --list` to see
   the available behavior presets and skills.
2. **Show the choices to the user** with `AskUserQuestion`, pre-selecting the
   Jod defaults — this skill exists so the taste is theirs, not assumed:
   - **Behavior preset**: `jod` (default), `minimal`, `team`, or `tdd-strict`.
   - **Skills to copy in** (multi-select): `create-pr`, `setup-git-hooks`,
     `tdd-loop`, `test-scenarios`.
   Also collect the project name, one-line description, ticket prefix, and
   branch prefix (infer sensible defaults from the target dir).
3. Run the scaffolder with the chosen values:
   `setup-project.sh --preset <p> --skills <a,b,c> --name <..> --desc <..>
   --ticket <..> --target $ARGUMENTS`. It writes `AGENTS.md`, symlinks
   `CLAUDE.md`, and copies each chosen skill + its slash command. It refuses
   to overwrite an existing charter without `--force`.
4. Read the generated `AGENTS.md` back and refine the description and any
   preset assumptions with the user — the scaffold is a starting point.
5. If `setup-git-hooks` was copied, run `/setup-git-hooks` in the target so
   the commit convention matches the charter.
6. Commit the scaffold on a feature branch per the charter's branching rule.
