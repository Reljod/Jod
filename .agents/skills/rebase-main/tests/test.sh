#!/usr/bin/env bash
#
# test.sh — deterministic tests for rebase-main.sh, enumerated against
# test-scenarios/references/scenario-checklist.md and built on its assert.sh.
# Run: .agents/skills/rebase-main/tests/test.sh
#
# Everything runs against throwaway repos in a temp dir with a local bare
# "origin" — no network, no clock, no randomness, and nothing that can touch
# the repo this file lives in. The interesting assertions are the *refusals*:
# this script's whole job is to be the thing that will not force-push a branch
# whose tests never ran.
#
set -u

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd -- "$TEST_DIR/.." && pwd)"
SCRIPT="$SKILL_DIR/scripts/rebase-main.sh"
# shellcheck source=/dev/null
source "$SKILL_DIR/../test-scenarios/scripts/assert.sh"

WORK="$(mktemp -d)"
LOG="$WORK/out.txt"
trap 'rm -rf "$WORK"' EXIT

# Isolate from the developer's own git config: no signing, no hooks, no
# templates, no init.defaultBranch surprise.
export GIT_CONFIG_NOSYSTEM=1
export HOME="$WORK/home"; mkdir -p "$HOME"
export GIT_AUTHOR_NAME=Test GIT_AUTHOR_EMAIL=test@example.invalid
export GIT_COMMITTER_NAME=Test GIT_COMMITTER_EMAIL=test@example.invalid
export NO_COLOR=1

RC=0
# run <dir> <args...> -> stores exit in $RC, output in $LOG
run() {
  local dir="$1"; shift
  ( cd "$dir" && "$SCRIPT" "$@" ) >"$LOG" 2>&1
  RC=$?
  return 0
}
# code <expected> <dir> <args...>
code() {
  local want="$1" dir="$2"; shift 2
  run "$dir" "$@"
  if [ "$RC" -eq "$want" ]; then
    pass "exit $want: $* (in ${dir##*/})"
  else
    fail "expected exit $want, got $RC: $* — $(tail -n2 "$LOG" | tr '\n' ' ')"
  fi
}
log_has() { assert_grep "$1" "$LOG" "${2:-output mentions '$1'}"; }

commit() {  # commit <file> <content> <message>
  printf '%s\n' "$2" > "$1"
  git add "$1" >/dev/null
  git commit --quiet --no-verify -m "$3"
}

# new_repo [feature-branch-name] -> prints the working-clone path.
# origin.git (bare, default branch main) + work/ with one commit on main and
# an unpushed feature branch on top.
new_repo() {
  local branch="${1:-feat/thing}" d
  d="$(mktemp -d "$WORK/repoXXXXXX")"
  git -c init.defaultBranch=main init --quiet --bare "$d/origin.git"
  git clone --quiet "$d/origin.git" "$d/work" 2>/dev/null
  (
    cd "$d/work" || exit 1
    git config user.name Test; git config user.email test@example.invalid
    git config commit.gpgsign false
    git symbolic-ref HEAD refs/heads/main
    commit base.txt "base" "chore: base"
    commit shared.txt $'one\ntwo\nthree' "chore: shared file"
    git push --quiet -u origin main 2>/dev/null
    git checkout --quiet -b "$branch"
    commit mine.txt "mine" "feat: my work"
  ) >/dev/null 2>&1
  printf '%s\n' "$d/work"
}

# advance_main <work-dir> <file> <content> — land a commit on origin/main from
# a second clone, the way a teammate would.
advance_main() {
  local work="$1" origin; origin="$(cd "$work" && git remote get-url origin)"
  local tmp; tmp="$(mktemp -d "$WORK/otherXXXXXX")"
  (
    git clone --quiet "$origin" "$tmp/c" && cd "$tmp/c" || exit 1
    git config user.name Other; git config user.email other@example.invalid
    git config commit.gpgsign false
    commit "$2" "$3" "feat: upstream change"
    git push --quiet origin main
  ) >/dev/null 2>&1
}

echo "== rebase-main test suite =="

# --- 1. the script itself ----------------------------------------------------
section "script is installed and self-describing"
assert_file "$SCRIPT"
ok "[ -x '$SCRIPT' ]" "rebase-main.sh is executable"
assert_ok bash -n "$SCRIPT"
run "$WORK" --help
log_has "preflight" "--help lists preflight"
log_has "push" "--help lists push"

section "usage errors"
code 1 "$WORK" ""                      # no such subcommand
code 1 "$WORK" bogus-command
r="$(new_repo)"
code 1 "$r" preflight --nope
code 1 "$r" check --nope
code 1 "$r" push --nope

section "outside a repository"
mkdir -p "$WORK/plain"
code 1 "$WORK/plain" status
log_has "not inside a git repository"

# --- 2. preflight ------------------------------------------------------------
section "preflight on a healthy feature branch"
r="$(new_repo)"
code 0 "$r" preflight
log_has "ahead:"   "reports how many commits replay"
log_has "behind:"  "reports how many commits land underneath"
log_has "saved:"   "records the pre-rebase HEAD"
saved="$(sed -n 's/^pre_rebase_sha=//p' "$r/.git/jod-rebase-main.state")"
assert_eq "$saved" "$(cd "$r" && git rev-parse HEAD)" "saved SHA is the current HEAD"

section "preflight refuses branches it must never rewrite"
r="$(new_repo)"; ( cd "$r" && git checkout --quiet main )
code 1 "$r" preflight
log_has "default branch" "refuses on the default branch"
for b in master develop release production trunk; do
  r2="$(new_repo "$b")"
  code 1 "$r2" preflight
done
r="$(new_repo)"; ( cd "$r" && git checkout --quiet --detach HEAD )
code 1 "$r" preflight
log_has "detached"

section "preflight warns without blocking"
r="$(new_repo)"
( cd "$r" && printf 'dirty\n' >> base.txt )
code 0 "$r" preflight
log_has "uncommitted" "flags a dirty tree as a warning, not a refusal"

# --- 3. rebase, clean --------------------------------------------------------
section "clean rebase"
r="$(new_repo)"
advance_main "$r" upstream.txt "landed"
code 0 "$r" preflight
code 0 "$r" start
ok "[ -f '$r/upstream.txt' ]" "the upstream commit is now underneath"
ok "[ -f '$r/mine.txt' ]"     "and my commit survived on top"
assert_eq "$(cd "$r" && git rev-list --count 'origin/main..HEAD')" "1" \
  "exactly one commit of mine replays"

section "start refuses a dirty tree, --autostash allows it"
r="$(new_repo)"; advance_main "$r" upstream.txt "landed"
( cd "$r" && printf 'scratch\n' >> base.txt )
code 0 "$r" preflight
code 1 "$r" start
log_has "dirty"
code 0 "$r" start --autostash
assert_grep "scratch" "$r/base.txt" "--autostash reapplies the local edit"

section "an explicit --base overrides the detected default branch"
r="$(new_repo)"; advance_main "$r" upstream.txt "landed"
( cd "$r" && git fetch --quiet origin && git branch --quiet other origin/main~1 )
code 0 "$r" preflight --base other
log_has "base:     other"
code 0 "$r" start --base other
assert_eq "$(cd "$r" && git rev-list --count 'other..HEAD')" "1" "replayed onto 'other', not main"
r="$(new_repo)"
code 1 "$r" preflight --base no/such/ref
log_has "does not resolve"

section "start refuses without a base and without preflight"
r="$(new_repo)"
code 1 "$r" start
log_has "preflight"

# --- 4. rebase, conflicting --------------------------------------------------
# Both sides edit shared.txt, so the replay stops with unmerged paths.
make_conflict() {
  local r; r="$(new_repo)"
  advance_main "$r" shared.txt $'one\nUPSTREAM\nthree'
  ( cd "$r" && commit shared.txt $'one\nMINE\nthree' "feat: my edit" ) >/dev/null 2>&1
  printf '%s\n' "$r"
}

section "a conflicting rebase stops with exit 3 and names the file"
r="$(make_conflict)"
code 0 "$r" preflight
code 3 "$r" start
log_has "shared.txt" "the conflicted file is listed"
code 0 "$r" status
log_has "IN PROGRESS"
log_has "shared.txt"
# HEAD is detached mid-rebase; status still has to name the branch being
# replayed, or the one line a reader needs is the one line it drops.
log_has "feat/thing" "status names the branch being replayed, not '<detached>'"

section "continue refuses to bury an unfinished resolution"
code 1 "$r" continue
log_has "unmerged" "refuses while paths are still unmerged"
# Stage the file with its markers still in it — the exact mistake that
# otherwise commits cleanly and breaks the next build.
( cd "$r" && git add shared.txt )
code 1 "$r" continue
log_has "conflict markers" "refuses a staged file that still has markers"

section "a real resolution completes the rebase"
( cd "$r" && printf 'one\nMINE+UPSTREAM\nthree\n' > shared.txt && git add shared.txt )
code 0 "$r" continue
ok "[ ! -d '$r/.git/rebase-merge' ] && [ ! -d '$r/.git/rebase-apply' ]" \
  "no rebase left in progress"
assert_grep "MINE+UPSTREAM" "$r/shared.txt" "the resolved content is what landed"
code 0 "$r" status
log_has "conflicts: none"

section "abort returns the branch to where it started"
r="$(make_conflict)"
code 0 "$r" preflight
before="$(cd "$r" && git rev-parse HEAD)"
code 3 "$r" start
code 0 "$r" abort
assert_eq "$(cd "$r" && git rev-parse HEAD)" "$before" "HEAD is back to the pre-rebase commit"
code 1 "$r" abort                       # nothing in progress any more
code 1 "$r" continue

section "restore undoes a rebase that already finished"
r="$(new_repo)"; advance_main "$r" upstream.txt "landed"
code 0 "$r" preflight
before="$(sed -n 's/^pre_rebase_sha=//p' "$r/.git/jod-rebase-main.state")"
code 0 "$r" start
ok "[ '$(cd "$r" && git rev-parse HEAD)' != '$before' ]" "the rebase moved HEAD"
code 0 "$r" restore
assert_eq "$(cd "$r" && git rev-parse HEAD)" "$before" "restore puts HEAD back"

section "start refuses to stack a second rebase on an unfinished one"
r="$(make_conflict)"
code 0 "$r" preflight
code 3 "$r" start
code 1 "$r" start
log_has "already in progress"
code 1 "$r" preflight
code 1 "$r" check --cmd true
code 1 "$r" restore

# --- 5. the test gate --------------------------------------------------------
section "check runs the command and reports honestly"
r="$(new_repo)"
code 0 "$r" check --cmd true
log_has "GREEN"
code 4 "$r" check --cmd false
log_has "RED"
log_has "do not push"

section "check refuses to guess a test command"
r="$(new_repo)"
code 1 "$r" detect
code 1 "$r" check
log_has "no test command detected" "an undetectable suite is an error, not a silent pass"

section "detection names a command the repo actually has"
detects() {  # detects <file> <content> <expected-command> [extra-file]
  local r; r="$(new_repo)"
  printf '%s\n' "$2" > "$r/$1"
  [ $# -ge 4 ] && : > "$r/$4"
  run "$r" detect
  assert_eq "$(head -n1 "$LOG")" "$3" "$1 -> $3"
}
detects Cargo.toml   '[workspace]'          'cargo test --workspace'
detects go.mod       'module example.com/x' 'go test ./...'
detects Makefile     'test:'                'make test'
detects package.json '{"scripts":{"test":"vitest"}}' 'npm test'
detects package.json '{"scripts":{"test":"vitest"}}' 'pnpm test' pnpm-lock.yaml
detects pyproject.toml '[project]'          'pytest'
r="$(new_repo)"; printf '{"name":"x"}\n' > "$r/package.json"
code 1 "$r" detect  # a package.json with no test script is not a test command

# --- 6. push -----------------------------------------------------------------
section "push is gated on a green run at THIS commit"
r="$(new_repo)"; advance_main "$r" upstream.txt "landed"
code 0 "$r" preflight
code 0 "$r" start
code 1 "$r" push
log_has "no green test run" "refuses before check has ever run"
code 4 "$r" check --cmd false
code 1 "$r" push                        "a red run does not open the gate"
code 0 "$r" check --cmd true
( cd "$r" && git commit --quiet --no-verify --amend -m "feat: my work, amended" )
code 1 "$r" push
log_has "no green test run" "amending after the green run reopens the gate"
code 0 "$r" check --cmd true
code 0 "$r" push
log_has "pushed"
assert_eq "$(cd "$r" && git rev-parse HEAD)" \
          "$(cd "$r" && git rev-parse origin/feat/thing)" "the remote now matches HEAD"

section "the second push force-rewrites the branch it already published"
( cd "$r" && commit extra.txt "extra" "feat: more work" ) >/dev/null 2>&1
( cd "$r" && git commit --quiet --no-verify --amend -m "feat: more work, amended" )
code 0 "$r" check --cmd true
code 0 "$r" push
assert_eq "$(cd "$r" && git rev-parse HEAD)" \
          "$(cd "$r" && git rev-parse origin/feat/thing)" "the rewrite landed"

section "push refuses every unsafe state"
r="$(new_repo)"; code 0 "$r" check --cmd true
( cd "$r" && git checkout --quiet main )
code 1 "$r" push
log_has "refusing to force-push" "never force-pushes the default branch"
for b in master develop production; do
  r2="$(new_repo "$b")"; code 0 "$r2" check --cmd true; code 1 "$r2" push
done
r="$(make_conflict)"; code 0 "$r" preflight; code 3 "$r" start
code 1 "$r" push
log_has "rebase still in progress"
r="$(new_repo)"; code 0 "$r" check --cmd true
( cd "$r" && printf 'dirty\n' >> base.txt )
code 1 "$r" push
log_has "dirty"

section "a stale lease stops the push instead of destroying the other commits"
r="$(new_repo)"
code 0 "$r" check --cmd true
code 0 "$r" push                        # publish feat/thing
other="$(cd "$r" && git rev-parse HEAD)"
# A teammate pushes to the same branch, and we never fetch it.
tmp="$(mktemp -d "$WORK/mateXXXXXX")"
(
  git clone --quiet "$(cd "$r" && git remote get-url origin)" "$tmp/c" && cd "$tmp/c" || exit 1
  git config user.name Other; git config user.email other@example.invalid
  git config commit.gpgsign false
  git checkout --quiet feat/thing
  commit theirs.txt "theirs" "feat: their work"
  git push --quiet origin feat/thing
) >/dev/null 2>&1
( cd "$r" && git commit --quiet --no-verify --amend -m "feat: my work, rewritten" )
code 0 "$r" check --cmd true
code 4 "$r" push
log_has "lease" "the rejection explains that the remote moved"
log_has "do NOT use --force" "and points away from --force rather than at it"
theirs="$(cd "$tmp/c" && git rev-parse HEAD)"
assert_eq "$(git --git-dir="$(cd "$r" && git remote get-url origin)" rev-parse feat/thing)" \
  "$theirs" "their commit is still the remote tip — nothing was destroyed"

section "--dry-run pushes nothing"
r="$(new_repo)"
code 0 "$r" check --cmd true
code 0 "$r" push --dry-run
assert_fails git --git-dir="$(cd "$r" && git remote get-url origin)" \
  rev-parse --verify feat/thing
log_has "dry run"

assert_summary
