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

Whichever way you show it, the body also carries **evidence** — the real
output of the check, plus the diff-derived deltas from step 4. Writing the
code was never the expensive part; verifying it is, so the PR's job is to
make verification cheap rather than to describe the work well.

This is a companion to the repo/host's standing PR-creation instructions
(draft PRs, template detection, etc.) — follow those for *whether and how*
to open the PR. This skill is about what goes *in* the description.

## 1. Classify the diff

Run the categorizer against the diff you're about to open a PR for:

```
${CLAUDE_SKILL_DIR}/scripts/categorize_diff.sh <base>...<head>
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
  image with `${CLAUDE_SKILL_DIR}/scripts/compose_side_by_side.py` instead
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

Reference them by **commit SHA**, not branch name:

```
https://raw.githubusercontent.com/<owner>/<repo>/<head-sha>/.github/pr-assets/<branch-slug>/<file>
```

Branch-pinned URLs die when the branch is deleted after merge. SHA-pinned
ones survive while the commit stays reachable — reliable through review and
GitHub's post-merge retention window, not an archival guarantee (squash-merge
eventually orphans the commit). Accepted trade-off for review speed.

Mermaid needs none of this — embed fenced ` ```mermaid ` blocks; GitHub
renders them natively.

## 4. Generate the evidence bundle

"Visual" doesn't mean prettier prose — it means **deltas**. Run this after
the work is done, which is what makes it free: a bundle attached to the PR
costs zero synchronous approval, unlike a plan someone has to read first.

```
${CLAUDE_SKILL_DIR}/scripts/evidence_bundle.sh <base>...<head> [--spec SPEC.md]
```

Four sections, each answering a question the reviewer would otherwise
answer by reading everything:

- **Blast radius** — changed files tiered high/medium/low, so attention goes
  to auth, money, migrations, contracts, CI and deps first.
- **Contract diff** — the public surface that moved: exports, routes, flags,
  env vars, schema. "Nothing detected" is itself a useful result.
- **Substitutions** — newly skipped tests, silenced failures, mocks in
  shipped code, credential-shaped literals, net assertions removed. Every
  line is either deliberate (say so in one sentence) or a workaround that
  should have been a `BLOCKED.md`. Never delete a flagged line from the
  report: a substitution that's invisible in a summary is obvious in raw
  output, and hiding it is what destroys review trust.
- **Spec deviation** — files changed that the spec never named, and files it
  named that went untouched. The plan-vs-diff report, computed from git
  rather than from memory.

Paste it in as-is. It flags, it doesn't judge; the reviewer weighs it. Two
things it deliberately does: skips prose (`*.md`, `docs/`) in the contract and
substitution scans, since a doc that *states* a rule isn't breaking it, and
caps long sections while printing the overflow count — a truncated list that
looks complete is the same failure as a summary hiding a skipped test.

## 5. Assemble the body, visuals first

Start from the skeleton, which seeds the sections from the categories the
diff actually touches:

```
${CLAUDE_SKILL_DIR}/scripts/pr_body_skeleton.sh <base>...<head> > pr-body.md
```

Fixed order, so the reviewer sees the artifact before any prose:

1. **Summary** — 1-2 sentences, no filler.
2. **Visuals** — screenshots/GIFs/diagrams, right after the summary.
3. **What changed** — terse bullets.
4. **Verification** — the command a skeptic would run and its **real
   output**, pasted. A ticked checkbox is an assertion; output is evidence,
   and it's the only form that works for a session nobody watched. If the
   check couldn't run, say so and link the `BLOCKED.md` — a documented
   blockage is a valid outcome, not something to paper over.
5. **Evidence** — the bundle from step 4.
6. **Decisions** — calls made without asking, one line each with a
   confidence marker, so review reads the shaky ones instead of all of them.
7. Long logs in a collapsed `<details>` at the end.

If the repo has its own PR template, populate its sections but still
front-load visuals and keep Verification/Evidence somewhere — treat the
template as a layout, not a reason to bury the proof.

## 6. Size the review ask to the blast radius

Too many PRs is a routing problem, not a rendering one. Say in the body
which kind this is, so a reviewer knows how hard to look:

- **Reversible, tested, no contract change** → automated review plus a spot
  check. Say so outright.
- **Auth, money, migrations, deletion, public contracts** → ask for full
  attention, and name the specific decision you want checked.

For a stack of related PRs, one digest across the feature with each PR's
evidence attached beats N independent review loads.

## 7. Create and clean up

Open the PR per the standing draft/template rules already in force for this
session. Remove any temporary git worktree you created for before/after
capture. Report back briefly which visual strategy you used per category,
plus anything the substitutions scan flagged — decisions worth surfacing,
not narrating.

## 8. Close it out yourself when it's safe to

A green, trivial PR left open for a human who has nothing to add is the
review budget being spent in the wrong place — and it trains whoever does
open it to skim. If the change is genuinely unremarkable, finish the job.

Once checks have **finished** (not merely started), hand the PR to the
**auto-merge** skill and follow it. If it is installed alongside this one,
its script is the sibling directory:

```
${CLAUDE_SKILL_DIR}/../auto-merge/scripts/merge_pr.sh <pr> --ready
```

If that path doesn't exist, this repo didn't take the auto-merge skill —
leave the PR open and say so. Do not hand-roll the checks.

That script decides, not you. It merges only if the triage verdict is
`auto-merge`, every check is green, the branch is not behind base, and no
reviewer asked for changes; otherwise it exits 1 and prints why. `--ready`
publishes the draft as part of merging, and only when everything else
already holds.

On exit 1, report the reasons verbatim and leave the PR open. Do not run
`gh pr merge`, do not edit the classifier, do not re-run hoping for a
different answer — a refusal is a correct outcome, and the whole point of
routing through a script is that your read of your own diff is not the
check. Only ever do this for work you carried end to end; a teammate's
branch closes when they say it does.

See the **auto-merge** skill for what the categories mean.
