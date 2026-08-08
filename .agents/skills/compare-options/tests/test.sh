#!/usr/bin/env bash
#
# Scenario suite for compare-options.
#
# The claims this skill makes about its own rigor are mechanical, so they are
# testable: filters must run before scoring, low-confidence rows must be
# penalised by the Monte Carlo, and a malformed dataset must stop the pipeline.
# Each case builds a throwaway study and asserts one of those properties.
#
# Run: .agents/skills/compare-options/tests/test.sh
set -uo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$TEST_DIR/.." && pwd)"
NEW_STUDY="$SKILL_DIR/scripts/new-study.sh"
VALIDATE="$SKILL_DIR/scripts/validate.py"
SCORE="$SKILL_DIR/scripts/score.py"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0

ok()   { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  FAIL %s\n     %s\n' "$1" "${2:-}"; }

check() { # check <name> <expected-substring> <actual>
  if grep -qF -- "$2" <<<"$3"; then ok "$1"; else bad "$1" "expected to find: $2"; fi
}

check_not() {
  if grep -qF -- "$2" <<<"$3"; then bad "$1" "should NOT contain: $2"; else ok "$1"; fi
}

# fresh <name> — scaffold a study, echo its path
fresh() {
  local d="$WORK/$1"
  "$NEW_STUDY" "$d" >/dev/null 2>&1
  echo "$d"
}

echo "compare-options"

# --- the scaffold is runnable out of the box ------------------------------
echo
echo "scaffold"

S="$(fresh basic)"
out="$(python3 "$VALIDATE" "$S" 2>&1)"
check "template dataset validates" "no errors" "$out"

out="$(python3 "$SCORE" "$S" --profile stated-goal --trials 200 2>&1)"
check "template study scores" "Example A" "$out"

# --- hard filters run before scoring --------------------------------------
echo
echo "filters run before scoring"

out="$(python3 "$SCORE" "$S" --profile stated-goal --trials 200 2>&1)"
check     "excluded flag disqualifies"       "Example C: flagged excluded:unsupported" "$out"
check     "numeric floor disqualifies"       "Example D: capacity=2 below minimum 4"   "$out"
check     "unbuyable row disqualifies"       "Example E: availability='out' is excluded" "$out"
check_not "excluded row absent from ranking" "Example C |" "$out"
check_not "unbuyable row absent from ranking" "Example E |" "$out"

# The point of filtering before scoring: Example C is the CHEAPEST row in the
# template. Under a pure-cost weighting it would rank first if it were scored
# at all, so its absence here is the property under test.
out2="$(python3 "$SCORE" "$S" --profile cheapest --trials 200 2>&1)"
check_not "cheapest row never ranks despite lowest price" "  1  Example C" "$out2"
check     "and is reported as filtered"                   "flagged excluded:unsupported" "$out2"

# Same property for the out-of-stock row, which is the failure this suite exists
# to catch: Example E is the cheapest row that passes every other rule, so under
# a pure-cost weighting it wins outright unless availability gates it first.
check_not "unbuyable row never ranks despite near-lowest price" "  1  Example E" "$out2"

# --- confidence changes the ranking ---------------------------------------
echo
echo "confidence propagation"

# A/B the mechanism directly: same cheaper contender, scored twice, differing
# only in its `confidence`. Holding everything else fixed means any change in
# stability is attributable to confidence alone — no reliance on which of two
# equal rows happened to win.
S2="$(fresh confidence)"

win_pct_at() { # win_pct_at <confidence> -> contender's top-1 %
  cp "$TEST_DIR/fixtures/confidence-dataset.json" "$S2/data/dataset.json"
  python3 - "$S2/data/dataset.json" "$1" <<'PY'
import json, sys
p, conf = sys.argv[1], sys.argv[2]
d = json.load(open(p))
for c in d["candidates"]:
    if c["id"] == "contender":
        c["confidence"] = conf
json.dump(d, open(p, "w"), indent=2)
PY
  python3 "$SCORE" "$S2" --profile cheapest --trials 8000 --top-n 1 \
          --json "$S2/out/s.json" --quiet
  python3 - "$S2/out/s.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
print(next(r["stability"]["top_n_pct"] for r in d["ranking"] if r["id"] == "contender"))
PY
}

hi="$(win_pct_at high)"
lo="$(win_pct_at low)"

# Verified, the cheaper row should hold first place almost always. Unverified,
# its +/-25% error bar must sometimes carry it above the incumbent.
if python3 -c "import sys; sys.exit(0 if float(sys.argv[1]) - float(sys.argv[2]) > 5 else 1)" \
     "$hi" "$lo"; then
  ok "unverified price costs the cheaper row its lead (${hi}% -> ${lo}% top-1)"
else
  bad "unverified price costs the cheaper row its lead" \
      "high=${hi} low=${lo} — expected a drop of more than 5 points"
fi

# --- validation actually gates --------------------------------------------
echo
echo "validation gates the pipeline"

S3="$(fresh invalid)"
python3 - "$S3/data/dataset.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["candidates"][0]["reliability"] = 9      # outside the 1-5 scale
d["candidates"][1]["id"] = d["candidates"][0]["id"]  # duplicate id
json.dump(d, open(p, "w"), indent=2)
PY

out="$(python3 "$VALIDATE" "$S3" 2>&1)"; rc=$?
check "out-of-range rating is an error" "outside the 1-5 scale" "$out"
check "duplicate id is an error"        "duplicate id"          "$out"
[[ $rc -ne 0 ]] && ok "validator exits non-zero" || bad "validator exits non-zero" "got rc=$rc"

S4="$(fresh nospec)"
python3 - "$S4/data/dataset.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["_meta"].pop("reference_spec", None)
json.dump(d, open(p, "w"), indent=2)
PY
out="$(python3 "$VALIDATE" "$S4" 2>&1)"
check "missing reference spec warns" "reference_spec is missing" "$out"

# --- profiles genuinely differ --------------------------------------------
echo
echo "profiles"

out="$(python3 "$SCORE" "$S" --all-profiles --trials 200 2>&1)"
check "all profiles run" "Reliability above all" "$out"
check "cost profile runs"  "Pure cost"            "$out"

# --- scaffold safety -------------------------------------------------------
echo
echo "scaffold safety"

out="$("$NEW_STUDY" "$S" 2>&1)"; rc=$?
check "refuses to overwrite a populated study" "refusing to scaffold" "$out"
[[ $rc -ne 0 ]] && ok "overwrite attempt exits non-zero" || bad "overwrite exits non-zero" "rc=$rc"

echo
echo "-----------------------------------------"
printf '%d passed, %d failed\n' "$PASS" "$FAIL"
[[ $FAIL -eq 0 ]] || exit 1
