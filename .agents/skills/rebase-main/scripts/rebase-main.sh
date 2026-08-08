#!/usr/bin/env bash
#
# rebase-main.sh — the deterministic half of the rebase-main skill: rebase the
# current branch onto the default branch, surface conflicts as data, gate on a
# real test run, and force-push with a lease.
#
# Every step is a separate subcommand with a meaningful exit code, because the
# agent driving this has to make a decision between them (resolve a conflict,
# read a test failure) and a single do-everything command would either hide
# that decision or fake it.
#
# Exit codes are the contract:
#   0  step succeeded
#   1  usage error / precondition refused (wrong branch, dirty tree, no repo)
#   3  conflicts need a human-or-agent decision (start, continue)
#   4  the gate failed honestly (tests red, lease rejected)
#
# Usage: rebase-main.sh <command> [options]
#   preflight [--base <ref>]      report what a rebase would do; refuse early
#   start     [--base <ref>] [--autostash]
#   status                        conflict/rebase state, machine-readable
#   continue                      finish a conflicted step (rejects leftovers)
#   abort                         return to the pre-rebase state
#   detect                        print the test command check would run
#   check     [--cmd "<cmd>"]     run the test suite (auto-detected if unset)
#   push      [--dry-run] [--remote <name>]
#   restore                       hard-reset to the SHA saved by preflight
#
set -u

PROG="${0##*/}"

# --- output helpers ---------------------------------------------------------
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_R=$'\033[31m'; C_G=$'\033[32m'; C_Y=$'\033[33m'; C_B=$'\033[1m'; C_0=$'\033[0m'
else
  C_R=''; C_G=''; C_Y=''; C_B=''; C_0=''
fi
info() { printf '%s\n' "$*"; }
warn() { printf '%s! %s%s\n' "$C_Y" "$*" "$C_0" >&2; }
err()  { printf '%sx %s%s\n' "$C_R" "$*" "$C_0" >&2; exit 1; }
banner() { printf '\n%s== %s ==%s\n' "$C_B" "$*" "$C_0"; }

usage() {
  # The header comment block above `set -u` is the help text — one source.
  sed -n '/^# Usage:/,/^#$/p' "$0" | sed 's/^# \{0,1\}//;$d'
}

# Help must work anywhere, including outside a repository — so it is answered
# before the git check below, not in the dispatch table at the bottom.
case "${1-}" in
  -h|--help|help) usage; exit 0 ;;
  '') usage; exit 1 ;;
esac

# --- repo facts -------------------------------------------------------------
git rev-parse --git-dir >/dev/null 2>&1 || err "not inside a git repository"
GIT_DIR="$(git rev-parse --absolute-git-dir)"
STATE="$GIT_DIR/jod-rebase-main.state"

# Branches this script will never force-push to, whatever the caller asks.
# The repo's default branch is added to this at push time.
PROTECTED_RE='^(main|master|trunk|develop|development|release|prod|production)$'

current_branch() { git symbolic-ref --quiet --short HEAD 2>/dev/null || true; }

# Resolve the remote's default branch, cheapest source first: the local
# origin/HEAD symref, then the remote itself, then the conventional names.
default_branch() {
  local remote="${1:-origin}" ref name
  ref="$(git symbolic-ref --quiet --short "refs/remotes/$remote/HEAD" 2>/dev/null || true)"
  if [ -n "$ref" ]; then printf '%s\n' "${ref#"$remote"/}"; return 0; fi
  name="$(git ls-remote --symref "$remote" HEAD 2>/dev/null \
          | awk '/^ref:/ {sub("refs/heads/","",$2); print $2; exit}')"
  if [ -n "$name" ]; then printf '%s\n' "$name"; return 0; fi
  for name in main master trunk; do
    if git show-ref --verify --quiet "refs/remotes/$remote/$name"; then
      printf '%s\n' "$name"; return 0
    fi
  done
  return 1
}

rebase_in_progress() {
  [ -d "$GIT_DIR/rebase-merge" ] || [ -d "$GIT_DIR/rebase-apply" ]
}

conflicted_files() { git diff --name-only --diff-filter=U 2>/dev/null; }

tree_dirty() { [ -n "$(git status --porcelain --untracked-files=no 2>/dev/null)" ]; }

state_get() {
  [ -f "$STATE" ] || return 1
  sed -n "s/^$1=//p" "$STATE" | head -n1
}
state_put() {
  local key="$1" val="$2" tmp="$STATE.tmp"
  mkdir -p "$(dirname "$STATE")"
  { [ -f "$STATE" ] && grep -v "^$key=" "$STATE"; printf '%s=%s\n' "$key" "$val"; } \
    > "$tmp" 2>/dev/null
  mv "$tmp" "$STATE"
}

# --- test-command detection -------------------------------------------------
# Deliberately conservative: it only names a command a repo demonstrably has,
# and prints nothing when it cannot tell. "I could not find your tests" is a
# useful answer; guessing one and reporting green is not.
detect_test_cmd() {
  local root; root="$(git rev-parse --show-toplevel)"
  if [ -f "$root/Cargo.toml" ]; then
    printf 'cargo test --workspace\n'; return 0
  fi
  if [ -f "$root/package.json" ] && grep -q '"test"[[:space:]]*:' "$root/package.json"; then
    if   [ -f "$root/pnpm-lock.yaml" ];     then printf 'pnpm test\n'
    elif [ -f "$root/yarn.lock" ];          then printf 'yarn test\n'
    elif [ -f "$root/bun.lockb" ];          then printf 'bun test\n'
    else                                         printf 'npm test\n'; fi
    return 0
  fi
  if [ -f "$root/go.mod" ]; then printf 'go test ./...\n'; return 0; fi
  if [ -f "$root/pyproject.toml" ] || [ -f "$root/pytest.ini" ] || [ -f "$root/tox.ini" ]; then
    if [ -f "$root/uv.lock" ]; then printf 'uv run pytest\n'; else printf 'pytest\n'; fi
    return 0
  fi
  if [ -f "$root/Makefile" ] && grep -qE '^test:' "$root/Makefile"; then
    printf 'make test\n'; return 0
  fi
  # Shell-suite repos (this one included): every tests/*.test.sh, in order.
  if compgen -G "$root/tests/*.test.sh" >/dev/null 2>&1; then
    printf 'for t in tests/*.test.sh; do echo "-- $t"; "$t" || exit 1; done\n'; return 0
  fi
  return 1
}

# --- subcommands ------------------------------------------------------------
cmd_preflight() {
  local base="" remote="origin"
  while [ $# -gt 0 ]; do
    case "$1" in
      --base)   base="${2:?--base needs a ref}"; shift 2 ;;
      --remote) remote="${2:?--remote needs a name}"; shift 2 ;;
      *) err "preflight: unknown option '$1'" ;;
    esac
  done

  local branch; branch="$(current_branch)"
  [ -n "$branch" ] || err "HEAD is detached — check out the branch you want to rebase"

  rebase_in_progress && err "a rebase is already in progress — run '$PROG status', then 'continue' or 'abort'"

  local def; def="$(default_branch "$remote")" \
    || err "cannot determine $remote's default branch — pass --base explicitly"
  [ -n "$base" ] || base="$remote/$def"

  if [ "$branch" = "$def" ]; then
    err "on '$branch', the default branch — rebasing it onto itself is not the workflow; check out your feature branch"
  fi
  if printf '%s' "$branch" | grep -qE "$PROTECTED_RE"; then
    err "'$branch' is a protected branch name — this skill never rewrites it"
  fi

  banner "preflight"
  info "branch:   $branch"
  info "base:     $base (default branch: $def)"
  info "remote:   $remote"

  git fetch --prune "$remote" || err "git fetch $remote failed — fix connectivity first"
  git rev-parse --verify --quiet "$base^{commit}" >/dev/null \
    || err "base ref '$base' does not resolve after fetch"

  local head; head="$(git rev-parse HEAD)"
  state_put pre_rebase_sha "$head"
  state_put branch "$branch"
  state_put base "$base"
  state_put remote "$remote"

  local ahead behind
  ahead="$(git rev-list --count "$base..HEAD")"
  behind="$(git rev-list --count "HEAD..$base")"
  info "ahead:    $ahead commit(s) of yours will be replayed"
  info "behind:   $behind commit(s) from $base will land underneath them"
  info "saved:    pre-rebase HEAD $head (restore with '$PROG restore')"

  if tree_dirty; then
    warn "working tree has uncommitted tracked changes"
    info "  commit them, or run 'start --autostash' to stash and reapply around the rebase"
  fi
  if [ "$ahead" -eq 0 ]; then
    warn "nothing to replay — this branch has no commits $base does not already have"
  fi

  local upstream
  upstream="$(git rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)"
  if [ -n "$upstream" ]; then
    info "upstream: $upstream (a force-push will rewrite it)"
  else
    info "upstream: none yet — push will set it with -u, no force needed"
  fi
  printf '%sok%s preflight passed\n' "$C_G" "$C_0"
}

cmd_start() {
  local base="" autostash=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --base)      base="${2:?--base needs a ref}"; shift 2 ;;
      --autostash) autostash=1; shift ;;
      *) err "start: unknown option '$1'" ;;
    esac
  done
  rebase_in_progress && err "a rebase is already in progress — 'continue' or 'abort' first"
  [ -n "$base" ] || base="$(state_get base || true)"
  [ -n "$base" ] || err "no base recorded — run '$PROG preflight' first, or pass --base"

  if [ "$autostash" -eq 0 ] && tree_dirty; then
    err "working tree is dirty — commit it, or re-run with --autostash"
  fi

  banner "rebase onto $base"
  # A plain, flattening rebase on purpose: --rebase-merges would replay any
  # merge commits on the branch and hand you their conflicts a second time.
  local -a args=()
  [ "$autostash" -eq 1 ] && args+=(--autostash)
  if git rebase ${args+"${args[@]}"} "$base"; then
    printf '%sok%s rebase applied cleanly\n' "$C_G" "$C_0"
    return 0
  fi
  if rebase_in_progress; then
    cmd_status
    warn "conflicts to resolve — edit the files, 'git add' them, then '$PROG continue'"
    return 3
  fi
  err "rebase failed before it started (see git's message above); working tree unchanged"
}

cmd_status() {
  banner "status"
  local branch; branch="$(current_branch)"
  info "branch:   ${branch:-<detached>}"
  info "base:     $(state_get base || echo '<unknown — run preflight>')"
  if rebase_in_progress; then
    # rebase-merge (interactive/merge backend) counts in msgnum/end;
    # rebase-apply (the am backend) counts in next/last.
    local done_n total_n
    done_n="$(cat "$GIT_DIR/rebase-merge/msgnum" "$GIT_DIR/rebase-apply/next" 2>/dev/null | head -n1)"
    total_n="$(cat "$GIT_DIR/rebase-merge/end" "$GIT_DIR/rebase-apply/last" 2>/dev/null | head -n1)"
    info "rebase:   IN PROGRESS (step ${done_n:-?} of ${total_n:-?})"
  else
    info "rebase:   not in progress"
  fi
  local files; files="$(conflicted_files)"
  if [ -n "$files" ]; then
    info "conflicts:"
    printf '%s\n' "$files" | sed 's/^/  - /'
  else
    info "conflicts: none"
  fi
}

cmd_continue() {
  rebase_in_progress || err "no rebase in progress"
  local unmerged; unmerged="$(conflicted_files)"
  if [ -n "$unmerged" ]; then
    printf '%s\n' "$unmerged" | sed 's/^/  - /' >&2
    err "still unmerged — resolve each file and 'git add' it before continuing"
  fi
  # A staged file that still carries markers means a resolution was skipped;
  # committing it would bury a syntax error inside "the rebase succeeded".
  local marked=""
  local f
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    if git show ":$f" 2>/dev/null | grep -qE '^(<<<<<<< |>>>>>>> )'; then
      marked="$marked$f"$'\n'
    fi
  done < <(git diff --cached --name-only --diff-filter=ACM)
  if [ -n "$marked" ]; then
    printf '%s' "$marked" | sed 's/^/  - /' >&2
    err "conflict markers still staged in the files above — finish the resolution"
  fi

  banner "continue"
  if GIT_EDITOR=true git rebase --continue; then
    if rebase_in_progress; then
      cmd_status
      warn "next commit conflicted too — resolve, 'git add', then '$PROG continue'"
      return 3
    fi
    printf '%sok%s rebase complete\n' "$C_G" "$C_0"
    return 0
  fi
  if rebase_in_progress; then
    cmd_status
    warn "conflicts remain — resolve, 'git add', then '$PROG continue'"
    return 3
  fi
  err "git rebase --continue failed (see message above)"
}

cmd_abort() {
  rebase_in_progress || err "no rebase in progress — nothing to abort ('$PROG restore' undoes a finished one)"
  banner "abort"
  git rebase --abort || err "git rebase --abort failed"
  printf '%sok%s back to the pre-rebase state\n' "$C_G" "$C_0"
}

cmd_detect() {
  local cmd; cmd="$(detect_test_cmd || true)"
  [ -n "$cmd" ] || err "no test command detected for this repo — 'check' will need --cmd"
  printf '%s\n' "$cmd"
}

cmd_check() {
  local cmd=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --cmd) cmd="${2:?--cmd needs a command}"; shift 2 ;;
      --)    shift; cmd="$*"; break ;;
      *) err "check: unknown option '$1'" ;;
    esac
  done
  rebase_in_progress && err "rebase still in progress — finish it before running tests"
  if [ -z "$cmd" ]; then
    cmd="$(detect_test_cmd || true)"
    [ -n "$cmd" ] || err "no test command detected — pass --cmd \"<command>\"; do not skip this step"
    info "detected test command: $cmd"
  fi
  banner "tests"
  info "\$ $cmd"
  if ( cd "$(git rev-parse --show-toplevel)" && eval "$cmd" ); then
    printf '\n%sGREEN%s  %s\n' "$C_G" "$C_0" "$cmd"
    state_put tests_passed "$(git rev-parse HEAD)"
    return 0
  fi
  printf '\n%sRED%s    %s\n' "$C_R" "$C_0" "$cmd"
  warn "do not push. Fix the failure, or abort the rebase — never narrow the command to make it green"
  return 4
}

cmd_push() {
  local dry=0 remote=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --dry-run) dry=1; shift ;;
      --remote)  remote="${2:?--remote needs a name}"; shift 2 ;;
      *) err "push: unknown option '$1'" ;;
    esac
  done
  [ -n "$remote" ] || remote="$(state_get remote 2>/dev/null || true)"
  [ -n "$remote" ] || remote="origin"

  # Checked before the branch lookup: mid-rebase HEAD is detached, and
  # "HEAD is detached" would be a confusing way to say "you are mid-rebase".
  rebase_in_progress && err "rebase still in progress — never push a half-rebased branch"
  local branch; branch="$(current_branch)"
  [ -n "$branch" ] || err "HEAD is detached — nothing to push"
  [ -n "$(conflicted_files)" ] && err "unmerged paths remain — resolve them first"
  tree_dirty && err "working tree is dirty — commit or stash before pushing"

  if printf '%s' "$branch" | grep -qE "$PROTECTED_RE"; then
    err "refusing to force-push protected branch '$branch'"
  fi
  local def; def="$(default_branch "$remote" || true)"
  if [ -n "$def" ] && [ "$branch" = "$def" ]; then
    err "refusing to force-push '$branch' — it is $remote's default branch"
  fi

  # The gate: tests must have passed at *this* commit, not an earlier one.
  local head passed
  head="$(git rev-parse HEAD)"
  passed="$(state_get tests_passed 2>/dev/null || true)"
  if [ "$passed" != "$head" ]; then
    err "no green test run recorded for $head — run '$PROG check' first"
  fi

  banner "push"
  local upstream
  upstream="$(git rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)"

  local -a push_args
  if [ -z "$upstream" ]; then
    # Nothing to overwrite, so nothing to force.
    push_args=(push -u "$remote" "$branch")
    info "no upstream yet — plain push, setting $remote/$branch"
  else
    # --force-with-lease alone still overwrites commits you fetched but never
    # looked at; --force-if-includes is what makes the lease mean "I have seen
    # everything that is up there". Older git lacks it, so it is conditional.
    push_args=(push --force-with-lease "$remote" "$branch")
    if git push --help 2>/dev/null | grep -q -- '--force-if-includes'; then
      push_args=(push --force-with-lease --force-if-includes "$remote" "$branch")
    else
      warn "git is too old for --force-if-includes; the lease is weaker"
    fi
    info "rewriting $upstream with a lease"
  fi

  if [ "$dry" -eq 1 ]; then
    info "dry run: git ${push_args[*]} --dry-run"
    git "${push_args[@]}" --dry-run || return 4
    printf '%sok%s dry run clean\n' "$C_G" "$C_0"
    return 0
  fi

  if git "${push_args[@]}"; then
    printf '%sok%s pushed %s -> %s/%s\n' "$C_G" "$C_0" "$branch" "$remote" "$branch"
    return 0
  fi
  warn "push rejected — the lease failed, which means $remote/$branch moved since you fetched"
  warn "someone else's commits are up there. Re-run preflight and rebase again; do NOT use --force"
  return 4
}

cmd_restore() {
  local sha; sha="$(state_get pre_rebase_sha || true)"
  [ -n "$sha" ] || err "no pre-rebase SHA saved — use 'git reflog' to find it yourself"
  rebase_in_progress && err "a rebase is in progress — '$PROG abort' first"
  git rev-parse --verify --quiet "$sha^{commit}" >/dev/null || err "saved SHA $sha no longer resolves"
  banner "restore"
  info "hard-resetting to the pre-rebase HEAD $sha"
  git reset --hard "$sha" || err "reset failed"
  printf '%sok%s back to %s\n' "$C_G" "$C_0" "$sha"
}

# --- dispatch ---------------------------------------------------------------
sub="$1"; shift
case "$sub" in
  preflight) cmd_preflight "$@" ;;
  start)     cmd_start "$@" ;;
  status)    cmd_status "$@" ;;
  continue)  cmd_continue "$@" ;;
  abort)     cmd_abort "$@" ;;
  detect)    cmd_detect "$@" ;;
  check)     cmd_check "$@" ;;
  push)      cmd_push "$@" ;;
  restore)   cmd_restore "$@" ;;
  *) err "unknown command '$sub' (try '$PROG --help')" ;;
esac
