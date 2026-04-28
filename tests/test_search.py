"""Vector search tests using FakeEmbedder (deterministic hashing)."""

from __future__ import annotations

from pathlib import Path

import pytest

from smolbren.config import load_config, write_default_config
from smolbren.embed import embed_pending
from smolbren.errors import SearchError
from smolbren.index import connect
from smolbren.ingest import ingest_vault
from smolbren.search import vector_search

from .fake_embedder import FakeEmbedder


def _build_vault_with_known_content(root: Path) -> None:
    write_default_config(root)
    pages = {
        "people/jane.md": (
            "---\ntype: person\ntitle: Jane Doe\n---\n\n# Jane\n\n"
            "## Role\n\nJane owns the snowflake adapter and warehouse pipeline.\n"
        ),
        "systems/snowflake.md": (
            "---\ntype: system\ntitle: dbt Snowflake adapter\n---\n\n# dbt Snowflake\n\n"
            "## Overview\n\nThe snowflake adapter handles warehouse connections and migrations.\n"
        ),
        "notes/cooking.md": (
            "---\ntype: doc\ntitle: Cooking pasta\n---\n\n# Cooking\n\n"
            "## Recipe\n\nBoil water, add salt, cook pasta until al dente.\n"
        ),
    }
    for rel, body in pages.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(body, encoding="utf-8")


def test_vector_search_returns_relevant_first(tmp_path: Path) -> None:
    _build_vault_with_known_content(tmp_path)
    embedder = FakeEmbedder()
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)
        hits = vector_search(
            conn, cfg, "snowflake adapter warehouse", top_k=3, embedder=embedder
        )
        assert hits, "expected at least one hit"
        assert hits[0].slug in {"systems/snowflake", "people/jane"}
        # cooking page should not outrank the technical pages
        assert hits[0].slug != "notes/cooking"
        assert all(-1.0 <= h.score <= 1.0 for h in hits)
    finally:
        conn.close()


def test_vector_search_top_k_caps_results(tmp_path: Path) -> None:
    _build_vault_with_known_content(tmp_path)
    embedder = FakeEmbedder()
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)
        hits = vector_search(conn, cfg, "anything", top_k=2, embedder=embedder)
        assert len(hits) <= 2
    finally:
        conn.close()


def test_vector_search_empty_query_errors(tmp_path: Path) -> None:
    _build_vault_with_known_content(tmp_path)
    embedder = FakeEmbedder()
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)
        with pytest.raises(SearchError):
            vector_search(conn, cfg, "   ", top_k=3, embedder=embedder)
    finally:
        conn.close()


def test_vector_search_returns_empty_for_empty_index(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    embedder = FakeEmbedder()
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        hits = vector_search(conn, cfg, "anything", top_k=5, embedder=embedder)
        assert hits == []
    finally:
        conn.close()
