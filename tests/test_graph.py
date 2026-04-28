"""Graph-query and backlink-boost tests."""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from smolbren.config import load_config, write_default_config
from smolbren.errors import GraphError
from smolbren.graph import (
    backlink_counts,
    graph_stats,
    invalidate_cache,
    load_graph,
    neighbors,
    shortest_path,
)
from smolbren.index import (
    bump_graph_version,
    connect,
    delete_page_by_slug,
    get_graph_version,
    replace_edges_for_source,
)
from smolbren.ingest import ingest_vault

# --- helpers ---------------------------------------------------------------


def _seed_edges(conn: object, edges: list[tuple[str, str, str, str, float]]) -> None:
    """Insert raw rows into `links` for a unit test (bypassing extract)."""
    import time as _t

    now = _t.time()
    conn.executemany(  # type: ignore[attr-defined]
        "INSERT INTO links (src_slug, dst_slug, type, source_page, confidence, extracted_at) "
        "VALUES (?, ?, ?, ?, ?, ?)",
        [(s, d, t, sp, c, now) for s, d, t, sp, c in edges],
    )
    bump_graph_version(conn)  # type: ignore[arg-type]


@pytest.fixture(autouse=True)
def _clear_graph_cache() -> None:
    invalidate_cache()


# --- cache invalidation ----------------------------------------------------


def test_load_graph_caches_until_version_bumps(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        # Empty graph at first.
        g1 = load_graph(conn)
        assert g1.number_of_nodes() == 0

        # Same version → identical object (cache hit).
        g2 = load_graph(conn)
        assert g1 is g2

        _seed_edges(conn, [("a", "b", "mentions", "a", 1.0)])
        g3 = load_graph(conn)
        assert g3 is not g1
        assert g3.has_edge("a", "b")
    finally:
        conn.close()


def test_replace_edges_bumps_version(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        v0 = get_graph_version(conn)
        replace_edges_for_source(
            conn, source_slug="a", edges=[("a", "b", "mentions", 1.0)]
        )
        assert get_graph_version(conn) > v0
    finally:
        conn.close()


def test_replace_edges_with_no_change_does_not_bump(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        v0 = get_graph_version(conn)
        replace_edges_for_source(conn, source_slug="a", edges=[])
        # No edges to delete, no edges to insert → no bump.
        assert get_graph_version(conn) == v0
    finally:
        conn.close()


# --- neighbors -------------------------------------------------------------


def test_neighbors_basic_outbound(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        _seed_edges(
            conn,
            [
                ("a", "b", "mentions", "a", 1.0),
                ("b", "c", "mentions", "b", 1.0),
                ("a", "d", "owns", "a", 1.0),
            ],
        )
        graph = load_graph(conn)
        hits = neighbors(graph, "a", depth=1)
        assert {h.slug for h in hits} == {"b", "d"}
        assert all(h.distance == 1 for h in hits)

        hits = neighbors(graph, "a", depth=2)
        assert {h.slug for h in hits} == {"b", "c", "d"}
        c_hit = next(h for h in hits if h.slug == "c")
        assert c_hit.distance == 2
    finally:
        conn.close()


def test_neighbors_filtered_by_type(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        _seed_edges(
            conn,
            [
                ("a", "b", "owns", "a", 1.0),
                ("a", "c", "mentions", "a", 1.0),
                ("a", "d", "owns", "a", 1.0),
            ],
        )
        graph = load_graph(conn)
        hits = neighbors(graph, "a", edge_type="owns", depth=1)
        assert {h.slug for h in hits} == {"b", "d"}
    finally:
        conn.close()


def test_neighbors_inbound(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        _seed_edges(
            conn,
            [
                ("x", "target", "mentions", "x", 1.0),
                ("y", "target", "mentions", "y", 1.0),
            ],
        )
        graph = load_graph(conn)
        hits = neighbors(graph, "target", direction="in", depth=1)
        assert {h.slug for h in hits} == {"x", "y"}
    finally:
        conn.close()


def test_neighbors_unknown_node_returns_empty(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        graph = load_graph(conn)
        assert neighbors(graph, "ghost", depth=2) == []
    finally:
        conn.close()


def test_neighbors_invalid_args(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        graph = load_graph(conn)
        with pytest.raises(GraphError):
            neighbors(graph, "a", depth=0)
        with pytest.raises(GraphError):
            neighbors(graph, "a", direction="sideways")
    finally:
        conn.close()


# --- shortest_path ---------------------------------------------------------


def test_shortest_path_finds_route(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        _seed_edges(
            conn,
            [
                ("a", "b", "mentions", "a", 1.0),
                ("b", "c", "mentions", "b", 1.0),
                ("c", "d", "mentions", "c", 1.0),
            ],
        )
        graph = load_graph(conn)
        assert shortest_path(graph, "a", "d") == ["a", "b", "c", "d"]
        assert shortest_path(graph, "d", "a") is None  # one-way
    finally:
        conn.close()


def test_shortest_path_missing_node(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        graph = load_graph(conn)
        assert shortest_path(graph, "nope", "also-nope") is None
    finally:
        conn.close()


# --- graph_stats -----------------------------------------------------------


def test_graph_stats_basic(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        _seed_edges(
            conn,
            [
                ("a", "popular", "mentions", "a", 1.0),
                ("b", "popular", "mentions", "b", 1.0),
                ("c", "popular", "mentions", "c", 1.0),
                ("a", "b", "owns", "a", 1.0),
            ],
        )
        graph = load_graph(conn)
        s = graph_stats(graph, top_n=3)
        assert s.nodes == 4
        assert s.edges == 4
        assert s.type_distribution["mentions"] == 3
        assert s.type_distribution["owns"] == 1
        # `popular` has 3 inbound edges → top-1 by in-degree.
        assert s.top_in_degree[0] == ("popular", 3)
    finally:
        conn.close()


# --- backlink_counts -------------------------------------------------------


def test_backlink_counts_distinct_source_pages(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        _seed_edges(
            conn,
            [
                # Same source page contributes two edges (mentions + works_on)
                # → counted once.
                ("a", "target", "mentions", "a", 1.0),
                ("a", "target", "works_on", "a", 1.0),
                # Different source page → counted as a separate backlink.
                ("b", "target", "mentions", "b", 1.0),
            ],
        )
        counts = backlink_counts(conn, ["target", "lonely"])
        assert counts == {"target": 2, "lonely": 0}
    finally:
        conn.close()


def test_backlink_counts_empty_input(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        assert backlink_counts(conn, []) == {}
    finally:
        conn.close()


# --- end-to-end with ingest + page deletion --------------------------------


def test_page_deletion_drops_edges_and_bumps_version(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    p = tmp_path / "p.md"
    p.write_text(
        "---\ntype: doc\n---\n\n# x\n\n## A\n\nattended [[meetings/q1]]\n",
        encoding="utf-8",
    )
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        v_before = get_graph_version(conn)
        graph_before = load_graph(conn)
        assert graph_before.has_edge("p", "meetings/q1")

        delete_page_by_slug(conn, "p")
        assert get_graph_version(conn) > v_before
        graph_after = load_graph(conn)
        assert not graph_after.has_edge("p", "meetings/q1")
    finally:
        conn.close()


# --- perf check ------------------------------------------------------------


def test_graph_queries_under_50ms_on_10k_edges(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    try:
        # Generate a 10k-edge graph: 1000 nodes, ~10 outgoing edges each.
        rows = []
        for i in range(1000):
            for j in range(10):
                src = f"n{i}"
                dst = f"n{(i * 31 + j * 7) % 1000}"
                rows.append((src, dst, "mentions", src, 1.0))
        _seed_edges(conn, rows)

        load_start = time.perf_counter()
        graph = load_graph(conn)
        load_dur = (time.perf_counter() - load_start) * 1000
        assert graph.number_of_edges() >= 9000  # some self-collisions are fine

        # Neighbors and stats both under 50ms once cached.
        for _ in range(2):
            t = time.perf_counter()
            _ = neighbors(graph, "n0", depth=2)
            assert (time.perf_counter() - t) * 1000 < 50.0
            t = time.perf_counter()
            _ = graph_stats(graph, top_n=10)
            assert (time.perf_counter() - t) * 1000 < 50.0

        print(f"\n  graph load: {load_dur:.1f}ms for {graph.number_of_edges()} edges")
    finally:
        conn.close()


# --- backlink boost shifts ranking -----------------------------------------


def test_backlink_boost_promotes_popular_page(tmp_path: Path) -> None:
    """When two chunks tie on RRF, the more-cited page wins."""
    from smolbren.embed import embed_pending
    from smolbren.search import hybrid_search

    from .fake_embedder import FakeEmbedder

    write_default_config(tmp_path)
    # Two near-identical pages so RRF ranks them similarly.
    (tmp_path / "popular.md").write_text(
        "---\ntype: doc\n---\n\n# Popular\n\n## A\n\nthe widget framework rocks\n",
        encoding="utf-8",
    )
    (tmp_path / "obscure.md").write_text(
        "---\ntype: doc\n---\n\n# Obscure\n\n## A\n\nthe widget framework rocks\n",
        encoding="utf-8",
    )
    # Three pages each cite popular → 3 backlinks; obscure has 0.
    for i in range(3):
        (tmp_path / f"linker-{i}.md").write_text(
            f"---\ntype: doc\n---\n\n# linker {i}\n\n## A\n\nsee [[popular]] for details\n",
            encoding="utf-8",
        )

    cfg = load_config(tmp_path)
    conn = connect(cfg.db_path)
    embedder = FakeEmbedder()
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)
        hits = hybrid_search(conn, cfg, "widget framework rocks", top_k=5, embedder=embedder)
        slugs = [h.slug for h in hits]
        # popular and obscure both should appear; popular ranked at-least-as-high
        # because of the backlink boost.
        assert "popular" in slugs and "obscure" in slugs
        assert slugs.index("popular") <= slugs.index("obscure")
    finally:
        conn.close()


def test_backlink_boost_disabled_by_zero_coef(tmp_path: Path) -> None:
    """Setting backlink_boost = 0 in config short-circuits the boost path."""
    import dataclasses

    from smolbren.embed import embed_pending
    from smolbren.search import hybrid_search

    from .fake_embedder import FakeEmbedder

    write_default_config(tmp_path)
    (tmp_path / "n.md").write_text(
        "---\ntype: doc\n---\n\n# n\n\n## A\n\nhello world\n", encoding="utf-8"
    )
    cfg = load_config(tmp_path)
    cfg = dataclasses.replace(
        cfg, search=dataclasses.replace(cfg.search, backlink_boost=0.0)
    )
    conn = connect(cfg.db_path)
    embedder = FakeEmbedder()
    try:
        ingest_vault(conn, cfg)
        embed_pending(conn, cfg, embedder=embedder)
        hits = hybrid_search(conn, cfg, "hello world", top_k=3, embedder=embedder)
        # Just verifies the disabled path runs without error.
        assert hits
    finally:
        conn.close()
