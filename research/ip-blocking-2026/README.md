# IP-blocking research — keeping Jod's browsing unblocked

Scripted comparison of **55 approaches** to one question: how does Jod fetch a
Cloudflare-protected page without being blocked, as cheaply as possible?

**→ Read [`REPORT.md`](REPORT.md).**

Follow-on from the [VPS study](../vps-comparison-2026/REPORT.md), which found
the IP — not the server — is the real constraint. This study corrects that
finding: the IP is only **one of six** layers Cloudflare checks, and it is
checked fifth.

## Layout

```
REPORT.md          the analysis and the recommendation  ← start here
criteria.py        domain scoring (cost normalization, the 6-layer model)
data/
  dataset.json     55 candidates: proxies, unblocker APIs, stealth browsers, stacks
  profiles.json    four weightings + shared hard filters
out/
  RANKINGS.md      generated tables (never hand-edited)
  scores-*.csv     per-profile ranked data
report.py          regenerates every table
```

## Run it

```bash
python3 ../../.agents/skills/compare-options/scripts/validate.py .
python3 ../../.agents/skills/compare-options/scripts/score.py . --all-profiles
python3 report.py
```

Built with the [`compare-options`](../../.agents/skills/compare-options/SKILL.md)
skill — Python stdlib only, no dependencies.

## The short version

A residential proxy alone fixes 1 of Cloudflare's 6 checks. A stealth browser
alone fixes 4, from an IP that's already flagged. **Stack them:** Camoufox or
Patchright egressing through ~10 static ISP proxy IPs covers 5 of 6 for about
**$3/month**, with a pay-per-success unblocker API as the fallback for whatever
still fails.

## Known limits

Success rates in this market are adversarial, perishable, and mostly
vendor-reported — 60% of rows are low confidence, and the recommended stacks'
figures are *composed* from two separately-measured components rather than
benchmarked as pairings. The Monte Carlo perturbs success rate (not price)
because that is the contested number here. Re-run this after ~6 months; it
decays faster than the VPS study.
