# VPS research — hosting the Jod orchestrator

Scripted comparison of **60 VPS providers** against one question: where should
the Jod LLM-agent orchestrator live, given it must be free to do anything on the
internet, reliably available, well-connected, globally reachable, and cheap.

**→ Read [`REPORT.md`](REPORT.md).** That is the deliverable.

> **Corrected 2026-08-09.** The original recommendation, Advin Servers, is sold
> out in every region and never had the plan it was priced at. Chasing that led
> to a systematic fault: the dataset priced *advertisements* rather than
> purchases. Six providers have now had their order page opened and **all six
> were wrong**, by up to 6.8×. Prices, stock and the recommendation have all
> changed — see the top of the report.

## Layout

```
REPORT.md              the analysis and the recommendation  ← start here
data/
  providers.json       60 providers, one flat record each
  profiles.json        weight profiles + hard filters + FX rates
  schema.md            what every field means
scripts/
  validate.py          dataset integrity check (run first)
  check_stock.py       can these plans still be ordered? (fails on contradictions)
  score.py             filters, weighted scoring, Monte Carlo sensitivity
  netcheck.py          measures real TCP latency from your machine
  report.py            regenerates every table in out/
  run_all.sh           all of the above, in order
out/
  RANKINGS.md          generated tables (never hand-edited)
  scores-*.csv         per-profile ranked data
  netcheck.json        raw latency measurements
```

## Run it

```bash
./scripts/run_all.sh              # full pipeline
./scripts/run_all.sh --skip-net   # skip the latency measurement
```

Python 3 standard library only — no dependencies, no install step.

## Disagree with the result?

The ranking is a function of the weights, and the weights are data. Edit
`data/profiles.json` and rerun:

```bash
python3 scripts/score.py --profile freedom     # permissiveness above all
python3 scripts/score.py --profile cheapest    # price above all
python3 scripts/score.py --profile production  # uptime above all
```

Each profile produces a different winner. That is the point — there is no
universally best VPS, only a best one for a stated weighting.

## How it avoids fooling itself

- **Hard filters run before scoring.** A host that can't run Docker is not a
  low-scoring option, it's not an option. Weighted averages otherwise let a good
  price paper over a disqualifying flaw.
- **Being purchasable is one of those filters.** Added after the original
  recommendation turned out to be sold out. A plan nobody can buy is not a cheap
  plan; `stock: out` never reaches the scoring stage.
- **Where a price came from is recorded.** `price_basis` separates a standing
  catalogue price from a promo or a "starting at" banner. The
  `--profile verified` ranking contains only rows priced from a real catalogue,
  and it is currently three rows long out of sixty.
- **Uncertainty is propagated, not hidden.** Every row carries a `confidence`.
  The Monte Carlo perturbs unverified prices ±25% against ±4% for verified ones,
  so a cheap-but-unconfirmed provider must survive being wrong about its own
  price before it can rank.
- **Stability is reported alongside score.** A provider that only wins under one
  exact weighting is an artifact, not a recommendation. The "top-5 stability"
  column separates the two.
- **Measurements are labelled with their limits.** `netcheck.py` measures
  corporate websites, most CDN-fronted — it says so in its own output rather
  than letting the number look more meaningful than it is.

## Known limits

Read the "How much to trust this" section of `REPORT.md`. In short: 35 of 60
prices are unverified market figures, no provider was benchmarked on real
hardware, and the FX rates are assumptions. Verify any row before spending money
on it.
