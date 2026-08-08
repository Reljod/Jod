#!/usr/bin/env bash
# Categorises a pull request's diff and decides whether it can be merged
# without a human reading it.
#
# Usage: pr_triage.sh <base>...<head> [--format md|env] [--max-files N]
#                     [--max-lines N] [--allow "docs tests code assets"]
#
# The whole point of this script is that it is a *regex, not a judgement*.
# The diff it reads is written by whoever opened the PR — a teammate, or an
# agent grading its own homework. A model asked "is this safe to merge?"
# reads that text as instructions; a pattern match cannot be talked out of
# firing. So the merge gate is deterministic, and the model's job is reduced
# to running it and obeying the answer.
#
# It only ever escalates. Every rule below can move a PR from auto-merge to
# human-review; nothing can move one the other way. A false positive costs
# one human read. A pattern that never fires costs an unreviewed merge —
# which is why the path patterns are deliberately broad.
#
# No `declare -A` anywhere: associative arrays need bash 4 and macOS ships
# bash 3.2, where the script would abort before its first rule and report a
# clean bill of health for every PR. Findings travel as tab-separated lines,
# which every bash can do. (Same lesson as categorize_diff.sh, and the
# failure mode here is worse: silently green instead of silently empty.)
set -euo pipefail

range=""
format="md"
max_files="${TRIAGE_MAX_FILES:-20}"
max_lines="${TRIAGE_MAX_LINES:-400}"
allow="${TRIAGE_AUTOMERGE_CATEGORIES:-docs research rules tests code assets}"

die() { echo "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --format)     format="${2:-}"; shift 2 ;;
    --max-files)  max_files="${2:-}"; shift 2 ;;
    --max-lines)  max_lines="${2:-}"; shift 2 ;;
    --allow)      allow="${2:-}"; shift 2 ;;
    -h|--help)    sed -n '2,8p' "$0"; exit 0 ;;
    -*)           die "Unknown flag: $1" ;;
    *)            [ -n "$range" ] && die "Unexpected argument: $1"; range="$1"; shift ;;
  esac
done

[ -n "$range" ] || die "Usage: $0 <base>...<head> [--format md|env]"
case "$format" in md|env) ;; *) die "--format must be md or env" ;; esac
case "$max_files$max_lines" in *[!0-9]*) die "--max-files/--max-lines must be integers" ;; esac

git rev-parse --verify --quiet "${range%%...*}" >/dev/null 2>&1 \
  || die "Not a resolvable ref: ${range%%...*}"

# --- what changed ------------------------------------------------------------

all_files="$(git diff --name-only "$range")"
deleted_files="$(git diff --name-only --diff-filter=D "$range")"
numstat="$(git diff --numstat "$range")"

n_files="$(printf '%s' "$all_files" | grep -c . || true)"
n_lines="$(printf '%s\n' "$numstat" \
  | awk -F'\t' '$1 != "-" { s += $1 + $2 } END { print s + 0 }')"

# Files carrying the executable bit, so a script can be told from a note even
# when it sits in a prose directory.
exec_list=" $(git diff --raw "$range" | awk '$2 == "100755" { print $NF }' \
  | tr '\n' ' ')"

# --- categories --------------------------------------------------------------
#
# Descriptive buckets, not exclusive: AGENTS.md is both `docs` and `gate`, and
# the blocking one wins. Anything a pattern doesn't claim is `code`.

CATEGORIES="security gate rules ci deps data contract docs research tests assets"

pattern_for() {
  case "$1" in
    # Auth, secrets, crypto, permissions. Highest-consequence code there is.
    security) printf '%s' '(^|/)(auth|authz|authentication|security|secrets?|crypto|keys?|credentials?)/|(^|/)\.env|(^|/)secrets?\.(ya?ml|json|toml)$|\.(pem|key|p12|keystore)$|(^|/)permissions?\.' ;;
    # What the machine *enforces*: CI, hooks, tool permissions, the plugin
    # manifests, required reviewers — and this skill, prose included, since
    # its prose is the merge policy. Editing any of it changes what checks
    # can even run, so it is never judged by the checks it is editing.
    # Matched by shape, not by a spelled-out path, so the rule survives
    # being installed somewhere else.
    gate)     printf '%s' '(^|/)auto-merge/|(^|/)\.github/|(^|/)\.githooks/|(^|/)\.claude/(hooks|settings)|(^|/)\.claude-plugin/|(^|/)CODEOWNERS$' ;;
    # What the machine *reads*: the charter, skills, agent definitions,
    # commands. Instructions, not enforcement — auto-mergeable unless the
    # change edits the merge policy itself (see the self-amendment scan).
    rules)    printf '%s' '(^|/)(AGENTS|CLAUDE|REVIEW)\.md$|(^|/)\.agents/.*\.md$|(^|/)skills/.*\.md$|(^|/)\.claude/(commands|agents)/|(^|/)agents/.*\.md$' ;;
    # CI can disable every other check, so changing it is never routine.
    ci)       printf '%s' '(^|/)\.github/|(^|/)\.gitlab-ci\.ya?ml$|(^|/)\.circleci/|(^|/)Jenkinsfile$|(^|/)azure-pipelines\.ya?ml$' ;;
    # Supply chain: a lockfile bump is a code change you did not write.
    deps)     printf '%s' '(^|/)(package-lock\.json|pnpm-lock\.ya?ml|yarn\.lock|Cargo\.lock|poetry\.lock|uv\.lock|go\.sum|Gemfile\.lock)$|(^|/)requirements[^/]*\.txt$|(^|/)(package|deno)\.json$|(^|/)(Cargo|pyproject)\.toml$|(^|/)go\.mod$|(^|/)Gemfile$' ;;
    # Migrations are the one change a revert does not undo.
    data)     printf '%s' '(^|/)migrations?/|(^|/)alembic/|(^|/)prisma/|\.sql$|(^|/)schema\.' ;;
    # Anything someone else has already built against.
    contract) printf '%s' '(^|/)install\.sh$|(^|/)bin/|(^|/)(api|routes|handlers|resolvers)/|\.proto$|(^|/)openapi\.(ya?ml|json)$|\.d\.ts$' ;;
    docs)     printf '%s' '\.(md|mdx|rst|txt)$|(^|/)docs/|(^|/)LICENSE' ;;
    # Findings, notes, datasets — writing that nothing executes. Auto-merged
    # like prose, but only while it stays inert: anything here that is a
    # script (by extension or by mode bit) is reclassified `code` below and
    # has to clear the code rules on its own.
    research) printf '%s' '(^|/)(research|notes|findings|experiments|explorations|analysis|scratch)/|\.(ipynb|csv|tsv)$' ;;
    tests)    printf '%s' '(^|/)tests?/|(^|/)__tests__/|(^|/)spec/|(\.|_)test\.[a-z]+$|\.spec\.[a-z]+$|(^|/)test_[^/]+$|(^|/)conftest\.py$' ;;
    assets)   printf '%s' '\.(png|jpe?g|gif|svg|webp|ico|woff2?|ttf)$' ;;
  esac
}

hits=""      # "<category>\t<file>" per line
found=""     # space-padded list of categories that matched at least one file

while IFS= read -r f; do
  [ -z "$f" ] && continue

  # Prose directories earn their exemption by being inert. A `.sh` under
  # research/ is a script that happens to live next to notes, so it is judged
  # as code — size limits, content scans and all — not waved through as
  # writing.
  is_script=""
  case "$f" in *.sh|*.bash|*.zsh|*.fish|*.py|*.rb|*.pl|*.ps1|*.js|*.mjs|*.ts) is_script=1 ;; esac
  case "$exec_list" in *" $f "*) is_script=1 ;; esac

  claimed=""
  for cat in $CATEGORIES; do
    if [ -n "$is_script" ]; then
      case "$cat" in docs|research) continue ;; esac
    fi
    re="$(pattern_for "$cat")"
    # Unquoted on purpose — a quoted right-hand side is matched literally.
    if [[ "$f" =~ $re ]]; then
      hits="$hits$cat	$f
"
      claimed="1"
      case " $found " in *" $cat "*) ;; *) found="$found $cat" ;; esac
    fi
  done
  if [ -z "$claimed" ]; then
    hits="${hits}code	$f
"
    case " $found " in *" code "*) ;; *) found="$found code" ;; esac
  fi
done <<< "$all_files"

found="$(printf '%s' "$found" | sed 's/^ //')"

# --- content findings --------------------------------------------------------
#
# Path patterns catch where a change lands; these catch what it does. The list
# is REVIEW.md's "substitutions" section made mechanical, so the thing a human
# reviewer is told to flag is the same thing that blocks an auto-merge.
#
# Lockfiles are excluded: they already block via `deps`, and grepping a
# 30k-line lock for the word "token" produces nothing but noise.

added="$(git diff -U0 "$range" -- . \
  ':(exclude)*.lock' ':(exclude)*lock.json' ':(exclude)*.sum' 2>/dev/null \
  | grep '^+' | grep -v '^+++' || true)"

findings=""   # "<category>\t<reason>" per line
note() { findings="$findings$1	$2
"; }

scan() { # scan <extended-regex> <category> <reason>
  local m
  m="$(printf '%s\n' "$added" | grep -Eic -- "$1" || true)"
  [ "$m" -gt 0 ] && note "$2" "$3 ($m added line(s))"
  return 0
}

if [ -n "$added" ]; then
  scan '@pytest\.mark\.(skip|xfail)|pytest\.skip\(|#\[ignore\]|t\.Skip\(|@Ignore\b|\b(xit|xdescribe)\(|\.(skip|only)\(' \
    substitution "a test is skipped, disabled, or narrowed to .only"
  scan 'except[[:space:]]*:|except[[:space:]]+Exception|catch[[:space:]]*\([^)]*\)[[:space:]]*\{[[:space:]]*\}|catch[[:space:]]*\{[[:space:]]*\}' \
    substitution "a failure is swallowed by a bare or empty catch"
  scan '#[[:space:]]*noqa|#[[:space:]]*type:[[:space:]]*ignore|@ts-(ignore|expect-error)|eslint-disable|#\[allow\(|--no-verify|continue-on-error:[[:space:]]*true' \
    substitution "a check is silenced rather than satisfied"
  scan '(api[-_]?key|secret|passwd|password|token|credential|private[-_]?key)[[:space:]]*[:=][[:space:]]*.?["'"'"'][^"'"'"']{8,}' \
    security "a credential-shaped literal is hardcoded"
  scan '\bdebugger\b|dbg!\(|binding\.pry|pdb\.set_trace\(|breakpoint\(\)' \
    debug "a debugger breakpoint is left in"

  # Inert changes — notes, findings, prose — are safe to merge unread because
  # nothing happens when they land. A command that deletes, force-pushes, or
  # pipes the internet into a shell is the opposite, wherever it appears. The
  # targets are deliberately narrow (`/`, `~`, `$HOME`, a bare glob) so the
  # `rm -rf "$tmpdir"` in every test fixture doesn't cry wolf until people
  # switch the gate off.
  scan 'rm[[:space:]]+-[a-z]*[rf][a-z]*[[:space:]]+(/|~|\*|\$\{?HOME)|(curl|wget)[^|]*\|[[:space:]]*(sudo[[:space:]]+)?(ba|z)?sh|\bsudo[[:space:]]|git[[:space:]]+push[^;|]*(--force|[[:space:]]-f[[:space:]])|git[[:space:]]+reset[[:space:]]+--hard|(DROP|TRUNCATE)[[:space:]]+(TABLE|DATABASE|SCHEMA)|DELETE[[:space:]]+FROM|\bdd[[:space:]]+if=|\bmkfs|\bshred[[:space:]]|>[[:space:]]*/dev/sd|chmod[[:space:]]+(-R[[:space:]]+)?0?777|terraform[[:space:]]+destroy|kubectl[[:space:]]+delete|aws[[:space:]]+s3[[:space:]]+(rm|rb)[[:space:]]|docker[[:space:]]+system[[:space:]]+prune' \
    destructive "a destructive or privilege-escalating command is introduced"
fi

# Rules files are prose an agent obeys, so most edits to them are as inert as
# any other writing — a clarified charter paragraph, a new skill, a fixed
# example. What is not inert is a rules change that edits the merge policy
# itself, because that is the branch quietly granting itself permission.
#
# Removed lines count as much as added ones here, and only here: deleting
# "never merge X" is the dangerous direction, and it leaves no `+` line to
# find.
rules_files="$(printf '%s' "$hits" | awk -F'\t' '$1 == "rules" { print $2 }' | sort -u)"
if [ -n "$rules_files" ]; then
  rules_diff="$(printf '%s\n' "$rules_files" | tr '\n' '\0' \
    | xargs -0 git diff -U0 "$range" -- 2>/dev/null \
    | grep -E '^[+-]' | grep -Ev '^(\+\+\+|---)' || true)"
  amend="$(printf '%s\n' "$rules_diff" | grep -Eic \
    'auto[-_ ]?merge|merge:auto|human-review|never_automerge|triage_[a-z]|--allow[[:space:]]|max-(files|lines)|merge_pr|pr_triage|branch protection|required[[:space:]]+(status[[:space:]]+)?check|--no-verify|--admin|bypass' \
    || true)"
  [ "$amend" -gt 0 ] && note self-amendment \
    "a rules file edits the merge policy itself ($amend changed line(s))"
fi

# A mock in shipped code is a substitution; a mock in a test is the point.
non_test_files="$(printf '%s' "$all_files" | grep -Ev "$(pattern_for tests)" || true)"
if [ -n "$non_test_files" ]; then
  mocked="$(printf '%s\n' "$non_test_files" | tr '\n' '\0' \
    | xargs -0 git diff -U0 "$range" -- 2>/dev/null \
    | grep '^+' | grep -v '^+++' \
    | grep -Eic 'MagicMock|unittest\.mock|@patch\(|jest\.mock\(|sinon\.stub\(|mockito' || true)"
  [ "$mocked" -gt 0 ] && note substitution \
    "a mock or stub appears in non-test code ($mocked added line(s))"
fi

# Deleting a test is the substitution the diff makes hardest to notice.
deleted_tests="$(printf '%s' "$deleted_files" | grep -E "$(pattern_for tests)" || true)"
[ -n "$deleted_tests" ] && note substitution \
  "test files deleted: $(printf '%s' "$deleted_tests" | tr '\n' ' ')"

# Deleting anything else that isn't prose still needs eyes.
deleted_code="$(printf '%s' "$deleted_files" | grep -Ev "$(pattern_for docs)" || true)"
[ -n "$deleted_code" ] && note deletion \
  "non-doc files deleted: $(printf '%s' "$deleted_code" | tr '\n' ' ')"

# A binary blob is unreviewable as text, so it is never unattended-mergeable.
binaries="$(printf '%s\n' "$numstat" | awk -F'\t' '$1 == "-" { print $3 }' \
  | grep -Ev "$(pattern_for assets)" || true)"
[ -n "$binaries" ] && note binary \
  "non-asset binary files changed: $(printf '%s' "$binaries" | tr '\n' ' ')"

# BLOCKED.md is a successful ending per AGENTS.md — and a human's cue, not a
# merge signal. Never auto-merge one.
case "$all_files" in *BLOCKED.md*) note blocked "the branch carries a BLOCKED.md" ;; esac

# --- size and emptiness ------------------------------------------------------
#
# The limits count executable weight, not volume. A 3,000-line research
# writeup is not 7× riskier than a 400-line one — nothing runs it — whereas
# 400 lines of new code is about as much as a reviewer reads carefully. So
# prose and assets are excluded from the count, and the thresholds stay tight
# enough to mean something for the files that do execute.

prose_re="$(pattern_for docs)|$(pattern_for research)|$(pattern_for assets)"
code_names="$(printf '%s' "$hits" \
  | awk -F'\t' '$1 != "docs" && $1 != "research" && $1 != "assets" { print $2 }' \
  | sort -u)"
n_code_files="$(printf '%s' "$code_names" | grep -c . || true)"
n_code_lines="$(printf '%s\n' "$numstat" | awk -F'\t' -v re="$prose_re" \
  '$1 != "-" && $3 !~ re { s += $1 + $2 } END { print s + 0 }')"

[ "$n_files" -eq 0 ] && note empty "the range contains no changed files"
[ "$n_code_files" -gt "$max_files" ] && note size \
  "$n_code_files code files changed, over the $max_files-file limit"
[ "$n_code_lines" -gt "$max_lines" ] && note size \
  "$n_code_lines code lines changed, over the $max_lines-line limit"

# --- verdict -----------------------------------------------------------------

# Four categories are a floor, not a default: no --allow, no environment
# variable and no config file can make them auto-mergeable. A knob that can
# switch the gate off for CI or for auth is the gate being optional, and the
# first thing a shortcut reaches for is the knob.
NEVER_AUTOMERGE="security gate ci data"

for cat in $found; do
  case " $NEVER_AUTOMERGE " in
    *" $cat "*) note "$cat" "touches $cat, which is never auto-mergeable"; continue ;;
  esac
  case " $allow " in
    *" $cat "*) ;;
    *) note "$cat" "touches $cat, which this repo does not auto-merge" ;;
  esac
done

blockers="$(printf '%s' "$findings" | grep -c . || true)"
if [ "$blockers" -gt 0 ]; then verdict="human-review"; else verdict="auto-merge"; fi

# --- report ------------------------------------------------------------------

if [ "$format" = env ]; then
  echo "verdict=$verdict"
  echo "categories=$found"
  echo "blocker_count=$blockers"
  echo "files_changed=$n_files"
  echo "lines_changed=$n_lines"
  echo "code_files_changed=$n_code_files"
  echo "code_lines_changed=$n_code_lines"
  labels="merge:$( [ "$verdict" = auto-merge ] && echo auto || echo human )"
  for cat in $found; do labels="$labels,area:$cat"; done
  echo "labels=$labels"
  exit 0
fi

echo "<!-- jod:pr-triage -->"
echo "## PR triage: \`$verdict\`"
echo
if [ "$verdict" = auto-merge ]; then
  echo "Every rule passed. **A human read is not required** — an agent may merge"
  echo "this once all required checks are green."
else
  echo "**A human must read this before it merges.** Reasons below; an agent"
  echo "must not merge it, and clearing them is a review, not a re-run."
fi
echo
echo "| | |"
echo "|---|---|"
echo "| Categories | $( [ -n "$found" ] && echo "\`${found// /\`, \`}\`" || echo "none" ) |"
echo "| Size | $n_files files, $n_lines lines |"
echo "| Counted against limits | $n_code_files code files, $n_code_lines code lines (limits: $max_files / $max_lines; prose and assets excluded) |"
echo "| Auto-mergeable categories | \`${allow// /\`, \`}\` |"
echo

if [ "$blockers" -gt 0 ]; then
  echo "### Why a human is needed"
  echo
  printf '%s' "$findings" | while IFS='	' read -r cat reason; do
    [ -z "$cat" ] && continue
    echo "- **$cat** — $reason"
  done
  echo
fi

echo "<details><summary>Files by category</summary>"
echo
for cat in $CATEGORIES code; do
  list="$(printf '%s' "$hits" | awk -F'\t' -v c="$cat" '$1 == c { print $2 }')"
  [ -z "$list" ] && continue
  echo "**$cat**"
  printf '%s\n' "$list" | sed 's/^/- `/; s/$/`/'
  echo
done
echo "</details>"
