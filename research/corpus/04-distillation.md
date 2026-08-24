# Phase 1b — Distilling 32 approaches to a top five

This is the ranking **before** the experiments, from published evidence plus an
independent panel. Keeping it separate matters: the final ranking should be
judged by how much the evidence moved it, and that is only visible if the
starting position is written down first.

## Two inputs, and they disagree

### Input 1: weight of published evidence

Ranked by how much the system degrades without the technique, for a single
always-on agent that manages its own context and delegates.

1. **B4 — sub-agent context isolation.** The mechanism the whole pattern rests
   on. A worker burns 10k+ tokens and returns 1-2k, so the main agent's context
   stays bounded no matter how much work happens. Without it, "delegation" is
   just a longer transcript.
2. **A5 — single-writer, multi-reader.** The one structural rule that survived
   contact with production at two independent shops. Many agents may read and
   analyse; exactly one loop holds state and performs writes.
3. **B5/D4 — externalised state.** Notes and artefacts on disk rather than in
   the window. The only technique here that survives a change of harness, and
   the precondition for any run that outlives one context window.
4. **C2 — durable execution.** The moment a task outlives a process, resumability
   stops being an optimisation and becomes the difference between a system and a
   demo.
5. **E2 — clean-context reviewer.** The cheapest quality mechanism available:
   a reviewer that did not see the work is not blinded by the reasoning that
   produced it.

### Input 2: an independent panel of five model families

Fifteen ballots through `agy`, list order shuffled per ballot, Borda tallied.

| rank | id | technique | borda | ballots | firsts |
|---|---|---|---|---|---|
| 1 | C4 | Agent inbox — a priority queue in front of the main loop | 57 | 15/15 | 4 |
| 2 | C2 | Durable execution | 51 | 15/15 | 5 |
| 3 | C1 | Ambient operation | 36 | 13/15 | 3 |
| 4 | B4 | Sub-agent context isolation | 27 | 11/15 | 0 |
| 5 | A1 | Orchestrator-worker | 22 | 7/15 | 2 |

Then a sharp drop: B1 compaction (18), A2 agent-as-tool (6), B5 filesystem (6),
and single votes for D1 and D3.

## Where they disagree, and which to believe

**A5 and E2 received zero votes.** The two techniques the published record
treats as hardest-won — single-threaded writes, and reviewing with a clean
context — were picked by none of fifteen ballots.

Two readings, and they are not equally good.

The charitable reading of the panel is that the question emphasised always-on
operation, hours-long runs and interruption, so the operational scaffolding was
salient and the panel answered the question asked. That framing effect is real
and I introduced it.

But it does not explain the zeros. A5 and E2 are both directly about running
unsupervised for hours without corrupting state, which is precisely what was
asked. The better explanation is that **A5 and E2 are lessons that only appear
after a system has failed.** They are not derivable from a description of the
system; they are derivable from watching two agents overwrite each other, or
from watching a reviewer wave through its own bug. A model reasoning from a
one-paragraph brief will not reach them, and fifteen ballots agreeing on that
is not fifteen pieces of evidence — it is one blind spot, sampled fifteen times.

Panel agreement is therefore a measure of what is *obvious*, not of what is
*true*. It is most useful exactly where it disagrees with the record, because
that disagreement locates the non-obvious findings.

## The pre-experiment top five

Combining both inputs, weighting published evidence over panel consensus where
they conflict, and keeping the panel's strongest signal where the record is
silent:

1. **B4 — sub-agent context isolation.** Top of the evidence ranking, fourth on
   the panel, on 11 of 15 ballots. The only item both inputs rate highly.
2. **C2 — durable execution.** Second on the evidence, second on the panel,
   on 15 of 15 ballots with the most first-place votes. The strongest agreement
   in the whole exercise.
3. **A5 — single-writer, multi-reader.** Promoted over panel objection, for the
   reason argued above.
4. **C4 — agent inbox.** The panel's clear winner and genuinely
   under-represented in the papers, which mostly assume a chat turn. For an
   agent that runs for hours, how a human interrupts it is a first-order design
   question, and the panel is right that the literature under-weights it.
5. **E2 — clean-context reviewer.** Promoted over panel objection.

## What the experiments now have to settle

The published record contains a real contradiction: an orchestrator-worker
system reported at 90.2% above single-agent, against a widely-read argument that
multi-agent systems are fragile and context engineering matters more. Both
sources are credible, which means they are probably describing opposite sides of
a crossover nobody has located.

The experiments are designed to locate it. The specific prediction: delegation
is **strictly dominated** below the crossover — equal accuracy, more cost, more
latency — and the crossover sits where the working set stops fitting in one
context window.
