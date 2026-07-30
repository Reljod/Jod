---
description: Interview the user, then write a self-contained SPEC.md a fresh session can execute.
argument-hint: "[one-line description of what to build]"
---

Write a spec for: $ARGUMENTS

Use the **write-spec** skill at `.agents/skills/write-spec/SKILL.md`. Read it
and its `references/interview.md` question bank, then follow it.

Wherever that skill writes `${CLAUDE_SKILL_DIR}`, read it as
`.agents/skills/write-spec` — the skill's own directory in this repo.

Do not start implementing. The output of this command is a spec, not a diff.

1. Read the relevant code first — never ask what the repo already answers.
2. Interview with `AskUserQuestion`: 2-4 questions per round, concrete
   options with your recommendation first, stop when a round would not
   change the diff. Always ask what command proves it works, and whether its
   dependencies exist in this environment right now.
3. Fill in `templates/SPEC.md` — named files, out of scope, one runnable
   check, sanctioned fakes, escalation list.
4. Validate with `scripts/check-spec.sh SPEC.md` and close any gap it finds.
5. Hand it off: tell the user to execute it in a **fresh session**
   ("Implement SPEC.md"), since a new reader is the real test of whether the
   spec stands alone.
