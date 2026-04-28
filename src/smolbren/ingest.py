"""Ingest pipeline: walk → parse → chunk → upsert.

Owns:
- Walking the vault for `*.md`, respecting `ignore.patterns`.
- Parsing frontmatter via `python-frontmatter`.
- Chunking by H2 (default strategy) with token-window overlap when a section
  exceeds `chunking.max_chunk_tokens`.
- Idempotent upserts keyed on a per-file `content_hash`.
- Deletion reconciliation when files disappear.
- A 500ms-debounced `watchdog` watcher for `--watch` mode.
"""

from __future__ import annotations

import fnmatch
import hashlib
import logging
import re
import sqlite3
import threading
import time
from collections.abc import Callable, Iterable, Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import frontmatter
from watchdog.events import (
    FileCreatedEvent,
    FileDeletedEvent,
    FileModifiedEvent,
    FileMovedEvent,
    FileSystemEvent,
    FileSystemEventHandler,
)
from watchdog.observers import Observer
from watchdog.observers.api import BaseObserver

from .config import Config
from .errors import IngestError
from .extract import extract_edges
from .index import (
    all_slugs,
    delete_pages_by_slugs,
    get_page_by_slug,
    replace_chunks,
    replace_edges_for_source,
    transaction,
    upsert_page,
)

log = logging.getLogger(__name__)

H2_RE = re.compile(r"^##\s+(.*)$")
H1_RE = re.compile(r"^#\s+(.*)$")
FENCE_RE = re.compile(r"^```")


# --- chunking --------------------------------------------------------------


@dataclass(frozen=True)
class Chunk:
    ord: int
    heading: str | None
    text: str
    content_hash: str


def split_h2_sections(markdown_text: str) -> list[tuple[str | None, str]]:
    """Split markdown into (heading, body) sections by H2.

    Code fences are respected: `## ` lines inside a fenced block are not section
    boundaries. The text before the first H2 is returned with heading=None.
    """
    sections: list[tuple[str | None, list[str]]] = [(None, [])]
    in_fence = False
    for line in markdown_text.splitlines():
        if FENCE_RE.match(line):
            in_fence = not in_fence
            sections[-1][1].append(line)
            continue
        if not in_fence:
            m = H2_RE.match(line)
            if m:
                sections.append((m.group(1).strip(), []))
                continue
        sections[-1][1].append(line)
    out: list[tuple[str | None, str]] = []
    for heading, body_lines in sections:
        body = "\n".join(body_lines).strip()
        if heading is None and not body:
            continue
        out.append((heading, body))
    return out


def _window(words: list[str], max_tokens: int, overlap_tokens: int) -> list[list[str]]:
    if max_tokens <= 0:
        raise IngestError("chunking.max_chunk_tokens must be > 0")
    if overlap_tokens >= max_tokens:
        raise IngestError("chunking.overlap_tokens must be < max_chunk_tokens")
    if len(words) <= max_tokens:
        return [words]
    step = max_tokens - overlap_tokens
    out: list[list[str]] = []
    start = 0
    while start < len(words):
        end = min(start + max_tokens, len(words))
        out.append(words[start:end])
        if end == len(words):
            break
        start += step
    return out


def chunk_markdown(
    body: str,
    *,
    max_tokens: int,
    overlap_tokens: int,
    strategy: str = "h2",
) -> list[tuple[str | None, str]]:
    """Chunk markdown body into a list of (heading, text) chunks."""
    if strategy != "h2":
        raise IngestError(f"Unsupported chunking strategy: {strategy!r}")
    chunks: list[tuple[str | None, str]] = []
    for heading, section in split_h2_sections(body):
        if not section.strip():
            continue
        words = section.split()
        for window in _window(words, max_tokens, overlap_tokens):
            chunks.append((heading, " ".join(window)))
    return chunks


# --- file walking ----------------------------------------------------------


def _matches_any(rel_path: str, patterns: Iterable[str]) -> bool:
    for pat in patterns:
        if fnmatch.fnmatch(rel_path, pat):
            return True
        # fnmatch's `**` doesn't recurse like glob; emulate dir-prefix match
        if pat.endswith("/**") and (
            rel_path.startswith(pat[:-3] + "/") or rel_path == pat[:-3]
        ):
            return True
    return False


def iter_markdown_files(vault: Path, ignore_patterns: Iterable[str]) -> Iterator[Path]:
    """Yield absolute paths to *.md files in the vault, honoring ignore patterns."""
    patterns = tuple(ignore_patterns)
    for path in vault.rglob("*.md"):
        if not path.is_file():
            continue
        rel = path.relative_to(vault).as_posix()
        if _matches_any(rel, patterns):
            continue
        yield path


def slug_for(vault: Path, file_path: Path) -> str:
    rel = file_path.relative_to(vault).as_posix()
    if rel.endswith(".md"):
        rel = rel[:-3]
    return rel


# --- ingest ----------------------------------------------------------------


@dataclass(frozen=True)
class IngestResult:
    processed: int
    upserted: int
    skipped_unchanged: int
    deleted: int
    chunks_written: int
    edges_written: int
    duration_s: float


def _hash_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _hash_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _extract_title(metadata: dict[str, Any], body: str, slug: str) -> str:
    title_meta = metadata.get("title")
    if isinstance(title_meta, str) and title_meta.strip():
        return title_meta.strip()
    for line in body.splitlines():
        m = H1_RE.match(line)
        if m:
            return m.group(1).strip()
    return slug.rsplit("/", 1)[-1]


@dataclass(frozen=True)
class FileIngestResult:
    was_upserted: bool
    chunk_count: int
    edge_count: int


def ingest_file(
    conn: sqlite3.Connection,
    config: Config,
    file_path: Path,
) -> FileIngestResult:
    """Ingest a single markdown file.

    Returns a `FileIngestResult`. `was_upserted=False` means the file's
    content_hash matched the existing row and nothing was rewritten.
    """
    try:
        raw = file_path.read_bytes()
    except OSError as e:
        raise IngestError(f"Could not read {file_path}: {e}") from e

    file_hash = _hash_bytes(raw)
    slug = slug_for(config.vault, file_path)

    existing = get_page_by_slug(conn, slug)
    if existing is not None and existing.content_hash == file_hash:
        return FileIngestResult(was_upserted=False, chunk_count=0, edge_count=0)

    try:
        post = frontmatter.loads(raw.decode("utf-8"))
    except Exception as e:  # frontmatter raises a variety of exceptions
        raise IngestError(f"Frontmatter parse failed for {file_path}: {e}") from e

    metadata: dict[str, Any] = dict(post.metadata)
    body: str = post.content
    title = _extract_title(metadata, body, slug)
    type_value = metadata.get("type")
    type_: str | None = str(type_value) if isinstance(type_value, str) else None

    chunk_pairs = chunk_markdown(
        body,
        max_tokens=config.chunking.max_chunk_tokens,
        overlap_tokens=config.chunking.overlap_tokens,
        strategy=config.chunking.strategy,
    )

    try:
        mtime = file_path.stat().st_mtime
    except OSError as e:
        raise IngestError(f"Could not stat {file_path}: {e}") from e

    chunk_rows: list[tuple[int, str | None, str, str]] = [
        (i, heading, text, _hash_text(text))
        for i, (heading, text) in enumerate(chunk_pairs)
    ]

    edges = extract_edges(slug=slug, frontmatter=metadata, body=body)
    edge_tuples: list[tuple[str, str, str, float]] = [
        (e.src_slug, e.dst_slug, e.relation_type, e.confidence) for e in edges
    ]

    with transaction(conn):
        page_id = upsert_page(
            conn,
            slug=slug,
            path=str(file_path),
            title=title,
            type_=type_,
            frontmatter=metadata,
            content_hash=file_hash,
            mtime=mtime,
        )
        replace_chunks(conn, page_id=page_id, chunks=chunk_rows)
        replace_edges_for_source(conn, source_slug=slug, edges=edge_tuples)

    return FileIngestResult(
        was_upserted=True,
        chunk_count=len(chunk_rows),
        edge_count=len(edge_tuples),
    )


def ingest_vault(
    conn: sqlite3.Connection,
    config: Config,
    *,
    progress: Callable[[Path], None] | None = None,
) -> IngestResult:
    """Full vault ingest with deletion reconciliation."""
    started = time.perf_counter()
    seen: set[str] = set()
    processed = upserted = chunks_written = edges_written = 0
    skipped_unchanged = 0

    for file_path in iter_markdown_files(config.vault, config.ignore.patterns):
        slug = slug_for(config.vault, file_path)
        seen.add(slug)
        if progress is not None:
            progress(file_path)
        try:
            res = ingest_file(conn, config, file_path)
        except IngestError:
            log.exception("ingest failed: %s", file_path)
            continue
        processed += 1
        if res.was_upserted:
            upserted += 1
            chunks_written += res.chunk_count
            edges_written += res.edge_count
        else:
            skipped_unchanged += 1

    deleted = 0
    db_slugs = all_slugs(conn)
    stale = db_slugs - seen
    if stale:
        with transaction(conn):
            deleted = delete_pages_by_slugs(conn, stale)

    return IngestResult(
        processed=processed,
        upserted=upserted,
        skipped_unchanged=skipped_unchanged,
        deleted=deleted,
        chunks_written=chunks_written,
        edges_written=edges_written,
        duration_s=time.perf_counter() - started,
    )


# --- watcher ---------------------------------------------------------------


class _DebouncedHandler(FileSystemEventHandler):
    """Coalesce per-path filesystem events into a single delayed action.

    Each path has a timer; new events on the same path push the deadline back
    by `debounce_s`. When the deadline fires, `on_action(path, deleted)` is
    invoked from a background thread.
    """

    def __init__(
        self,
        *,
        vault: Path,
        ignore_patterns: tuple[str, ...],
        debounce_s: float,
        on_action: Callable[[Path, bool], None],
    ) -> None:
        super().__init__()
        self._vault = vault
        self._ignore = ignore_patterns
        self._debounce = debounce_s
        self._on_action = on_action
        self._timers: dict[Path, threading.Timer] = {}
        self._lock = threading.Lock()

    def _is_relevant(self, path_str: str) -> Path | None:
        path = Path(path_str)
        if path.suffix.lower() != ".md":
            return None
        try:
            rel = path.relative_to(self._vault).as_posix()
        except ValueError:
            return None
        if _matches_any(rel, self._ignore):
            return None
        return path

    def _schedule(self, path: Path, deleted: bool) -> None:
        with self._lock:
            existing = self._timers.pop(path, None)
            if existing is not None:
                existing.cancel()

            def fire() -> None:
                with self._lock:
                    self._timers.pop(path, None)
                self._on_action(path, deleted)

            timer = threading.Timer(self._debounce, fire)
            timer.daemon = True
            self._timers[path] = timer
            timer.start()

    def on_created(self, event: FileSystemEvent) -> None:
        if event.is_directory:
            return
        if isinstance(event, FileCreatedEvent):
            path = self._is_relevant(str(event.src_path))
            if path is not None:
                self._schedule(path, deleted=False)

    def on_modified(self, event: FileSystemEvent) -> None:
        if event.is_directory:
            return
        if isinstance(event, FileModifiedEvent):
            path = self._is_relevant(str(event.src_path))
            if path is not None:
                self._schedule(path, deleted=False)

    def on_deleted(self, event: FileSystemEvent) -> None:
        if event.is_directory:
            return
        if isinstance(event, FileDeletedEvent):
            path = self._is_relevant(str(event.src_path))
            if path is not None:
                self._schedule(path, deleted=True)

    def on_moved(self, event: FileSystemEvent) -> None:
        if event.is_directory:
            return
        if isinstance(event, FileMovedEvent):
            src = self._is_relevant(str(event.src_path))
            dst = self._is_relevant(str(event.dest_path))
            if src is not None:
                self._schedule(src, deleted=True)
            if dst is not None:
                self._schedule(dst, deleted=False)


def watch_vault(
    conn: sqlite3.Connection,
    config: Config,
    *,
    debounce_s: float = 0.5,
    on_event: Callable[[Path, bool, bool, int], None] | None = None,
) -> BaseObserver:
    """Start a watchdog observer that re-ingests changed files.

    `on_event(path, deleted, was_upserted, chunk_count)` fires after each
    settled action (useful for tests and CLI logging). Returns the running
    observer; the caller must `stop()` and `join()` it.
    """

    lock = threading.Lock()

    def handle(path: Path, deleted: bool) -> None:
        with lock:
            try:
                if deleted or not path.exists():
                    slug = slug_for(config.vault, path)
                    with transaction(conn):
                        existed = delete_pages_by_slugs(conn, [slug]) > 0
                    if on_event is not None:
                        on_event(path, True, existed, 0)
                    return
                res = ingest_file(conn, config, path)
                if on_event is not None:
                    on_event(path, False, res.was_upserted, res.chunk_count)
            except IngestError:
                log.exception("watch ingest failed: %s", path)

    handler = _DebouncedHandler(
        vault=config.vault,
        ignore_patterns=config.ignore.patterns,
        debounce_s=debounce_s,
        on_action=handle,
    )
    observer = Observer()
    observer.schedule(handler, str(config.vault), recursive=True)
    observer.start()
    return observer
