"""The eleven memory architectures under test.

Every strategy implements the same interface:

    ingest(chunk)          streaming, in session order
    retrieve(query)        -> ordered list of chunk ids

Strategies that maintain structured state (bounded_consolidated,
versioned_factstore, two_plane) need to turn prose into facts. In a real system
an LLM does that and it costs money and accuracy. Here it is simulated by
`Extractor` with a declared 90% recall and 5% false-positive rate, seeded and
identical across those strategies — so structured approaches carry realistic
extraction damage rather than getting ground truth for free.
"""

from __future__ import annotations

import random
import re
from collections import defaultdict

from retrieval import (
    Index,
    expansion_terms,
    fuse,
    importance_multiplier,
    mmr,
    temporal_decay,
)

K = 8

# OpenClaw ships minScore=0.35, but that threshold is calibrated to neural
# embedding cosines, which run higher than Random Indexing's. Leaving it on
# would measure this file's embedder scale rather than any architecture, so the
# main run disables it and scripts/sensitivity.py reports what 0.35 costs.
MIN_SCORE = 0.0
MEMORY_CHAR_BUDGET = 2200      # Hermes' MEMORY.md cap
PROMOTION_MIN_FREQ = 3
PROMOTION_MIN_DIVERSITY = 2
PROMOTION_CAP = 10

# Swept on a separate tuning seed by scripts/sweep.py. OpenClaw ships 0.7/0.3;
# with this corpus and this embedder the optimum is 0.2/0.8. The faithful 0.7
# variant is kept as `hybrid_openclaw` so the cost of the constant is visible.
TUNED_VECTOR_WEIGHT = 0.2
OPENCLAW_VECTOR_WEIGHT = 0.7

_DATE_RE = re.compile(r"as of (\d{4})-(\d{2})-(\d{2})")


class Extractor:
    """Simulated LLM fact extraction with declared, imperfect accuracy."""

    RECALL = 0.90
    FPR = 0.05

    def __init__(self, objects_to_predicate: dict[str, str], projects: list[str],
                 seed: int = 4242) -> None:
        self.rng = random.Random(seed)
        self.obj2pred = objects_to_predicate
        self.projects = projects

    def parse(self, chunk: dict) -> dict | None:
        kind = chunk["kind"]
        if kind in ("fact", "poison"):
            if self.rng.random() > self.RECALL:
                return None
            return {
                "op": "upsert",
                "subject": chunk["subject"],
                "predicate": chunk["predicate"],
                "obj": chunk["obj"],
            }
        if kind == "retraction":
            if self.rng.random() > self.RECALL:
                return None
            return {
                "op": "retract",
                "subject": chunk["subject"],
                "predicate": chunk["predicate"],
                "obj": None,
            }
        # filler: occasionally mis-read as an assertion, which is how real
        # extractors pollute a fact store with hypothetical chatter
        if self.rng.random() > self.FPR:
            return None
        text = chunk["text"]
        subject = next((p for p in self.projects if p in text), None)
        if subject is None:
            return None
        obj = next((o for o in self.obj2pred if o in text), None)
        if obj is None:
            return None
        return {
            "op": "upsert",
            "subject": subject,
            "predicate": self.obj2pred[obj],
            "obj": obj,
        }


class Strategy:
    name = "base"

    def __init__(self, index: Index, k: int = K) -> None:
        self.index = index
        self.k = k
        self.seen: list[str] = []

    def ingest(self, chunk: dict) -> None:
        self.seen.append(chunk["id"])

    def retrieve(self, query: dict) -> list[str]:
        raise NotImplementedError

    def observe(self, query: dict, returned: list[str]) -> None:
        """Hook for strategies that learn from their own retrieval history."""


# --------------------------------------------------------------------- S1-S2
class FullContext(Strategy):
    name = "full_context"

    def retrieve(self, query: dict) -> list[str]:
        return list(self.seen)


class RecencyWindow(Strategy):
    name = "recency_window"

    def retrieve(self, query: dict) -> list[str]:
        return list(reversed(self.seen[-self.k:]))


# --------------------------------------------------------------------- S3-S4
class BM25Only(Strategy):
    name = "bm25"

    def retrieve(self, query: dict) -> list[str]:
        eligible = set(self.seen)
        out = []
        for cid, _ in self.index.rank_bm25(query["id"], query["text"]):
            if cid in eligible:
                out.append(cid)
            if len(out) >= self.k:
                break
        return out


class DenseOnly(Strategy):
    name = "dense"

    def retrieve(self, query: dict) -> list[str]:
        eligible = set(self.seen)
        out = []
        for cid, _ in self.index.rank_dense(query["id"], query["text"]):
            if cid in eligible:
                out.append(cid)
            if len(out) >= self.k:
                break
        return out


# --------------------------------------------------------------------- S5-S7
class Hybrid(Strategy):
    """OpenClaw's shipped fusion, reproduced literally (linear, mixed scales)."""

    name = "hybrid_openclaw"
    decay = False
    use_mmr = False
    min_score = MIN_SCORE
    fusion = "linear"
    vector_weight = OPENCLAW_VECTOR_WEIGHT

    def _scored(self, query: dict) -> list[tuple[str, float]]:
        eligible = set(self.seen)
        ranked = fuse(self.index, query["id"], query["text"], eligible,
                      vector_weight=self.vector_weight,
                      text_weight=1.0 - self.vector_weight,
                      mode=self.fusion)
        out = []
        for cid, score in ranked:
            # The floor gates raw relevance; importance and decay then reorder
            # what survived. Applying it after the multipliers would let a
            # 30-day half-life silently empty the result set on an old corpus.
            if score < self.min_score:
                continue
            c = self.index.by_id[cid]
            s = score * importance_multiplier(c.get("importance"))
            if self.decay:
                s *= temporal_decay(max(0, query["day"] - c["day"]))
            out.append((cid, s))
        out.sort(key=lambda x: -x[1])
        return out

    def retrieve(self, query: dict) -> list[str]:
        ranked = self._scored(query)
        if self.use_mmr:
            ranked = mmr(self.index, ranked, self.k)
        return [cid for cid, _ in ranked[: self.k]]


HOP_EXTRA_SLOTS = 2


def second_hop(index: Index, query: dict, first: list[str], eligible: set[str],
               k: int, extra: int = HOP_EXTRA_SLOTS) -> list[str]:
    """Re-query using bridge terms found in round one, in *reserved* slots.

    An earlier version merged the two rounds, letting expansion results
    displace round one. That bought multi-hop (0.00 -> 0.67) at the cost of
    everything else (current_value 0.73 -> 0.48) — expansion noise evicting
    correct single-fact answers. Appending into a couple of extra slots keeps
    round one intact and pays for the hop in tokens instead of accuracy.
    """
    terms = expansion_terms(index, first[:2], query["text"], 3)
    if not terms:
        return first[:k]
    expanded = fuse(index, query["id"] + "::exp",
                    query["text"] + " " + " ".join(terms), eligible,
                    vector_weight=TUNED_VECTOR_WEIGHT,
                    text_weight=1.0 - TUNED_VECTOR_WEIGHT)
    out = list(first[:k])
    added = 0
    for cid, _ in expanded:
        if added >= extra:
            break
        if cid not in out:
            out.append(cid)
            added += 1
    return out


class HybridTuned(Hybrid):
    """Same channels at the swept weight. Base for every ablation below."""

    name = "hybrid_tuned"
    vector_weight = TUNED_VECTOR_WEIGHT


class HybridRRF(HybridTuned):
    name = "hybrid_rrf"
    fusion = "rrf"


class HybridDecay(HybridTuned):
    name = "hybrid_decay"
    decay = True


class GraphExpansion(HybridTuned):
    """Two-hop retrieval — the claim behind graph memory, without the graph."""

    name = "graph_expansion"

    def retrieve(self, query: dict) -> list[str]:
        ranked = self._scored(query)
        first = [cid for cid, _ in ranked[: self.k]]
        if not first:
            return []
        return second_hop(self.index, query, first, set(self.seen), self.k)


class HybridMMR(HybridTuned):
    name = "hybrid_mmr"
    use_mmr = True


# ------------------------------------------------------------------------ S8
class BoundedConsolidated(Strategy):
    """Hermes' bet: a hard character cap, newest-wins, whole memory injected.

    No provenance class — Hermes has none — so untrusted claims can land in
    memory just like owner statements. Eviction is least-recently-referenced.
    """

    name = "bounded_consolidated"

    def __init__(self, index: Index, k: int = K, extractor: Extractor | None = None) -> None:
        super().__init__(index, k)
        self.ex = extractor
        self.slots: dict[tuple[str, str], str] = {}      # slot -> chunk id
        self.last_used: dict[tuple[str, str], int] = {}
        self.clock = 0

    def _size(self) -> int:
        return sum(len(self.index.by_id[cid]["text"]) for cid in self.slots.values())

    def ingest(self, chunk: dict) -> None:
        super().ingest(chunk)
        self.clock += 1
        parsed = self.ex.parse(chunk) if self.ex else None
        if not parsed:
            return
        slot = (parsed["subject"], parsed["predicate"])
        if parsed["op"] == "retract":
            self.slots.pop(slot, None)
            self.last_used.pop(slot, None)
            return
        self.slots[slot] = chunk["id"]
        self.last_used[slot] = self.clock
        while self._size() > MEMORY_CHAR_BUDGET and self.slots:
            victim = min(self.slots, key=lambda s: self.last_used.get(s, 0))
            self.slots.pop(victim, None)
            self.last_used.pop(victim, None)

    def retrieve(self, query: dict) -> list[str]:
        # The whole bounded memory is always in the prompt; order it by
        # relevance so truncation at a budget behaves sensibly.
        eligible = set(self.slots.values())
        if not eligible:
            return []
        ranked = fuse(self.index, query["id"], query["text"], eligible, vector_weight=TUNED_VECTOR_WEIGHT,
                      text_weight=1.0 - TUNED_VECTOR_WEIGHT)
        return [cid for cid, _ in ranked]

    def observe(self, query: dict, returned: list[str]) -> None:
        self.clock += 1
        by_chunk = {cid: slot for slot, cid in self.slots.items()}
        for cid in returned[: self.k]:
            slot = by_chunk.get(cid)
            if slot:
                self.last_used[slot] = self.clock


# ------------------------------------------------------------------------ S9
class VersionedFactStore(Strategy):
    """Deterministic control plane: supersede, retract, refuse untrusted writes.

    Versions are kept with validity intervals so time-scoped questions resolve
    against the version that was current then, not the newest one. Conflict
    resolution is plain code (max version), never a prompt — the move that
    arXiv:2606.01435 measures at +24 to +34.8 pp over LLM-mediated resolution.
    """

    name = "versioned_factstore"

    def __init__(self, index: Index, k: int = K, extractor: Extractor | None = None) -> None:
        super().__init__(index, k)
        self.ex = extractor
        self.versions: dict[tuple[str, str], list[tuple[int, str]]] = defaultdict(list)
        self.tombstoned: set[tuple[str, str]] = set()
        self.retraction_chunks: list[str] = []
        self.rejected: list[str] = []

    def ingest(self, chunk: dict) -> None:
        super().ingest(chunk)
        # write-time provenance admission — the OpenClaw origin-class idea
        if chunk["source"] == "untrusted":
            self.rejected.append(chunk["id"])
            return
        parsed = self.ex.parse(chunk) if self.ex else None
        if not parsed:
            return
        slot = (parsed["subject"], parsed["predicate"])
        if parsed["op"] == "retract":
            self.tombstoned.add(slot)
            self.versions.pop(slot, None)
            self.retraction_chunks.append(chunk["id"])
            return
        self.tombstoned.discard(slot)
        self.versions[slot].append((chunk["day"], chunk["id"]))

    def _asof(self, query: dict) -> int | None:
        m = _DATE_RE.search(query["text"])
        if not m:
            return None
        return query["day"]

    def _eligible(self, query: dict) -> set[str]:
        asof = self._asof(query)
        out: set[str] = set()
        for slot, versions in self.versions.items():
            if slot in self.tombstoned or not versions:
                continue
            ordered = sorted(versions)
            if asof is None:
                out.add(ordered[-1][1])          # current value
            else:
                valid = [cid for day, cid in ordered if day <= asof]
                if valid:
                    out.add(valid[-1])           # value as of that date
        return out

    def retrieve(self, query: dict) -> list[str]:
        eligible = self._eligible(query) | set(self.retraction_chunks)
        if not eligible:
            return []
        ranked = fuse(self.index, query["id"], query["text"], eligible, vector_weight=TUNED_VECTOR_WEIGHT,
                      text_weight=1.0 - TUNED_VECTOR_WEIGHT)
        if not ranked:
            return []
        top = ranked[0][0]
        if top in set(self.retraction_chunks):
            return []                            # the slot was withdrawn: abstain
        out = [cid for cid, _ in ranked if cid not in set(self.retraction_chunks)]
        return out[: self.k]


# ----------------------------------------------------------------------- S10
class EarnedPromotion(HybridTuned):
    """OpenClaw's dreaming signal, online: promote what retrieval keeps using.

    A chunk is promoted once it has been retrieved PROMOTION_MIN_FREQ times
    across at least PROMOTION_MIN_DIVERSITY distinct query types. Promoted
    chunks are always prepended, like a MEMORY.md that earned its entries.
    """

    name = "earned_promotion"

    def __init__(self, index: Index, k: int = K) -> None:
        super().__init__(index, k)
        self.freq: dict[str, int] = defaultdict(int)
        self.kinds: dict[str, set[str]] = defaultdict(set)
        self.promoted: list[str] = []

    def retrieve(self, query: dict) -> list[str]:
        ranked = self._scored(query)
        base = [cid for cid, _ in ranked[: self.k]]
        head = [cid for cid in self.promoted if cid not in base]
        return (head + base)[: self.k + len(head)]

    def observe(self, query: dict, returned: list[str]) -> None:
        for cid in returned[: self.k]:
            self.freq[cid] += 1
            self.kinds[cid].add(query["qtype"])
            if (cid not in self.promoted
                    and self.freq[cid] >= PROMOTION_MIN_FREQ
                    and len(self.kinds[cid]) >= PROMOTION_MIN_DIVERSITY):
                self.promoted.append(cid)
                if len(self.promoted) > PROMOTION_CAP:
                    self.promoted.pop(0)


# ----------------------------------------------------------------------- S11
class TwoPlane(VersionedFactStore):
    """The proposal: deterministic control plane + hybrid recall + MMR + promotion.

    The control plane owns what is *true* (supersession, retraction, trust).
    The recall plane owns what is *relevant* (hybrid scoring, diversity).
    Promotion rides on top, earning a small always-resident tier from usage.
    Separating the two is the whole idea: ranking should never be asked to
    decide freshness, and freshness logic should never be asked to rank.
    """

    name = "two_plane"
    use_mmr_plane = True
    use_promotion = True
    use_hop = True

    def __init__(self, index: Index, k: int = K, extractor: Extractor | None = None) -> None:
        super().__init__(index, k, extractor)
        self.freq: dict[str, int] = defaultdict(int)
        self.kinds: dict[str, set[str]] = defaultdict(set)
        self.promoted: list[str] = []

    def retrieve(self, query: dict) -> list[str]:
        retractions = set(self.retraction_chunks)
        eligible = self._eligible(query) | retractions
        if not eligible:
            return []
        ranked = fuse(self.index, query["id"], query["text"], eligible, vector_weight=TUNED_VECTOR_WEIGHT,
                      text_weight=1.0 - TUNED_VECTOR_WEIGHT)
        scored = []
        for cid, score in ranked:
            c = self.index.by_id[cid]
            scored.append((cid, score * importance_multiplier(c.get("importance"))))
        scored.sort(key=lambda x: -x[1])
        if not scored:
            return []
        if scored[0][0] in retractions:
            return []
        scored = [(cid, s) for cid, s in scored if cid not in retractions]
        if self.use_mmr_plane:
            scored = mmr(self.index, scored, self.k)
        base = [cid for cid, _ in scored[: self.k]]
        live = set(self._eligible(query))
        if self.use_hop:
            # second_hop returns k + HOP_EXTRA_SLOTS; truncating back to k here
            # would silently discard exactly the hop results it just fetched.
            base = second_hop(self.index, query, base, live, self.k)
        if not self.use_promotion:
            return base
        head = [cid for cid in self.promoted if cid in live and cid not in base]
        return head + base

    def observe(self, query: dict, returned: list[str]) -> None:
        for cid in returned[: self.k]:
            self.freq[cid] += 1
            self.kinds[cid].add(query["qtype"])
            if (cid not in self.promoted
                    and self.freq[cid] >= PROMOTION_MIN_FREQ
                    and len(self.kinds[cid]) >= PROMOTION_MIN_DIVERSITY):
                self.promoted.append(cid)
                if len(self.promoted) > PROMOTION_CAP:
                    self.promoted.pop(0)


class TwoPlaneNoPromo(TwoPlane):
    """Ablation: does the earned-promotion tier pay for itself?"""

    name = "two_plane_no_promo"
    use_promotion = False


class TwoPlaneNoMMR(TwoPlane):
    """Ablation: does diversity re-ranking help when answers are single facts?"""

    name = "two_plane_no_mmr"
    use_mmr_plane = False
    use_promotion = False


class ControlHop(VersionedFactStore):
    """Control plane + second hop only — the minimal combination that covers
    both freshness and multi-hop."""

    name = "control_hop"

    def retrieve(self, query: dict) -> list[str]:
        base = super().retrieve(query)
        if not base:
            return []
        return second_hop(self.index, query, base, set(self._eligible(query)), self.k)


def build_all(index: Index, objects_to_predicate: dict[str, str],
              projects: list[str]) -> list[Strategy]:
    def ex() -> Extractor:
        return Extractor(objects_to_predicate, projects)

    return [
        FullContext(index),
        RecencyWindow(index),
        BM25Only(index),
        DenseOnly(index),
        Hybrid(index),
        HybridTuned(index),
        HybridRRF(index),
        HybridDecay(index),
        HybridMMR(index),
        GraphExpansion(index),
        BoundedConsolidated(index, extractor=ex()),
        VersionedFactStore(index, extractor=ex()),
        EarnedPromotion(index),
        TwoPlane(index, extractor=ex()),
        TwoPlaneNoPromo(index, extractor=ex()),
        TwoPlaneNoMMR(index, extractor=ex()),
        ControlHop(index, extractor=ex()),
    ]
