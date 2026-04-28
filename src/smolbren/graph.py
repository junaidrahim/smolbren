"""Knowledge graph queries over the `links` table.

The graph is loaded into a NetworkX `MultiDiGraph` (parallel edges keyed by
relation type). A process-local cache keeps the graph in memory and uses the
DB-side `graph_state.version` counter to detect staleness — the counter is
bumped by `index.replace_edges_for_source` and `index.delete_page_by_slug`,
so any code path that mutates edges automatically invalidates the cache.

For ranking, `backlink_counts` runs a direct SQL aggregate (no graph load
needed) and counts DISTINCT source_page so duplicate `mentions` from a
single page don't inflate popularity.
"""

from __future__ import annotations

import sqlite3
import threading
from collections import Counter
from collections.abc import Iterable, Sequence
from dataclasses import dataclass

import networkx as nx

from .errors import GraphError
from .index import get_graph_version

# --- cache -----------------------------------------------------------------


_CACHE_LOCK = threading.Lock()
_cached_version: int | None = None
_cached_graph: nx.MultiDiGraph | None = None


def load_graph(conn: sqlite3.Connection) -> nx.MultiDiGraph:
    """Return a MultiDiGraph view of the links table.

    Cached across calls; rebuilt when `graph_state.version` advances.
    """
    global _cached_version, _cached_graph
    cur_version = get_graph_version(conn)
    with _CACHE_LOCK:
        if _cached_graph is not None and _cached_version == cur_version:
            return _cached_graph
        graph = _build_graph(conn)
        _cached_version = cur_version
        _cached_graph = graph
        return graph


def invalidate_cache() -> None:
    """Force a rebuild on the next `load_graph` call. Tests use this to be
    explicit; production code relies on the version counter."""
    global _cached_version, _cached_graph
    with _CACHE_LOCK:
        _cached_version = None
        _cached_graph = None


def _build_graph(conn: sqlite3.Connection) -> nx.MultiDiGraph:
    g: nx.MultiDiGraph = nx.MultiDiGraph()
    for src, dst, typ, source_page, confidence in conn.execute(
        "SELECT src_slug, dst_slug, type, source_page, confidence FROM links"
    ):
        g.add_edge(
            str(src),
            str(dst),
            key=str(typ),
            source_page=str(source_page),
            confidence=float(confidence),
        )
    return g


# --- queries ---------------------------------------------------------------


@dataclass(frozen=True)
class NeighborHit:
    slug: str
    distance: int
    edge_types: tuple[str, ...]  # types on the path edge into this node


def neighbors(
    graph: nx.MultiDiGraph,
    slug: str,
    *,
    edge_type: str | None = None,
    depth: int = 1,
    direction: str = "out",
) -> list[NeighborHit]:
    """BFS reachable neighbors of `slug`.

    `direction` is "out" (default), "in", or "both".
    `edge_type` filters edges traversed at every hop.
    """
    if depth < 1:
        raise GraphError("depth must be >= 1")
    if direction not in {"out", "in", "both"}:
        raise GraphError(f"unknown direction {direction!r}")
    if slug not in graph:
        return []

    visited: dict[str, NeighborHit] = {}
    frontier: list[tuple[str, int, tuple[str, ...]]] = [(slug, 0, ())]
    while frontier:
        node, dist, path_types = frontier.pop(0)
        if dist >= depth:
            continue
        for neighbor, etype in _step(graph, node, direction):
            if edge_type is not None and etype != edge_type:
                continue
            if neighbor == slug:
                continue
            new_types = path_types + (etype,)
            existing = visited.get(neighbor)
            if existing is None or existing.distance > dist + 1:
                hit = NeighborHit(slug=neighbor, distance=dist + 1, edge_types=new_types)
                visited[neighbor] = hit
                frontier.append((neighbor, dist + 1, new_types))
    return sorted(visited.values(), key=lambda h: (h.distance, h.slug))


def _step(
    graph: nx.MultiDiGraph, node: str, direction: str
) -> Iterable[tuple[str, str]]:
    if direction in {"out", "both"}:
        for _, nbr, k in graph.out_edges(node, keys=True):
            yield str(nbr), str(k)
    if direction in {"in", "both"}:
        for src, _, k in graph.in_edges(node, keys=True):
            yield str(src), str(k)


def shortest_path(
    graph: nx.MultiDiGraph, src: str, dst: str
) -> list[str] | None:
    """Shortest unweighted path src → dst (any edge type). None if unreachable."""
    if src not in graph or dst not in graph:
        return None
    try:
        path: list[str] = nx.shortest_path(graph, source=src, target=dst)
    except nx.NetworkXNoPath:
        return None
    return [str(n) for n in path]


@dataclass(frozen=True)
class GraphStats:
    nodes: int
    edges: int
    components: int  # weakly connected
    type_distribution: dict[str, int]
    top_in_degree: list[tuple[str, int]]
    top_out_degree: list[tuple[str, int]]


def graph_stats(graph: nx.MultiDiGraph, *, top_n: int = 10) -> GraphStats:
    types: Counter[str] = Counter()
    for _, _, k in graph.edges(keys=True):
        types[str(k)] += 1
    in_deg = sorted(
        ((str(n), int(d)) for n, d in graph.in_degree()),
        key=lambda x: -x[1],
    )[:top_n]
    out_deg = sorted(
        ((str(n), int(d)) for n, d in graph.out_degree()),
        key=lambda x: -x[1],
    )[:top_n]
    components = nx.number_weakly_connected_components(graph) if graph.number_of_nodes() else 0
    return GraphStats(
        nodes=graph.number_of_nodes(),
        edges=graph.number_of_edges(),
        components=components,
        type_distribution=dict(types.most_common()),
        top_in_degree=in_deg,
        top_out_degree=out_deg,
    )


# --- backlinks for ranking -------------------------------------------------


def backlink_counts(
    conn: sqlite3.Connection, slugs: Sequence[str]
) -> dict[str, int]:
    """Return `{slug: distinct_source_page_count}` for each input slug.

    Distinct `source_page` is the meaningful "popularity" signal — duplicate
    `mentions` rows from one page (e.g. wikilink + regex match for the same
    target) don't inflate the count.
    """
    if not slugs:
        return {}
    out: dict[str, int] = dict.fromkeys(slugs, 0)
    placeholders = ",".join("?" * len(slugs))
    rows = conn.execute(
        f"""
        SELECT dst_slug, COUNT(DISTINCT source_page)
        FROM links
        WHERE dst_slug IN ({placeholders})
        GROUP BY dst_slug
        """,
        list(slugs),
    ).fetchall()
    for r in rows:
        out[str(r[0])] = int(r[1])
    return out
