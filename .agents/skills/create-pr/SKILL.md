---
name: create-pr
description: >
  Use before opening or creating a pull request in any repo, or when the
  user says "create a PR", "open a PR", "make a pull request", "ship
  this". Builds a PR description that favors screenshots, short GIFs, and
  diagrams over long prose, tailored to what actually changed (UI, API,
  architecture/design, tooling/CLI, docs, infra, or other).
---

# create-pr

The goal is a PR a reviewer can approve from the description alone,
without reading a wall of text. Default to showing, not telling. There are
three ways to show, and the change decides which:

1. **Visuals** — a screenshot, short GIF, or diagram, when the change has
   something visual or structural to show (UI, a flow, a topology).
2. **Positive/negative examples** — when the change adds or changes a
   **rule, convention, or gate** (a commit-message format, a lint rule, a
   validator, a TDD RED/GREEN gate), don't describe the rule in prose.
   Give one line on how it's invoked, then a compact ✓-passes / ✗-rejected
   example set per rule. The examples *are* the spec — a reviewer sees
   exactly what the rule does and doesn't do without parsing a regex or a
   paragraph.
3. **Tight bullets** — a pure logic fix or config tweak that's neither
   visual nor rule-shaped. Don't force a visual or a table in; a short
   bullet list is the right amount here.

Prose is the fallback, not the default. If you're writing a paragraph to
explain a rule, stop and write two examples instead.

This is a companion to the repo/host's standing PR-creation instructions
(draft PRs, template detection, etc.) — follow those for *whether and how*
to open the PR. This skill is about what goes *in* the description.

## 1. Classify the diff

Run the categorizer against the diff you're about to open a PR for:

```
.agents/skills/create-pr/scripts/categorize_diff.sh <base>...<head>
```

It buckets changed paths into `ui`, `api`, `architecture`, `tooling`,
`infra`, `docs`, `other` by path/extension heuristics and prints a
category → filelist summary. Treat it as a first pass, not gospel —
recategorize by judgment when a path is ambiguous (a `.yaml` might be a
k8s manifest or an app config; a `.ts` might be a React component or a
backend service). Multiple categories can apply to one PR; that's normal.

## 2. Capture the right visual per category

Full detail and worked examples: `references/category-playbook.md`.
Summary:

- **UI** — before/after screenshots. Use the `run` skill to launch the
  app (base ref, then head ref — use a temporary `git worktree` for the
  base ref so the working tree you're actually shipping isn't disturbed),
  then Playwright (Chromium is pre-installed) to capture each state. If
  the change is interactive or animated, a short GIF beats a static shot.
  When before/after are comparable in size, stitch them into one labeled
  image with `scripts/compose_side_by_side.py` instead of posting two
  (needs Pillow: `pip install pillow` if not already available).
- **API** — a compact `mermaid sequenceDiagram` of the request/response
  flow, but only when the flow itself is the non-obvious part. Pair it
  with a short curl/JSON example. A schema shape change gets a small
  before/after field table, not a paragraph.
- **Architecture / design** — a `mermaid` flowchart or C4-style diagram.
  No screenshot needed; the diagram is the artifact.
- **Tooling / CLI** — before/after terminal output as fenced code blocks.
  That output already *is* the artifact; reach for a screenshot or GIF
  only if the tool is a genuinely visual TUI.
- **Infra** — a `mermaid` diagram of the topology change, plus the
  relevant plan/diff output (`terraform plan`, `kubectl diff`, etc.)
  wrapped in a collapsed `<details>` block so it doesn't dominate the
  body.
- **Docs / markdown** — rendered before/after preview screenshots only
  for structural or layout changes. For wording-only edits, skip visuals
  entirely — the diff already reads at a glance, and a screenshot would
  just be padding.
- **Rules / conventions / gates** — a commit-message format, a lint or
  validation rule, a required-check, a TDD RED/GREEN gate. One line on how
  it's invoked, then a compact **✓-passes / ✗-rejected** example set per
  rule (a two-column table, or paired bullets). No prose describing the
  rule — the examples define it. This is the digestible form for anything
  deterministic: a reviewer reads five example rows faster than one regex.
- **Other / pure logic** — no forced visualization. Tight bullets. Add a
  small flow diagram only if it genuinely clarifies non-obvious control
  flow, never as decoration.

If a PR spans multiple categories, repeat this per relevant category —
but the total body length is a budget, not a checklist. Two tight visuals
beat five thin ones.

## 3. Store generated assets

Commit any screenshots/GIFs/composites into the same branch/push, under:

```
.github/pr-assets/<branch-slug>/
```

Reference them in the PR body via the **commit SHA**, not the branch
name:

```
https://raw.githubusercontent.com/<owner>/<repo>/<head-sha>/.github/pr-assets/<branch-slug>/<file>
```

Branch-name-pinned URLs break the moment the branch is deleted after
merge; SHA-pinned URLs keep working as long as that commit stays
reachable. Know the limit: this is reliable while the PR is open and
through GitHub's dangling-commit retention window after merge — it is
not a long-term archival guarantee, especially under squash-merge, where
the original commit can eventually become unreachable. That's an accepted
trade-off for review-time speed, not something to build extra
infrastructure around.

Mermaid diagrams need none of this — embed them directly as fenced
` ```mermaid ` blocks; GitHub renders them natively in PR bodies.

## 4. Assemble the body, visuals first

Start from the skeleton, which seeds the sections from the categories the
diff actually touches:

```
.agents/skills/create-pr/scripts/pr_body_skeleton.sh <base>...<head> > pr-body.md
```

Fixed section order, so a reviewer sees the visual before they scroll to
any text:

1. **Summary** — 1-2 sentences, no filler.
2. **Visuals** — screenshots/GIFs/diagrams, right after the summary.
3. **What changed** — terse bullets, not paragraphs.
4. **Test plan** — a checklist.
5. **Checks** — a short checklist confirming the deterministic gate is
   green (CI required checks, lint/format/type-check, patch coverage). The
   visuals tell the reviewer *what* changed; this tells them the machine
   already verified the boring correctness, so their attention goes to
   judgment, not to re-running your checks in their head.
6. Anything long (raw logs, full plan output) goes in a collapsed
   `<details>` block at the end, not inline.

If the target repo has its own PR template, populate its sections but
still front-load visuals wherever they fit (or add a "Visuals" section
near the top if the template doesn't already have an equivalent) — treat
the template as a layout to fill in, not a reason to bury the images
under prose.

## 5. Create and clean up

Open the PR per the standing draft/template rules already in force for
this session. Remove any temporary git worktree you created for
before/after capture. Report back, briefly, which visual strategy you
used per category — this is a decision worth surfacing, not narrating.
