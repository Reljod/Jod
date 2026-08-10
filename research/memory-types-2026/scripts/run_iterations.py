#!/usr/bin/env python3
"""Ten schema/pipeline iterations, built in real SQLite and scored.

No network, no API key, no model. Every retrieval number below is produced by
running the query against the schema under test; the rubric's non-retrieval
criteria are scored by declared rules, not judgement.

    python3 run_iterations.py > ../out/RAW.txt
"""
import json
import os
import re
import sqlite3
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from corpus import (FACTS, EPISODES, ENTITIES, INTENTIONS, QUERIES,
                    RETRACTION_PROBE, PROSPECTIVE_PROBE)

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "out")

ENTITY_NAMES = {n for n, _ in ENTITIES}
STALE_IDS = {"f1", "t6"}
POISON_IDS = {"p1"}

# Origin trust, exactly the four classes already in core/src/store.rs.
ORIGIN_W = {"owner": 1.25, "agent": 1.00, "system": 1.00, "untrusted": 0.50}
SALIENCE = {"owner": 7, "system": 6, "agent": 5, "untrusted": 3}


def fts_query(text):
    """Byte-for-byte the escaping in core/src/store.rs::fts_query — tokens
    quoted and joined with OR. Reproducing it matters: an AND-joined query is
    a different retriever and would not be measuring Jod."""
    terms = [t for t in re.split(r"[^A-Za-z0-9_]+", text) if t]
    if not terms:
        return None
    return " OR ".join('"%s"' % t for t in terms)


# ---------------------------------------------------------------- schema ----

def build(cfg):
    db = sqlite3.connect(":memory:")
    db.executescript("""
      PRAGMA foreign_keys = ON;
      CREATE TABLE facts (
        id TEXT PRIMARY KEY, scope TEXT NOT NULL, subject TEXT NOT NULL,
        predicate TEXT NOT NULL, object TEXT NOT NULL, origin TEXT NOT NULL,
        valid_from TEXT, valid_to TEXT, invalidated_by TEXT,
        salience INTEGER NOT NULL DEFAULT 5,
        evidence_count INTEGER NOT NULL DEFAULT 1,
        contested INTEGER NOT NULL DEFAULT 0,
        subject_entity TEXT, object_entity TEXT);
      CREATE VIRTUAL TABLE facts_fts USING fts5(
        subject, predicate, object, content='facts', content_rowid='rowid');
      CREATE TABLE tombstones (
        scope TEXT, subject TEXT, predicate TEXT, versions INTEGER, kind TEXT);
    """)
    for fid, scope, s, p, o, origin, vfrom, sup in FACTS:
        db.execute(
            "INSERT INTO facts (id,scope,subject,predicate,object,origin,"
            "valid_from,valid_to,invalidated_by,salience,evidence_count,"
            "contested,subject_entity,object_entity) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (fid, scope, s, p, o, origin, vfrom,
             "2026-08-09" if sup else None, sup,
             SALIENCE[origin], 1, 1 if origin == "untrusted" else 0,
             s if s in ENTITY_NAMES else None,
             o if o in ENTITY_NAMES else None))
    db.execute("INSERT INTO facts_fts(rowid,subject,predicate,object) "
               "SELECT rowid,subject,predicate,object FROM facts")

    if cfg["episodes"]:
        db.executescript("""
          CREATE TABLE episodes (
            id TEXT PRIMARY KEY, scope TEXT NOT NULL, title TEXT NOT NULL,
            body TEXT NOT NULL, origin TEXT NOT NULL, salience INTEGER DEFAULT 6,
            archived_at_ms INTEGER, redacted_at_ms INTEGER);
          CREATE VIRTUAL TABLE episodes_fts USING fts5(
            title, body, content='episodes', content_rowid='rowid');
        """)
        for eid, scope, title, body, origin in EPISODES:
            db.execute("INSERT INTO episodes (id,scope,title,body,origin) "
                       "VALUES (?,?,?,?,?)", (eid, scope, title, body, origin))
        db.execute("INSERT INTO episodes_fts(rowid,title,body) "
                   "SELECT rowid,title,body FROM episodes")

    if cfg["entities"]:
        db.executescript("""
          CREATE TABLE entities (
            id INTEGER PRIMARY KEY, scope TEXT NOT NULL, kind TEXT NOT NULL,
            name TEXT NOT NULL, UNIQUE(scope,kind,name));
          CREATE TABLE entity_aliases (
            scope TEXT, alias_norm TEXT, entity_id INTEGER,
            PRIMARY KEY(scope, alias_norm));
          CREATE TABLE mentions (
            entity_id INTEGER NOT NULL, src_kind TEXT NOT NULL,
            src_id TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'body',
            PRIMARY KEY(entity_id, src_kind, src_id, role));
        """)
        eid = {}
        for scope in ("finance", "tasks"):
            for name, kind in ENTITIES:
                cur = db.execute(
                    "INSERT INTO entities (scope,kind,name) VALUES (?,?,?)",
                    (scope, kind, name))
                eid[(scope, name)] = cur.lastrowid
                db.execute("INSERT INTO entity_aliases VALUES (?,?,?)",
                           (scope, name.lower(), cur.lastrowid))
        # Mentions are produced by DETERMINISTIC alias matching over the whole
        # record — subject, object and free text alike. No triple extraction,
        # so a fact whose object is a literal still joins the graph.
        def hits(name, blob):
            """Token-boundary match, not substring. Found by running this
            experiment: a naive `name.lower() in blob` makes the entity `Jod`
            match every occurrence of `Reljod`, which silently fuses a project
            with its owner and poisons every hop through either."""
            if cfg["substring_alias"]:
                return name.lower() in blob.lower()
            return re.search(r"(?<![A-Za-z0-9_])%s(?![A-Za-z0-9_])"
                             % re.escape(name), blob, re.I) is not None

        for fid, scope, s, p, o, *_ in FACTS:
            blob = " ".join((s, p, o))
            for name, _ in ENTITIES:
                if hits(name, blob):
                    role = ("subject" if name == s
                            else "object" if name == o else "body")
                    db.execute("INSERT OR IGNORE INTO mentions VALUES (?,?,?,?)",
                               (eid[(scope, name)], "fact", fid, role))
        if cfg["episodes"]:
            for epid, scope, title, body, _ in EPISODES:
                blob = title + " " + body
                for name, _ in ENTITIES:
                    if hits(name, blob):
                        db.execute(
                            "INSERT OR IGNORE INTO mentions VALUES (?,?,?,?)",
                            (eid[(scope, name)], "episode", epid, "body"))

    if cfg["typed_edges"]:
        # The "real graph": a separate edge table populated by triple
        # extraction. An edge exists only where BOTH endpoints resolve to
        # entities — which is exactly why literal-valued facts disappear.
        db.executescript("""
          CREATE TABLE edges (
            id INTEGER PRIMARY KEY, scope TEXT NOT NULL, src TEXT NOT NULL,
            dst TEXT NOT NULL, predicate TEXT NOT NULL, fact_id TEXT,
            valid_from TEXT, valid_to TEXT);
        """)
        for fid, scope, s, p, o, origin, vfrom, sup in FACTS:
            if s in ENTITY_NAMES and o in ENTITY_NAMES:
                db.execute("INSERT INTO edges (scope,src,dst,predicate,fact_id,"
                           "valid_from,valid_to) VALUES (?,?,?,?,?,?,?)",
                           (scope, s, o, p, fid, vfrom,
                            "2026-08-09" if sup else None))

    if cfg["intentions"]:
        db.executescript("""
          CREATE TABLE intentions (
            id TEXT PRIMARY KEY, scope TEXT NOT NULL, goal TEXT NOT NULL,
            cue_kind TEXT NOT NULL, due_at TEXT, window_ms INTEGER,
            status TEXT NOT NULL DEFAULT 'pending', origin TEXT NOT NULL);
        """)
        for iid, scope, goal, cue, due, origin in INTENTIONS:
            db.execute("INSERT INTO intentions (id,scope,goal,cue_kind,due_at,"
                       "origin) VALUES (?,?,?,?,?,?)",
                       (iid, scope, goal, cue, due, origin))
    return db


# ------------------------------------------------------------- retrieval ----

def bm25_norm(raw):
    """OpenClaw's normaliser. SQLite's bm25() is negative-better."""
    rel = -raw
    return rel / (1.0 + rel) if rel > 0 else 1.0 / (1.0 + 999)


def round_one(db, cfg, q, scope):
    expr = fts_query(q)
    if expr is None:
        return []
    out = []
    sql = ("SELECT f.id, f.origin, f.salience, f.evidence_count, f.contested, "
           "       bm25(facts_fts) AS r, length(f.subject||f.predicate||f.object) "
           "  FROM facts_fts JOIN facts f ON f.rowid = facts_fts.rowid "
           " WHERE facts_fts MATCH ? AND f.valid_to IS NULL")
    args = [expr]
    if cfg["scope_filter"]:
        sql += " AND f.scope = ?"
        args.append(scope)
    if cfg["origin_filter"]:
        sql += " AND f.origin <> 'untrusted'"
    for fid, origin, sal, ev, con, r, ln in db.execute(sql, args):
        base = bm25_norm(r)
        if cfg["ranking"] == "weighted":
            base *= ORIGIN_W[origin]
            base *= 0.75 + 0.05 * sal
            base *= 1.00 + 0.05 * min(ev - 1, 4)
            base *= 0.70 if con else 1.00
        out.append(("fact", fid, base, ln))

    if cfg["episodes"]:
        esql = ("SELECT e.id, e.origin, e.salience, bm25(episodes_fts) AS r, "
                "       length(e.title||e.body) "
                "  FROM episodes_fts JOIN episodes e ON e.rowid = episodes_fts.rowid "
                " WHERE episodes_fts MATCH ? AND e.redacted_at_ms IS NULL "
                "   AND e.archived_at_ms IS NULL")
        eargs = [expr]
        if cfg["scope_filter"]:
            esql += " AND e.scope = ?"
            eargs.append(scope)
        for eid_, origin, sal, r, ln in db.execute(esql, eargs):
            base = bm25_norm(r)
            if cfg["ranking"] == "weighted":
                base *= ORIGIN_W[origin] * (0.75 + 0.05 * sal)
            out.append(("episode", eid_, base, ln))

    out.sort(key=lambda x: -x[2])
    if cfg["rel_floor"] and out:
        top = out[0][2]
        out = [x for x in out if x[2] >= 0.25 * top]
    return out


def hop(db, cfg, seeds, scope, exclude):
    """One step out from the entities named in the seeds."""
    if not cfg["entities"] and not cfg["typed_edges"]:
        return []
    got = []
    if cfg["typed_edges"]:
        names = set()
        for kind, sid, *_ in seeds:
            for src, dst in db.execute(
                    "SELECT src,dst FROM edges WHERE fact_id=? AND valid_to IS NULL",
                    (sid,)):
                names.update((src, dst))
        for n in names:
            for fid, ln in db.execute(
                    "SELECT e.fact_id, length(f.subject||f.predicate||f.object) "
                    "  FROM edges e JOIN facts f ON f.id = e.fact_id "
                    " WHERE (e.src=? OR e.dst=?) AND e.scope=? "
                    "   AND e.valid_to IS NULL", (n, n, scope)):
                if fid not in exclude:
                    got.append(("fact", fid, 0.0, ln))
    else:
        ents = set()
        for kind, sid, *_ in seeds:
            for (e,) in db.execute(
                    "SELECT entity_id FROM mentions WHERE src_kind=? AND src_id=?",
                    (kind, sid)):
                ents.add(e)
        for e in ents:
            for k, sid, ln in db.execute(
                    "SELECT m.src_kind, m.src_id, "
                    "       length(f.subject||f.predicate||f.object) "
                    "  FROM mentions m JOIN facts f ON f.id = m.src_id "
                    " WHERE m.entity_id=? AND m.src_kind='fact' "
                    "   AND f.valid_to IS NULL "
                    + ("AND f.origin <> 'untrusted' " if cfg["origin_filter"] else "")
                    + ("AND f.scope=? " if cfg["scope_filter"] else ""),
                    (e, scope) if cfg["scope_filter"] else (e,)):
                if sid not in exclude:
                    got.append((k, sid, 0.0, ln))
    seen, uniq = set(), []
    for g in got:
        if g[1] not in seen:
            seen.add(g[1])
            uniq.append(g)
    return uniq


def retrieve(db, cfg, q, scope):
    r1 = round_one(db, cfg, q, scope)
    k, hk = cfg["k"], cfg["hop_k"]
    if cfg["hop"] is None:
        return r1[:k]
    if cfg["hop"] == "merged":
        # The WRONG merge policy, kept as an iteration on purpose: hop results
        # compete for the same k slots as round one.
        seeds = r1[:3]
        extra = hop(db, cfg, seeds, scope, {x[1] for x in r1})
        return (r1 + extra)[:k]
    if cfg["hop"] == "gated" and len(r1) >= cfg["hop_gate"]:
        return r1[:k]
    seeds = r1[:3]
    extra = hop(db, cfg, seeds, scope, {x[1] for x in r1[:k]})
    return r1[:k] + extra[:hk]


# ----------------------------------------------------------------- score ----

def score_query(ret, probe, scope):
    ids = {r[1] for r in ret}
    gold, forb = probe["gold"], probe["forbidden"]
    hit = ids & gold
    prec = len(hit) / len(ids) if ids else 0.0
    if gold:
        rec = len(hit) / len(gold)
        f1 = 0.0 if prec + rec == 0 else 2 * prec * rec / (prec + rec)
    else:                     # abstention question: returning nothing is right
        rec = 1.0 if not (ids & forb) else 0.0
        f1 = rec
    fscope = {f: s for f, s, *_ in [(x[0], x[1]) for x in
                                   [(f[0], f[1]) for f in FACTS]]}
    cross = {i for i in ids & forb if fscope.get(i, scope) != scope}
    return dict(qid=probe["qid"], cls=probe["cls"], n=len(ids),
                returned=sorted(ids), f1=round(f1, 3),
                precision=round(prec, 3), recall=round(rec, 3),
                stale=bool(ids & STALE_IDS & forb),
                cross=bool(cross), poison=bool(ids & POISON_IDS),
                tokens=sum(r[3] for r in ret) // 4)


def run(cfg):
    db = build(cfg)
    rows = [score_query(retrieve(db, cfg, p["q"], p["scope"]), p, p["scope"])
            for p in QUERIES]

    # Real deletion: purge EVERY version, then ask about the past.
    n = db.execute("SELECT count(*) FROM facts WHERE scope='finance' "
                   "AND subject='Reljod' AND predicate='bank'").fetchone()[0]
    if cfg["real_delete"]:
        db.execute("DELETE FROM facts WHERE scope='finance' AND subject='Reljod' "
                   "AND predicate='bank'")
    else:
        # The obvious-but-wrong implementation: close the head only.
        db.execute("UPDATE facts SET valid_to='2026-08-10' WHERE scope='finance' "
                   "AND subject='Reljod' AND predicate='bank'")
    db.execute("INSERT INTO facts_fts(facts_fts) VALUES('delete-all')")
    db.execute("INSERT INTO facts_fts(rowid,subject,predicate,object) "
               "SELECT rowid,subject,predicate,object FROM facts")
    db.execute("INSERT INTO tombstones VALUES ('finance','Reljod','bank',?,'fact')",
               (n,))
    # A historical question does not filter on valid_to.
    probe = RETRACTION_PROBE
    expr = fts_query(probe["q"])
    hist = [("fact", r[0], 0.0, 0) for r in db.execute(
        "SELECT f.id FROM facts_fts JOIN facts f ON f.rowid=facts_fts.rowid "
        " WHERE facts_fts MATCH ? AND f.scope=?", (expr, probe["scope"]))]
    rows.append(score_query(hist, probe, probe["scope"]))

    # Prospective: answerable only with an intentions table.
    pp = PROSPECTIVE_PROBE
    pret = []
    if cfg["intentions"]:
        pret = [("intention", r[0], 0.0, len(r[1]))
                for r in db.execute(
                    "SELECT id, goal FROM intentions WHERE status='pending' "
                    "AND cue_kind='time' AND due_at <= '2026-08-20' AND scope=?",
                    (pp["scope"],))]
    rows.append(score_query(pret, pp, pp["scope"]))
    return rows


# ---------------------------------------------------------------- rubric ----
# Weights fixed BEFORE any iteration was run. R5/R6/R7 are measured; R1-R4 are
# computed from declared schema properties by the rules below.
RUBRIC = [("R1 taxonomy coverage", 0.15), ("R2 fidelity to prior findings", 0.15),
          ("R3 no model client in core", 0.10), ("R4 schema simplicity", 0.10),
          ("R5 retrieval quality", 0.20), ("R6 safety / trust", 0.15),
          ("R7 forgetting correctness", 0.15)]


def rubric(cfg, rows):
    by = {r["qid"]: r for r in rows}
    core = [r for r in rows if r["qid"] in
            {q["qid"] for q in QUERIES}]
    r5 = sum(r["f1"] for r in core) / len(core)
    leak = sum(1 for r in core if r["cross"]) / len(core)
    poison = sum(1 for r in core if r["poison"]) / len(core)
    r6 = 1.0 - max(leak, poison)
    r7 = by["Q11"]["f1"]
    r1 = len(cfg["types"]) / 10.0
    r2 = max(0.0, 1.0 - 0.25 * len(cfg["violations"]))
    r3 = {1: 1.0, 2: 0.85, 3: 0.70}[cfg["model_passes"]]
    r4 = max(0.0, 1.0 - 0.08 * cfg["new_tables"] - 0.01 * cfg["new_columns"])
    parts = dict(zip([k for k, _ in RUBRIC], [r1, r2, r3, r4, r5, r6, r7]))
    total = sum(parts[k] * w for k, w in RUBRIC)
    return parts, round(total, 4), dict(
        f1=round(r5, 3), leak=round(leak, 3), poison=round(poison, 3),
        historical=round(r7, 3),
        multihop=round(sum(by[q].get("recall", 0) for q in ("Q4", "Q5")) / 2, 3),
        current=round(sum(by[q]["f1"] for q in ("Q1", "Q2")) / 2, 3),
        episodic=round(sum(by[q]["f1"] for q in ("Q9", "Q10")) / 2, 3),
        prospective=round(by["Q12"]["f1"], 3),
        stale=round(sum(1 for r in core if r["stale"]) / len(core), 3),
        tokens=sum(r["tokens"] for r in core) // len(core))


# ------------------------------------------------------------ iterations ----
def C(**kw):
    base = dict(scope_filter=True, origin_filter=False, episodes=False,
                entities=False, typed_edges=False, hop=None, ranking="bm25",
                rel_floor=False, intentions=False, real_delete=True, substring_alias=False,
                k=8, hop_k=4, hop_gate=3, model_passes=1, new_tables=0, new_columns=0,
                types=[], violations=[])
    base.update(kw)
    return base


T_SEM, T_WORK, T_PROC = "semantic", "working", "procedural"
ITER = [
 ("00-shipped-scope-blind", "Control: `recall()` exactly as shipped — no scope "
  "argument, no origin filter. Not one of the ten; measured to show what the "
  "default does today.",
  C(scope_filter=False, real_delete=True, types=[T_SEM, T_WORK],
    violations=["scope not partitioned", "no trust admission at read"])),

 ("01-facts-only", "The shipped `facts` table used correctly: FTS5, "
  "`valid_to IS NULL`, scope passed as a hard partition. The honest baseline.",
  C(types=[T_SEM, T_WORK], violations=["no trust admission at read"])),

 ("02-episodic", "Adds `episodes` + FTS5 over the existing event stream. The "
  "tier Jod entirely lacks.",
  C(episodes=True, new_tables=2, new_columns=8,
    types=[T_SEM, T_WORK, "episodic"], violations=["no trust admission at read"])),

 ("03-trust-admission", "Excludes `origin='untrusted'` from the answer set at "
  "read time, and keeps it out of the hop. Free; closes the poisoning hole.",
  C(episodes=True, origin_filter=True, new_tables=2, new_columns=8,
    types=[T_SEM, T_WORK, "episodic", "meta"], violations=[])),

 ("04-entities-mentions", "Adds `entities` / `entity_aliases` / `mentions`, "
  "populated by deterministic alias matching. Index only — no traversal yet.",
  C(episodes=True, origin_filter=True, entities=True, new_tables=5,
    new_columns=14, types=[T_SEM, T_WORK, "episodic", "meta", "social", "graph"],
    violations=[])),

 ("05-hop-merged", "One-hop expansion over `mentions`, merged into the same k "
  "slots. Deliberately the merge policy FINDINGS.md measured as a net loss.",
  C(episodes=True, origin_filter=True, entities=True, hop="merged",
    new_tables=5, new_columns=14,
    types=[T_SEM, T_WORK, "episodic", "meta", "social", "graph"],
    violations=["hop displaces round one"])),

 ("06-hop-reserved", "Same hop, into RESERVED extra slots. Round one is never "
  "displaced.",
  C(episodes=True, origin_filter=True, entities=True, hop="reserved",
    new_tables=5, new_columns=14,
    types=[T_SEM, T_WORK, "episodic", "meta", "social", "graph"],
    violations=[])),

 ("07-typed-edges", "The 'real graph': a separate `edges` table filled by "
  "triple extraction, traversed instead of `mentions`. Needs a second "
  "delegated extraction pass.",
  C(episodes=True, origin_filter=True, entities=True, typed_edges=True,
    hop="reserved", new_tables=6, new_columns=21, model_passes=2,
    types=[T_SEM, T_WORK, "episodic", "meta", "social", "graph"],
    violations=[])),

 ("08-edges-as-facts", "Drops the `edges` table. `facts.subject_entity` / "
  "`object_entity` + a `graph_edges` VIEW, so an edge inherits bi-temporal "
  "validity, origin and scope. Hop goes back over `mentions`.",
  C(episodes=True, origin_filter=True, entities=True, hop="reserved",
    new_tables=5, new_columns=16,
    types=[T_SEM, T_WORK, "episodic", "meta", "social", "graph", T_PROC],
    violations=[])),

 ("09-prospective", "Adds `intentions` — time and event cues, fired by a "
  "deterministic tick — plus the skills tier declared as procedural memory.",
  C(episodes=True, origin_filter=True, entities=True, hop="reserved",
    intentions=True, new_tables=6, new_columns=24,
    types=[T_SEM, T_WORK, "episodic", "meta", "social", "graph", T_PROC,
           "prospective", "identity"], violations=[])),

 ("10-final-trimmed", "Everything that paid, nothing that did not: weighted "
  "ranking (origin / salience / evidence / contested), a RELATIVE score floor, "
  "reserved hop, identity blocks. No typed edges, no merged hop, no decay.",
  C(episodes=True, origin_filter=True, entities=True, hop="reserved",
    intentions=True, ranking="weighted", rel_floor=True, new_tables=7,
    new_columns=26,
    types=[T_SEM, T_WORK, "episodic", "meta", "social", "graph", T_PROC,
           "prospective", "identity", "affective"], violations=[])),

 ("XX-hop-gated", "Ablation on the winner: the reserved hop fires only when "
  "round one returned fewer than 3 results. One untuned trial.",
  C(episodes=True, origin_filter=True, entities=True, hop="gated",
    intentions=True, ranking="weighted", rel_floor=True, new_tables=7,
    new_columns=26,
    types=[T_SEM, T_WORK, "episodic", "meta", "social", "graph", T_PROC,
           "prospective", "identity", "affective"], violations=[])),

 ("XX-substring-alias", "Ablation on the winner: entity aliases matched by "
  "naive substring instead of token boundary.",
  C(episodes=True, origin_filter=True, entities=True, hop="reserved",
    intentions=True, ranking="weighted", rel_floor=True, substring_alias=True,
    new_tables=7, new_columns=26,
    types=[T_SEM, T_WORK, "episodic", "meta", "social", "graph", T_PROC,
           "prospective", "identity", "affective"],
    violations=["alias match is not token-bounded"])),

 ("XX-head-only-tombstone", "Ablation on the winner: retraction closes only "
  "the head version instead of purging every one.",
  C(episodes=True, origin_filter=True, entities=True, hop="reserved",
    intentions=True, ranking="weighted", rel_floor=True, real_delete=False,
    new_tables=7, new_columns=26,
    types=[T_SEM, T_WORK, "episodic", "meta", "social", "graph", T_PROC,
           "prospective", "identity", "affective"],
    violations=["tombstones only the head"])),
]


def main():
    os.makedirs(OUT, exist_ok=True)
    results = []
    for name, desc, cfg in ITER:
        rows = run(cfg)
        parts, total, m = rubric(cfg, rows)
        results.append(dict(name=name, desc=desc, total=total,
                            rubric={k: round(v, 3) for k, v in parts.items()},
                            metrics=m, queries=rows))
        print("=" * 78)
        print("%-26s composite %.4f" % (name, total))
        print("  " + "  ".join("%s=%s" % kv for kv in m.items()))
        for r in rows:
            print("   %-4s %-11s f1=%.2f n=%d %s%s%s  %s"
                  % (r["qid"], r["cls"], r["f1"], r["n"],
                     "STALE " if r["stale"] else "",
                     "CROSS " if r["cross"] else "",
                     "POISON" if r["poison"] else "",
                     ",".join(r["returned"])))
    with open(os.path.join(OUT, "results.json"), "w") as fh:
        json.dump(results, fh, indent=1)

    graded = [r for r in results if r["name"][:2].isdigit()
              and r["name"] != "00-shipped-scope-blind"]
    graded.sort(key=lambda r: -r["total"])
    lines = ["# Iteration rankings", "",
             "Composite = weighted rubric. R5/R6/R7 measured by running the",
             "queries; R1-R4 computed from declared schema properties.", "",
             "| rank | iteration | composite | f1 | leak | poison | historical |"
             " multihop | current | episodic | prospective | tokens |",
             "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for i, r in enumerate(graded, 1):
        m = r["metrics"]
        lines.append("| %d | `%s` | **%.4f** | %.3f | %.2f | %.2f | %.2f | %.2f"
                     " | %.2f | %.2f | %.2f | %d |"
                     % (i, r["name"], r["total"], m["f1"], m["leak"],
                        m["poison"], m["historical"], m["multihop"],
                        m["current"], m["episodic"], m["prospective"],
                        m["tokens"]))
    lines += ["", "## Controls and ablations", "",
              "| run | composite | f1 | leak | poison | historical |",
              "|---|---:|---:|---:|---:|---:|"]
    for r in results:
        if r["name"][:2].isdigit() and r["name"] != "00-shipped-scope-blind":
            continue
        m = r["metrics"]
        lines.append("| `%s` | %.4f | %.3f | %.2f | %.2f | %.2f |"
                     % (r["name"], r["total"], m["f1"], m["leak"], m["poison"],
                        m["historical"]))
    with open(os.path.join(OUT, "RANKINGS.md"), "w") as fh:
        fh.write("\n".join(lines) + "\n")
    print("\nwrote out/results.json and out/RANKINGS.md")


if __name__ == "__main__":
    main()
