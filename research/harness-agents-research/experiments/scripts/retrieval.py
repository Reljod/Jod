"""Shared retrieval primitives: BM25, a distributional dense channel, fusion, MMR.

Both channels are computed once over the whole corpus and cached per query, so
every strategy is scored against *identical* signals. Differences in the
rankings therefore come from architecture, never from channel jitter.

The dense channel is Random Indexing (Kanerva 2000; Sahlgren 2005): each term
gets a sparse random signature, a term's context vector accumulates the
signatures of terms it co-occurs with, and a document is the tf-idf weighted
mean of its terms' context vectors. It is genuinely distributional — it matches
paraphrases that share no words — but it is weaker than a trained neural
embedder. See HYPOTHESES.md, limitation 1.
"""

from __future__ import annotations

import math
import random
import re
from collections import defaultdict
from operator import mul as _mul

DIM = 192
SIGNATURE_NNZ = 8
RI_SEED = 7717

K1 = 1.5
B = 0.75

_TOKEN_RE = re.compile(r"[a-z0-9][a-z0-9._-]*")
_STOP = {
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "to", "of", "in",
    "on", "for", "and", "or", "it", "its", "we", "our", "that", "this", "with",
    "as", "at", "by", "from", "into", "does", "do", "did", "what", "which",
    "who", "where", "how", "there", "not", "no", "but", "if", "then", "than",
    "so", "up", "out", "about", "over", "under", "again", "will", "would",
}


_SUFFIXES = (("ment", 6), ("ence", 6), ("ance", 6), ("ing", 5), ("ed", 4),
             ("es", 5), ("er", 5), ("s", 4))


def stem(token: str) -> str:
    """Conservative suffix stripping.

    Without it "owner" and "owned" are unrelated strings, which no real system
    suffers from — SQLite FTS5 ships a porter tokenizer and neural embedders
    handle morphology implicitly. Consistency matters more than linguistic
    correctness here: both sides of a comparison get the same transform.
    """
    if token.endswith("ss"):
        return token
    for suf, min_len in _SUFFIXES:
        if len(token) >= min_len and token.endswith(suf):
            return token[: -len(suf)]
    return token


def tokenize(text: str) -> list[str]:
    return [stem(t) for t in _TOKEN_RE.findall(text.lower())
            if t not in _STOP and len(t) > 1]


def estimate_tokens(text: str) -> int:
    return max(1, len(text) // 4)


def jaccard(a: set[str], b: set[str]) -> float:
    if not a or not b:
        return 0.0
    return len(a & b) / len(a | b)


class Index:
    """Immutable index over the full corpus, shared by every strategy."""

    def __init__(self, chunks: list[dict]) -> None:
        self.chunks = chunks
        self.by_id = {c["id"]: c for c in chunks}
        self.ids = [c["id"] for c in chunks]
        self.tokens = {c["id"]: tokenize(c["text"]) for c in chunks}
        self.token_sets = {cid: set(t) for cid, t in self.tokens.items()}
        self.tok_count = {c["id"]: estimate_tokens(c["text"]) for c in chunks}

        # ---- BM25 statistics ----
        self.df: dict[str, int] = defaultdict(int)
        self.tf: dict[str, dict[str, int]] = {}
        for cid, toks in self.tokens.items():
            counts: dict[str, int] = defaultdict(int)
            for t in toks:
                counts[t] += 1
            self.tf[cid] = dict(counts)
            for t in counts:
                self.df[t] += 1
        self.n_docs = len(chunks)
        self.avg_len = sum(len(t) for t in self.tokens.values()) / max(1, self.n_docs)
        self.postings: dict[str, list[str]] = defaultdict(list)
        for cid, counts in self.tf.items():
            for t in counts:
                self.postings[t].append(cid)
        self.idf = {
            t: math.log(1 + (self.n_docs - d + 0.5) / (d + 0.5))
            for t, d in self.df.items()
        }

        self._build_dense()
        self._dense_cache: dict[str, list[tuple[str, float]]] = {}
        self._bm25_cache: dict[str, list[tuple[str, float]]] = {}

    # ------------------------------------------------------------------ dense
    def _build_dense(self) -> None:
        rng = random.Random(RI_SEED)
        vocab = list(self.df)
        signature: dict[str, list[tuple[int, float]]] = {}
        for t in vocab:
            positions = rng.sample(range(DIM), SIGNATURE_NNZ)
            signature[t] = [(p, 1.0 if rng.random() < 0.5 else -1.0) for p in positions]

        # Term context vectors: accumulate the signatures of co-occurring terms.
        context: dict[str, list[float]] = {t: [0.0] * DIM for t in vocab}
        for cid, toks in self.tokens.items():
            uniq = set(toks)
            if len(uniq) < 2:
                continue
            # Weight each contribution by idf, or ubiquitous terms (project
            # names appearing in hundreds of chunks) dominate every context
            # vector and the whole space collapses toward one direction.
            pooled = [0.0] * DIM
            for t in uniq:
                w = self.idf.get(t, 1.0)
                for p, v in signature[t]:
                    pooled[p] += w * v
            for t in uniq:
                ctx = context[t]
                w = self.idf.get(t, 1.0)
                for i in range(DIM):
                    ctx[i] += pooled[i]
                for p, v in signature[t]:      # remove self-contribution
                    ctx[p] -= w * v

        # Blend in the term's own signature so rare exact terms stay separable.
        for t in vocab:
            ctx = context[t]
            norm = math.sqrt(sum(x * x for x in ctx)) or 1.0
            for i in range(DIM):
                ctx[i] /= norm
            for p, v in signature[t]:
                ctx[p] += 0.6 * v
            norm = math.sqrt(sum(x * x for x in ctx)) or 1.0
            for i in range(DIM):
                ctx[i] /= norm
        self.term_vec = context

        self.doc_vec: dict[str, list[float]] = {}
        for cid, toks in self.tokens.items():
            self.doc_vec[cid] = self._embed_tokens(toks)

    def _embed_tokens(self, toks: list[str]) -> list[float]:
        vec = [0.0] * DIM
        if not toks:
            return vec
        counts: dict[str, int] = defaultdict(int)
        for t in toks:
            counts[t] += 1
        for t, c in counts.items():
            tv = self.term_vec.get(t)
            if tv is None:
                continue
            w = (1.0 + math.log(c)) * self.idf.get(t, 1.0)
            for i in range(DIM):
                vec[i] += w * tv[i]
        norm = math.sqrt(sum(x * x for x in vec)) or 1.0
        return [x / norm for x in vec]

    # ---------------------------------------------------------------- ranking
    def rank_dense(self, qid: str, text: str) -> list[tuple[str, float]]:
        hit = self._dense_cache.get(qid)
        if hit is not None:
            return hit
        qv = self._embed_tokens(tokenize(text))
        mul = _mul
        doc_vec = self.doc_vec
        scored = [(cid, sum(map(mul, qv, doc_vec[cid]))) for cid in self.ids]
        scored.sort(key=lambda x: -x[1])
        self._dense_cache[qid] = scored
        return scored

    def rank_bm25(self, qid: str, text: str) -> list[tuple[str, float]]:
        hit = self._bm25_cache.get(qid)
        if hit is not None:
            return hit
        qtoks = tokenize(text)
        scores: dict[str, float] = defaultdict(float)
        for t in set(qtoks):
            if t not in self.postings:
                continue
            idf = self.idf[t]
            for cid in self.postings[t]:
                f = self.tf[cid][t]
                dl = len(self.tokens[cid])
                denom = f + K1 * (1 - B + B * dl / self.avg_len)
                scores[cid] += idf * (f * (K1 + 1)) / denom
        ranked = sorted(scores.items(), key=lambda x: -x[1])
        self._bm25_cache[qid] = ranked
        return ranked


def bm25_rank_to_score(rank: int) -> float:
    """OpenClaw's normalisation, verified in extensions/memory-core/.../hybrid.ts."""
    return 1.0 / (1.0 + rank)


def importance_multiplier(importance: int | None) -> float:
    """OpenClaw's importance.ts: bounded 1-10, mapped to 0.80-1.25."""
    if importance is None:
        return 1.0
    bounded = max(1, min(10, int(importance)))
    return 0.75 + bounded * 0.05


def temporal_decay(age_days: float, half_life_days: float = 30.0) -> float:
    """OpenClaw's temporal-decay.ts."""
    if half_life_days <= 0:
        return 1.0
    lam = math.log(2) / half_life_days
    return math.exp(-lam * max(0.0, age_days))


RRF_K = 60


def fuse(
    index: Index,
    qid: str,
    text: str,
    eligible: set[str] | None,
    vector_weight: float = 0.7,
    text_weight: float = 0.3,
    pool: int = 400,
    mode: str = "linear",
) -> list[tuple[str, float]]:
    """Fuse the two channels.

    mode="linear" reproduces OpenClaw's shipped recipe: clamped cosine from the
    dense side, reciprocal rank from BM25, combined by weight. That mixes two
    incomparable scales — whichever channel happens to produce larger numbers
    dominates regardless of its weight.

    mode="rrf" is Reciprocal Rank Fusion (Cormack et al. 2009): both channels
    contribute 1/(K+rank), so only rank order matters and the weights mean what
    they say. Comparing the two isolates how much the scale mismatch costs.
    """
    total = vector_weight + text_weight
    vw = vector_weight / total if total > 0 else 0.7
    tw = text_weight / total if total > 0 else 0.3

    merged: dict[str, list[float]] = {}
    taken = 0
    for rank, (cid, s) in enumerate(index.rank_dense(qid, text)):
        if eligible is not None and cid not in eligible:
            continue
        val = 1.0 / (RRF_K + taken) if mode == "rrf" else max(0.0, min(1.0, s))
        merged[cid] = [val, 0.0]
        taken += 1
        if taken >= pool:
            break

    taken = 0
    for rank, (cid, _s) in enumerate(index.rank_bm25(qid, text)):
        if eligible is not None and cid not in eligible:
            continue
        entry = merged.setdefault(cid, [0.0, 0.0])
        entry[1] = (1.0 / (RRF_K + taken)) if mode == "rrf" else bm25_rank_to_score(taken)
        taken += 1
        if taken >= pool:
            break

    out = [(cid, vw * v[0] + tw * v[1]) for cid, v in merged.items()]
    out.sort(key=lambda x: -x[1])
    return out


def expansion_terms(
    index: Index,
    seed_ids: list[str],
    query_text: str,
    top_n: int = 3,
) -> list[str]:
    """Pseudo-relevance feedback: the rarest new terms in the top hits.

    A second hop needs a bridge term the question never contained — "who owns
    atlas" retrieves "atlas is owned by dana", and only *then* is "dana" a
    usable query. This is the classical Rocchio move rather than an entity
    graph, so it needs no extracted schema and cannot leak gold labels.
    """
    qt = set(tokenize(query_text))
    best: dict[str, float] = {}
    for cid in seed_ids:
        for t in index.token_sets.get(cid, ()):
            if t in qt:
                continue
            best[t] = max(best.get(t, 0.0), index.idf.get(t, 0.0))
    return [t for t, _ in sorted(best.items(), key=lambda x: -x[1])[:top_n]]


def mmr(
    index: Index,
    ranked: list[tuple[str, float]],
    k: int,
    lam: float = 0.7,
) -> list[tuple[str, float]]:
    """Maximal Marginal Relevance, Jaccard similarity — OpenClaw's mmr.ts."""
    if len(ranked) <= 1:
        return ranked[:k]
    pool = ranked[: max(k * 6, 30)]
    selected: list[tuple[str, float]] = []
    remaining = list(pool)
    while remaining and len(selected) < k:
        best_i, best_v = 0, -1e9
        for i, (cid, score) in enumerate(remaining):
            sim = 0.0
            for scid, _ in selected:
                sim = max(sim, jaccard(index.token_sets[cid], index.token_sets[scid]))
            val = lam * score - (1 - lam) * sim
            if val > best_v:
                best_i, best_v = i, val
        selected.append(remaining.pop(best_i))
    return selected
