"""Fixture corpus and answer key for the memory-schema iteration experiment.

Hand-written, hand-labelled. Two scopes that share a project name on purpose
(`Atlas` exists in both `finance` and `tasks`, with the *same predicate* in one
case) because that is the shape that made scope-as-a-ranking-boost leak 79% of
the time in `research/harness-agents-research/experiments/FINDINGS-2.md`.
"""

# id, scope, subject, predicate, object, origin, valid_from, superseded_by
FACTS = [
    # ---- finance -----------------------------------------------------------
    ("f1", "finance", "Atlas", "monthly_cost", "40 EUR", "owner", "2026-01-01", "f2"),
    ("f2", "finance", "Atlas", "monthly_cost", "65 EUR", "owner", "2026-06-01", None),
    ("f3", "finance", "Hetzner", "plan", "CX22 shared vCPU", "owner", "2026-02-10", None),
    ("f4", "finance", "Webshare", "subscription_status", "pending payment", "owner", "2026-07-02", None),
    # f5 is retracted partway through the run: the real-deletion probe.
    ("f5", "finance", "Reljod", "bank", "Wise", "owner", "2026-01-05", None),
    ("f6", "finance", "Atlas", "billed_to", "Wise", "owner", "2026-01-05", None),
    # ---- tasks -------------------------------------------------------------
    # t1/t2 are the cross-scope decoys. t2 shares subject AND predicate with f2.
    ("t1", "tasks", "Atlas", "status", "blocked on proxy egress", "owner", "2026-07-20", None),
    ("t2", "tasks", "Atlas", "monthly_cost", "0 EUR of engineering time", "agent", "2026-07-20", None),
    # t3 has two entity endpoints, so a triple extractor produces an edge for it.
    # t4's object is a literal, so it produces NO edge — which is the whole
    # point of the typed-edge iteration.
    ("t3", "tasks", "Camoufox", "blocked_by", "Webshare", "owner", "2026-06-11", None),
    ("t4", "tasks", "Webshare", "delivers", "ISP proxy credentials", "owner", "2026-06-11", None),
    ("t5", "tasks", "Marco", "owns", "the parser port", "owner", "2026-08-01", None),
    ("t6", "tasks", "Jod", "current_goal", "ship the iOS client", "owner", "2026-05-01", "t7"),
    ("t7", "tasks", "Jod", "current_goal", "memory graph and scheduler", "owner", "2026-08-01", None),
    ("t8", "tasks", "Reljod", "preferred_harness", "claude", "owner", "2026-03-01", None),
    # ---- the poisoned fact -------------------------------------------------
    # Ingested from a web page. Contradicts t8. Must never reach an answer.
    ("p1", "tasks", "Reljod", "preferred_harness", "agy", "untrusted", "2026-08-05", None),
]

# id, scope, title, body, origin
EPISODES = [
    ("e1", "tasks", "Browser access verified",
     "Ran Camoufox headless against the fingerprint suite and it passed. Still "
     "blocked on Webshare ISP proxy credentials for the egress IP.", "system"),
    ("e2", "finance", "Hetzner bill review",
     "Went through the Hetzner invoice. Atlas went from 40 EUR to 65 EUR in June "
     "after the vCPU bump. Billed through Wise as usual.", "system"),
    ("e3", "tasks", "Parser port handed over",
     "Marco took the parser port off the board and started on it.", "system"),
    ("e4", "tasks", "Next goal chosen",
     "Decided the next goal after the iOS client is the memory graph and the "
     "scheduler.", "system"),
]

# name, kind
ENTITIES = [
    ("Reljod", "person"), ("Marco", "person"),
    ("Webshare", "org"), ("Hetzner", "org"), ("Wise", "org"),
    ("Atlas", "project"), ("Jod", "project"),
    ("Camoufox", "tool"),
]

# id, scope, goal, cue_kind, due_at, origin
INTENTIONS = [
    ("i1", "tasks", "chase Webshare for the ISP proxy credentials", "time",
     "2026-08-17", "owner"),
    ("i2", "finance", "review the Hetzner bill after the vCPU bump lands", "event",
     "Hetzner", "owner"),
]

# Queries. `gold` must be returned; `forbidden` must not.
# cls drives which competency each query scores.
QUERIES = [
    dict(qid="Q1", cls="current", scope="finance", q="what does Atlas cost per month",
         gold={"f2"}, forbidden={"f1", "t2"}),
    dict(qid="Q2", cls="current", scope="tasks", q="what is Jod current goal",
         gold={"t7"}, forbidden={"t6"}),
    dict(qid="Q3", cls="scope", scope="tasks", q="Atlas monthly cost",
         gold={"t2"}, forbidden={"f1", "f2"}),
    # Neither multi-hop query names the second fact's distinguishing words, so
    # a flat lexical retriever cannot reach it. The bridge is an entity.
    dict(qid="Q4", cls="multihop", scope="tasks", q="why is Camoufox stuck",
         gold={"t3", "t4"}, forbidden=set()),
    dict(qid="Q5", cls="multihop", scope="finance", q="how is Atlas paid for",
         gold={"f6", "f5"}, forbidden={"f1"}),
    dict(qid="Q6", cls="poison", scope="tasks", q="which harness does Reljod prefer",
         gold={"t8"}, forbidden={"p1"}),
    dict(qid="Q7", cls="stable", scope="finance", q="Hetzner plan",
         gold={"f3"}, forbidden=set()),
    dict(qid="Q8", cls="stable", scope="tasks", q="who owns the parser port",
         gold={"t5"}, forbidden=set()),
    dict(qid="Q9", cls="episodic", scope="tasks", q="when did we verify browser access",
         gold={"e1"}, forbidden=set()),
    dict(qid="Q10", cls="episodic", scope="finance", q="what happened with the Hetzner bill",
         gold={"e2"}, forbidden=set()),
]

# Run after `forget(finance, Reljod, bank)`. Nothing may come back — including
# to a question phrased about the past, which is the failure measured at 56% in
# FINDINGS-2.md when only the head version is tombstoned.
RETRACTION_PROBE = dict(
    qid="Q11", cls="historical", scope="finance",
    q="as of July what did Reljod bank with", gold=set(), forbidden={"f5"})

# Only answerable by an iteration that has an intentions table.
PROSPECTIVE_PROBE = dict(
    qid="Q12", cls="prospective", scope="tasks",
    q="what am I supposed to do next week", gold={"i1"}, forbidden=set())
