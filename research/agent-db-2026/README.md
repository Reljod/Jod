# Agent storage research — what should hold Jod's state on a VPS

One question: **several agent processes share one VPS and all read and write the
same state — what should that state live in?** Markdown files are the incumbent
and the working assumption is that they are not the answer.

**→ Read [`REPORT.md`](REPORT.md).** That is the deliverable.

46 options surveyed, 5 hard filters applied, 9 engines benchmarked with real
concurrent OS processes inside Linux containers, ranked under 4 weightings.

## Layout

```
REPORT.md              the analysis and the recommendation  ← start here
HYPOTHESES.md          predictions, written before the benchmark ran
data/
  candidates.json      46 options, one record each, each with a `lesson`
  gates.json           pass/fail against the 5 hard filters, with reasons
  profiles.json        criteria, weight profiles, hard-filter definitions
  scores.json          0-5 per criterion for survivors, marked measured vs judged
  schema.md            what every field means
bench/
  adapters.py          one adapter per engine, plus deliberately naive variants
  harness.py           multi-process workload driver
  summarize.py         one-line console summary of a result
  Dockerfile           the Linux client image
  run_all.sh           provision servers, run the full sweep, tear down
scripts/
  score.py             hard filters, weighted scoring, sensitivity analysis
  report.py            regenerates out/RANKINGS.md
out/
  raw-results.jsonl    every benchmark run, one JSON object per line
  RANKINGS.md          generated tables (never hand-edited)
  scores-*.csv         per-profile ranked data
```

## Run it

```bash
./bench/run_all.sh              # full sweep, ~20 min, needs Docker
./bench/run_all.sh --quick      # shorter durations, smaller vector set
python3 scripts/score.py --show-eliminated
python3 scripts/report.py
```

The benchmark needs Docker. The scoring and reporting scripts are Python 3
standard library only.

## Why the benchmark runs in Docker

The deployment target is a Linux VPS. Measuring embedded engines on macOS would
measure APFS and the Docker VM's file sharing rather than the engine, and it
would compare in-process SQLite against containerised Postgres — different
kernels, different filesystems, meaningless numbers. Everything therefore runs
inside Linux containers on one 4 vCPU / 8 GB VM, sharing that budget the way
they would share a real box.

## Disagree with the result?

The ranking is a function of the weights, and the weights are data:

```bash
python3 scripts/score.py --profile simplicity    # fewest moving parts
python3 scripts/score.py --profile throughput    # 20+ concurrent agents
python3 scripts/score.py --profile recall-first  # memory quality above all
```

Each profile produces a different order. That is the point.

## How it avoids fooling itself

- **Hypotheses were written down first** ([`HYPOTHESES.md`](HYPOTHESES.md)),
  with numeric predictions and a stated list of results that would overturn the
  recommendation. Of eight, three were confirmed, four were partly wrong, and
  one turned out to have no control measurement at all. The report scores each
  one and says so.
- **Hard filters run before scoring.** An engine a second process cannot open is
  not a low score, it is not an option.
- **Every engine is also tested misused.** The `-naive` variants use the
  plausible primitive instead of the correct one. This separates "the engine is
  unsafe" from "the engine is easy to hold wrong" — and the results differ
  sharply between engines on exactly that axis.
- **Correctness is verified, not assumed.** Every run re-reads the store and
  compares what survived against what was acknowledged. Several engines
  acknowledged writes they did not keep.
- **Multi-process, not multi-threaded.** Threads in one interpreter would share
  connection state and hide the contention being measured.
- **Sensitivity is reported.** An option that only wins under one exact
  weighting is an artifact; the top-3 stability column separates the two.

## Known limits

Read "How much to trust this" in `REPORT.md`. In short: one machine, one run per
cell, synthetic random vectors (which are a *harder* recall test than real
embeddings, not an easier one), 30k vectors rather than millions, and no
network-latency simulation for the managed options. Durability settings are
stated per engine and are not identical — `synchronous_commit=on` for Postgres
versus `synchronous=NORMAL` for SQLite is called out in the report where it
matters.
