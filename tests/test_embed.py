"""Embed pipeline tests using FakeEmbedder."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from smolbren.config import load_config, write_default_config
from smolbren.embed import OllamaEmbedder, embed_pending
from smolbren.errors import EmbedError
from smolbren.index import EMBEDDING_DIM, chunks_without_embedding, connect
from smolbren.ingest import ingest_vault

from .fake_embedder import FakeEmbedder
from .synthetic_vault import build_vault


def _setup(tmp_path: Path, n_pages: int = 4) -> tuple[Path, FakeEmbedder]:
    build_vault(tmp_path, n_pages=n_pages)
    write_default_config(tmp_path)
    return tmp_path, FakeEmbedder()


def test_embed_pending_embeds_all_new_chunks(tmp_path: Path) -> None:
    vault, embedder = _setup(tmp_path, n_pages=3)
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        before = len(chunks_without_embedding(conn))
        assert before > 0

        result = embed_pending(conn, cfg, embedder=embedder)
        assert result.embedded == before
        assert result.cache_hits == 0
        assert chunks_without_embedding(conn) == []
    finally:
        conn.close()


def test_embed_pending_is_idempotent(tmp_path: Path) -> None:
    vault, embedder = _setup(tmp_path, n_pages=3)
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)
        before_calls = embedder.total_texts_embedded
        assert before_calls > 0

        # Re-run ingest; nothing changed → no new chunks → no new embed calls.
        ingest_vault(conn, cfg)
        result = embed_pending(conn, cfg, embedder=embedder)
        assert result.embedded == 0
        assert result.cache_hits == 0
        assert embedder.total_texts_embedded == before_calls
    finally:
        conn.close()


def test_embed_cache_avoids_recomputing_identical_chunk_text(tmp_path: Path) -> None:
    vault = tmp_path
    write_default_config(vault)
    p = vault / "a.md"
    p.write_text("---\ntype: doc\n---\n\n## Body\n\nidentical content here.\n", encoding="utf-8")
    embedder = FakeEmbedder()
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)
        first_calls = embedder.total_texts_embedded
        assert first_calls >= 1

        # Force the page row to change but keep the chunk text identical:
        # touch frontmatter only.
        p.write_text(
            "---\ntype: doc\nextra: 1\n---\n\n## Body\n\nidentical content here.\n",
            encoding="utf-8",
        )
        ingest_vault(conn, cfg)
        result = embed_pending(conn, cfg, embedder=embedder)
        assert result.embedded == 0, "chunk text unchanged → must come from cache"
        assert result.cache_hits >= 1
        assert embedder.total_texts_embedded == first_calls
    finally:
        conn.close()


class _StubResponse:
    def __init__(self, embeddings: list[list[float]]) -> None:
        self.embeddings = embeddings


class _StubClient:
    def __init__(
        self,
        *,
        embeddings: list[list[float]] | None = None,
        raises: Exception | None = None,
    ) -> None:
        self._embeddings = embeddings or []
        self._raises = raises
        self.calls: list[tuple[str, list[str]]] = []

    def embed(self, *, model: str, input: list[str]) -> _StubResponse:  # noqa: A002
        self.calls.append((model, list(input)))
        if self._raises is not None:
            raise self._raises
        return _StubResponse(self._embeddings)


def _patch_client(monkeypatch: pytest.MonkeyPatch, stub: _StubClient) -> None:
    def factory(host: str) -> _StubClient:
        return stub

    monkeypatch.setattr("smolbren.embed.ollama.Client", factory)


def test_ollama_embedder_normalizes_and_returns_vectors(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    raw: list[list[float]] = [[3.0, 4.0] + [0.0] * (EMBEDDING_DIM - 2)]
    _patch_client(monkeypatch, _StubClient(embeddings=raw))
    e = OllamaEmbedder(model="nomic-embed-text", base_url="http://localhost:11434")
    [vec] = e.embed(["hello"])
    # 3-4-0... has L2 norm 5; normalized → 0.6, 0.8.
    assert pytest.approx(vec[0], rel=1e-6) == 0.6
    assert pytest.approx(vec[1], rel=1e-6) == 0.8


def test_ollama_embedder_wraps_errors(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_client(monkeypatch, _StubClient(raises=RuntimeError("connection refused")))
    e = OllamaEmbedder(model="nomic-embed-text", base_url="http://localhost:11434")
    with pytest.raises(EmbedError, match="connection refused"):
        e.embed(["hi"])


def test_ollama_embedder_dim_mismatch_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_client(monkeypatch, _StubClient(embeddings=[[1.0, 2.0, 3.0]]))
    e = OllamaEmbedder(model="nomic-embed-text", base_url="http://localhost:11434")
    with pytest.raises(EmbedError, match="dim"):
        e.embed(["hi"])


def test_ollama_embedder_count_mismatch_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_client(
        monkeypatch, _StubClient(embeddings=[[1.0] * EMBEDDING_DIM] * 2)
    )
    e = OllamaEmbedder(model="nomic-embed-text", base_url="http://localhost:11434")
    with pytest.raises(EmbedError, match="for 1 inputs"):
        e.embed(["only one"])


def test_ollama_embedder_handles_dict_response(monkeypatch: pytest.MonkeyPatch) -> None:
    class _DictClient:
        def embed(self, *, model: str, input: list[str]) -> dict[str, Any]:  # noqa: A002
            return {"embeddings": [[1.0] + [0.0] * (EMBEDDING_DIM - 1) for _ in input]}

    monkeypatch.setattr("smolbren.embed.ollama.Client", lambda host: _DictClient())
    e = OllamaEmbedder(model="nomic-embed-text", base_url="http://localhost:11434")
    [vec] = e.embed(["hi"])
    assert vec[0] == 1.0


def test_embed_cleans_up_when_chunk_deleted(tmp_path: Path) -> None:
    vault = tmp_path
    write_default_config(vault)
    p = vault / "a.md"
    p.write_text("---\ntype: doc\n---\n\n## A\n\nfirst body\n", encoding="utf-8")
    embedder = FakeEmbedder()
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)
        # Replace the chunk with completely different text.
        p.write_text("---\ntype: doc\n---\n\n## B\n\ntotally different\n", encoding="utf-8")
        ingest_vault(conn, cfg)

        # New chunk → must be embedded fresh; old vec_chunks rows are gone.
        before = len(chunks_without_embedding(conn))
        assert before == 1
        result = embed_pending(conn, cfg, embedder=embedder)
        assert result.embedded == 1
    finally:
        conn.close()
