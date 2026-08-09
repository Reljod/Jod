"""Round-2 strategies: the mechanisms the big labs actually ship.

Each maps to a row in HYPOTHESES-2.md. Shared channels (BM25, Random Indexing,
fusion) come from retrieval.py unchanged, so any difference here is
architectural rather than channel jitter — same discipline as round 1.
"""

from __future__ import annotations

import random
import re
from collections import defaultdict

from retrieval import Index, expansion_terms, fuse, importance_multiplier, mmr

K = 8
VECTOR_WEIGHT = 0.2          # swept in round 1, reused unchanged
SCOPE_BOOST = 1.5            # "scope as a ranking signal"
MEMORY_CHAR_BUDGET = 2200
SUMMARY_CHARS = 1200         # Codex reads memory_summary.md whole, to budget
HOP_EXTRA = 2

_DATE_RE = re.compile(r"as of (\d{4})-(\d{2})-(\d{2})")


class LabExtractor:
    """Same simulated extractor as round 1, plus authored-convention parsing."""

    RECALL = 0.90
    FPR = 0.05

    def __init__(self, obj2pred: dict[str, str], subjects: list[str],
                 seed: int = 4242) -> None:
        self.rng = random.Random(seed)
        self.obj2pred = obj2pred
        self.subjects = subjects

    def parse(self, chunk: dict) -> dict | None:
        kind = chunk["kind"]
        if kind in ("fact", "poison", "authored"):
            if self.rng.random() > self.RECALL:
                return None
            return {"op": "upsert", "subject": chunk["subject"],
                    "predicate": chunk["predicate"], "obj": chunk["obj"]}
        if kind == "retraction":
            if self.rng.random() > self.RECALL:
                return None
            return {"op": "retract", "subject": chunk["subject"],
                    "predicate": chunk["predicate"], "obj": None}
        if self.rng.random() > self.FPR:
            return None
        text = chunk["text"]
        subject = next((s for s in self.subjects if s in text), None)
        obj = next((o for o in self.obj2pred if o in text), None)
        if subject is None or obj is None:
            return None
        return {"op": "upsert", "subject": subject,
                "predicate": self.obj2pred[obj], "obj": obj}


class LabStrategy:
    name = "base"
    scope_mode = "filter"    # none | boost | filter

    def __init__(self, index: Index, k: int = K, extractor: LabExtractor | None = None) -> None:
        self.index = index
        self.k = k
        self.ex = extractor
        self.seen: list[str] = []

    def ingest(self, chunk: dict) -> None:
        self.seen.append(chunk["id"])

    def retrieve(self, query: dict) -> list[str]:
        raise NotImplementedError

    def observe(self, query: dict, returned: list[str]) -> None:
        """Hook for strategies that learn from their own retrieval history."""

    # ---------------------------------------------------------------- helpers
    def _in_scope(self, cid: str, query: dict) -> bool:
        return self.index.by_id[cid]["scope"] == query["scope"]

    def _pool(self, query: dict, ids) -> set[str]:
        if self.scope_mode == "filter":
            return {c for c in ids if self._in_scope(c, query)}
        return set(ids)

    def _rank(self, query: dict, eligible: set[str]) -> list[tuple[str, float]]:
        ranked = fuse(self.index, query["id"], query["text"], eligible,
                      vector_weight=VECTOR_WEIGHT, text_weight=1.0 - VECTOR_WEIGHT)
        out = []
        for cid, score in ranked:
            c = self.index.by_id[cid]
            s = score * importance_multiplier(c.get("importance"))
            if self.scope_mode == "boost" and c["scope"] == query["scope"]:
                s *= SCOPE_BOOST
            out.append((cid, s))
        out.sort(key=lambda x: (-x[1], x[0]))
        return out


# ------------------------------------------------------------------ L1: scope
class HybridNoScope(LabStrategy):
    """Scope-blind retrieval — the baseline that should leak."""

    name = "hybrid_noscope"
    scope_mode = "none"

    def retrieve(self, query: dict) -> list[str]:
        ranked = self._rank(query, self._pool(query, self.seen))
        return [cid for cid, _ in ranked[: self.k]]


class HybridScopeBoost(HybridNoScope):
    """Scope as a ranking signal — a thumb on the scale."""

    name = "hybrid_scope_boost"
    scope_mode = "boost"


class HybridScopeFilter(HybridNoScope):
    """Scope as a hard partition applied before ranking — the lab design."""

    name = "hybrid_scope_filter"
    scope_mode = "filter"


# ------------------------------------------- L4/L5/L6/L7: control-plane family
class ControlScoped(LabStrategy):
    """Deterministic control plane, scope-partitioned, with a second hop.

    `purge_history=False` models the common implementation: retraction removes
    the current value but leaves prior versions reachable by a time-scoped
    query. `RedactHistory` below sets it True.
    """

    name = "control_scoped"
    purge_history = False
    use_hop = True
    keep_versions = True
    always_authored = False

    def __init__(self, index: Index, k: int = K, extractor: LabExtractor | None = None) -> None:
        super().__init__(index, k, extractor)
        self.versions: dict[tuple, list[tuple[int, str]]] = defaultdict(list)
        self.tombstoned: set[tuple] = set()
        self.retractions: list[str] = []
        self.authored: list[str] = []

    def ingest(self, chunk: dict) -> None:
        super().ingest(chunk)
        if chunk["source"] == "untrusted":
            return                                  # write-time admission
        if self.always_authored and chunk["kind"] == "authored":
            self.authored.append(chunk["id"])       # separate tier, no extraction
            return
        parsed = self.ex.parse(chunk) if self.ex else None
        if not parsed:
            return
        slot = (chunk["scope"], parsed["subject"], parsed["predicate"])
        if parsed["op"] == "retract":
            self.tombstoned.add(slot)
            if self.purge_history:
                self.versions.pop(slot, None)
            # else: prior versions stay reachable by a time-scoped query —
            # deleting the head is not deleting the record. That is the leak
            # RedactHistory fixes, and the reason Anthropic ships redaction.
            self.retractions.append(chunk["id"])
            return
        self.tombstoned.discard(slot)
        if self.keep_versions:
            self.versions[slot].append((chunk["day"], chunk["id"]))
        else:
            self.versions[slot] = [(chunk["day"], chunk["id"])]   # rewrite in place

    def _asof(self, query: dict) -> int | None:
        return query["day"] if _DATE_RE.search(query["text"]) else None

    def _eligible(self, query: dict) -> set[str]:
        asof = self._asof(query)
        out: set[str] = set()
        for slot, versions in self.versions.items():
            if slot[0] != query["scope"] or not versions:
                continue
            if slot in self.tombstoned and (asof is None or self.purge_history):
                continue
            ordered = sorted(versions)
            if asof is None:
                out.add(ordered[-1][1])
            else:
                valid = [cid for day, cid in ordered if day <= asof]
                if valid:
                    out.add(valid[-1])
        return out

    def _scoped_retractions(self, query: dict) -> set[str]:
        return {c for c in self.retractions if self._in_scope(c, query)}

    def retrieve(self, query: dict) -> list[str]:
        marks = self._scoped_retractions(query)
        eligible = self._eligible(query) | marks
        head = [c for c in self.authored if self._in_scope(c, query)]
        if not eligible:
            return head
        ranked = self._rank(query, eligible)
        if not ranked:
            return head
        if ranked[0][0] in marks:
            return []                               # slot withdrawn: abstain
        ranked = [(c, s) for c, s in ranked if c not in marks]
        base = [c for c, _ in mmr(self.index, ranked, self.k)]
        if self.use_hop:
            base = self._hop(query, base, set(self._eligible(query)))
        return head + base

    def _hop(self, query: dict, first: list[str], live: set[str]) -> list[str]:
        terms = expansion_terms(self.index, first[:2], query["text"], 3)
        if not terms:
            return first[: self.k]
        expanded = fuse(self.index, query["id"] + "::exp",
                        query["text"] + " " + " ".join(terms), live,
                        vector_weight=VECTOR_WEIGHT, text_weight=1.0 - VECTOR_WEIGHT)
        out, added = list(first[: self.k]), 0
        for cid, _ in expanded:
            if added >= HOP_EXTRA:
                break
            if cid not in out:
                out.append(cid)
                added += 1
        return out


class RedactHistory(ControlScoped):
    """L7 — retraction purges every version, not just the current value."""

    name = "redact_history"
    purge_history = True


class AppendSupersede(ControlScoped):
    """L4 baseline — append-only versioning, hop off for a clean A/B."""

    name = "append_supersede"
    use_hop = False


class RewriteInPlace(ControlScoped):
    """L4 — destructive rewrite: the new value replaces the old, history gone."""

    name = "rewrite_inplace"
    use_hop = False
    keep_versions = False


class AuthoredCore(ControlScoped):
    """L5 — authored conventions as a separate always-resident tier."""

    name = "authored_core"
    always_authored = True


def _conflicting(index: Index, a_id: str, b_id: str) -> bool:
    a, b = index.by_id[a_id], index.by_id[b_id]
    if a["scope"] != b["scope"]:
        return True
    return (a["kind"] == b["kind"] == "fact"
            and a["subject"] == b["subject"]
            and a["predicate"] == b["predicate"]
            and a["obj"] != b["obj"])


class AbstainAmbiguous(ControlScoped):
    """L6 — return nothing when the top two candidates conflict."""

    name = "abstain_ambiguous"

    def retrieve(self, query: dict) -> list[str]:
        out = super().retrieve(query)
        if len(out) < 2:
            return out
        return [] if _conflicting(self.index, out[0], out[1]) else out


class AbstainOnNoScope(HybridNoScope):
    """L6 on a base where conflicts actually occur — no control plane, no scope
    filter, so the top two candidates frequently disagree."""

    name = "abstain_on_noscope"

    def retrieve(self, query: dict) -> list[str]:
        out = super().retrieve(query)
        if len(out) < 2:
            return out
        return [] if _conflicting(self.index, out[0], out[1]) else out


# ------------------------------------------------------------------- L2: grep
class CodexGrep(LabStrategy):
    """L2 — bounded consolidated summary, always injected, plus literal grep.

    No embeddings, no scoring: the summary is read whole to a character budget
    and the long-form store is matched on stemmed tokens. This is the shape
    Codex CLI publishes, not its implementation.
    """

    name = "codex_grep"

    def __init__(self, index: Index, k: int = K, extractor: LabExtractor | None = None) -> None:
        super().__init__(index, k, extractor)
        self.current: dict[tuple, str] = {}

    def ingest(self, chunk: dict) -> None:
        super().ingest(chunk)
        if chunk["source"] == "untrusted":
            return
        parsed = self.ex.parse(chunk) if self.ex else None
        if not parsed:
            return
        slot = (chunk["scope"], parsed["subject"], parsed["predicate"])
        if parsed["op"] == "retract":
            self.current.pop(slot, None)
        else:
            self.current[slot] = chunk["id"]

    def _summary(self, query: dict) -> list[str]:
        ids = [cid for slot, cid in self.current.items() if slot[0] == query["scope"]]
        ids.sort(key=lambda c: (-self.index.by_id[c].get("importance", 0), c))
        out, spent = [], 0
        for cid in ids:
            n = len(self.index.by_id[cid]["text"])
            if spent + n > SUMMARY_CHARS:
                break
            out.append(cid)
            spent += n
        return out

    def retrieve(self, query: dict) -> list[str]:
        summary = self._summary(query)
        qt = set(self.index.tokens[query["id"]]) if query["id"] in self.index.tokens else None
        from retrieval import tokenize
        terms = set(tokenize(query["text"])) if qt is None else qt
        hits = []
        for cid in self.seen:
            if not self._in_scope(cid, query):
                continue
            overlap = terms & self.index.token_sets[cid]
            if not overlap:
                continue
            hits.append((cid, sum(self.index.idf.get(t, 0.0) for t in overlap)))
        hits.sort(key=lambda x: (-x[1], x[0]))
        out = list(summary)
        for cid, _ in hits:
            if len(out) >= len(summary) + self.k:
                break
            if cid not in out:
                out.append(cid)
        return out


# -------------------------------------------------------------- L3: eviction
class BoundedCap(LabStrategy):
    """Hermes-style hard cap; subclasses differ only in what they evict."""

    name = "bounded_lru"
    policy = "lru"

    def __init__(self, index: Index, k: int = K, extractor: LabExtractor | None = None) -> None:
        super().__init__(index, k, extractor)
        self.slots: dict[tuple, str] = {}
        self.touched: dict[tuple, int] = {}     # last write or read
        self.recalled: dict[tuple, int] = {}    # last time retrieval used it
        self.clock = 0

    def _size(self) -> int:
        return sum(len(self.index.by_id[c]["text"]) for c in self.slots.values())

    def _victim(self):
        if self.policy == "importance":
            return min(self.slots, key=lambda s: self.index.by_id[self.slots[s]]
                       .get("importance", 0))
        if self.policy == "unrecalled":
            # Never recalled sorts before anything that has been.
            return min(self.slots, key=lambda s: (self.recalled.get(s, -1),
                                                  self.touched.get(s, 0)))
        return min(self.slots, key=lambda s: self.touched.get(s, 0))

    def ingest(self, chunk: dict) -> None:
        super().ingest(chunk)
        self.clock += 1
        parsed = self.ex.parse(chunk) if self.ex else None
        if not parsed:
            return
        slot = (chunk["scope"], parsed["subject"], parsed["predicate"])
        if parsed["op"] == "retract":
            self.slots.pop(slot, None)
            return
        self.slots[slot] = chunk["id"]
        self.touched[slot] = self.clock
        while self._size() > MEMORY_CHAR_BUDGET and self.slots:
            v = self._victim()
            self.slots.pop(v, None)
            self.touched.pop(v, None)
            self.recalled.pop(v, None)

    def retrieve(self, query: dict) -> list[str]:
        eligible = {c for slot, c in self.slots.items() if slot[0] == query["scope"]}
        if not eligible:
            return []
        return [c for c, _ in self._rank(query, eligible)]

    def observe(self, query: dict, returned: list[str]) -> None:
        self.clock += 1
        by_chunk = {cid: slot for slot, cid in self.slots.items()}
        for cid in returned[: self.k]:
            slot = by_chunk.get(cid)
            if slot:
                self.recalled[slot] = self.clock
                self.touched[slot] = self.clock


class BoundedUnrecalled(BoundedCap):
    name = "bounded_unrecalled"
    policy = "unrecalled"


class BoundedImportance(BoundedCap):
    name = "bounded_importance"
    policy = "importance"


def build_all(index: Index, obj2pred: dict[str, str],
              subjects: list[str]) -> list[LabStrategy]:
    def ex() -> LabExtractor:
        return LabExtractor(obj2pred, subjects)

    return [
        HybridNoScope(index),
        HybridScopeBoost(index),
        HybridScopeFilter(index),
        ControlScoped(index, extractor=ex()),
        RedactHistory(index, extractor=ex()),
        AppendSupersede(index, extractor=ex()),
        RewriteInPlace(index, extractor=ex()),
        AuthoredCore(index, extractor=ex()),
        AbstainAmbiguous(index, extractor=ex()),
        AbstainOnNoScope(index),
        CodexGrep(index, extractor=ex()),
        BoundedCap(index, extractor=ex()),
        BoundedUnrecalled(index, extractor=ex()),
        BoundedImportance(index, extractor=ex()),
    ]
