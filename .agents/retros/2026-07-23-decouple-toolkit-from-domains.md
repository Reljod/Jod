# Retro — 2026-07-23 — Decouple the toolkit from personal domains

**Session:** Adding the `/retro` command + `create-retro` skill.
**Trigger:** back-and-forth *and* divergence from the first attempt.

## What changed vs. the first attempt

- The first version routed retro outputs and skill cross-references into
  `domains/coding/README.md` and `domains/coding/retros/`, and treated Coding
  as a personal domain sitting alongside Linear / Notion / finance →
  **all reusable coding conventions and workflow moved into a portable
  `.agents/conventions/` + `.agents/retros/`, `domains/coding/` was removed,
  and every `domains/` reference was stripped from the skills, the charter, and
  the top-level README** — because the repo has two distinct purposes and the
  toolkit half must stay copyable into other projects that have no `domains/`
  at all.

## Preferences captured

- **Portability rule** — nothing in the toolkit (skills, conventions,
  `AGENTS.md`) may reference or route into `domains/`; it must stay copyable
  into a repo with no `domains/`. → routed to
  [`.agents/conventions/README.md`](../conventions/README.md) and
  [`AGENTS.md`](../../AGENTS.md).
- **Two halves** — the repo is a *portable toolkit* under `.agents/`
  (improve-my-workflows) plus *personal domains* under `domains/`
  (duplicate-me). Coding conventions/workflow are toolkit, not a personal
  domain. → routed to [`AGENTS.md`](../../AGENTS.md) and the top-level README.
- **Retro scope** — `create-retro`, being part of the toolkit, routes learnings
  only into the toolkit; a preference about operating inside a personal domain
  is out of scope for it. → routed to
  [`create-retro/SKILL.md`](../skills/create-retro/SKILL.md).

## Not yet durable

- None parked — the directive was explicit and fully promoted this session.
  (This entry also serves as the first worked example of the retro format.)
