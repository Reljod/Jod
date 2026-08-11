#!/usr/bin/env bash
# Follow-ups the first memory pass could not settle: the hop cap needs a graph
# deeper than the cap, the trust claim needs an isolated repro, and
# re-asserting a fact needs checking against the columns that exist for it.
set -uo pipefail
AREA=memlimits
. "$(dirname "$0")/env.sh"

rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
DB="$JOD_HOME/jod.db"
q() { python3 "$REPO/tests/e2e/jod/db.py" "$DB" "$1"; }

section "A. a chain of 7, to find where `related` really stops"
run jod remember n0 links n1
run jod remember n1 links n2
run jod remember n2 links n3
run jod remember n3 links n4
run jod remember n4 links n5
run jod remember n5 links n6
for h in 1 2 3 4 5 6 7; do
  echo "--- related n0 -n $h ---"
  run jod related n0 -n "$h"
done

section "B. `path` over the same chain, past the default and past the flag"
run jod path n0 n3
run jod path n0 n5
run jod path n0 n6
run jod path n0 n6 -n 6
run jod path n0 n6 -n 10
run jod path n0 n6 -n 99

section "C. is `path` directed? `related` is documented undirected"
run jod path n6 n0
run jod path n6 n0 -n 10
run jod related n6 -n 3

section "D. TRUST: isolated repro — one untrusted fact, one graph walk"
rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
run jod remember reljod owns jod-cloud
echo "-- an ingested page asserts something. It is marked untrusted. --"
run jod remember jod-cloud "is-controlled-by" "attacker.example" --origin untrusted --source "https://evil.example/page"
echo "-- recall correctly refuses it: --"
run jod recall attacker.example
run jod recall jod-cloud
echo "-- but the graph walks straight into it: --"
run jod related reljod -n 2
run jod related reljod -n 2 --json
echo "-- and it is usable as a starting point: --"
run jod related attacker.example -n 2
echo "-- and it appears on a path: --"
run jod path reljod attacker.example

section "E. re-asserting a fact: does the new value supersede the old?"
rm -rf "$JOD_HOME"; mkdir -p "$JOD_HOME"
run jod remember server ip "10.0.0.1"
run jod remember server ip "10.0.0.2"
run jod remember server ip "10.0.0.3"
echo "-- what does Jod answer when asked for the server's ip? --"
run jod recall "server ip"
echo "-- the schema has state/invalidated_by/valid_to for exactly this: --"
runsh "q \"SELECT id, object, state, invalidated_by, valid_from, valid_to FROM facts WHERE subject='server'\""
echo "-- and the graph now has three live edges for one attribute: --"
runsh "q \"SELECT r.id, se.name, r.predicate, de.name, r.valid_to_ms FROM relations r JOIN entities se ON se.id=r.src JOIN entities de ON de.id=r.dst\""
run jod related server -n 1

section "F. does forget leave orphan entities behind?"
run jod forget server ip
runsh "q \"SELECT count(*) AS facts FROM facts\""
runsh "q \"SELECT count(*) AS relations FROM relations\""
runsh "q \"SELECT id, name FROM entities ORDER BY id\""
echo "-- an entity with no facts and no edges is still addressable: --"
run jod related "10.0.0.2" -n 2
run jod path "10.0.0.1" "10.0.0.2"

section "G. scope names that do not exist, and odd scopes"
run jod remember a b c --scope "with space"
run jod recall c --scope "with space"
run jod related a -n 1 --scope "with space"
run jod recall c --scope ""
run jod remember d e f --scope ""
runsh "q \"SELECT id, scope, subject FROM facts\""
