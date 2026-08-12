#!/usr/bin/env bash
#
# reclaim-disk.test.sh — proves the sweep deletes the abandoned build directory
# and refuses the three that are still wanted.
#
# The reason this test exists is that the script's failure mode is silent and
# expensive in the other direction. A sweep that is too timid wastes disk, which
# announces itself. A sweep that is too eager deletes the `target/` an agent is
# three minutes into, and what that agent reports is a build failure with no
# apparent cause — so the bug lands on somebody else's task, looking like theirs.
#
# Each guard is asserted separately, because they fail independently: the mtime
# check and the process check catch different directories, and a script that
# happened to skip everything would satisfy a single "did it spare the live one"
# assertion while being useless.
#
# Deliberately offline and hermetic: every path is inside a temp fixture, so a
# bug in the script under test cannot reach the real repository. Nothing here
# runs cargo.
# Run: tests/reclaim-disk.test.sh
set -u

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$REPO_ROOT/.agents/skills/test-scenarios/scripts/assert.sh"

SWEEP="$REPO_ROOT/.agents/skills/reclaim-disk/scripts/sweep_targets.sh"
ARM="$REPO_ROOT/.agents/skills/reclaim-disk/scripts/arm_schedule.sh"

FIX="$(mktemp -d)"

# The fixture starts a real background process, so cleanup has to reach it from
# every exit path — including the early exit an assertion failure takes. Killing
# only the parent would leave its `sleep` child alive holding this script's
# stdout open, and a caller reading through a pipe would then block until the
# sleep expired rather than seeing the results.
BUSY_PID=""
cleanup() {
  if [ -n "$BUSY_PID" ]; then
    pkill -P "$BUSY_PID" 2>/dev/null
    kill "$BUSY_PID" 2>/dev/null
  fi
  rm -rf "$FIX"
}
trap cleanup EXIT

# --- fixture ----------------------------------------------------------------
# A stand-in repo with four worktrees. Sizes are padded so `du` reports
# something, and mtimes are set explicitly rather than by waiting.
mk_target() { # mk_target <worktree> <minutes-old>
  local wt="$FIX/$1" age="$2"
  mkdir -p "$wt/target/debug/deps" "$wt/src"
  dd if=/dev/zero of="$wt/target/debug/deps/blob.rlib" bs=1024 count=64 \
     status=none 2>/dev/null
  printf 'fn main() {}\n' > "$wt/src/main.rs"
  # -d accepts a relative offset, so no date arithmetic is needed here.
  find "$wt/target" -exec touch -d "-${age} minutes" {} +
}

mk_target stale 600        # abandoned: ten hours untouched
mk_target fresh 5          # an agent paused mid-task
mk_target busy 600         # old on disk, but a compiler is inside it
mk_target alsostale 900    # older still, so ordering is observable

section "report-only is the default"

out="$($SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 2>&1)"
assert_dir "$FIX/stale/target"      # named it, did not remove it
printf '%s' "$out" | grep -q 'would free' \
  && pass "says 'would free', not 'freed'" \
  || fail "expected a would-free report, got: $out"
printf '%s' "$out" | grep -q 'stale/target' \
  && pass "nominates the abandoned target" \
  || fail "did not nominate stale/target: $out"

section "a recently-written target is never a candidate"

printf '%s' "$out" | grep -q 'fresh/target' \
  && fail "nominated a target written 5 minutes ago" \
  || pass "spares the target written 5 minutes ago"

section "pressure gate: plenty of space means do nothing, silently"

# --min-free-gb 99999 forces "there is pressure"; 0 here forces the opposite by
# asking for no free space at all, which is always already satisfied.
out_quiet="$($SWEEP --root "$FIX" --min-free-gb 1 --idle-minutes 90 2>&1)"
if [ "$(df -Pk "$FIX" | awk 'NR==2 {print $4}')" -ge $((1*1024*1024)) ]; then
  assert_eq "" "$out_quiet" "prints nothing when free space is above the threshold"
else
  pass "skipped: this filesystem has under 1 GB free, so the gate cannot be tested"
fi

section "a busy target is spared even when it is old"

# A real process whose cwd is inside the busy worktree — the exact condition the
# script scans /proc for.
#
# It has to be a script rather than a symlink to `sleep`: coreutils ships as one
# multi-call binary that dispatches on argv[0] and refuses to run under a name it
# does not recognise ("unknown program 'cargo'"). And a symlink would not help
# anyway — /proc/pid/exe resolves to the real inode, so the link name never shows
# up there. A script named `cargo` is caught by the command-line half of the
# check, which is the half that catches real wrappers and shims too.
mkdir -p "$FIX/fakebin"
# Deliberately not `exec sleep` — exec would replace this process, and with it
# the only occurrence of the name `cargo`, leaving a bare `sleep` that nothing
# should match. Sleeping as a child keeps `.../cargo` on this process's command
# line, which is what the check reads.
cat > "$FIX/fakebin/cargo" <<'FAKE'
#!/usr/bin/env bash
sleep "$1"
FAKE
chmod +x "$FIX/fakebin/cargo"
# Redirected away from this script's own stdout, so the fixture can never hold
# the results pipe open. 60s is long enough for the assertions and short enough
# that a leaked one expires on its own.
( cd "$FIX/busy" && exec "$FIX/fakebin/cargo" 60 ) >/dev/null 2>&1 </dev/null &
BUSY_PID=$!
busy_pid="$BUSY_PID"
# Wait for the process to actually be visible in /proc with the right cwd,
# rather than assuming it scheduled instantly.
for _ in $(seq 1 50); do
  [ "$(readlink "/proc/$busy_pid/cwd" 2>/dev/null)" = "$FIX/busy" ] && break
  sleep 0.1
done
assert_eq "$FIX/busy" "$(readlink "/proc/$busy_pid/cwd" 2>/dev/null)" \
  "the fixture process really is rooted in busy/"

out_busy="$($SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 2>&1)"
printf '%s' "$out_busy" | grep -q 'busy/target' \
  && fail "nominated a target with a cargo process inside it: $out_busy" \
  || pass "spares the target a compiler is working in"

section "--apply deletes exactly the abandoned ones"

$SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 --apply >/dev/null 2>&1
assert_missing "$FIX/stale/target"      "deleted the abandoned target"
assert_missing "$FIX/alsostale/target"  "deleted the older abandoned target"
assert_dir     "$FIX/fresh/target"      "kept the recently-written target"
assert_dir     "$FIX/busy/target"       "kept the busy target"

section "source is never touched"

assert_file "$FIX/stale/src/main.rs"  "left source in the swept worktree alone"
assert_file "$FIX/fresh/src/main.rs"  "left source in the spared worktree alone"

section "a second sweep with nothing to do is silent"

# The busy process is still running on purpose. What is left is one target too
# recent to touch and one a compiler is inside, so a correct sweep has nothing to
# say — and says nothing, which is what keeps the hourly monitor quiet.
out_again="$($SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 2>&1)"
assert_eq "" "$out_again" "prints nothing once the abandoned targets are gone"

section "node_modules is opt-in"

mkdir -p "$FIX/nodewt/node_modules/pkg"
dd if=/dev/zero of="$FIX/nodewt/node_modules/pkg/blob" bs=1024 count=64 status=none 2>/dev/null
find "$FIX/nodewt/node_modules" -exec touch -d '-600 minutes' {} +

out_default="$($SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 2>&1)"
printf '%s' "$out_default" | grep -q 'node_modules' \
  && fail "considered node_modules without --with-node" \
  || pass "ignores node_modules by default"

out_node="$($SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 --with-node 2>&1)"
printf '%s' "$out_node" | grep -q 'node_modules' \
  && pass "considers node_modules with --with-node" \
  || fail "--with-node did not nominate it: $out_node"

section "the busy target becomes sweepable once the compiler exits"

# Proves the process check was the only thing sparing busy/target — not its age,
# not its path, not some incidental skip. Without this, a script that silently
# refused to consider that directory at all would pass every assertion above.
pkill -P "$BUSY_PID" 2>/dev/null
kill "$BUSY_PID" 2>/dev/null
wait "$BUSY_PID" 2>/dev/null
BUSY_PID=""
# Wait for /proc to actually drop it, rather than assuming the kill was synchronous.
for _ in $(seq 1 50); do
  pgrep -f "$FIX/fakebin/cargo" >/dev/null 2>&1 || break
  sleep 0.1
done

out_released="$($SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 2>&1)"
printf '%s' "$out_released" | grep -q 'busy/target' \
  && pass "nominates the formerly-busy target once its compiler is gone" \
  || fail "still spares busy/target after the process exited: $out_released"

section "--skip puts a path permanently out of reach"

# The concrete case this exists for: apps/ios/node_modules is the iOS session's
# only build artifact, and reinstalling it needs the network — which is exactly
# what is unavailable when the disk is full, the one moment it would be asked to.
# So it must be excludable by path, not merely off by default.
mkdir -p "$FIX/apps/ios/node_modules/pkg"
dd if=/dev/zero of="$FIX/apps/ios/node_modules/pkg/blob" bs=1024 count=64 status=none 2>/dev/null
find "$FIX/apps/ios/node_modules" -exec touch -d '-600 minutes' {} +

out_skip="$($SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 --with-node \
             --skip 'apps/ios/*' 2>&1)"
printf '%s' "$out_skip" | grep -q 'apps/ios' \
  && fail "nominated an excluded path: $out_skip" \
  || pass "excludes apps/ios/node_modules even with --with-node"
# And the exclusion has to survive an actual --apply, not just the report.
$SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 --with-node \
       --skip 'apps/ios/*' --apply >/dev/null 2>&1
assert_dir "$FIX/apps/ios/node_modules/pkg" "--apply left the excluded path alone"
# Without the exclusion the same directory *is* a candidate — otherwise this
# test would pass against a script that simply never considered it.
out_noskip="$($SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 --with-node 2>&1)"
printf '%s' "$out_noskip" | grep -q 'apps/ios' \
  && pass "the same path is a candidate without --skip, so the test bites" \
  || fail "apps/ios was never a candidate; --skip proves nothing: $out_noskip"

section "arming refuses a probe path that dies with the worktree"

# A monitor whose command has vanished fails silently every hour, which is worse
# than never arming it. Which branch runs depends on where this test is run from,
# and both are asserted rather than one being skipped — the guard matters most
# from a worktree, which is where this suite usually runs.
COMMON="$(git -C "$REPO_ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"
MAIN_CHECKOUT="$(dirname -- "$COMMON")"
DURABLE="$MAIN_CHECKOUT/.agents/skills/reclaim-disk/scripts/sweep_targets.sh"

if command -v jod >/dev/null 2>&1; then
  arm_raw="$($ARM --dry-run 2>&1)"; arm_rc=$?
  if [ -x "$DURABLE" ]; then
    assert_eq "0" "$arm_rc" "arming succeeds once the durable copy exists"
    printf '%s' "$arm_raw" | grep -qF -- "$DURABLE" \
      && pass "the probe points at the shared checkout, not at a worktree" \
      || fail "the probe does not use the durable path: $arm_raw"
  else
    assert_eq "1" "$arm_rc" "refuses to arm while only the worktree copy exists"
    printf '%s' "$arm_raw" | grep -q 'refusing to arm a probe inside a worktree' \
      && pass "explains that the path would die with the worktree" \
      || fail "refused without explaining why: $arm_raw"
  fi
else
  pass "skipped: jod is not on PATH"
fi

section "the scheduled sweep cannot reach node_modules at all"

# Belt and braces on top of --skip: the probe never passes --with-node, so no
# hourly run can take a node_modules however it is configured. Read from the
# script's own text, so it holds regardless of which branch above ran.
grep -q 'PROBE=.*--with-node' "$ARM" \
  && fail "arm_schedule.sh builds a probe with --with-node" \
  || pass "the armed probe omits --with-node, so node_modules is unreachable hourly"

section "a checkout is never removable, even named like a build directory"

# The expensive asymmetry, asserted directly: a deleted `target/` costs compile
# time, a deleted checkout costs unpushed work permanently. A worktree that
# happened to be named `target` — or any future edit that widened the candidate
# search — must still be refused at the last moment before `rm -rf`.
mkdir -p "$FIX/trap/target"
printf 'gitdir: /nowhere\n' > "$FIX/trap/target/.git"
dd if=/dev/zero of="$FIX/trap/target/blob" bs=1024 count=64 status=none 2>/dev/null
printf 'precious unpushed work\n' > "$FIX/trap/target/WORK.md"
find "$FIX/trap/target" -exec touch -d '-600 minutes' {} +

out_trap="$($SWEEP --root "$FIX" --min-free-gb 0 --idle-minutes 90 --apply 2>&1)"
assert_dir  "$FIX/trap/target"          "refused to remove a directory containing .git"
assert_file "$FIX/trap/target/WORK.md"  "the unpushed work is still there"
printf '%s' "$out_trap" | grep -q 'refusing to remove' \
  && pass "says out loud that it refused, rather than skipping silently" \
  || fail "refused without explaining why: $out_trap"

section "bad input is refused, not guessed at"

assert_fails "$SWEEP" --root "$FIX" --min-free-gb notanumber
assert_fails "$SWEEP" --root "$FIX" --idle-minutes -5
assert_fails "$SWEEP" --root "$FIX" --nonsense-flag
assert_fails "$SWEEP" --root "$FIX/does-not-exist"

section "both scripts are valid bash and shellcheck-clean"

assert_ok bash -n "$SWEEP"
assert_ok bash -n "$ARM"
if command -v shellcheck >/dev/null 2>&1; then
  assert_ok shellcheck -S warning "$SWEEP"
  assert_ok shellcheck -S warning "$ARM"
else
  pass "skipped: shellcheck is not installed"
fi

section "the schedule wiring is deterministic and inspectable"

# --dry-run must never touch jod's state, so this is safe on a live box.
if command -v jod >/dev/null 2>&1; then
  # Compared before and against after, rather than against "no schedules": once
  # the sweep is armed for real this box *does* have one, and an assertion that
  # assumed an empty list would start failing for the wrong reason.
  before="$(jod schedule ls 2>/dev/null)"
  arm_out="$($ARM --dry-run 2>&1)"
  after="$(jod schedule ls 2>/dev/null)"
  assert_eq "$before" "$after" "--dry-run left jod's schedule list untouched"
else
  pass "skipped: jod is not on PATH"
fi

# Asserted from the script's text rather than from a dry run, so these hold even
# where arming correctly refuses (a worktree, or a box without jod). The two
# properties are the whole design: the script is the job, and no model is woken.
grep -q -- '--no-agent' "$ARM" \
  && pass "attaches the monitor with --no-agent, so no model is ever woken" \
  || fail "arm_schedule.sh does not pass --no-agent"
grep -q 'PROBE=.*sweep_targets.sh.*--apply\|PROBE="\$SWEEP --apply' "$ARM" \
  && pass "the probe is the sweep itself, running with --apply" \
  || fail "the probe does not run the sweep with --apply"

assert_summary
