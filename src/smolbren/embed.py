"""Embeddings: Ollama client, content-hash cache, batch writer.

The cache lives in `embedding_cache(content_hash, model) → vector`. It survives
chunk deletions, so renaming a file or recreating identical content costs zero
Ollama calls. `vec_chunks` rows are materialized from cache on hit.
"""

from __future__ import annotations

import logging
import math
import sqlite3
import time
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Protocol

import ollama
import sqlite_vec

from .config import Config
from .errors import EmbedError
from .index import (
    EMBEDDING_DIM,
    chunks_without_embedding,
    lookup_cached_embedding,
    store_embedding,
    transaction,
)

log = logging.getLogger(__name__)


class Embedder(Protocol):
    """A pure function from text → unit-length vector (dim=EMBEDDING_DIM)."""

    model: str
    dim: int

    def embed(self, texts: list[str]) -> list[list[float]]: ...


def _l2_normalize(vec: list[float]) -> list[float]:
    norm = math.sqrt(sum(x * x for x in vec)) or 1.0
    return [x / norm for x in vec]


@dataclass
class OllamaEmbedder:
    """Embedder backed by a local Ollama server.

    Vectors are L2-normalized after the call so cosine similarity ==
    1 - L2_distance²/2; that lets us use either metric interchangeably.
    """

    model: str
    base_url: str
    dim: int = EMBEDDING_DIM
    _client: ollama.Client | None = None

    def __post_init__(self) -> None:
        self._client = ollama.Client(host=self.base_url)

    def embed(self, texts: list[str]) -> list[list[float]]:
        if not texts:
            return []
        if self._client is None:  # pragma: no cover (safety)
            raise EmbedError("Ollama client not initialized")
        try:
            response = self._client.embed(model=self.model, input=texts)
        except Exception as e:  # ollama raises a variety; normalize
            raise EmbedError(
                f"Ollama embed call failed (model={self.model}, host={self.base_url}): {e}"
            ) from e
        vectors_obj = getattr(response, "embeddings", None)
        if vectors_obj is None and isinstance(response, dict):
            vectors_obj = response.get("embeddings")
        if vectors_obj is None:
            raise EmbedError(f"Ollama returned no embeddings (response={response!r})")
        vectors: list[list[float]] = [list(v) for v in vectors_obj]
        if len(vectors) != len(texts):
            raise EmbedError(
                f"Ollama returned {len(vectors)} embeddings for {len(texts)} inputs"
            )
        for i, v in enumerate(vectors):
            if len(v) != self.dim:
                raise EmbedError(
                    f"Embedding {i} has dim {len(v)}, expected {self.dim}"
                )
        return [_l2_normalize(v) for v in vectors]


def make_embedder(config: Config) -> OllamaEmbedder:
    return OllamaEmbedder(
        model=config.embeddings.model,
        base_url=config.embeddings.ollama_url,
    )


@dataclass(frozen=True)
class EmbedResult:
    embedded: int  # actually embedded via the Embedder
    cache_hits: int  # served from embedding_cache
    duration_s: float


def _serialize(vec: Iterable[float]) -> bytes:
    # sqlite-vec exposes a packer that writes the FLOAT[N] format vec0 expects.
    packed: bytes = sqlite_vec.serialize_float32(list(vec))
    return packed


def embed_pending(
    conn: sqlite3.Connection,
    config: Config,
    *,
    embedder: Embedder | None = None,
    batch_size: int = 32,
) -> EmbedResult:
    """Resolve all chunks missing a vec_chunks row.

    Cache hits reuse the stored vector. Cache misses are batched to the
    Embedder. Both paths write `vec_chunks`; misses also populate the cache.
    """
    started = time.perf_counter()
    pending = chunks_without_embedding(conn)
    if not pending:
        return EmbedResult(embedded=0, cache_hits=0, duration_s=time.perf_counter() - started)

    embedder = embedder or make_embedder(config)
    model = embedder.model
    dim = embedder.dim
    cache_hits = 0
    misses: list[tuple[int, str, str]] = []  # (chunk_id, content_hash, text)

    with transaction(conn):
        for ch in pending:
            cached = lookup_cached_embedding(
                conn, content_hash=ch.content_hash, model=model
            )
            if cached is not None:
                store_embedding(
                    conn,
                    chunk_id=ch.chunk_id,
                    content_hash=ch.content_hash,
                    model=model,
                    dim=dim,
                    embedding_bytes=cached,
                    cache=False,  # already cached
                )
                cache_hits += 1
            else:
                misses.append((ch.chunk_id, ch.content_hash, ch.text))

    embedded = 0
    for batch_start in range(0, len(misses), batch_size):
        batch = misses[batch_start : batch_start + batch_size]
        texts = [t for _, _, t in batch]
        vectors = embedder.embed(texts)
        with transaction(conn):
            for (chunk_id, content_hash, _), vec in zip(batch, vectors, strict=True):
                store_embedding(
                    conn,
                    chunk_id=chunk_id,
                    content_hash=content_hash,
                    model=model,
                    dim=dim,
                    embedding_bytes=_serialize(vec),
                    cache=True,
                )
                embedded += 1

    return EmbedResult(
        embedded=embedded,
        cache_hits=cache_hits,
        duration_s=time.perf_counter() - started,
    )
