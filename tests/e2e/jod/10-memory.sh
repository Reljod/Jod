#!/usr/bin/env bash
# Memory: remember / recall / forget / related / path, the trust boundary, and
# scope partitioning. Every assertion is made against the shipped binary and,
# where the claim is about storage rather than output, against the rows.
set -uo pipefail
AREA=memory
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

section "1. remember and recall round-trip"
run jod remember reljod uses jod
run jod remember reljod prefers "linear for tasks"
run jod recall reljod
run jod recall linear

section "2. recall --json shape"
run jod recall reljod --json

section "3. versioning: the same subject+predicate asserted twice"
run jod remember reljod uses "jod on a vps"
echo "-- recall should show the current value --"
run jod recall "reljod uses"
echo "-- but both versions should still be on disk --"
runsh "q \"SELECT id, subject, predicate, object, origin, state FROM facts WHERE subject='reljod' AND predicate='uses' ORDER BY id\""

section "4. THE TRUST BOUNDARY: an untrusted fact must not answer"
run jod remember zenith ships "a quantum blockchain" --origin untrusted --source "https://spam.example/page"
echo "-- the fact is stored --"
runsh "q \"SELECT id, subject, predicate, object, origin FROM facts WHERE subject='zenith'\""
echo "-- ...but recall must not surface it --"
run jod recall zenith
run jod recall quantum
run jod recall "a quantum blockchain"

section "4b. the other three origins SHOULD answer"
run jod remember alpha statedby agent-origin --origin agent
run jod remember beta statedby system-origin --origin system
run jod remember gamma statedby owner-origin --origin owner
run jod recall agent-origin
run jod recall system-origin
run jod recall owner-origin

section "5. scope partitioning"
run jod remember budget is "50 eur" --scope finance
run jod remember budget is "10 story points" --scope work
echo "-- restricted to one scope --"
run jod recall budget --scope finance
run jod recall budget --scope work
echo "-- --scope with a scope that has no such fact --"
run jod recall budget --scope health
echo "-- omitting --scope: help says it searches every scope --"
run jod recall budget

section "6. forget destroys every version, and tombstones the deletion"
echo "-- before --"
runsh "q \"SELECT count(*) AS versions FROM facts WHERE subject='reljod' AND predicate='uses'\""
run jod forget reljod uses
echo "-- after: zero rows of any version --"
runsh "q \"SELECT id, object, state FROM facts WHERE subject='reljod' AND predicate='uses'\""
echo "-- the FTS index must not keep a ghost --"
run jod recall "jod on a vps"
echo "-- a tombstone records that it happened, and how many versions died --"
runsh "q \"SELECT scope, subject, predicate, versions FROM tombstones\""

section "7. forget on something that was never there"
run jod forget nosuch thing

section "8. the graph: three facts written one at a time"
run jod remember jod runs-on jod-cloud
run jod remember jod-cloud hosted-by hetzner
run jod remember hetzner located-in falkenstein
echo "-- entities and relations are derived --"
runsh "q \"SELECT id, scope, kind, name FROM entities ORDER BY id\""
runsh "q \"SELECT r.id, se.name AS src, r.predicate, de.name AS dst, r.fact_id FROM relations r JOIN entities se ON se.id=r.src JOIN entities de ON de.id=r.dst ORDER BY r.id\""

section "9. jod related — walking out from a node"
run jod related jod -n 1
run jod related jod -n 2
run jod related jod -n 3
run jod related jod -n 3 --json

section "9b. the documented hop cap"
run jod related jod -n 4
run jod related jod -n 99
run jod related jod -n 0

section "10. jod path"
run jod path jod falkenstein
run jod path jod hetzner
run jod path falkenstein jod
echo "-- two things that are not connected --"
run jod path jod gamma
echo "-- something that does not exist at all --"
run jod path jod nonexistent-thing

section "11. untrusted must be excluded from graph SEEDING too"
run jod remember jod-cloud compromised-by "evil corp" --origin untrusted
echo "-- the edge exists in the derived index? --"
runsh "q \"SELECT r.id, se.name AS src, r.predicate, de.name AS dst, f.origin FROM relations r JOIN entities se ON se.id=r.src JOIN entities de ON de.id=r.dst JOIN facts f ON f.id=r.fact_id WHERE f.origin='untrusted'\""
echo "-- can an untrusted name be used as a starting point? --"
run jod related "evil corp" -n 2
echo "-- does an untrusted edge get traversed from a trusted seed? --"
run jod related jod -n 2

section "12. forget cascades to the graph"
echo "-- before: edges touching hetzner --"
runsh "q \"SELECT count(*) AS edges FROM relations WHERE fact_id IN (SELECT id FROM facts WHERE subject='hetzner' AND predicate='located-in')\""
run jod forget hetzner located-in
echo "-- after --"
runsh "q \"SELECT count(*) AS edges FROM relations WHERE fact_id IN (SELECT id FROM facts WHERE subject='hetzner' AND predicate='located-in')\""
runsh "q \"SELECT count(*) AS orphan_edges FROM relations WHERE fact_id NOT IN (SELECT id FROM facts)\""
run jod path jod falkenstein
run jod related jod -n 3

section "13. graph is scope-partitioned too"
run jod remember jod runs-on "a secret box" --scope private
run jod related jod -n 2 --scope private
run jod related jod -n 2 --scope default

section "14. hostile input: quoting, unicode, very long values"
run jod remember "sub'ject" "pred\"icate" "obj; DROP TABLE facts;--"
run jod recall "DROP TABLE"
runsh "q \"SELECT count(*) AS facts_table_still_here FROM facts\""
run jod remember emoji means "🎉 café 日本語"
run jod recall café
run jod recall 日本語

section "15. recall limit"
run jod recall reljod -l 1
run jod recall "" -l 3
run jod recall

section "final state"
runsh "q \"SELECT id, scope, subject, predicate, object, origin, state FROM facts ORDER BY id\""
