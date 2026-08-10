# Iteration rankings

Composite = weighted rubric. R5/R6/R7 measured by running the
queries; R1-R4 computed from declared schema properties.

| rank | iteration | composite | f1 | leak | poison | historical | multihop | current | episodic | prospective | tokens |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | `10-final-trimmed` | **0.8328** | 0.574 | 0.00 | 0.00 | 1.00 | 1.00 | 0.50 | 0.42 | 1.00 | 44 |
| 2 | `09-prospective` | **0.8224** | 0.547 | 0.00 | 0.00 | 1.00 | 1.00 | 0.50 | 0.42 | 1.00 | 50 |
| 3 | `03-trust-admission` | **0.8134** | 0.637 | 0.00 | 0.00 | 1.00 | 0.50 | 0.58 | 0.83 | 0.00 | 43 |
| 4 | `04-entities-mentions` | **0.8134** | 0.637 | 0.00 | 0.00 | 1.00 | 0.50 | 0.58 | 0.83 | 0.00 | 43 |
| 5 | `08-edges-as-facts` | **0.8084** | 0.547 | 0.00 | 0.00 | 1.00 | 1.00 | 0.50 | 0.42 | 0.00 | 50 |
| 6 | `06-hop-reserved` | **0.7954** | 0.547 | 0.00 | 0.00 | 1.00 | 1.00 | 0.50 | 0.42 | 0.00 | 50 |
| 7 | `07-typed-edges` | **0.7867** | 0.653 | 0.00 | 0.00 | 1.00 | 0.75 | 0.53 | 0.83 | 0.00 | 43 |
| 8 | `05-hop-merged` | **0.7579** | 0.547 | 0.00 | 0.00 | 1.00 | 1.00 | 0.50 | 0.42 | 0.00 | 50 |
| 9 | `01-facts-only` | **0.7509** | 0.617 | 0.00 | 0.10 | 1.00 | 0.50 | 0.83 | 0.00 | 0.00 | 8 |
| 10 | `02-episodic` | **0.7392** | 0.604 | 0.00 | 0.10 | 1.00 | 0.50 | 0.58 | 0.83 | 0.00 | 43 |

## Controls and ablations

| run | composite | f1 | leak | poison | historical |
|---|---:|---:|---:|---:|---:|
| `00-shipped-scope-blind` | 0.6810 | 0.530 | 0.20 | 0.10 | 1.00 |
| `XX-hop-gated` | 0.8273 | 0.547 | 0.00 | 0.00 | 1.00 |
| `XX-substring-alias` | 0.7853 | 0.524 | 0.00 | 0.00 | 1.00 |
| `XX-head-only-tombstone` | 0.6453 | 0.574 | 0.00 | 0.00 | 0.00 |
