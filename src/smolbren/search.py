"""Search: vector (M2), keyword + RRF (M3), backlink boost (M5).

Currently implements vector search only. Keyword and hybrid arrive in M3.
"""

from __future__ import annotations

import sqlite3
from dataclasses import dataclass

import sqlite_vec

from .config import Config
from .embed import Embedder, make_embedder
from .errors import SearchError
from .index import EMBEDDING_DIM, get_chunk_contexts


@dataclass(frozen=True)
class SearchHit:
    chunk_id: int
    page_id: int
    slug: str
    title: str | None
    heading: str | None
    score: float  # cosine similarity in [-1, 1]; higher is better
    snippet: str


def _snippet(text: str, max_chars: int = 200) -> str:
    text = " ".join(text.split())
    return text if len(text) <= max_chars else text[: max_chars - 1].rstrip() + "…"


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
        hits.append(
            SearchHit(
                chunk_id=c.chunk_id,
                page_id=c.page_id,
                slug=c.slug,
                title=c.title,
                heading=c.heading,
                score=cos_sim,
                snippet=_snippet(c.text),
            )
        )
    return hits
