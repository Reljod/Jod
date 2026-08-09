#!/usr/bin/env bash
#
# Scenario suite for the PR shepherd.
#
# The sweep decides which PRs an unattended routine even looks at, so the two
# properties worth testing hardest are the ones that would fail silently:
#
#   1. The hard filters actually exclude. A fork PR or a teammate's PR slipping
#      into the candidate list looks exactly like a working sweep right up
#      until it merges something.
#   2. The sweep never merges. It runs the gate in --dry-run and reports; if it
#      ever reached `gh pr merge` the whole layer model collapses, because the
#      thing doing the enumerating would also be the thing acting.
#
# `gh` is stubbed (canned JSON, every call logged), but git is real: each
# scenario builds a work repo with a genuine bare origin, so merge_pr.sh's
# fetch, rev-list and triage all run against real objects rather than mocks.
#
# Run: .agents/skills/shepherd-prs/tests/test.sh
set -uo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$TEST_DIR/.." && pwd)"
SWEEP="$SKILL_DIR/scripts/pr_sweep.sh"
MERGE="$SKILL_DIR/../auto-merge/scripts/merge_pr.sh"
REPO_ROOT="$(cd "$SKILL_DIR/../../.." && pwd)"

# shellcheck source=../../test-scenarios/scripts/assert.sh
source "$TEST_DIR/../../test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# --- the gh stub --------------------------------------------------------------
#
# Answers from $STUB/*.json and logs every invocation to $STUB/calls.log, so a
# test can assert on what was *not* called as well as what came back.

STUB_BIN="$WORK/bin"
mkdir -p "$STUB_BIN"
cat > "$STUB_BIN/gh" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$STUB/calls.log"
case "$1 $2" in
  "repo view")
    cat "$STUB/owner.json" ;;
  "pr list")
    cat "$STUB/pr_list.json" ;;
  "pr view")
    n="$3"
    [ -f "$STUB/pr_$n.json" ] || { echo "no such PR" >&2; exit 1; }
    cat "$STUB/pr_$n.json" ;;
  *)
    echo "gh stub: refusing unexpected call: $*" >&2; exit 1 ;;
esac
STUB
chmod +x "$STUB_BIN/gh"
PATH="$STUB_BIN:$PATH"
export PATH

# scenario <name> — a work repo with a real bare origin, cwd inside it.
scenario() {
  local d="$WORK/$1"
  export STUB="$d/stub"
  mkdir -p "$d" "$STUB" || return 1
  : > "$STUB/calls.log"
  printf '{"owner":{"login":"owner"}}\n' > "$STUB/owner.json"
  printf '[]\n' > "$STUB/pr_list.json"

  git init -q --bare "$d/origin.git"
  git init -q "$d/work"
  cd "$d/work" || return 1
  git config user.email "t@example.com"
  git config user.name "T"
  git config commit.gpgsign false
  git remote add origin "$d/origin.git"
  mkdir -p src docs
  printf 'def run():\n    return 1\n' > src/app.py
  printf '# Guide\n' > docs/guide.md
  git add -A && git commit -qm base
  git branch -M main
  git push -q origin main
  git fetch -q origin
}

# branch <name> <line> [path] — a feature branch pushed to origin; echoes its
# head SHA. The path decides what the triage classifier sees, so scenarios pick
# it deliberately: docs/guide.md is prose, .github/workflows/x.yml is CI.
branch() {
  git checkout -q -b "$1" main
  local path="${3:-docs/guide.md}"
  mkdir -p "$(dirname "$path")"
  printf '%s\n' "$2" >> "$path"
  git add -A && git commit -qm "$1"
  git push -q origin "$1"
  git checkout -q main
  git rev-parse "$1"
}

# mkpr <n> <sha> [author] [head-owner] [draft] [check-conclusion]
mkpr() {
  local n="$1" sha="$2" author="${3:-owner}" howner="${4:-owner}"
  local draft="${5:-false}" concl="${6:-SUCCESS}"
  cat > "$STUB/pr_$n.json" <<JSON
{
  "number": $n,
  "title": "PR $n",
  "state": "OPEN",
  "isDraft": $draft,
  "author": { "login": "$author" },
  "headRefName": "feat-$n",
  "headRefOid": "$sha",
  "baseRefName": "main",
  "headRepositoryOwner": { "login": "$howner" },
  "url": "https://example.invalid/pull/$n",
  "mergeable": "MERGEABLE",
  "mergeStateStatus": "CLEAN",
  "reviewDecision": "",
  "statusCheckRollup": [
    { "__typename": "CheckRun", "name": "Tests",
      "status": "COMPLETED", "conclusion": "$concl" }
  ]
}
JSON
}

# list <n>... — build the pr list response from the per-PR files.
list() {
  local out="[" first=1 n
  for n in "$@"; do
    [ $first -eq 1 ] || out="$out,"
    first=0
    out="$out$(cat "$STUB/pr_$n.json")"
  done
  printf '%s]\n' "$out" > "$STUB/pr_list.json"
}

sweep() { "$SWEEP" --format tsv "$@" 2>/dev/null; }

# status_of <pr> [sweep-args...] — the status column for one PR. The PR number
# is a filter on the output, not an argument to the sweep.
status_of() {
  local n="$1"; shift
  sweep "$@" | awk -F'\t' -v n="$n" '$1 == n { print $2 }'
}

# report_has <format> <pattern> <name> — assert the report contains a pattern.
#
# The output is captured before it is grepped, never piped straight from the
# script: `grep -q` exits at the first match, the still-writing script takes a
# SIGPIPE, and `pipefail` then reports a *matched* pattern as a failed
# assertion. Capturing first makes the assertion about the text, not about who
# closed the pipe.
report_has() {
  local fmt="$1" pat="$2" name="$3" out
  out="$("$SWEEP" --format "$fmt" 2>/dev/null)"
  if printf '%s\n' "$out" | grep -q -- "$pat"; then pass "$name"
  else fail "$name (no match for: $pat)"; fi
}
tsv_has() { report_has tsv "$1" "$2"; }
md_has()  { report_has md  "$1" "$2"; }

# =============================================================================
section "wiring"

assert_file "$SWEEP" "pr_sweep.sh exists"
ok "[ -x '$SWEEP' ]" "pr_sweep.sh is executable"
assert_file "$MERGE" "it reaches the auto-merge gate as a sibling skill"
assert_ok bash -n "$SWEEP"
command -v shellcheck >/dev/null \
  && assert_ok shellcheck -S warning "$SWEEP"

assert_fails "$SWEEP" --format yaml
assert_fails "$SWEEP" --limit lots
assert_fails "$SWEEP" --pr not-a-number
assert_fails "$SWEEP" --nonsense
assert_ok bash -c "'$SWEEP' --help | grep -q 'never merges'"
ok "grep -q 'dry-run' '$SWEEP'" "the sweep runs the gate in dry-run only"

section "the sweep never merges"

scenario never-merges
sha="$(branch feat-1 hello)"
mkpr 1 "$sha"
list 1
sweep >/dev/null
assert_no_grep "pr merge" "$STUB/calls.log" "no gh pr merge during a sweep"
assert_no_grep "pr ready" "$STUB/calls.log" "no gh pr ready during a sweep"
assert_no_grep "pr edit" "$STUB/calls.log" "no gh pr edit during a sweep"
assert_no_grep "update-branch" "$STUB/calls.log" "no branch updates during a sweep"
ok "grep -q 'pr view' '$STUB/calls.log'" "it does read each PR"

section "hard filter: forks are never candidates"

scenario forks
sha="$(branch feat-1 hello)"
mkpr 1 "$sha" outsider outsider
list 1
assert_eq "$(status_of 1)" "skipped" "a fork PR is skipped, not gated"
tsv_has 'from a fork' "the skip names forks as the reason"
assert_no_grep "pr view 1 " "$STUB/calls.log" "a fork PR never reaches the gate"

# A fork PR by an allowlisted author is still a fork: the head is the risk,
# not the author field, which the fork's owner controls anyway.
scenario fork-by-owner
sha="$(branch feat-1 hello)"
mkpr 1 "$sha" owner someone-else
list 1
assert_eq "$(status_of 1)" "skipped" "a fork is skipped even when authored by the owner"

section "hard filter: author allowlist"

scenario authors
sha="$(branch feat-1 hello)"
mkpr 1 "$sha" teammate
list 1
assert_eq "$(status_of 1)" "skipped" "a teammate's PR is skipped by default"
assert_eq "$(sweep --author teammate | awk -F'\t' '{print $2}')" "ready" \
  "--author admits one deliberately"

scenario authors-partial
sha="$(branch feat-1 hello)"
mkpr 1 "$sha" own
list 1
assert_eq "$(status_of 1)" "skipped" "a login that is a prefix of the owner is not admitted"

section "the gate decides the rest"

scenario green
sha="$(branch feat-1 hello)"
mkpr 1 "$sha"
list 1
assert_eq "$(status_of 1)" "ready" "a docs-only PR with green checks is ready"
tsv_has 'categories:' "a ready row carries the categories"

scenario failing-check
sha="$(branch feat-1 hello)"
mkpr 1 "$sha" owner owner false FAILURE
list 1
assert_eq "$(status_of 1)" "blocked" "a failing check blocks"
tsv_has 'check not green' "the reason is the gate's own words"

scenario draft
sha="$(branch feat-1 hello)"
mkpr 1 "$sha" owner owner true
list 1
assert_eq "$(status_of 1)" "blocked" "a draft blocks without --ready"
assert_eq "$(sweep --ready | awk -F'\t' '{print $2}')" "ready" \
  "--ready passes through to the gate"

scenario human-review
sha="$(branch feat-1 'jobs:' .github/workflows/x.yml)"
mkpr 1 "$sha"
list 1
assert_eq "$(status_of 1)" "blocked" "a PR touching CI blocks on the triage verdict"
tsv_has 'human-review' "the verdict is the stated reason"

section "targeting and shape"

scenario targeting
sha1="$(branch feat-1 one)"
sha2="$(branch feat-2 two)"
mkpr 1 "$sha1"
mkpr 2 "$sha2"
list 1 2
assert_eq "$(sweep | grep -c .)" "2" "a plain sweep covers every open PR"
assert_eq "$(sweep --pr 2 | grep -c .)" "1" "--pr shepherds exactly one"
assert_eq "$(sweep --pr 2 | cut -f1)" "2" "--pr targets the requested PR"

# The log is cleared first: the plain sweeps above legitimately enumerate, and
# the claim under test is about what a --pr run does on its own.
: > "$STUB/calls.log"
sweep --pr 2 >/dev/null
assert_no_grep "pr list" "$STUB/calls.log" "--pr does not enumerate"

scenario empty
list
assert_eq "$(sweep | grep -c . || true)" "0" "no open PRs is an empty sweep, not an error"
assert_ok "$SWEEP" --format tsv
md_has 'No open pull requests' "the markdown report says so plainly"

scenario shape
sha="$(branch feat-1 hello)"
mkpr 1 "$sha"
list 1
assert_eq "$(sweep | awk -F'\t' '{print NF}')" "4" \
  "tsv rows are number/status/title/detail"
assert_eq "$(sweep | cut -f3)" "PR 1" "the row carries the PR title"
md_has '^| #1 |' "markdown renders a table row per PR"
md_has 'PR 1' "the markdown table shows the title, not just the number"
md_has 'ready, .* blocked, .* skipped' "the report carries a tally"
md_has 'not\*\* that the PR should merge' \
  "a ready row disclaims that it is an approval"

# A title is free text. An unescaped `|` would shear the markdown table into
# the wrong columns, so a row's status would render under Title and the reader
# would see a verdict that was never issued.
scenario piped-title
sha="$(branch feat-1 hello)"
mkpr 1 "$sha"
sed -i.bak 's/"title": "PR 1"/"title": "fix: a || b"/' "$STUB/pr_1.json"
list 1
# Matched with a glob rather than a regex: the expected text is itself made of
# backslashes and pipes, and spelling that as a regex tests the escaping of the
# assertion more than the escaping of the script.
row="$("$SWEEP" 2>/dev/null | grep '^| #1 |')"
case "$row" in
  *'a \|\| b'*) pass "a pipe in a PR title is escaped for the table" ;;
  *) fail "a pipe in a PR title is escaped for the table (got: $row)" ;;
esac
assert_eq "$("$SWEEP" 2>/dev/null | grep -c '^| #1 |')" "1" \
  "the row is still exactly one table row"

section "the routine is documented where an agent will read it"

SKILL="$SKILL_DIR/SKILL.md"
CHECKER="$REPO_ROOT/.claude/agents/merge-checker.md"
WRAPPER="$REPO_ROOT/.claude/commands/shepherd-prs.md"
FLOW="$REPO_ROOT/.github/workflows/pr-shepherd.yml"

assert_file "$SKILL" "SKILL.md exists"
assert_file "$CHECKER" "the merge-checker agent is defined"
assert_file "$WRAPPER" "/shepherd-prs wrapper exists"
assert_file "$FLOW" "the unattended trigger exists"

assert_grep "VERDICT: CLEAR" "$SKILL" "the skill states the clear protocol"
assert_grep "VERDICT: BLOCK" "$CHECKER" "the checker states the block protocol"
assert_grep "never permit" "$SKILL" "the layer model says agents cannot permit"
assert_grep "read-only" "$CHECKER" "the checker is declared read-only"

# The frontmatter `tools:` line is the part that actually grants anything, so
# assert on that rather than on the whole file — the prose below it says "no
# Write or Edit tool on purpose", and a whole-file grep would read that
# sentence as the very grant it is disclaiming.
checker_tools="$(sed -n 's/^tools:[[:space:]]*//p' "$CHECKER" | head -1)"
ok "[ -n '$checker_tools' ]" "the checker declares its tools explicitly"
ok "! printf '%s' '$checker_tools' | grep -qE 'Write|Edit|NotebookEdit'" \
  "the checker is granted no writing tool"
assert_grep "one at a time" "$SKILL" "merging serially is stated"

# The workflow must not become the thing that decides. It supplies a trigger.
assert_grep "concurrency" "$FLOW" "only one shepherd runs at a time"
assert_grep "fetch-depth: 0" "$FLOW" "the gate gets real history to diff against"
assert_no_grep "gh pr merge" "$FLOW" "the workflow never merges directly"
assert_grep "allowed-tools" "$FLOW" "the unattended run is given a narrow toolset"

assert_summary
