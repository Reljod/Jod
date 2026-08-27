# LLM provider research — cheapest capacity with the fewest guardrails

One question: **which model subscription should Jod run on**, given two jobs —
an LLM that browses the web at volume, and a Claude Code replacement for coding
at limits comparable to a Max plan? Cheap matters. Minimal guardrails matters
more. Flat-rate plans with good limits are the priority, benchmarked against
OpenCode.

**→ Read [`REPORT.md`](REPORT.md).** That is the deliverable.

69 options surveyed, 4 hard filters applied, 15 survivors, 20 scored under 5
weightings. The answer is **OpenCode Go at $10/month plus the Z.ai GLM Coding
Plan at $18/month**, with a Z.ai pay-as-you-go key behind both as overflow.

## Layout

```
REPORT.md              the analysis and the recommendation  ← start here
data/
  candidates.json      69 options, one record each, each with a `lesson`
  gates.json           pass/fail against the 4 hard filters, with reasons
  profiles.json        criteria, weight profiles, hard-filter definitions
  scores.json          0-5 per criterion for the 20-option shortlist
  schema.md            what every field means
scripts/
  score.py             hard filters, weighted scoring, sensitivity analysis
  report.py            regenerates out/RANKINGS.md
out/
  RANKINGS.md          generated tables (never hand-edited)
  scores-*.csv         per-profile ranked data
```

## Run it

```bash
python3 scripts/score.py                    # every profile + top-3 stability
python3 scripts/score.py --show-eliminated  # why each of the 69 failed
python3 scripts/report.py                   # regenerate out/RANKINGS.md
```

Python 3 standard library only. No network access needed — the data is in the
repo.

## Disagree with the result?

The ranking is a function of the weights, and the weights are data:

```bash
python3 scripts/score.py --profile charter          # the brief as written
python3 scripts/score.py --profile max-replacement  # all-day agentic coding
python3 scripts/score.py --profile guardrail-free   # permissiveness above all
python3 scripts/score.py --profile web-agent        # browsing at volume
python3 scripts/score.py --profile budget           # cheapest that works
```

Each profile produces a different order. That is the point. OpenCode Go is the
only option that lands in the top three under all five.

## The three findings worth knowing without reading the report

1. **A subscription is no longer portable capacity.** On 9 January 2026
   Anthropic began rejecting subscription OAuth tokens outside its own client.
   That single change eliminates Claude Max, ChatGPT Pro's Codex quota, Google
   AI Ultra, Copilot, Cursor, Windsurf and Warp from this comparison, and it is
   why OpenCode Go exists at all.

2. **The uncensored-LLM market cannot do this job.** Essentially none of the
   models people mean by "uncensored" — TheDrummer, Euryale, Magnum, MythoMax,
   Dolphin, even Hermes 4 — support tool calling. Meanwhile 224 of OpenRouter's
   417 catalogued models are both tool-capable *and* served with no provider
   moderation layer. Low-guardrail agentic model access is the mainstream, not a
   niche.

3. **The burst rate is not the budget.** OpenCode Go advertises $12 per 5-hour
   window against a $30 weekly cap. A week holds 33.6 such windows, so the
   headline figure is reachable 2.5 times a week. Plan against the weekly
   number; every plan in this market is marketed on the other one.

## How it tries not to fool itself

- **Hard filters run before scoring.** A plan whose credentials cannot leave the
  vendor's binary is not a low score, it is not an option.
- **Every eliminated candidate keeps a lesson.** 54 of the 69 fail a hard filter,
  and the reason each one fails is recorded in `gates.json`, with the
  transferable idea kept in `candidates.json` rather than discarded.
- **Sensitivity is reported.** An option that wins under exactly one weighting is
  an artifact of that weighting. `score.py` prints top-3 stability across all
  five.
- **Provenance is marked per score.** Every number is `documented`, `derived` or
  `judged`, and the report is explicit that **nothing here was measured**.
- **Vendor pages beat aggregators.** Two comparison sites disagreed on GLM, Kimi
  and MiniMax tiers on the same day; where sources conflicted the vendor's own
  page won, and where only aggregators had a number the confidence is lowered.

## Known limits

Read "How much to trust this" in `REPORT.md`. In short: this is a desk study and
not a benchmark, unlike [`research/agent-db-2026`](../agent-db-2026). No refusal
testing, no throughput testing and no coding evaluation was run — the central
guardrail claim is an inference from documentation and provider metadata, not a
measurement. Prices in this market move monthly and several moved during the
sources used here. Everything is as read on **2026-08-27**.
