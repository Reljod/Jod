---
name: compare-options
description: >
  Use when the user asks to compare, evaluate, or pick the best among many
  real-world options — vendors, hosts, providers, libraries, models, plans,
  tools, APIs. Triggers on "compare X", "which X should I use", "research
  the top N", "find me the best X", "evaluate these options", "cheapest X
  that does Y". Builds a scripted, reproducible comparison: a fixed
  reference spec, a dataset with per-row confidence, hard filters before
  scoring, weighted profiles, and a Monte Carlo pass that propagates data
  uncertainty into the ranking — then a report that separates the numbers
  from the judgement.
---

# compare-options

A comparison is easy to make and easy to fake. Ten providers, five columns,
a bolded winner — and no way for the reader to tell whether the ranking
came from evidence or from the order you happened to research things in.

This skill produces the version that survives someone checking it. Four
properties do the work; the rest is presentation:

1. **A fixed reference spec** so the rows are comparable at all.
2. **Hard filters before scoring** so a disqualifying flaw can't be averaged away.
3. **Per-row confidence, propagated** so an unverified bargain can't win on a number you didn't check.
4. **Weights as data** so "best" is a stated position, not a hidden one.
5. **Proof each option can actually be obtained**, because the cheapest row is
   very often the one nobody can buy.

## 1. Fix the reference spec before collecting anything

The single most common way a comparison lies is by pricing different
things. Provider A's $5 plan and Provider B's $5 plan are not comparable
unless you *defined* what you were pricing first.

Write the spec down as one sentence, in the dataset's `_meta`, before the
first row:

> *cheapest plan with ≥2 vCPU, ≥4 GB RAM, ≥40 GB SSD, KVM-class virt*

Then price every candidate at that spec, even when it's awkward. When a
candidate's headline plan doesn't meet it, you use the one that does and
say so — that is a finding, not an inconvenience. A real example: a host's
famous $15 plan turned out to have one CPU core, so its qualifying plan was
$30, and it fell eight places. Fixing the spec first is what surfaced that.

**The spec disciplines your columns but not your sources, and that gap is
where comparisons go wrong.** The failure looks like this: you record the
spec-meeting plan's *specs*, then fill in the price from whatever number
the page showed most prominently — which belonged to a smaller plan. Every
downstream check passes, because the recorded specs are correct. Only the
price came from somewhere else. In one audited study this had happened to
three of six rows checked, understating prices by up to 6.8×, and no
validator could have caught it. When you write a price, write down which
plan name it came from in the same edit.

## 2. Build the dataset flat, with provenance per row

One flat record per candidate — flat maps to CSV, and a typo in a nested
blob is much harder to spot in review. Every row carries:

- `confidence`: `high` (confirmed on the vendor's own live page) ·
  `medium` (reputable secondary source) · `low` (plausible figure, unconfirmed)
- `availability`: `in` · `partial` · `out` · `unknown` — can you buy it *today*
- `price_basis`: `standing` (published catalogue) · `promo` (time-limited SKU) ·
  `advertised-headline` (a "starting at" banner, not a plan) · `unknown`
- `sources`: the URLs
- `flags`: disqualifiers and hazards (`excluded:<reason>`, `sanctions-risk`,
  `renewal-hike`, `stock-limited`)
- `notes`: what a score can't express

**`confidence` is not decoration** — §4 makes it change the ranking. Set it
honestly, especially on rows you want to win.

**`availability` and `price_basis` are the two fields people skip, and they
are where the embarrassing errors live.** Confidence answers "did I read
this somewhere reliable"; it does not answer "is this purchasable" or "will
this price exist next week". A promo price is not a lie, but it sells out,
expires, and often carries different specs from the standing plan of the
same name — so rank on `standing` and put the promo in `notes`. Where the
two diverge sharply, the gap *is* a finding: one host in an audited study
was $3.62/mo on a promo and $24.59/mo standing, with no annual discount at
all.

Rate subjective dimensions 1-5 and say in the schema which end is good.
Document every field in a `schema.md`; a rating whose direction is
ambiguous will be inverted by someone eventually.

## 3. Filter before you score

A weighted average will happily let a great price paper over a fatal flaw.
So anything that makes a candidate *unusable* is a filter, not a low
weight:

```json
"filters": {
  "min_ram_gb": 4,
  "require_virt": ["KVM", "bare-metal"],
  "exclude_availability": ["out"],
  "exclude_flags": ["excluded:", "sanctions-risk"]
}
```

`exclude_availability` belongs here rather than in the scoring, for the
same reason as everything else in this section: **an option you cannot
acquire is not a cheap option, it is not an option.** No weighting should
be able to average that away. The scaffold ships an Example E that is
nearly the cheapest row, passes every other rule, and is out of stock —
and a test asserting it never ranks.

Let `unknown` through. Most rows in a wide comparison will never have their
order path checked, and silently dropping them distorts the ranking far
more than admitting the gap does. Report the count instead.

Report what got filtered and why. The disqualified list is often the most
useful table in the report, because those are exactly the options a naive
comparison would have ranked highly.

## 4. Score, then attack your own ranking

Weighted sub-scores on 0-100 are the easy part. The part that makes the
result trustworthy is the Monte Carlo:

- perturb the **weights** ±35% (your weighting is a guess)
- perturb the **uncertain field** — usually price — by each row's
  confidence-scaled error bar: ±25% for `low`, ±4% for `high`
- re-rank, thousands of times, and report how often each candidate held a
  top-5 slot

This converts "it scored highest" into "it stayed on top in 100% of 20,000
perturbed trials", which is a different and much stronger claim. A
candidate that only wins under one exact weighting is an artifact of your
weighting, and this is how you find that out before the reader does.

Run the whole thing:

```bash
${CLAUDE_SKILL_DIR}/scripts/new-study.sh <dir>      # scaffold
${CLAUDE_SKILL_DIR}/scripts/validate.py <dir>       # dataset integrity
${CLAUDE_SKILL_DIR}/scripts/score.py <dir> --profile <name>
```

`validate.py` is the runnable check: it fails on out-of-range ratings,
unknown currencies, duplicate ids, and missing sources. A ranking built on
a dataset that failed validation is worse than no ranking.

Edit `criteria.py` in the study directory to define the domain's
sub-scores. Everything else — filters, normalization, Monte Carlo, CSV and
JSON output — is already in `comparelib.py`.

## 5. Define at least three profiles

One weighting produces one winner and hides the trade. Always run:

- **the user's stated goal**, weighted in the order they stated it
- **one single-axis extreme** per major criterion (cheapest, most X)
- **the opposite of their stated goal**, if it's plausible they're wrong

Then put the winners side by side. Where a candidate tops several columns,
the recommendation is robust. Where every column disagrees, say plainly
that there is no universal best — that *is* the finding, and burying it
under one bolded winner is the dishonest move.

## 6. Verify the leaders by opening the order path, not the marketing page

Research the field broadly, then **verify the top candidates against live
vendor pages**. Do not verify everything; do not verify nothing.

Budget the verification for the leaders, because that's where being wrong
is expensive. Then report what verification changed. In practice it always
changes something, and a report that says "verification moved four rows"
earns more trust than one claiming everything was right first time.

**Verify at the point of purchase.** A homepage banner is marketing; the
cart is the product. In an audited study, six providers had their order
page opened and **all six rows were wrong** — two were sold out entirely,
one had no such plan, and three were priced from promos or smaller tiers.
The row that started it was marked "VERIFIED against the live site", and it
was: the *banner* said $6.00/month. Nobody had clicked through to discover
that every plan read "Out of Stock".

So the check is: can I select this plan, in a location, and reach a
checkout? Note the answer in `availability` and stamp `stock_checked` with
the date. Where a vendor renders its catalogue client-side, a plain fetch
returns the empty shell and greps clean — render the page or say you
couldn't, but never let "no sold-out text found" become "in stock".

Upgrade `confidence` as you confirm rows. The Monte Carlo automatically
trusts them more, so verification tightens the result mechanically. But
note its limit: a ±25% error bar on a low-confidence row does not survive
contact with a 6.8× error. Propagated uncertainty models *noise*, not
*wrong-plan*. Only opening the page fixes that, which is why this step is
not optional.

When most rows remain unchecked, add a **verified-only profile** — the same
weights, filtered to rows priced from a standing catalogue — and recommend
from that. Present the broad ranking as a queue of candidates to verify,
not a verdict.

## 7. Split the report: numbers generated, judgement written

Two files:

- **`out/RANKINGS.md`** — every table, emitted by a script, never
  hand-edited. It cannot drift from the data.
- **`REPORT.md`** — the recommendation and the reasoning, written by hand.

The report leads with the answer, then justifies it. If you recommend
something other than the top-scoring row — which is legitimate, models
can't weigh "annual prepay to a vendor with support complaints" — show both
and say exactly what the difference buys.

Close with **what the analysis does not establish**: what wasn't
benchmarked, which measurements are weak evidence, which numbers are
assumptions. Stating limits is what makes the rest credible.

## Boundaries

- **Don't use this for two or three options.** The machinery costs more
  than it saves; just compare them in prose. The trigger is *many*.
- **Don't fabricate precision.** If you couldn't verify a price, it's
  `low` confidence and the report says so next to the number. Inventing a
  plausible figure and marking it `high` defeats the entire method.
- **Don't recommend anything you haven't confirmed is purchasable.** The
  broad ranking can be topped by unverified rows; the recommendation cannot
  be one of them.
- **Don't let the weights be invisible.** They're the whole argument. In a
  data file, printed in the report, editable by the reader.
- **Measurements need their limits attached.** If you measure latency to a
  vendor's CDN-fronted marketing site, say that in the output itself — not
  in a footnote the reader will skip.
- Deeper reasoning and the failure modes this method exists to prevent:
  [`references/methodology.md`](references/methodology.md).
