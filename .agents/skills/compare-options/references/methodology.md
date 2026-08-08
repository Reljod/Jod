# Methodology — why each step exists

`SKILL.md` is the procedure. This is the reasoning, and the specific failure it
prevents. Read it when you're tempted to skip a step.

## The failure this method exists to prevent

A comparison is the easiest artifact to fake convincingly. The output — a
ranked table with a bolded winner — looks identical whether it came from
careful research or from asking a model to list ten vendors and inventing
plausible prices. The reader cannot tell by looking.

Worse, the fake version is *more* pleasant to read: it has no confidence
qualifiers, no missing cells, no "verify this before buying". Rigor makes a
comparison uglier. That asymmetry is why the rigor has to be structural rather
than a matter of intention.

Everything below is a mechanism that makes the honest version cheaper to
produce than the dishonest one.

## Reference spec: the comparability problem

Two vendors' "$5 plans" are not comparable. One may be 1 GB and one 8 GB; one
may be annual prepay and one monthly; one may include an IPv4 address and one
bill it separately.

Without a fixed spec, you unconsciously price each vendor at whatever plan
their marketing page pushes hardest — which is exactly the plan chosen to look
best. **The spec has to be written before the first row**, because after that
you know which spec favors your emerging preference.

Real consequence from the VPS study: fixing the spec at "≥2 vCPU" revealed that
a famous $15 plan had a single core. Its qualifying plan was $30 and it fell
from 4th to 12th. Nobody writing that comparison casually would have caught it.

## Filters: why weights are not enough

A weighted average is a compensatory model — strength on one axis offsets
weakness on another. That's correct for trade-offs and *wrong* for
requirements.

If a vendor cannot run your workload, no price makes it viable. Encoding that
as "score 0 on compatibility, weight 15%" lets a low enough price outvote it.
This is the single most common way a comparison recommends something unusable.

So: anything that makes a candidate **unusable** is a filter. Anything that
makes it **worse** is a weight. Test each criterion with "would I accept this
at any price?" — if no, it's a filter.

Report the filtered list. Those are precisely the candidates a naive comparison
would have ranked highly, so the exclusions carry real information.

## Confidence: propagating what you don't know

Research over many candidates is always uneven. You'll verify the ones that
look promising and estimate the rest — which introduces a bias with a nasty
shape: **an unverified row's error is not random with respect to its rank.**
Optimistic estimates rise in the ranking, and rising makes them look worth
believing.

The fix is to make uncertainty cost something. Each row carries a `confidence`,
and the Monte Carlo perturbs its price by ±25% when unverified against ±4% when
confirmed. A cheap-but-unverified candidate must then survive being wrong about
its own price before it can rank.

This is why the skill insists `confidence` is set honestly even for rows you
want to win. Marking an invented figure `high` doesn't just misreport the
number — it disables the one mechanism that would have caught it.

## Monte Carlo: separating signal from your own weighting

Any single weighting produces a winner. The question that matters is whether
that winner is a property of the *candidates* or of *your weights*.

Perturbing the weights ±35% across thousands of trials answers it directly. A
candidate holding a top-5 slot in 100% of trials is robust to your judgement
being somewhat wrong. One holding 30% won because of a weighting choice you
made, possibly arbitrarily — and reporting it as "the best" would be
overclaiming.

Report stability next to score, always. "Scored highest" and "held first place
in 97% of 20,000 perturbed trials" are very different claims, and only the
second is worth acting on.

## Multiple profiles: making the value judgement visible

"Best" is meaningless without "for what". A single ranking hides the value
judgement inside a number, where it looks like a measurement.

Running three or more profiles — the stated goal, single-axis extremes, and
plausibly the opposite of the stated goal — externalizes it. When the columns
agree, you have an unusually strong recommendation. When they disagree
completely, the honest finding is *that they disagree*, and the reader gets to
apply their own weighting.

In the VPS study, four profiles produced four different winners with a 12×
price spread and no candidate in all four columns. Presenting only the balanced
column would have concealed the most useful thing the analysis found.

## Verification: budget it where being wrong is expensive

Verifying 60 candidates against live pages is not feasible; verifying none is
not credible. Verify the leaders — being wrong about 40th place changes
nothing, being wrong about 1st changes everything.

Then **report what verification changed.** In practice it always changes
something. A report stating "verification moved four rows, here they are" is
far more trustworthy than one implying every figure was right first time,
because the reader can see the process actually had teeth.

## Separating generated numbers from written judgement

Two files, one rule: **the script owns every number, the human owns every
claim.**

If tables are hand-written they drift from the data — someone corrects a price
and three tables silently disagree. If the analysis is generated, it reads like
a spreadsheet with opinions bolted on, and you lose the one thing a human adds:
knowing that "annual prepay to a vendor with support complaints" is worth $43 a
year to avoid.

The report may recommend something other than the top-scoring row. That's
legitimate and often correct — but then it must show both and state exactly
what the difference buys, so the reader can disagree with the judgement without
having to re-derive the arithmetic.

## Stating limits

Close every report with what it does not establish: what wasn't benchmarked,
which measurements are weak evidence, which figures are assumptions.

This feels like undermining the work. It does the opposite. A reader who finds
an unstated limitation stops trusting everything; a reader handed the
limitations up front trusts the rest. The limits section is what converts "here
is a ranking" into "here is what I actually know".

## Measurement honesty

If the study measures something live, the measurement must carry its own
caveats *in its own output*, not in a footnote.

The VPS study measured TCP latency to vendors' marketing sites — most behind a
CDN, none in a datacenter you could buy. That is a reachability signal and
nothing more, so `netcheck.py` prints that caveat itself and labels every
CDN-fronted row. A number without its limits attached will be quoted without
them.
