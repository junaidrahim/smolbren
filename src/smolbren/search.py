"""Search: vector, keyword (FTS5), and hybrid (RRF fusion).

Public surface:
    SearchHit             — a single ranked result
    rrf                   — pure Reciprocal Rank Fusion of integer rankings
    vector_search         — semantic search via sqlite-vec
    keyword_search        — bag-of-words search via FTS5 / BM25
    hybrid_search         — RRF fusion of vector + keyword (default mode)
"""

from __future__ import annotations

import math
import re
import sqlite3
from collections import defaultdict
from collections.abc import Sequence
from dataclasses import dataclass

import sqlite_vec

from .config import Config
from .embed import Embedder, make_embedder
from .errors import SearchError
from .index import EMBEDDING_DIM, ChunkContext, get_chunk_contexts


@dataclass(frozen=True)
class SearchHit:
    chunk_id: int
    page_id: int
    slug: str
    title: str | None
    heading: str | None
    score: float
    snippet: str


SNIPPET_CHARS = 200


def _snippet(text: str, max_chars: int = SNIPPET_CHARS) -> str:
    text = " ".join(text.split())
    return text if len(text) <= max_chars else text[: max_chars - 1].rstrip() + "…"


def _hit_from_context(
    c: ChunkContext, score: float, *, snippet: str | None = None
) -> SearchHit:
    return SearchHit(
        chunk_id=c.chunk_id,
        page_id=c.page_id,
        slug=c.slug,
        title=c.title,
        heading=c.heading,
        score=score,
        snippet=snippet if snippet is not None else _snippet(c.text),
    )


# --- vector ----------------------------------------------------------------


def vector_search(
    conn: sqlite3.Connection,
    config: Config,
    query: str,
    *,
    top_k: int = 5,
    embedder: Embedder | None = None,
) -> list[SearchHit]:
    if not query.strip():
        raise SearchError("Empty query")
    if top_k <= 0:
        raise SearchError("top_k must be > 0")

    embedder = embedder or make_embedder(config)
    if embedder.dim != EMBEDDING_DIM:  # pragma: no cover (config sanity)
        raise SearchError(
            f"Embedder dim {embedder.dim} does not match index dim {EMBEDDING_DIM}"
        )
    [query_vec] = embedder.embed([query])
    qbytes = sqlite_vec.serialize_float32(query_vec)

    rows = conn.execute(
        """
        SELECT chunk_id, distance
        FROM vec_chunks
        WHERE embedding MATCH ? AND k = ?
        ORDER BY distance
        """,
        (qbytes, top_k),
    ).fetchall()

    chunk_ids = [int(r[0]) for r in rows]
    if not chunk_ids:
        return []
    ctx = get_chunk_contexts(conn, chunk_ids)

    hits: list[SearchHit] = []
    for r in rows:
        chunk_id = int(r[0])
        # vec_chunks default distance for FLOAT[N] is L2 on (we store) unit
        # vectors. For unit vectors: cos_sim = 1 - L2² / 2.
        l2 = float(r[1])
        cos_sim = max(-1.0, min(1.0, 1.0 - (l2 * l2) / 2.0))
        c = ctx.get(chunk_id)
        if c is None:  # pragma: no cover (race / orphan)
            continue
        hits.append(_hit_from_context(c, score=cos_sim))
    return hits


# --- keyword (FTS5) --------------------------------------------------------


_WORD = re.compile(r"\w+", re.UNICODE)
_PHRASE = re.compile(r'"([^"]+)"')


def build_fts_query(query: str) -> str:
    """Build a safe FTS5 MATCH expression from free-form user input.

    - Double-quoted spans become literal phrases (their tokens stay
      adjacent at search time).
    - Other tokens are quoted individually so FTS5 special characters
      (`*`, `(`, `:`, `?`, etc.) can't slip through and break parsing.
    - Tokens are joined with `OR` for recall — BM25 still ranks by
      coverage and term importance, and the hybrid fusion uses the
      vector branch to tighten precision.
    - Returns "" if the input has no searchable tokens; callers should
      treat that as "no keyword results".
    """
    text = query.strip()
    if not text:
        return ""
    parts: list[str] = []
    for match in _PHRASE.finditer(text):
        tokens = _WORD.findall(match.group(1))
        if tokens:
            parts.append('"' + " ".join(tokens) + '"')
    text_no_phrases = _PHRASE.sub(" ", text)
    for tok in _WORD.findall(text_no_phrases):
        parts.append(f'"{tok}"')
    return " OR ".join(parts)


def keyword_search(
    conn: sqlite3.Connection,
    query: str,
    *,
    top_k: int = 5,
) -> list[SearchHit]:
    if not query.strip():
        raise SearchError("Empty query")
    if top_k <= 0:
        raise SearchError("top_k must be > 0")

    fts_query = build_fts_query(query)
    if not fts_query:
        return []

    rows = conn.execute(
        """
        SELECT rowid AS chunk_id, bm25(fts_chunks) AS score
        FROM fts_chunks
        WHERE fts_chunks MATCH ?
        ORDER BY score
        LIMIT ?
        """,
        (fts_query, top_k),
    ).fetchall()

    chunk_ids = [int(r[0]) for r in rows]
    if not chunk_ids:
        return []
    ctx = get_chunk_contexts(conn, chunk_ids)

    hits: list[SearchHit] = []
    for r in rows:
        chunk_id = int(r[0])
        # bm25 returns lower-is-better. Negate so a SearchHit's `score` field
        # stays "higher is better" across modes — convenient for sorting and
        # display, even though the absolute values aren't comparable across
        # modes.
        score = -float(r[1])
        c = ctx.get(chunk_id)
        if c is None:  # pragma: no cover (race / orphan)
            continue
        hits.append(_hit_from_context(c, score=score))
    return hits


# --- RRF fusion ------------------------------------------------------------


def rrf(
    rankings: Sequence[Sequence[int]],
    k: int = 60,
    weights: Sequence[float] | None = None,
) -> list[tuple[int, float]]:
    """Reciprocal Rank Fusion.

    Each input ranking is a list of chunk_ids in best-first order. RRF
    contributes `weight / (k + rank)` for each appearance, where `rank`
    is 0-indexed. Returns (chunk_id, score) sorted by descending score.
    """
    if weights is None:
        weights = [1.0] * len(rankings)
    if len(weights) != len(rankings):
        raise SearchError(
            f"rrf weights length {len(weights)} != rankings length {len(rankings)}"
        )
    scores: dict[int, float] = defaultdict(float)
    for w, ranking in zip(weights, rankings, strict=True):
        if w == 0.0:
            continue
        for rank, chunk_id in enumerate(ranking):
            scores[chunk_id] += w / (k + rank)
    return sorted(scores.items(), key=lambda kv: -kv[1])


def hybrid_search(
    conn: sqlite3.Connection,
    config: Config,
    query: str,
    *,
    top_k: int = 5,
    embedder: Embedder | None = None,
    overfetch: int = 3,
) -> list[SearchHit]:
    """Run vector + keyword search and fuse with RRF, then apply backlink boost.

    Each branch is asked for `top_k * overfetch` candidates so RRF has enough
    overlap to do useful fusion. After fusion, we multiply each fused score
    by `1 + backlink_boost · log(1 + backlinks)` — popular pages float up
    when scores are otherwise close. Boost reads `config.search.backlink_boost`
    (set to 0 to disable). Top_k slice happens AFTER the boost so a popular
    page can knock a less-cited but slightly-higher-RRF page off the list.
    """
    if top_k <= 0:
        raise SearchError("top_k must be > 0")
    candidate_k = max(top_k * overfetch, top_k)

    vec_hits = vector_search(
        conn, config, query, top_k=candidate_k, embedder=embedder
    )
    try:
        kw_hits = keyword_search(conn, query, top_k=candidate_k)
    except SearchError:
        kw_hits = []

    vec_weight, kw_weight = config.search.hybrid_weights
    fused = rrf(
        [
            [h.chunk_id for h in vec_hits],
            [h.chunk_id for h in kw_hits],
        ],
        k=config.search.rrf_k,
        weights=[vec_weight, kw_weight],
    )
    if not fused:
        return []

    # Hydrate hits before we apply backlink boost (we need slugs).
    by_id: dict[int, SearchHit] = {h.chunk_id: h for h in vec_hits}
    for h in kw_hits:
        by_id.setdefault(h.chunk_id, h)
    missing = [cid for cid, _ in fused if cid not in by_id]
    if missing:
        ctx = get_chunk_contexts(conn, missing)
        for cid in missing:
            c = ctx.get(cid)
            if c is None:  # pragma: no cover
                continue
            by_id[cid] = _hit_from_context(c, score=0.0)

    boost_coef = config.search.backlink_boost
    if boost_coef > 0.0:
        # Local import to avoid a circular dep (graph imports nothing search-y,
        # but the rule "search → graph for ranking only" stays one-way).
        from .graph import backlink_counts

        slugs = sorted({by_id[cid].slug for cid, _ in fused if cid in by_id})
        bl = backlink_counts(conn, slugs)
        boosted: list[tuple[int, float]] = []
        for cid, score in fused:
            base = by_id.get(cid)
            if base is None:  # pragma: no cover
                continue
            multiplier = 1.0 + boost_coef * math.log1p(bl.get(base.slug, 0))
            boosted.append((cid, score * multiplier))
        fused = sorted(boosted, key=lambda kv: -kv[1])

    fused = fused[:top_k]

    out: list[SearchHit] = []
    for cid, fused_score in fused:
        base = by_id.get(cid)
        if base is None:  # pragma: no cover
            continue
        out.append(
            SearchHit(
                chunk_id=base.chunk_id,
                page_id=base.page_id,
                slug=base.slug,
                title=base.title,
                heading=base.heading,
                score=fused_score,
                snippet=base.snippet,
            )
        )
    return out
