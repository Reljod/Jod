#!/usr/bin/env bash
#
# sweep_targets.sh — reclaim disk by deleting build output that nothing is using.
#
# The whole decision is here, in shell, so that it is the same decision every
# time. Nothing about "is this build directory still wanted" needs judgement: a
# directory is in use if a compiler is writing to it or wrote to it recently, and
# it is not if neither is true. A model asked the same question would sometimes
# answer differently, and the failure mode is deleting the build a teammate is
# three minutes into.
#
# Only ever deletes directories named `target/` (cargo) or `node_modules/`
# (--with-node) that sit inside the repository this script ships in. Source is
# never a candidate: `target/` is gitignored, so a deleted one costs time and
# nothing else.
#
# Exits 0 and prints nothing when there is nothing to do. That silence is load
# bearing — `jod monitor set --no-agent` treats empty stdout as "stay quiet", so
# a quiet hour produces no ledger entry and no woken model.
#
# Run:  sweep_targets.sh                 # report what it would free, delete nothing
#       sweep_targets.sh --apply         # actually delete
#       sweep_targets.sh --apply --min-free-gb 0   # sweep regardless of pressure
set -uo pipefail

# --- defaults ---------------------------------------------------------------
# Below this much free space, start sweeping. Above it, do nothing at all: a
# build cache is only waste when the space it holds is needed.
MIN_FREE_GB=8
# A directory written to more recently than this is treated as live, even with no
# compiler currently running — cargo goes quiet between crates, and an agent
# pausing to read a test failure is still mid-task.
IDLE_MINUTES=90
APPLY=0
WITH_NODE=0
JSON=0
ROOT=""
# Paths never to sweep, as globs. Empty-initialised, so every expansion below
# uses the `${a[@]+"${a[@]}"}` form — on bash 3.2 under `set -u` an unguarded
# `"${a[@]}"` on an empty array is an unbound-variable error, which is the one
# portability bug tests/shell-arrays.test.sh exists to catch.
SKIPS=()

usage() {
  cat <<'EOF'
sweep_targets.sh — delete build output nothing is using.

  --apply             Delete. Without it, report only and touch nothing.
  --min-free-gb N     Sweep only when free space is under N GB (default 8).
                      0 sweeps unconditionally.
  --idle-minutes N    Treat a directory as live if written within N minutes
                      (default 90).
  --with-node         Also consider node_modules/ (off by default: reinstalling
                      is a network round trip, not just CPU).
  --skip GLOB         Never sweep a path matching GLOB. Repeatable. Matched
                      against both the repo-relative and the absolute path, so
                      --skip 'apps/ios/*' and an absolute form both work.
  --root PATH         Repository to sweep (default: the repo this script is in).
  --json              Emit one JSON object per swept directory.
  -h, --help          This.

Exit 0 with no output means nothing needed doing.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --apply) APPLY=1 ;;
    --with-node) WITH_NODE=1 ;;
    --json) JSON=1 ;;
    --min-free-gb) MIN_FREE_GB="${2:-}"; shift ;;
    --idle-minutes) IDLE_MINUTES="${2:-}"; shift ;;
    --root) ROOT="${2:-}"; shift ;;
    --skip) SKIPS+=("${2:-}"); shift ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'sweep_targets.sh: unknown argument %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

for n in MIN_FREE_GB IDLE_MINUTES; do
  case "${!n}" in
    ''|*[!0-9]*) printf 'sweep_targets.sh: --%s wants a whole number, got %s\n' \
      "$(echo "$n" | tr 'A-Z_' 'a-z-')" "${!n}" >&2; exit 2 ;;
  esac
done

# The repo is found from this script's own location, never from $PWD, so a cron
# firing in / sweeps the right tree. Skills are addressed through
# ${CLAUDE_SKILL_DIR}, and this resolves the same way whether it is invoked from
# there, from the plugin, or by absolute path.
if [ -z "$ROOT" ]; then
  script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
  ROOT="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null || true)"
  # A worktree's toplevel is the worktree; the shared checkout above it owns the
  # rest of them, and sweeping needs to see all of them at once.
  if [ -n "$ROOT" ]; then
    common="$(git -C "$ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
    [ -n "$common" ] && ROOT="$(dirname -- "$common")"
  fi
fi
[ -n "$ROOT" ] && [ -d "$ROOT" ] || {
  printf 'sweep_targets.sh: no repository found (pass --root)\n' >&2; exit 2; }
# `-P` because the busy check compares this against paths the kernel reports, and
# the kernel has no symlinks left to report. On macOS `/tmp` and `/var` are links
# into `/private`, so a root given as `/var/folders/…` would be compared against
# a cwd of `/private/var/folders/…` — two spellings of one directory that no
# string match can reconcile, and every busy directory would look idle.
ROOT="$(cd -- "$ROOT" && pwd -P)"

# --- is there any pressure? -------------------------------------------------
free_kb() { df -Pk "$ROOT" | awk 'NR==2 {print $4}'; }

free_gb_now() { echo $(( $(free_kb) / 1024 / 1024 )); }

if [ "$MIN_FREE_GB" -gt 0 ] && [ "$(free_gb_now)" -ge "$MIN_FREE_GB" ]; then
  exit 0   # silence on purpose: nothing to report
fi

# --- which directories is a compiler actually inside? -----------------------
# One pass over /proc, because a per-candidate `pgrep` races: a build can start
# between two candidates and the second check would not see it.
#
# Only *build* processes count, not any process. An agent session's cwd sits in
# its worktree for the session's whole life, so treating that as "busy" would
# mean a worktree with an idle agent in it is never swept — which is precisely
# the case this exists to reclaim.
#
# A process is a build if a compiler name appears either as its executable or as
# any token on its command line. The command-line half matters: `/proc/pid/exe`
# resolves to the real inode, so a build driven through a wrapper, a shim or
# `bash -c 'cargo test …'` has an `exe` of bash and would otherwise be invisible.
# The bias is deliberate — a false positive spares a directory that could have
# been deleted, a false negative deletes a build in progress.
is_build_name() {
  case "$1" in
    cargo|rustc|rustdoc|cc|c++|gcc|clang|ld|lld|collect2|ar|make|node|npm|npx|tsc|vite|nextest|cargo-nextest) return 0 ;;
    *) return 1 ;;
  esac
}

# Record one build process: where it is standing, and what it named.
#
# Both halves matter and they catch different things. The cwd catches `cargo
# build` run from inside a worktree; the command line catches a build pointed at
# a target directory from somewhere else, which is why it is kept whenever it
# mentions the root at all.
note_build() { # note_build <cwd> <args>
  [ -n "$1" ] && busy_paths="$busy_paths$1"$'\n'
  case "$2" in
    *"$ROOT"*) busy_paths="$busy_paths$2"$'\n' ;;
  esac
}

# Whether any token of a command line is a compiler.
args_look_like_build() { # args_look_like_build <args, one token per line>
  local tok
  while IFS= read -r tok; do
    [ -n "$tok" ] || continue
    is_build_name "${tok##*/}" && return 0
  done <<< "$1"
  return 1
}

busy_paths=""
if [ -d /proc ]; then
  for pid_dir in /proc/[0-9]*; do
    [ -r "$pid_dir/cmdline" ] || continue
    looks_like_build=0
    exe="$(readlink "$pid_dir/exe" 2>/dev/null || true)"
    [ -n "$exe" ] && is_build_name "${exe##*/}" && looks_like_build=1
    # NUL-separated, so a path containing a space cannot split into two entries.
    args="$(tr '\0' '\n' < "$pid_dir/cmdline" 2>/dev/null || true)"
    if [ "$looks_like_build" -eq 0 ]; then
      args_look_like_build "$args" && looks_like_build=1
    fi
    [ "$looks_like_build" -eq 1 ] || continue
    note_build "$(readlink "$pid_dir/cwd" 2>/dev/null || true)" "$args"
  done
else
  # macOS has no /proc, so the loop above ran zero times and `busy_paths` stayed
  # empty — meaning no directory was ever busy and the guard this section opens
  # by explaining was simply absent on the machine whose disk fills up soonest.
  #
  # `ps` answers what is running and `lsof -d cwd` answers where it is standing.
  # `lsof` is asked only about the processes that already look like builds, which
  # is a handful, because asking it about every process on the box is slow enough
  # to matter in an hourly job.
  #
  # Tokens are split on whitespace here rather than on NUL, because `ps` has
  # already joined them and the separator is gone. That is only good enough for
  # spotting a compiler *name*, which is all this half does; the other half
  # matches the root as a fixed substring and is unaffected.
  while IFS= read -r line; do
    pid="${line%% *}"
    [ -n "$pid" ] || continue
    args="${line#"$pid" }"
    args_look_like_build "$(tr ' ' '\n' <<< "$args")" || continue
    # -Fn prints one `n<path>` line per record; the cwd is the only record asked
    # for. A process that exited between `ps` and here simply yields nothing.
    cwd="$(lsof -a -d cwd -Fn -p "$pid" 2>/dev/null | sed -n 's/^n//p' | head -1)"
    note_build "$cwd" "$args"
  done < <(ps -Ao pid=,args= 2>/dev/null | sed 's/^ *//')
fi

is_busy() { # is_busy <target-dir> <worktree>
  local target="$1" worktree="$2"
  case "$busy_paths" in
    *"$target"*) return 0 ;;   # a compiler names this target dir
  esac
  # A build process whose cwd is the worktree, or anywhere under it.
  #
  # Matched as fixed strings via `case`, never as a regex: worktree directory
  # names here routinely contain `+` and `.` (`feat+api-workspaces`), and a path
  # interpolated into a regex would match more than itself. The leading newline
  # makes the first entry testable with the same pattern as the rest.
  case $'\n'"$busy_paths" in
    *$'\n'"$worktree"$'\n'*) return 0 ;;
    *$'\n'"$worktree"/*)     return 0 ;;
  esac
  return 1
}

# --- collect candidates -----------------------------------------------------
names=(target)
[ "$WITH_NODE" -eq 1 ] && names+=(node_modules)

find_args=(-type d "(")
for i in "${!names[@]}"; do
  [ "$i" -gt 0 ] && find_args+=(-o)
  find_args+=(-name "${names[$i]}")
done
find_args+=(")" -prune -print0)

# How to ask one file for its mtime, settled once.
#
# `find -printf` is a GNU extension. BSD find, which is the one on a Mac, does
# not have it and wrote an error to a stderr this script was discarding, so
# `newest_mtime` returned nothing and awk turned that into 0 — an epoch stamp
# older than any cutoff. The effect was that on macOS the idle check was not
# merely wrong, it was off: every build directory read as abandoned no matter
# how recently it had been written, and `--apply` was one keystroke from
# deleting the build somebody was three minutes into. `stat` exists on both, and
# its two dialects differ only in the flag, so the flavour is decided here rather
# than per file.
#
# The probe uses the GNU flag rather than the BSD one, because only that
# direction gives a clean answer. BSD stat has no `-c` and exits non-zero, which
# is a real signal. GNU stat does have `-f`, but it means `--file-system`, so
# `stat -f '%m' .` cheerfully prints a mount point and exits 0 — probing that way
# round would have chosen the BSD dialect on Linux and put a mount point where an
# epoch stamp belongs.
if stat -c '%Y' . >/dev/null 2>&1; then
  STAT_MTIME=(stat -c '%Y')   # GNU
else
  STAT_MTIME=(stat -f '%m')   # BSD
fi

# newest write time inside a tree, as epoch seconds; 0 for an empty one
newest_mtime() {
  find "$1" -type f -exec "${STAT_MTIME[@]}" {} + 2>/dev/null \
    | awk 'BEGIN{m=0} {if ($1+0>m) m=$1+0} END{printf "%d\n", m}'
}

now="$(date +%s)"
idle_cutoff=$(( now - IDLE_MINUTES * 60 ))

# A path the caller has declared off limits. Checked before anything expensive,
# and before any deletion, so an excluded directory is never even measured.
is_skipped() {
  local dir="$1" rel="${1#"$ROOT"/}" pat
  for pat in ${SKIPS[@]+"${SKIPS[@]}"}; do
    [ -n "$pat" ] || continue
    # shellcheck disable=SC2254  # the glob is the point: $pat must not be quoted
    case "$rel" in $pat) return 0 ;; esac
    # shellcheck disable=SC2254
    case "$dir" in $pat) return 0 ;; esac
  done
  return 1
}

candidates=""
while IFS= read -r -d '' dir; do
  is_skipped "$dir" && continue
  # Its worktree is the directory holding the target dir.
  worktree="$(dirname -- "$dir")"
  mt="$(newest_mtime "$dir")"
  [ "$mt" -gt "$idle_cutoff" ] && continue          # written too recently
  is_busy "$dir" "$worktree" && continue            # a compiler is in it
  kb="$(du -sk "$dir" 2>/dev/null | awk '{print $1}')"
  [ -n "$kb" ] && [ "$kb" -gt 0 ] || continue
  candidates="$candidates$mt	$kb	$dir"$'\n'
done < <(find "$ROOT" "${find_args[@]}" 2>/dev/null)

[ -n "${candidates//[$'\n\t ']/}" ] || exit 0        # nothing sweepable, stay quiet

# --- sweep, stopping once there is room -------------------------------------
freed_kb=0
swept=0
report=""

while IFS=$'\t' read -r mt kb dir; do
  [ -n "${dir:-}" ] || continue
  # Re-check pressure each time: enough may already have been freed, and the
  # cheapest deletion is the one not made.
  if [ "$MIN_FREE_GB" -gt 0 ] && [ "$APPLY" -eq 1 ] \
     && [ "$(free_gb_now)" -ge "$MIN_FREE_GB" ]; then
    break
  fi
  age_min=$(( (now - mt) / 60 ))
  if [ "$APPLY" -eq 1 ]; then
    # Last-moment recheck. `du` and the sort above took real time, and a build
    # may have started inside this very directory since the scan.
    if [ "$(newest_mtime "$dir")" -gt "$idle_cutoff" ]; then
      continue
    fi
    # Defence in depth ahead of the only `rm -rf` in this script. The candidate
    # list is already built from `find -name`, so these can only fire if that
    # logic is changed by someone who has not read this far — which is exactly
    # when a guard earns its place. Reclaiming a `target/` costs compile time;
    # removing a checkout destroys unpushed work with no way back, so the two
    # must not be one bug apart.
    # Re-checked here as well as during the scan: an exclusion that holds only on
    # the path into the loop is one refactor away from not holding at all.
    if is_skipped "$dir"; then
      printf 'sweep_targets.sh: refusing to remove %s — excluded by --skip\n' "$dir" >&2
      continue
    fi
    base="$(basename -- "$dir")"
    case "$base" in
      target|node_modules) ;;
      *) printf 'sweep_targets.sh: refusing to remove %s — not a build directory\n' "$dir" >&2
         continue ;;
    esac
    # A git checkout has a `.git` file or directory at its root. A build
    # directory never does, so this distinguishes the two without asking git.
    if [ -e "$dir/.git" ]; then
      printf 'sweep_targets.sh: refusing to remove %s — it is a git checkout\n' "$dir" >&2
      continue
    fi
    rm -rf -- "$dir" || { printf 'sweep_targets.sh: could not remove %s\n' "$dir" >&2; continue; }
  fi
  freed_kb=$(( freed_kb + kb ))
  swept=$(( swept + 1 ))
  if [ "$JSON" -eq 1 ]; then
    report="$report{\"path\":\"$dir\",\"freed_kb\":$kb,\"idle_minutes\":$age_min,\"deleted\":$APPLY}"$'\n'
  else
    report="$report  $(awk -v k="$kb" 'BEGIN{printf "%6.1f GB", k/1024/1024}')  idle ${age_min}m  ${dir#"$ROOT"/}"$'\n'
  fi
# Sorted by staleness, oldest first, so the least-wanted bytes go first and the
# sweep can stop as soon as it has freed enough. Path is the tiebreak, which makes
# the order total and so the run reproducible.
done <<< "$(printf '%s' "$candidates" | sort -t$'\t' -k1,1n -k3,3)"

[ "$swept" -gt 0 ] || exit 0                        # everything got skipped, stay quiet

if [ "$JSON" -eq 1 ]; then
  printf '%s' "$report"
else
  verb="would free"; [ "$APPLY" -eq 1 ] && verb="freed"
  printf '%s %s across %d build director%s (%s free now):\n' \
    "$verb" \
    "$(awk -v k="$freed_kb" 'BEGIN{printf "%.1f GB", k/1024/1024}')" \
    "$swept" \
    "$([ "$swept" -eq 1 ] && echo y || echo ies)" \
    "$(awk -v k="$(free_kb)" 'BEGIN{printf "%.1f GB", k/1024/1024}')"
  printf '%s' "$report"
fi
