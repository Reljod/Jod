#!/usr/bin/env bash
#
# evidence_bundle.sh — the review-time deltas, computed from the diff after
# the work is done.
#
# Reviewing evidence is faster than re-deriving correctness, and everything
# here is derived from git rather than from what an agent says it did. Four
# sections, each answering a question a reviewer would otherwise answer by
# reading the whole diff:
#
#   Blast radius   — where do I need to actually pay attention?
#   Contract       — did anything callers depend on change shape?
#   Substitutions  — was the check satisfied, or was it made easier to satisfy?
#   Spec deviation — does the diff match what we agreed to build?
#
# It flags, it does not judge: every line is a fact about the diff for a human
# to weigh. Run it after the run — that's what makes it free in autonomy
# terms, since it costs no synchronous approval.
#
# Usage: evidence_bundle.sh <base>...<head> [--spec SPEC.md]
# Prints markdown to stdout. Exit 0 even when it flags things; it is a report.
set -uo pipefail

if [ $# -lt 1 ]; then
  echo "Usage: $0 <base>...<head> [--spec SPEC.md]" >&2
  exit 1
fi

RANGE="$1"; shift
SPEC=""
while [ $# -gt 0 ]; do
  case "$1" in
    --spec) SPEC="${2:?--spec needs a path}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
[ -n "$SPEC" ] || { [ -f SPEC.md ] && SPEC="SPEC.md"; }

FILES="$(git diff --name-only "$RANGE")"
if [ -z "$FILES" ]; then
  echo "No changed files in range: $RANGE" >&2
  exit 0
fi

# diff_side <+|-> — added or removed lines as "<file>\t<text>". Patterns are
# matched against these rather than whole files, so something already in the
# codebase is never reported as introduced here.
#
# Prose is excluded. A markdown file cannot skip a test or swallow an
# exception, but it very often *discusses* doing so — a charter, a review
# brief, or this script's own docs would otherwise flag on every rule they
# state. Contracts live in code; a doc describing one is not the contract.
diff_side() {
  git diff -U0 "$RANGE" | awk -v side="$1" '
    /^\+\+\+ b\// {
      f = substr($0, 7)
      skip = (f ~ /\.(md|markdown|txt|rst|adoc|png|gif|jpe?g|svg)$/ || f ~ /(^|\/)docs?\//)
      next
    }
    /^\+\+\+ / { f = "?"; skip = 0; next }
    /^(\+\+\+|---|@@|diff |index )/ { next }
    !skip && substr($0, 1, 1) == side { printf "%s\t%s\n", f, substr($0, 2) }
  '
}

# cap — keep a section readable, and say what was left out rather than
# silently truncating. A capped list that claims completeness is the same
# failure mode as a summary that hides a skipped test.
CAP=10
cap() {
  local n=0 line
  while IFS= read -r line; do
    n=$((n + 1))
    [ "$n" -le "$CAP" ] && printf '%s\n' "$line"
  done
  [ "$n" -gt "$CAP" ] && printf '  - … and %d more (capped at %d, run the scan yourself for the rest)\n' "$((n - CAP))" "$CAP"
  return 0
}

# scan_diff <+|-> <extended-regex> — matching lines, prefixed with their file.
scan_diff() {
  diff_side "$1" | grep -E -- "$2" \
    | awk -F'\t' '{ printf "  - `%s` — %s\n", $1, substr($2, 1, 120) }' | cap
}
scan_added() { scan_diff "+" "$1"; }

is_test_path() { printf '%s' "$1" | grep -qE '(^|/)(tests?|spec|__tests__|fixtures?)/|\.test\.|\.spec\.|_test\.|test_[^/]*$'; }

echo "## Blast radius"
echo
echo "<!-- Files by how much attention they deserve, not alphabetically. -->"
echo
high=""; med=""; low=""
while IFS= read -r f; do
  [ -n "$f" ] || continue
  stat="$(git diff --numstat "$RANGE" -- "$f" | awk '{printf "+%s/-%s", $1, $2}')"
  row="  - \`${f}\` (${stat:-binary})"$'\n'
  if is_test_path "$f" || printf '%s' "$f" | grep -qE '\.(md|txt|png|gif|jpe?g|svg)$|(^|/)docs?/'; then
    low+="$row"
  elif printf '%s' "$f" | grep -qiE '(^|/)(migrations?|schema|auth|billing|payments?)/|\.sql$|(^|/)\.github/workflows/|Dockerfile|(^|/)(api|routes)/|openapi|(package-lock\.json|yarn\.lock|poetry\.lock|go\.sum|requirements\.txt|Cargo\.lock)$|secret|credential|token|password'; then
    high+="$row"
  else
    med+="$row"
  fi
done <<< "$FILES"

echo "**High — review closely** (auth, money, migrations, public surface, CI, deps)"
echo
[ -n "$high" ] && printf '%s' "$high" || echo "  - none"
echo
echo "**Medium — logic**"
echo
[ -n "$med" ] && printf '%s' "$med" || echo "  - none"
echo
echo "**Low — tests, docs, assets**"
echo
[ -n "$low" ] && printf '%s' "$low" || echo "  - none"
echo

echo "## Contract diff"
echo
echo "<!-- Anything a caller could be depending on. Empty here means this PR"
echo "     is safe to read as internal-only. -->"
echo
CONTRACT_PAT='(export |public |def |func |class |type |interface |CREATE TABLE|ALTER TABLE|DROP TABLE|@(app|router|route|Get|Post|Put|Delete|Patch)\(|--[a-z][a-z0-9-]+|os\.environ|process\.env|getenv)'
c_add="$(scan_added "$CONTRACT_PAT")"
c_del="$(scan_diff "-" "$CONTRACT_PAT")"
if [ -n "$c_add" ] || [ -n "$c_del" ]; then
  [ -n "$c_add" ] && { echo "Added / changed:"; echo; printf '%s\n' "$c_add"; echo; }
  [ -n "$c_del" ] && { echo "Removed (breaking if anything called it):"; echo; printf '%s\n' "$c_del"; echo; }
else
  echo "No public surface, route, flag, env var, or schema change detected."
  echo
fi

echo "## Substitutions"
echo
echo "<!-- Ways a check can be satisfied by making it easier to satisfy. Each"
echo "     line is either deliberate and worth a sentence in the PR body, or a"
echo "     workaround that should have been a BLOCKED.md. -->"
echo
flagged=""
skips="$(scan_added '(\.skip\(|\.todo\(|\bxit\(|@pytest\.mark\.(skip|xfail)|unittest\.skip|t\.Skip\(|#\[ignore\]|--no-verify|continue-on-error: *true)')"
[ -n "$skips" ] && flagged+="**Skipped / disabled checks**"$'\n\n'"$skips"$'\n'
silenced="$(scan_added '(except[[:space:]]*:|except (Exception|BaseException)|catch[[:space:]]*\{|catch[[:space:]]*\([^)]*\)[[:space:]]*\{[[:space:]]*\}|rescue[[:space:]]*$|# noqa|# nosec|# type: ignore|@ts-ignore|eslint-disable|\|\|[[:space:]]*true)')"
[ -n "$silenced" ] && flagged+="**Silenced failures**"$'\n\n'"$silenced"$'\n'
creds="$(scan_added '(api[_-]?key|secret|token|password|passwd)[[:space:]]*[:=][[:space:]]*["'"'"'][^"'"'"']{6,}')"
[ -n "$creds" ] && flagged+="**Hardcoded credential-shaped values**"$'\n\n'"$creds"$'\n'

# A mock is unremarkable in a test and a red flag in shipped code.
mocks=""
while IFS= read -r line; do
  [ -n "$line" ] || continue
  f="$(printf '%s' "$line" | sed 's/^  - `\([^`]*\)`.*/\1/')"
  is_test_path "$f" || mocks+="$line"$'\n'
done <<< "$(scan_added '(mock|Mock|MagicMock|patch\(|stub|FakeClient|DummyClient)')"
[ -n "$mocks" ] && flagged+="**Mocks/stubs in non-test code**"$'\n\n'"$mocks"$'\n'

# Assertions are supposed to accumulate. A net loss is worth one look.
a_add="$(diff_side "+" | grep -cE 'assert|expect\(|should\.')"
a_del="$(diff_side "-" | grep -cE 'assert|expect\(|should\.')"
if [ "$a_del" -gt "$a_add" ]; then
  flagged+="**Net assertions removed** — $a_del removed vs $a_add added."$'\n'
fi

test_files="$(printf '%s\n' "$FILES" | while IFS= read -r f; do [ -n "$f" ] && is_test_path "$f" && echo "$f"; done)"
ci_files="$(printf '%s\n' "$FILES" | grep -E '(^|/)\.github/workflows/|(^|/)\.gitlab-ci|(^|/)Jenkinsfile' || true)"

if [ -n "$flagged" ]; then
  printf '%s\n' "$flagged"
else
  echo "None flagged."
  echo
fi
echo "Touched anyway (state it, don't hide it): $(printf '%s' "$test_files" | grep -c . ) test file(s), $(printf '%s' "$ci_files" | grep -c . ) CI file(s)."
echo

echo "## Spec deviation"
echo
if [ -z "$SPEC" ] || [ ! -f "$SPEC" ]; then
  echo "No spec found (looked for \`${SPEC:-SPEC.md}\`). For non-trivial work, write one"
  echo "first with \`/write-spec\` — review is much cheaper against a stated intent."
  echo
else
  # Paths the spec names, taken from its backticked tokens.
  spec_paths="$(grep -oE '`[^`]+`' "$SPEC" | tr -d '`' | grep -E '/|\.[a-z]{1,4}$' | sort -u)"
  unplanned=""; untouched=""
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ "$f" = "$SPEC" ] && continue
    printf '%s' "$f" | grep -q '\.github/pr-assets/' && continue
    grep -qF -- "$f" "$SPEC" || unplanned+="  - \`${f}\`"$'\n'
  done <<< "$FILES"
  while IFS= read -r p; do
    [ -n "$p" ] || continue
    printf '%s\n' "$FILES" | grep -qF -- "$p" || untouched+="  - \`${p}\`"$'\n'
  done <<< "$spec_paths"

  if [ -z "$unplanned" ] && [ -z "$untouched" ]; then
    echo "Diff matches \`${SPEC}\` — no files outside it, none of its files skipped."
  else
    [ -n "$unplanned" ] && { echo "Changed but not named in \`${SPEC}\` (scope creep, or the spec was incomplete):"; echo; printf '%s' "$unplanned"; echo; }
    [ -n "$untouched" ] && { echo "Named in \`${SPEC}\` but unchanged (dropped, or not needed after all):"; echo; printf '%s' "$untouched"; echo; }
  fi
  echo
fi
