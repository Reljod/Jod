#!/usr/bin/env bash
# Emit a ready-to-fill PR body skeleton, visuals-first, seeded with the
# categories the diff actually touches. Wraps categorize_diff.sh.
#
# Usage: pr_body_skeleton.sh <base>...<head>   (any valid git diff range)
#
# Prints markdown to stdout — pipe to a file, edit, then use as the PR body.
# The visual hints per category come straight from the skill's playbook;
# delete the ones you don't need. Two tight visuals beat five thin ones.
set -euo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

if [ $# -lt 1 ]; then
  echo "Usage: $0 <base>...<head>" >&2
  exit 1
fi

CATS="$("$HERE/categorize_diff.sh" "$1" | grep '^## ' | sed 's/^## //' || true)"

hint() {
  case "$1" in
    ui)           echo "- **UI** — before/after screenshots (compose_side_by_side.py), or a short GIF for interactions." ;;
    api)          echo "- **API** — \`mermaid sequenceDiagram\` of the flow (only if non-obvious) + a curl/JSON example." ;;
    architecture) echo "- **Architecture** — \`mermaid\` flowchart / C4 diagram; the diagram is the artifact." ;;
    tooling)      echo "- **Tooling/CLI** — before/after terminal output as fenced code blocks." ;;
    infra)        echo "- **Infra** — \`mermaid\` topology diagram + plan/diff in a collapsed <details>." ;;
    docs)         echo "- **Docs** — rendered before/after only for structural changes; skip for wording edits." ;;
    other)        echo "- **Logic** — no forced visual; tight bullets. Small flow diagram only if control flow is non-obvious." ;;
  esac
}

echo "## Summary"
echo
echo "<!-- 1-2 sentences. What and why, no filler. -->"
echo
echo "## Visuals"
echo
if [ -n "$CATS" ]; then
  echo "<!-- Touched categories, with the visual each one wants. Delete what you don't use. -->"
  while IFS= read -r c; do [ -n "$c" ] && hint "$c"; done <<< "$CATS"
else
  echo "<!-- No categories detected; add a visual only if it genuinely helps. -->"
fi
echo "- **Rules/gates** — for any rule, convention, or gate this PR adds"
echo "  (commit format, lint rule, validator, TDD gate): one usage line +"
echo "  a ✓-passes / ✗-rejected example table per rule. Show, don't describe."
echo
echo "## What changed"
echo
echo "<!-- Terse bullets, not paragraphs. -->"
echo "- "
echo
echo "## Verification"
echo
echo "<!-- The command a skeptic would run, and its REAL output. A ticked box is"
echo "     an assertion; pasted output is evidence. If the check could not run,"
echo "     say so here and link the BLOCKED.md — that is a valid outcome. -->"
echo
echo '```'
echo "\$ "
echo '```'
echo
echo "## Evidence"
echo
echo "<!-- Paste the output of:"
echo "     .agents/skills/create-pr/scripts/evidence_bundle.sh <base>...<head>"
echo "     Blast radius, contract diff, substitutions, spec deviation. Generated"
echo "     after the run, so it costs no approval and answers 'where do I look?'"
echo "     before the reviewer has to ask. -->"
echo
echo "## Decisions"
echo
echo "<!-- Calls made without asking, so review can read only the shaky ones."
echo "     Anything on the spec's escalation list should have been asked, not"
echo "     logged. Drop this section if there were none. -->"
echo
echo "| Decision | Why | Confidence |"
echo "|---|---|---|"
echo "|  |  | high/med/low |"
echo
echo "<!-- Long logs / full plan output go here, collapsed:"
echo "<details><summary>details</summary>"
echo
echo "</details> -->"
