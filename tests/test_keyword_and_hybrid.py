"""Keyword (FTS5) + hybrid search tests, plus a small MRR-based eval that
asserts the M3 acceptance criterion: hybrid ≥ max(vector, keyword) on a
hand-built fixture vault."""

from __future__ import annotations

import os
import sqlite3
from collections.abc import Sequence
from pathlib import Path

import pytest

from smolbren.config import load_config, write_default_config
from smolbren.embed import embed_pending
from smolbren.errors import SearchError
from smolbren.index import connect
from smolbren.ingest import ingest_vault
from smolbren.search import (
    SearchHit,
    build_fts_query,
    hybrid_search,
    keyword_search,
    vector_search,
)

from .fake_embedder import FakeEmbedder

# --- fixture vault ---------------------------------------------------------

EVAL_PAGES: dict[str, str] = {
    # Real ownership / system pages
    "people/jane.md": (
        "---\ntype: person\ntitle: Jane Doe\n---\n\n# Jane Doe\n\n"
        "## Role\n\nJane owns the snowflake adapter and warehouse pipeline. "
        "She handles warehouse connections and migrations day-to-day.\n"
    ),
    "systems/snowflake-adapter.md": (
        "---\ntype: system\ntitle: dbt Snowflake adapter\n---\n\n# dbt Snowflake adapter\n\n"
        "## Overview\n\nThe snowflake adapter handles warehouse connections, "
        "migrations, and schema evolution for the analytics pipeline.\n"
    ),
    "systems/adapter-framework.md": (
        "---\ntype: system\ntitle: Adapter framework\n---\n\n# Adapter framework\n\n"
        "## Description\n\nOur internal adapter framework lets each "
        "warehouse-specific connector plug in cleanly.\n"
    ),
    "decisions/oncall-rotation.md": (
        "---\ntype: decision\ntitle: On-call rotation policy\n---\n\n# On-call\n\n"
        "## Policy\n\nThe team rotates oncall duty weekly. Pager goes to "
        "whoever is on the rotation that week.\n"
    ),
    "people/bob.md": (
        "---\ntype: person\ntitle: Bob Smith\n---\n\n# Bob Smith\n\n"
        "## Role\n\nBob is on the on-call rotation and handles the alerting "
        "stack and dashboards.\n"
    ),
    # Lexical-noise pages — share keywords with queries but aren't relevant.
    # This is what makes pure-keyword search fall down on conceptual queries.
    "notes/warehouse-floor.md": (
        "---\ntype: doc\ntitle: Warehouse floor layout\n---\n\n# Warehouse layout\n\n"
        "## Floor plan\n\nThe warehouse storage floor is divided into bays. "
        "Forklifts move pallets between the warehouse zones.\n"
    ),
    "decisions/framework-choice.md": (
        "---\ntype: decision\ntitle: Decision framework selection\n---\n\n# Framework\n\n"
        "## Process\n\nWe evaluated several decision-making frameworks before "
        "settling on the lightweight one. Our framework choice impacts every "
        "team meeting.\n"
    ),
    "notes/cooking.md": (
        "---\ntype: doc\ntitle: Cooking pasta\n---\n\n# Cooking\n\n"
        "## Recipe\n\nBoil water, add salt, cook pasta until al dente.\n"
    ),
    "notes/airline-tickets.md": (
        "---\ntype: doc\ntitle: Booking flights\n---\n\n# Flights\n\n"
        "## Tips\n\nBook ticket connections through the carrier's app. "
        "Layovers are easier when terminals are connected.\n"
    ),
}


def _build_eval_vault(root: Path) -> None:
    write_default_config(root)
    for rel, body in EVAL_PAGES.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(body, encoding="utf-8")


def _setup(tmp_path: Path) -> tuple[FakeEmbedder, sqlite3.Connection]:
    _build_eval_vault(tmp_path)
    cfg = load_config(tmp_path)
    embedder = FakeEmbedder()
    conn = connect(cfg.db_path)
    ingest_vault(conn, cfg)
    embed_pending(conn, cfg, embedder=embedder)
    return embedder, conn


# --- build_fts_query unit tests --------------------------------------------


def test_fts_query_quotes_each_token() -> None:
    assert build_fts_query("hello world") == '"hello" OR "world"'


def test_fts_query_preserves_phrase_quotes() -> None:
    out = build_fts_query('the "adapter framework" rocks')
    # phrases first, then leftover tokens, joined with OR
    assert '"adapter framework"' in out
    assert ' OR ' in out
    assert '"rocks"' in out
    assert '"the"' in out


def test_fts_query_strips_punctuation() -> None:
    # Apostrophes / colons / parens would all break raw FTS5 MATCH.
    assert build_fts_query("who's the (warehouse) owner?") == (
        '"who" OR "s" OR "the" OR "warehouse" OR "owner"'
    )


def test_fts_query_empty() -> None:
    assert build_fts_query("") == ""
    assert build_fts_query("   ") == ""
    assert build_fts_query("!!!") == ""


# --- keyword search basics -------------------------------------------------


def test_keyword_search_finds_exact_phrase(tmp_path: Path) -> None:
    embedder, conn = _setup(tmp_path)
    try:
        hits = keyword_search(conn, '"adapter framework"', top_k=3)
        assert hits
        assert hits[0].slug == "systems/adapter-framework"
    finally:
        conn.close()


def test_keyword_search_returns_empty_for_no_match(tmp_path: Path) -> None:
    embedder, conn = _setup(tmp_path)
    try:
        hits = keyword_search(conn, "quantumchromodynamics", top_k=5)
        assert hits == []
    finally:
        conn.close()


def test_keyword_search_punctuated_query_does_not_crash(tmp_path: Path) -> None:
    embedder, conn = _setup(tmp_path)
    try:
        hits = keyword_search(conn, "who's on-call this week?", top_k=5)
        slugs = [h.slug for h in hits]
        # Both Bob's page and the oncall decision page mention oncall/rotation.
        assert "people/bob" in slugs or "decisions/oncall-rotation" in slugs
    finally:
        conn.close()


def test_keyword_search_empty_query_raises(tmp_path: Path) -> None:
    embedder, conn = _setup(tmp_path)
    try:
        with pytest.raises(SearchError):
            keyword_search(conn, "   ", top_k=3)
    finally:
        conn.close()


# --- hybrid mechanics ------------------------------------------------------


def test_hybrid_returns_top_k_at_most(tmp_path: Path) -> None:
    embedder, conn = _setup(tmp_path)
    try:
        cfg = load_config(tmp_path)
        hits = hybrid_search(conn, cfg, "warehouse adapter", top_k=2, embedder=embedder)
        assert len(hits) <= 2
        # Fused score is monotone non-increasing.
        scores = [h.score for h in hits]
        assert scores == sorted(scores, reverse=True)
    finally:
        conn.close()


def test_hybrid_handles_keyword_miss(tmp_path: Path) -> None:
    """Conceptual queries with no exact word match shouldn't crash hybrid."""
    embedder, conn = _setup(tmp_path)
    try:
        cfg = load_config(tmp_path)
        hits = hybrid_search(conn, cfg, "managing data warehouses", top_k=3, embedder=embedder)
        assert hits  # vector branch should still return something
    finally:
        conn.close()


# --- FTS5 trigger sync ------------------------------------------------------


def test_keyword_search_reflects_chunk_replacements(tmp_path: Path) -> None:
    """Editing a file → trigger fires → FTS5 reflects the new content."""
    write_default_config(tmp_path)
    p = tmp_path / "n.md"
    p.write_text(
        "---\ntype: doc\n---\n\n## A\n\nzebrafish populations declining\n", encoding="utf-8"
    )
    cfg = load_config(tmp_path)
    embedder = FakeEmbedder()
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)

        hits = keyword_search(conn, "zebrafish", top_k=5)
        assert any(h.slug == "n" for h in hits)

        # Replace content; "zebrafish" must no longer match.
        p.write_text(
            "---\ntype: doc\n---\n\n## A\n\nseagulls fly south now\n", encoding="utf-8"
        )
        ingest_vault(conn, cfg)
        hits = keyword_search(conn, "zebrafish", top_k=5)
        assert hits == []
        hits = keyword_search(conn, "seagulls", top_k=5)
        assert any(h.slug == "n" for h in hits)
    finally:
        conn.close()


def test_keyword_search_reflects_file_deletion(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    p = tmp_path / "deleteme.md"
    p.write_text("---\ntype: doc\n---\n\n## A\n\nuniqueword12345\n", encoding="utf-8")
    cfg = load_config(tmp_path)
    embedder = FakeEmbedder()
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)
        assert keyword_search(conn, "uniqueword12345", top_k=5)

        p.unlink()
        ingest_vault(conn, cfg)
        assert keyword_search(conn, "uniqueword12345", top_k=5) == []
    finally:
        conn.close()


# --- M3 acceptance: hybrid ≥ max(vector, keyword) on the eval set ----------


@pytest.mark.skipif(
    "OLLAMA_URL" not in os.environ,
    reason="set OLLAMA_URL=http://localhost:11434 to run the real-Ollama eval",
)
def test_hybrid_beats_or_matches_pure_modes_on_eval_set(tmp_path: Path) -> None:
    # Regression-guard: hybrid is never worse than either pure mode on the
    # fixture vault (MRR + P@1). On a 9-page vault both pure modes already
    # nail every query, so we don't expect a strict "beats" — the meaningful
    # check is that adding RRF fusion never demotes a correct top-1 hit.
    # Gated on OLLAMA_URL: FakeEmbedder is essentially bag-of-words, which
    # makes vector ≈ keyword and renders this comparison meaningless.
    from smolbren.embed import OllamaEmbedder

    _build_eval_vault(tmp_path)
    cfg = load_config(tmp_path)
    embedder = OllamaEmbedder(
        model="nomic-embed-text",
        base_url=os.environ["OLLAMA_URL"],
    )
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)

        eval_set: list[tuple[str, set[str]]] = [
            # Conceptual — query words barely overlap with answer pages.
            (
                "who manages our database connections to the cloud",
                {"systems/snowflake-adapter", "people/jane"},
            ),
            # Exact-phrase — keyword should win on its own.
            ('"adapter framework"', {"systems/adapter-framework"}),
            # Mixed — both modes contribute.
            (
                "who owns the snowflake adapter",
                {"people/jane", "systems/snowflake-adapter"},
            ),
            # Conceptual with lexical-noise overlap. The phrase "warehouse" is
            # shared with notes/warehouse-floor (a physical-warehouse page),
            # so pure keyword can be misled.
            (
                "who handles data warehouse migrations",
                {"systems/snowflake-adapter", "people/jane"},
            ),
            # Conceptual on-call query — "this week" doesn't appear in any page.
            ("who is paging this week", {"people/bob", "decisions/oncall-rotation"}),
            # Recipe / unrelated topic check.
            ("how to cook spaghetti", {"notes/cooking"}),
        ]

        def run(mode: str, query: str) -> Sequence[SearchHit]:
            if mode == "vector":
                return vector_search(conn, cfg, query, top_k=10, embedder=embedder)
            if mode == "keyword":
                try:
                    return keyword_search(conn, query, top_k=10)
                except SearchError:
                    return []
            return hybrid_search(conn, cfg, query, top_k=10, embedder=embedder)

        modes = ("vector", "keyword", "hybrid")
        # Per-query rank table — useful for debugging when an assertion fails.
        ranks: dict[str, list[int | None]] = {m: [] for m in modes}
        for query, expected in eval_set:
            row = []
            for m in modes:
                r = _first_match_rank(run(m, query), expected)
                ranks[m].append(r)
                row.append(f"{m}={r}")
            print(f"  {query!r}: {' '.join(row)}")

        def mrr(mode: str) -> float:
            return sum(1.0 / r if r else 0.0 for r in ranks[mode]) / len(eval_set)

        def p_at_1(mode: str) -> float:
            return sum(1.0 if r == 1 else 0.0 for r in ranks[mode]) / len(eval_set)

        v_mrr, k_mrr, h_mrr = mrr("vector"), mrr("keyword"), mrr("hybrid")
        v_p1, k_p1, h_p1 = p_at_1("vector"), p_at_1("keyword"), p_at_1("hybrid")
        print(
            f"\nMRR    — vector={v_mrr:.3f} keyword={k_mrr:.3f} hybrid={h_mrr:.3f}"
            f"\nP@1    — vector={v_p1:.3f} keyword={k_p1:.3f} hybrid={h_p1:.3f}"
        )
        assert h_mrr >= v_mrr - 1e-9, f"hybrid MRR ({h_mrr}) should match-or-beat vector ({v_mrr})"
        assert h_mrr >= k_mrr - 1e-9, f"hybrid MRR ({h_mrr}) should match-or-beat keyword ({k_mrr})"
        assert h_p1 >= v_p1 - 1e-9, f"hybrid P@1 ({h_p1}) should match-or-beat vector ({v_p1})"
        assert h_p1 >= k_p1 - 1e-9, f"hybrid P@1 ({h_p1}) should match-or-beat keyword ({k_p1})"
    finally:
        conn.close()


def _first_match_rank(hits: Sequence[SearchHit], expected_slugs: set[str]) -> int | None:
    for i, h in enumerate(hits, start=1):
        if h.slug in expected_slugs:
            return i
    return None
