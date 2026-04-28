"""Typer entry point. Subcommands live in their own modules; this file wires."""

from __future__ import annotations

import json
import logging
import signal
import sqlite3
import sys
import time
from pathlib import Path
from types import FrameType

import structlog
import typer
from rich.console import Console
from rich.table import Table

from . import config as config_mod
from .embed import embed_pending
from .errors import ConfigError, EmbedError, GraphError, SearchError, SmolbrenError
from .graph import (
    graph_stats as compute_graph_stats,
)
from .graph import (
    load_graph,
    shortest_path,
)
from .graph import (
    neighbors as graph_neighbors,
)
from .index import connect
from .index import stats as index_stats
from .ingest import ingest_vault, watch_vault
from .search import hybrid_search, keyword_search, vector_search

app = typer.Typer(
    name="smolbren",
    help="Local-first second-brain CLI for Obsidian vaults.",
    no_args_is_help=True,
    add_completion=False,
)

console = Console()
err_console = Console(stderr=True)


def _configure_logging(json_mode: bool) -> None:
    level = logging.INFO
    if json_mode:
        structlog.configure(
            processors=[
                structlog.processors.TimeStamper(fmt="iso"),
                structlog.processors.add_log_level,
                structlog.processors.JSONRenderer(),
            ],
            wrapper_class=structlog.make_filtering_bound_logger(level),
        )
    else:
        structlog.configure(
            processors=[
                structlog.processors.TimeStamper(fmt="iso"),
                structlog.dev.ConsoleRenderer(colors=sys.stderr.isatty()),
            ],
            wrapper_class=structlog.make_filtering_bound_logger(level),
        )
    logging.basicConfig(level=level, format="%(message)s")
    # httpx logs every request at INFO; the underlying ollama call is implied
    # by our own embed/search messages, so we mute it here.
    logging.getLogger("httpx").setLevel(logging.WARNING)
    logging.getLogger("httpcore").setLevel(logging.WARNING)


VaultOption = typer.Option(
    None,
    "--vault",
    help="Vault root directory. Defaults to $SMOLBREN_VAULT, then the current directory.",
)
JsonOption = typer.Option(False, "--json", help="Emit machine-readable JSON output.")


def _emit_json(data: object) -> None:
    console.print_json(json.dumps(data, default=str))


def _die(msg: str, code: int = 1) -> None:
    err_console.print(f"[red]error:[/red] {msg}")
    raise typer.Exit(code=code)


@app.command("init")
def init_cmd(
    vault: Path | None = VaultOption,
    json_out: bool = JsonOption,
) -> None:
    """Scaffold .smolbren/ in the vault and write the default config."""
    _configure_logging(json_out)
    vault_path = config_mod.resolve_vault(vault)
    if not vault_path.is_dir():
        _die(f"Vault path is not a directory: {vault_path}")
    try:
        config_path = config_mod.write_default_config(vault_path)
    except ConfigError as e:
        _die(str(e))
    cfg = config_mod.load_config(vault_path)
    conn = connect(cfg.db_path)
    conn.close()
    if json_out:
        _emit_json(
            {
                "vault": str(vault_path),
                "config": str(config_path),
                "db": str(cfg.db_path),
            }
        )
    else:
        console.print(f"[green]initialized[/green] vault at {vault_path}")
        console.print(f"  config: {config_path}")
        console.print(f"  db:     {cfg.db_path}")


def _load_or_die(vault_path: Path) -> config_mod.Config:
    try:
        return config_mod.load_config(vault_path)
    except ConfigError as e:
        _die(str(e))
        raise  # for type checker; _die raises


def _run_embed(conn: sqlite3.Connection, cfg: config_mod.Config) -> tuple[int, int, float] | None:
    """Try to embed pending chunks. Returns (embedded, cache_hits, duration) or
    None if Ollama is unreachable (we warn and continue)."""
    try:
        result = embed_pending(conn, cfg)
    except EmbedError as e:
        err_console.print(
            f"[yellow]embed skipped:[/yellow] {e}\n"
            "  Run `smolbren embed` later (or start Ollama and re-run ingest)."
        )
        return None
    return result.embedded, result.cache_hits, result.duration_s


@app.command("ingest")
def ingest_cmd(
    vault: Path | None = VaultOption,
    watch: bool = typer.Option(False, "--watch", help="Watch the vault and re-ingest on change."),
    no_embed: bool = typer.Option(
        False, "--no-embed", help="Skip the embedding pass after ingest."
    ),
    debounce_ms: int = typer.Option(
        500, "--debounce-ms", help="Per-file debounce window in --watch mode."
    ),
    json_out: bool = JsonOption,
) -> None:
    """Ingest the vault: parse → chunk → upsert → embed. With --watch, stays running."""
    _configure_logging(json_out)
    vault_path = config_mod.resolve_vault(vault)
    cfg = _load_or_die(vault_path)
    conn = connect(cfg.db_path)
    try:
        result = ingest_vault(conn, cfg)
        embed_summary: tuple[int, int, float] | None = None
        if not no_embed:
            embed_summary = _run_embed(conn, cfg)

        if json_out:
            payload = {
                "processed": result.processed,
                "upserted": result.upserted,
                "skipped_unchanged": result.skipped_unchanged,
                "deleted": result.deleted,
                "chunks_written": result.chunks_written,
                "edges_written": result.edges_written,
                "duration_s": round(result.duration_s, 4),
            }
            if embed_summary is not None:
                emb, hits, dur = embed_summary
                payload["embedded"] = emb
                payload["cache_hits"] = hits
                payload["embed_duration_s"] = round(dur, 4)
            _emit_json(payload)
        else:
            console.print(
                f"[green]ingest[/green] processed={result.processed} "
                f"upserted={result.upserted} skipped={result.skipped_unchanged} "
                f"deleted={result.deleted} chunks={result.chunks_written} "
                f"edges={result.edges_written} "
                f"in {result.duration_s:.2f}s"
            )
            if embed_summary is not None:
                emb, hits, dur = embed_summary
                console.print(
                    f"[green]embed[/green] embedded={emb} cache_hits={hits} in {dur:.2f}s"
                )

        if not watch:
            return

        if not json_out:
            console.print(f"[cyan]watching[/cyan] {vault_path} (debounce={debounce_ms}ms)…")

        def on_event(path: Path, deleted: bool, was_upserted: bool, chunks: int) -> None:
            if json_out:
                _emit_json(
                    {
                        "event": "delete" if deleted else "upsert",
                        "path": str(path),
                        "was_upserted": was_upserted,
                        "chunks": chunks,
                    }
                )
            else:
                kind = "deleted" if deleted else ("upserted" if was_upserted else "unchanged")
                console.print(f"  [{kind}] {path} ({chunks} chunks)")
            if not deleted and was_upserted and not no_embed:
                _run_embed(conn, cfg)

        observer = watch_vault(conn, cfg, debounce_s=debounce_ms / 1000.0, on_event=on_event)

        stop = False

        def handle_signal(signum: int, frame: FrameType | None) -> None:
            nonlocal stop
            stop = True

        signal.signal(signal.SIGINT, handle_signal)
        signal.signal(signal.SIGTERM, handle_signal)

        try:
            while not stop:
                time.sleep(0.2)
        finally:
            observer.stop()
            observer.join(timeout=5.0)
    finally:
        conn.close()


@app.command("embed")
def embed_cmd(
    vault: Path | None = VaultOption,
    json_out: bool = JsonOption,
) -> None:
    """Embed all chunks that don't yet have a vector (cache-aware)."""
    _configure_logging(json_out)
    vault_path = config_mod.resolve_vault(vault)
    cfg = _load_or_die(vault_path)
    conn = connect(cfg.db_path)
    try:
        try:
            result = embed_pending(conn, cfg)
        except EmbedError as e:
            _die(str(e))
            return
    finally:
        conn.close()
    if json_out:
        _emit_json(
            {
                "embedded": result.embedded,
                "cache_hits": result.cache_hits,
                "duration_s": round(result.duration_s, 4),
            }
        )
    else:
        console.print(
            f"[green]embed[/green] embedded={result.embedded} "
            f"cache_hits={result.cache_hits} in {result.duration_s:.2f}s"
        )


@app.command("search")
def search_cmd(
    query: str = typer.Argument(..., help="The search query."),
    vault: Path | None = VaultOption,
    mode: str = typer.Option(
        "hybrid", "--mode", help="hybrid (default) | vector | keyword."
    ),
    top_k: int = typer.Option(5, "--top-k", help="Number of results to return."),
    json_out: bool = JsonOption,
) -> None:
    """Search the vault by semantic / keyword / hybrid match."""
    _configure_logging(json_out)
    if mode not in {"vector", "keyword", "hybrid"}:
        _die(f"unknown --mode {mode!r}; expected vector|keyword|hybrid")
    vault_path = config_mod.resolve_vault(vault)
    cfg = _load_or_die(vault_path)
    conn = connect(cfg.db_path)
    try:
        try:
            if mode == "vector":
                hits = vector_search(conn, cfg, query, top_k=top_k)
            elif mode == "keyword":
                hits = keyword_search(conn, query, top_k=top_k)
            else:
                hits = hybrid_search(conn, cfg, query, top_k=top_k)
        except (SearchError, EmbedError) as e:
            _die(str(e))
            return
    finally:
        conn.close()

    if json_out:
        _emit_json(
            [
                {
                    "slug": h.slug,
                    "title": h.title,
                    "heading": h.heading,
                    "score": round(h.score, 4),
                    "snippet": h.snippet,
                }
                for h in hits
            ]
        )
        return

    if not hits:
        console.print("[yellow]no results[/yellow]")
        return
    table = Table(title=f"search ({mode}) — {query!r}")
    table.add_column("score", justify="right", style="cyan")
    table.add_column("slug", style="green")
    table.add_column("heading", style="magenta")
    table.add_column("snippet")
    for h in hits:
        table.add_row(f"{h.score:.4g}", h.slug, h.heading or "—", h.snippet)
    console.print(table)


@app.command("stats")
def stats_cmd(
    vault: Path | None = VaultOption,
    json_out: bool = JsonOption,
) -> None:
    """Print page / chunk / edge counts and type distribution."""
    _configure_logging(json_out)
    vault_path = config_mod.resolve_vault(vault)
    cfg = _load_or_die(vault_path)
    conn = connect(cfg.db_path)
    try:
        s = index_stats(conn)
    finally:
        conn.close()
    if json_out:
        _emit_json(
            {"pages": s.pages, "chunks": s.chunks, "edges": s.edges, "types": s.types}
        )
        return

    table = Table(title=f"smolbren stats — {vault_path}")
    table.add_column("metric", style="cyan")
    table.add_column("value", justify="right")
    table.add_row("pages", str(s.pages))
    table.add_row("chunks", str(s.chunks))
    table.add_row("edges", str(s.edges))
    console.print(table)

    if s.types:
        type_table = Table(title="page types")
        type_table.add_column("type", style="magenta")
        type_table.add_column("count", justify="right")
        for t, n in s.types.items():
            type_table.add_row(t, str(n))
        console.print(type_table)
        if "<untyped>" in s.types:
            console.print(
                f"[yellow]warning:[/yellow] {s.types['<untyped>']} page(s) lack a "
                "frontmatter `type:` — these will be skipped by ontology validation."
            )


graph_app = typer.Typer(
    help="Knowledge graph queries (neighbors / path / stats).",
    no_args_is_help=True,
)
app.add_typer(graph_app, name="graph")


@graph_app.command("neighbors")
def graph_neighbors_cmd(
    slug: str = typer.Argument(..., help="Source slug. Try `smolbren stats` to see slugs."),
    vault: Path | None = VaultOption,
    edge_type: str | None = typer.Option(
        None, "--type", help="Restrict traversal to this relation type."
    ),
    depth: int = typer.Option(2, "--depth", help="Max BFS depth from the source."),
    direction: str = typer.Option(
        "out", "--direction", help="out (default) | in | both"
    ),
    json_out: bool = JsonOption,
) -> None:
    """List slugs reachable from SLUG within --depth hops."""
    _configure_logging(json_out)
    vault_path = config_mod.resolve_vault(vault)
    cfg = _load_or_die(vault_path)
    conn = connect(cfg.db_path)
    try:
        try:
            graph = load_graph(conn)
            hits = graph_neighbors(
                graph, slug, edge_type=edge_type, depth=depth, direction=direction
            )
        except GraphError as e:
            _die(str(e))
            return
    finally:
        conn.close()

    if json_out:
        _emit_json(
            [
                {"slug": h.slug, "distance": h.distance, "edge_types": list(h.edge_types)}
                for h in hits
            ]
        )
        return
    if not hits:
        console.print(f"[yellow]no neighbors[/yellow] for {slug!r}")
        return
    table = Table(title=f"neighbors of {slug!r} (depth ≤ {depth})")
    table.add_column("dist", justify="right", style="cyan")
    table.add_column("slug", style="green")
    table.add_column("via", style="magenta")
    for h in hits:
        table.add_row(str(h.distance), h.slug, " → ".join(h.edge_types))
    console.print(table)


@graph_app.command("path")
def graph_path_cmd(
    src: str = typer.Argument(..., help="Source slug."),
    dst: str = typer.Argument(..., help="Destination slug."),
    vault: Path | None = VaultOption,
    json_out: bool = JsonOption,
) -> None:
    """Print the shortest path between two slugs (any edge type)."""
    _configure_logging(json_out)
    vault_path = config_mod.resolve_vault(vault)
    cfg = _load_or_die(vault_path)
    conn = connect(cfg.db_path)
    try:
        graph = load_graph(conn)
        path = shortest_path(graph, src, dst)
    finally:
        conn.close()

    if json_out:
        _emit_json({"path": path})
        return
    if path is None:
        console.print(f"[yellow]no path[/yellow] from {src!r} to {dst!r}")
        return
    console.print(" → ".join(path))


@graph_app.command("stats")
def graph_stats_cmd(
    vault: Path | None = VaultOption,
    top: int = typer.Option(10, "--top", help="How many top-degree nodes to show."),
    json_out: bool = JsonOption,
) -> None:
    """Print graph-level stats: node/edge count, components, type distribution."""
    _configure_logging(json_out)
    vault_path = config_mod.resolve_vault(vault)
    cfg = _load_or_die(vault_path)
    conn = connect(cfg.db_path)
    try:
        graph = load_graph(conn)
        s = compute_graph_stats(graph, top_n=top)
    finally:
        conn.close()

    if json_out:
        _emit_json(
            {
                "nodes": s.nodes,
                "edges": s.edges,
                "components": s.components,
                "type_distribution": s.type_distribution,
                "top_in_degree": [
                    {"slug": slug_, "in": deg} for slug_, deg in s.top_in_degree
                ],
                "top_out_degree": [
                    {"slug": slug_, "out": deg} for slug_, deg in s.top_out_degree
                ],
            }
        )
        return

    overview = Table(title="graph overview")
    overview.add_column("metric", style="cyan")
    overview.add_column("value", justify="right")
    overview.add_row("nodes", str(s.nodes))
    overview.add_row("edges", str(s.edges))
    overview.add_row("weakly-connected components", str(s.components))
    console.print(overview)

    if s.type_distribution:
        types = Table(title="edges by relation type")
        types.add_column("type", style="magenta")
        types.add_column("count", justify="right")
        for t, n in s.type_distribution.items():
            types.add_row(t, str(n))
        console.print(types)

    if s.top_in_degree:
        in_table = Table(title=f"top {top} by in-degree (most cited)")
        in_table.add_column("slug", style="green")
        in_table.add_column("in", justify="right")
        for slug_, deg in s.top_in_degree:
            in_table.add_row(slug_, str(deg))
        console.print(in_table)
    if s.top_out_degree:
        out_table = Table(title=f"top {top} by out-degree (most linking)")
        out_table.add_column("slug", style="green")
        out_table.add_column("out", justify="right")
        for slug_, deg in s.top_out_degree:
            out_table.add_row(slug_, str(deg))
        console.print(out_table)


def main() -> None:
    try:
        app()
    except SmolbrenError as e:
        _die(str(e))


if __name__ == "__main__":
    main()
