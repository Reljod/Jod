#!/usr/bin/env bash
#
# check-spec.sh — deterministic completeness check for a SPEC.md.
#
# A spec's whole job is to be self-contained enough for a fresh session to
# execute and a reviewer to check a diff against. "I think it's done" is the
# same non-answer here as it is for tests, so this is the runnable version:
# every required section present, actually filled in, and the two-exit rule
# (check passes, or BLOCKED.md) still intact.
#
# HTML comments are invisible to this checker on purpose. The template's
# guidance lives in comments, so a section still holding only its guidance
# counts as empty — which is exactly what "unfilled" means.
#
# Usage: check-spec.sh [path/to/SPEC.md]      (default: ./SPEC.md)
# Exit 0 = complete. Exit 1 = gaps, listed on stderr. Exit 2 = bad usage.
set -uo pipefail

SPEC="${1:-SPEC.md}"

# Self-locating rather than repo-relative: this skill runs copied into a repo,
# installed as the `jod` plugin (from ~/.claude/plugins/cache/…), or straight
# out of a Jod checkout, and only an absolute path derived from $0 names the
# template correctly in all three.
SKILL_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

if [ ! -f "$SPEC" ]; then
  echo "check-spec: no such spec file: $SPEC" >&2
  echo "Create one from $SKILL_DIR/templates/SPEC.md" >&2
  exit 2
fi

# Sections that must exist AND be filled. Kept in one list so the template
# and the check can't drift apart silently.
REQUIRED_FILLED=(
  "## Goal"
  "## Files & interfaces"
  "## Out of scope"
  "## Verification"
  "## Sanctioned fakes"
  "## Escalate on"
)
# Heading must exist; body is written during execution, not at spec time.
REQUIRED_PRESENT=(
  "## Decision log"
)

# Strip HTML comments (including multi-line ones) so guidance text never
# counts as content.
strip_comments() {
  awk '
    { line = $0 }
    {
      out = ""
      while (1) {
        if (inc) {
          i = index(line, "-->")
          if (i == 0) { line = ""; break }
          inc = 0
          line = substr(line, i + 3)
          continue
        }
        i = index(line, "<!--")
        if (i == 0) { out = out line; line = ""; break }
        out = out substr(line, 1, i - 1)
        line = substr(line, i + 4)
        inc = 1
      }
      print out
    }
  ' "$1"
}

# Body of one "## " section, exclusive of the heading, up to the next "## ".
section_body() {
  awk -v want="$1" '
    $0 == want { grab = 1; next }
    grab && /^## / { grab = 0 }
    grab { print }
  ' "$2"
}

# Non-blank, non-list-skeleton content. A lone "-" or an empty table row is
# the template's scaffolding, not an answer.
meaningful() {
  grep -v '^[[:space:]]*$' \
    | grep -v '^[[:space:]]*-[[:space:]]*$' \
    | grep -v '^[[:space:]]*|[[:space:]|`]*$' \
    | grep -v '^[[:space:]]*```[[:space:]]*$'
}

TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT
strip_comments "$SPEC" > "$TMP"

# Gaps accumulate as newline-separated text, not an array: bash 3.2 (still
# the system bash on macOS) errors on ${#arr[@]} for an empty array under -u.
gaps=""
ngaps=0
gap() { gaps+="  - $1"$'\n'; ngaps=$((ngaps + 1)); }

for h in "${REQUIRED_FILLED[@]}"; do
  if ! grep -qxF -- "$h" "$TMP"; then
    gap "missing section: ${h}"
  elif [ -z "$(section_body "$h" "$TMP" | meaningful)" ]; then
    gap "section is empty (only guidance/scaffolding left): ${h}"
  fi
done

for h in "${REQUIRED_PRESENT[@]}"; do
  grep -qxF -- "$h" "$TMP" || gap "missing section: ${h}"
done

# Verification must carry an actual command, not just prose about one.
if grep -qxF -- "## Verification" "$TMP"; then
  verif="$(section_body "## Verification" "$TMP")"
  cmd="$(printf '%s\n' "$verif" | awk '/^```/ { f = !f; next } f' | meaningful)"
  [ -n "$cmd" ] || gap "## Verification has no runnable command in a fenced block"
  printf '%s\n' "$verif" | grep -qF 'BLOCKED.md' \
    || gap "## Verification dropped the two-exit rule — blocked must stay a legal ending (mention BLOCKED.md)"
fi

# Unfilled template markers and deferred decisions. Both mean the interview
# is not finished, which is the one thing this check exists to catch.
while IFS= read -r marker; do
  [ -n "$marker" ] || continue
  if grep -qF -- "$marker" "$TMP"; then
    gap "unfilled placeholder still present: ${marker}"
  fi
done <<'MARKERS'
<feature name>
TBD
MARKERS

if [ "$ngaps" -gt 0 ]; then
  {
    echo "check-spec: ${SPEC} is not ready to execute (${ngaps} gap(s)):"
    printf '%s' "$gaps"
    echo
    echo "Each gap is a question the executing session would have to guess at."
    echo "Ask the user (AskUserQuestion) or move it under '## Escalate on'."
  } >&2
  exit 1
fi

echo "check-spec: ${SPEC} is complete."
exit 0
