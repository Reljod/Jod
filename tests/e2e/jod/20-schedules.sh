#!/usr/bin/env bash
# Schedules: arming, listing, timezone arithmetic, pause/resume, log, rm, and
# the policy flags. Nothing here spawns a harness — 25-daemon-real.sh does that
# separately so a spawn failure cannot be confused with a scheduling failure.
set -uo pipefail
AREA=schedules
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

section "1. arm one, and read it back"
run jod schedule add nightly "summarise the day" --cron "0 2 * * *"
run jod schedule ls
run jod schedule ls --json

section "2. TIMEZONE: 02:00 Asia/Manila must be 18:00 UTC, not 02:00 UTC"
run jod schedule add manila "morning brief" --cron "0 2 * * *" -z Asia/Manila
run jod schedule add newyork "morning brief" --cron "0 2 * * *" -z America/New_York
run jod schedule ls
echo "-- the stored instant, rendered in UTC and in its own zone --"
runsh "python3 - <<'PY'
import sqlite3, datetime, zoneinfo
con = sqlite3.connect('file:$DB?mode=ro', uri=True)
for name, tz, ms in con.execute(
    'SELECT name, timezone, next_fire_at_ms FROM schedules ORDER BY name'):
    u = datetime.datetime.fromtimestamp(ms/1000, datetime.timezone.utc)
    l = u.astimezone(zoneinfo.ZoneInfo(tz))
    print(f'{name:10} tz={tz:18} utc={u:%Y-%m-%d %H:%M} local={l:%Y-%m-%d %H:%M %Z}')
PY"

section "3. a bad zone and a bad cron must be refused at the boundary"
run jod schedule add badzone "x" --cron "0 2 * * *" -z "+08:00"
run jod schedule add badzone2 "x" --cron "0 2 * * *" -z "Mars/Olympus"
run jod schedule add badcron "x" --cron "not a cron"
run jod schedule add badcron2 "x" --cron "99 99 99 99 99"
run jod schedule add badcron3 "x" --cron ""
echo "-- none of those should have been stored --"
run jod schedule ls

section "4. cron dialects the help advertises"
run jod schedule add daily-at "x" --cron "@daily"
run jod schedule add every15 "x" --cron "*/15 * * * *"
run jod schedule add withsecs "x" --cron "0 0 2 * * *"
run jod schedule add weekdays "x" --cron "0 9 * * 1-5"
run jod schedule ls

section "5. duplicate name"
run jod schedule add nightly "a different prompt" --cron "0 3 * * *"
run jod schedule ls --json

section "6. pause must make run refuse, not fire silently"
run jod schedule pause nightly
run jod schedule ls
run jod schedule run nightly
echo "-- and the daemon must not claim it either --"
run jod daemon --once
runsh "q \"SELECT name, state FROM schedules ORDER BY name\""

section "7. resume re-arms it"
run jod schedule resume nightly
run jod schedule ls
runsh "q \"SELECT name, state, consecutive_failures FROM schedules WHERE name='nightly'\""

section "8. operations on a schedule that does not exist"
run jod schedule pause ghost
run jod schedule resume ghost
run jod schedule run ghost
run jod schedule rm ghost
run jod schedule log ghost

section "9. log of a schedule that has never fired"
run jod schedule log nightly

section "10. policy flags"
run jod schedule add pol1 "x" --cron "@daily" --misfire fire_once
run jod schedule add pol2 "x" --cron "@daily" --misfire skip
run jod schedule add pol3 "x" --cron "@daily" --misfire fire_all
run jod schedule add pol4 "x" --cron "@daily" --overlap skip
run jod schedule add pol5 "x" --cron "@daily" --overlap queue
run jod schedule add pol6 "x" --cron "@daily" --overlap kill
run jod schedule add polbad "x" --cron "@daily" --misfire nonsense
run jod schedule add polbad2 "x" --cron "@daily" --overlap nonsense
runsh "q \"SELECT name, misfire, overlap FROM schedules WHERE name LIKE 'pol%' ORDER BY name\""

section "11. harness selection"
run jod schedule add hc "x" --cron "@daily" -H claude
run jod schedule add ho "x" --cron "@daily" -H opencode
run jod schedule add ha "x" --cron "@daily" -H agy
run jod schedule add hbad "x" --cron "@daily" -H nosuch
runsh "q \"SELECT name, harness FROM schedules WHERE name LIKE 'h%' ORDER BY name\""

section "12. rm forgets the schedule and its history"
runsh "q \"SELECT count(*) AS before FROM schedules\""
run jod schedule rm pol1
run jod schedule rm pol2
runsh "q \"SELECT count(*) AS after FROM schedules\""
run jod schedule ls

section "13. the full stored row, for the record"
runsh "q \"SELECT id, name, cron, timezone, harness, state, misfire, overlap, consecutive_failures, next_fire_at_ms FROM schedules ORDER BY id\""
