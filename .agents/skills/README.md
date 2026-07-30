# Skills

Reusable Claude Code skills specific to Jod live here, one directory per
skill (`.agents/skills/<skill-name>/SKILL.md` plus any supporting files),
following the standard Claude Code skill format.

Promote a skill here once a behavior has proven itself more than once —
don't pre-build skills for hypothetical future needs.

**Reference bundled scripts as `${CLAUDE_SKILL_DIR}/scripts/<name>`**, never as
`.agents/skills/<skill>/scripts/<name>`. These skills ship three ways — copied
into a repo by `setup-project`, installed as the `jod` plugin, or read straight
out of this checkout — and only the substituted form resolves in all three. A
plugin's skills run from `~/.claude/plugins/cache/…` while the cwd is the user's
own project, so a repo-relative path finds nothing there. If you also add a
`.claude/commands/<skill>.md` wrapper, state in it what the variable resolves
to: a wrapper is *read as a file*, so nothing substitutes it.
`tests/plugin.test.sh` enforces both halves.
