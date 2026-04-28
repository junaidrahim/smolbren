"""SQLite index: schema, migrations, low-level writes.

Owns the on-disk store for pages, chunks, vector embeddings (`vec_chunks`),
keyword index (`fts_chunks`), and the typed link table.

Connection lifecycle is the caller's responsibility: open with `connect()` and
close when done. Migrations run at open time and are idempotent.

Milestone 1 only writes pages + chunks. The vec/fts/links tables exist from the
start so later milestones drop in cleanly without destructive migrations.
"""

from __future__ import annotations

import json
import sqlite3
import time
from collections.abc import Iterable, Sequence
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import sqlite_vec

from .errors import IndexError as SmolbrenIndexError

EMBEDDING_DIM = 768

MIGRATIONS: list[str] = [
    # 1: initial schema
    """
    CREATE TABLE IF NOT EXISTS pages (
      id INTEGER PRIMARY KEY,
      slug TEXT UNIQUE NOT NULL,
      path TEXT NOT NULL,
      title TEXT,
      type TEXT,
      frontmatter JSON,
      content_hash TEXT NOT NULL,
      mtime REAL NOT NULL,
      updated_at REAL NOT NULL
    );

    CREATE TABLE IF NOT EXISTS chunks (
      id INTEGER PRIMARY KEY,
      page_id INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
      ord INTEGER NOT NULL,
      heading TEXT,
      text TEXT NOT NULL,
      content_hash TEXT NOT NULL,
      UNIQUE(page_id, ord)
    );

    CREATE INDEX IF NOT EXISTS idx_chunks_page ON chunks(page_id);
    CREATE INDEX IF NOT EXISTS idx_chunks_hash ON chunks(content_hash);

    CREATE TABLE IF NOT EXISTS links (
      id INTEGER PRIMARY KEY,
      src_slug TEXT NOT NULL,
      dst_slug TEXT NOT NULL,
      type TEXT NOT NULL,
      source_page TEXT NOT NULL,
      confidence REAL NOT NULL DEFAULT 1.0,
      extracted_at REAL NOT NULL,
      UNIQUE(src_slug, dst_slug, type, source_page)
    );

    CREATE INDEX IF NOT EXISTS idx_links_src ON links(src_slug, type);
    CREATE INDEX IF NOT EXISTS idx_links_dst ON links(dst_slug, type);
    """,
    # 2: vector + fts virtual tables
    f"""
    CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks USING vec0(
      chunk_id INTEGER PRIMARY KEY,
      embedding FLOAT[{EMBEDDING_DIM}]
    );

    CREATE VIRTUAL TABLE IF NOT EXISTS fts_chunks USING fts5(
      text, heading, slug UNINDEXED,
      content=chunks, content_rowid=id,
      tokenize='porter unicode61'
    );
    """,
    # 3: content-hash-keyed embedding cache (survives chunk deletions, so
    #    renaming a file or recreating identical content avoids re-embedding).
    """
    CREATE TABLE IF NOT EXISTS embedding_cache (
      content_hash TEXT NOT NULL,
      model TEXT NOT NULL,
      dim INTEGER NOT NULL,
      embedding BLOB NOT NULL,
      created_at REAL NOT NULL,
      PRIMARY KEY (content_hash, model)
    );
    """,
    # 4: rebuild FTS5 without the bogus `slug` column (slug lives on pages,
    #    not chunks; with content=chunks FTS5 needs every named column to
    #    exist on the source table). Install AI/AD/AU triggers so FTS stays
    #    in sync going forward, then backfill from any existing chunks.
    """
    DROP TABLE IF EXISTS fts_chunks;

    CREATE VIRTUAL TABLE fts_chunks USING fts5(
      text, heading,
      content=chunks, content_rowid=id,
      tokenize='porter unicode61'
    );

    CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
      INSERT INTO fts_chunks(rowid, text, heading)
      VALUES (new.id, new.text, new.heading);
    END;

    CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
      INSERT INTO fts_chunks(fts_chunks, rowid, text, heading)
      VALUES('delete', old.id, old.text, old.heading);
    END;

    CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
      INSERT INTO fts_chunks(fts_chunks, rowid, text, heading)
      VALUES('delete', old.id, old.text, old.heading);
      INSERT INTO fts_chunks(rowid, text, heading)
      VALUES (new.id, new.text, new.heading);
    END;

    INSERT INTO fts_chunks(rowid, text, heading)
      SELECT id, text, heading FROM chunks;
    """,
]


@dataclass(frozen=True)
class PageRow:
    id: int
    slug: str
    path: str
    title: str | None
    type: str | None
    frontmatter: dict[str, Any]
    content_hash: str
    mtime: float
    updated_at: float


def connect(db_path: Path) -> sqlite3.Connection:
    """Open the SQLite db with foreign keys + sqlite-vec extension loaded.

    Runs migrations on open.
    """
    db_path.parent.mkdir(parents=True, exist_ok=True)
    # autocommit; we manage txns explicitly. check_same_thread=False so the
    # watch loop (which runs handlers in watchdog's thread) can share the
    # connection — callers serialize cross-thread access themselves.
    conn = sqlite3.connect(db_path, isolation_level=None, check_same_thread=False)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys = ON")
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute("PRAGMA synchronous = NORMAL")

    try:
        conn.enable_load_extension(True)
        sqlite_vec.load(conn)
        conn.enable_load_extension(False)
    except (AttributeError, sqlite3.OperationalError) as e:
        raise SmolbrenIndexError(
            "Could not load sqlite-vec extension. Ensure your Python sqlite3 supports "
            "extensions (Homebrew Python on macOS works; system Python may not)."
        ) from e

    _apply_migrations(conn)
    return conn


def _apply_migrations(conn: sqlite3.Connection) -> None:
    # `executescript` implicitly commits, so we cannot wrap it in our own
    # transaction. We rely on idempotent CREATE ... IF NOT EXISTS statements
    # and bump schema_version after each migration succeeds.
    conn.execute("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)")
    cur = conn.execute("SELECT COALESCE(MAX(version), 0) FROM schema_version")
    current = int(cur.fetchone()[0])
    for i, sql in enumerate(MIGRATIONS, start=1):
        if i <= current:
            continue
        try:
            conn.executescript(sql)
            conn.execute("INSERT INTO schema_version(version) VALUES (?)", (i,))
        except sqlite3.Error as e:
            raise SmolbrenIndexError(f"Migration {i} failed: {e}") from e


@contextmanager
def _txn(conn: sqlite3.Connection) -> Any:
    conn.execute("BEGIN")
    try:
        yield
    except Exception:
        conn.execute("ROLLBACK")
        raise
    else:
        conn.execute("COMMIT")


# --- writes ----------------------------------------------------------------


def upsert_page(
    conn: sqlite3.Connection,
    *,
    slug: str,
    path: str,
    title: str | None,
    type_: str | None,
    frontmatter: dict[str, Any],
    content_hash: str,
    mtime: float,
) -> int:
    """Insert or update a page row, returning its id.

    If the existing row's content_hash matches, only metadata is touched.
    """
    now = time.time()
    fm = json.dumps(frontmatter, sort_keys=True, ensure_ascii=False)
    cur = conn.execute(
        """
        INSERT INTO pages (slug, path, title, type, frontmatter, content_hash, mtime, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(slug) DO UPDATE SET
            path = excluded.path,
            title = excluded.title,
            type = excluded.type,
            frontmatter = excluded.frontmatter,
            content_hash = excluded.content_hash,
            mtime = excluded.mtime,
            updated_at = excluded.updated_at
        RETURNING id
        """,
        (slug, path, title, type_, fm, content_hash, mtime, now),
    )
    row = cur.fetchone()
    return int(row[0])


def _delete_vec_rows(conn: sqlite3.Connection, chunk_ids: Sequence[int]) -> None:
    """sqlite-vec virtual tables don't get FK cascades; drop their rows manually."""
    if not chunk_ids:
        return
    placeholders = ",".join("?" * len(chunk_ids))
    conn.execute(
        f"DELETE FROM vec_chunks WHERE chunk_id IN ({placeholders})",
        list(chunk_ids),
    )


def replace_chunks(
    conn: sqlite3.Connection,
    *,
    page_id: int,
    chunks: Sequence[tuple[int, str | None, str, str]],
) -> None:
    """Replace the chunk set for a page.

    Each tuple is (ord, heading, text, content_hash).
    """
    old_ids = [
        int(r[0]) for r in conn.execute("SELECT id FROM chunks WHERE page_id = ?", (page_id,))
    ]
    conn.execute("DELETE FROM chunks WHERE page_id = ?", (page_id,))
    _delete_vec_rows(conn, old_ids)
    if not chunks:
        return
    conn.executemany(
        "INSERT INTO chunks (page_id, ord, heading, text, content_hash) VALUES (?, ?, ?, ?, ?)",
        [(page_id, ord_, heading, text, h) for (ord_, heading, text, h) in chunks],
    )


def delete_page_by_slug(conn: sqlite3.Connection, slug: str) -> bool:
    """Remove a page (cascades to chunks). Returns True if a row was deleted.

    Also cleans up the page's vec_chunks rows (no FK cascade for virtual
    tables) and any links discovered on this page (the page's authority over
    its `source_page` rows ends when the page goes away).
    """
    chunk_ids = [
        int(r[0])
        for r in conn.execute(
            "SELECT c.id FROM chunks c JOIN pages p ON c.page_id = p.id WHERE p.slug = ?",
            (slug,),
        )
    ]
    cur = conn.execute("DELETE FROM pages WHERE slug = ? RETURNING id", (slug,))
    deleted = cur.fetchone() is not None
    if deleted:
        _delete_vec_rows(conn, chunk_ids)
        conn.execute("DELETE FROM links WHERE source_page = ?", (slug,))
    return deleted


def delete_pages_by_slugs(conn: sqlite3.Connection, slugs: Iterable[str]) -> int:
    deleted = 0
    for slug in slugs:
        if delete_page_by_slug(conn, slug):
            deleted += 1
    return deleted


def get_page_by_slug(conn: sqlite3.Connection, slug: str) -> PageRow | None:
    row = conn.execute(
        "SELECT id, slug, path, title, type, frontmatter, content_hash, mtime, updated_at "
        "FROM pages WHERE slug = ?",
        (slug,),
    ).fetchone()
    if row is None:
        return None
    return PageRow(
        id=int(row["id"]),
        slug=str(row["slug"]),
        path=str(row["path"]),
        title=row["title"],
        type=row["type"],
        frontmatter=json.loads(row["frontmatter"]) if row["frontmatter"] else {},
        content_hash=str(row["content_hash"]),
        mtime=float(row["mtime"]),
        updated_at=float(row["updated_at"]),
    )


def all_slugs(conn: sqlite3.Connection) -> set[str]:
    return {str(r[0]) for r in conn.execute("SELECT slug FROM pages")}


# --- edges -----------------------------------------------------------------


def replace_edges_for_source(
    conn: sqlite3.Connection,
    *,
    source_slug: str,
    edges: Sequence[tuple[str, str, str, float]],
) -> None:
    """Replace all edges discovered from `source_slug`.

    Each tuple is (src_slug, dst_slug, type, confidence). The reconciliation
    is delete-then-insert keyed on `source_page` — atomic and simpler than
    diffing.
    """
    conn.execute("DELETE FROM links WHERE source_page = ?", (source_slug,))
    if not edges:
        return
    now = time.time()
    conn.executemany(
        """
        INSERT OR IGNORE INTO links
            (src_slug, dst_slug, type, source_page, confidence, extracted_at)
        VALUES (?, ?, ?, ?, ?, ?)
        """,
        [(src, dst, t, source_slug, conf, now) for (src, dst, t, conf) in edges],
    )


def delete_edges_for_source(conn: sqlite3.Connection, source_slug: str) -> None:
    conn.execute("DELETE FROM links WHERE source_page = ?", (source_slug,))


# --- chunks for embedding/search ------------------------------------------


@dataclass(frozen=True)
class PendingChunk:
    chunk_id: int
    content_hash: str
    text: str


def chunks_without_embedding(conn: sqlite3.Connection) -> list[PendingChunk]:
    """Return chunks that don't yet have a row in `vec_chunks`."""
    rows = conn.execute(
        """
        SELECT c.id, c.content_hash, c.text
        FROM chunks c
        LEFT JOIN vec_chunks v ON v.chunk_id = c.id
        WHERE v.chunk_id IS NULL
        ORDER BY c.id
        """
    ).fetchall()
    return [
        PendingChunk(chunk_id=int(r[0]), content_hash=str(r[1]), text=str(r[2])) for r in rows
    ]


def lookup_cached_embedding(
    conn: sqlite3.Connection,
    *,
    content_hash: str,
    model: str,
) -> bytes | None:
    row = conn.execute(
        "SELECT embedding FROM embedding_cache WHERE content_hash = ? AND model = ?",
        (content_hash, model),
    ).fetchone()
    return bytes(row[0]) if row is not None else None


def store_embedding(
    conn: sqlite3.Connection,
    *,
    chunk_id: int,
    content_hash: str,
    model: str,
    dim: int,
    embedding_bytes: bytes,
    cache: bool = True,
) -> None:
    """Write an embedding to vec_chunks and (optionally) embedding_cache."""
    conn.execute(
        "INSERT OR REPLACE INTO vec_chunks(chunk_id, embedding) VALUES (?, ?)",
        (chunk_id, embedding_bytes),
    )
    if cache:
        conn.execute(
            """
            INSERT OR REPLACE INTO embedding_cache
                (content_hash, model, dim, embedding, created_at)
            VALUES (?, ?, ?, ?, ?)
            """,
            (content_hash, model, dim, embedding_bytes, time.time()),
        )


@dataclass(frozen=True)
class ChunkContext:
    chunk_id: int
    page_id: int
    slug: str
    title: str | None
    heading: str | None
    text: str


def get_chunk_contexts(
    conn: sqlite3.Connection, chunk_ids: Sequence[int]
) -> dict[int, ChunkContext]:
    """Bulk-load chunk + page context for a list of chunk ids."""
    if not chunk_ids:
        return {}
    placeholders = ",".join("?" * len(chunk_ids))
    rows = conn.execute(
        f"""
        SELECT c.id, c.page_id, p.slug, p.title, c.heading, c.text
        FROM chunks c
        JOIN pages p ON p.id = c.page_id
        WHERE c.id IN ({placeholders})
        """,
        list(chunk_ids),
    ).fetchall()
    return {
        int(r[0]): ChunkContext(
            chunk_id=int(r[0]),
            page_id=int(r[1]),
            slug=str(r[2]),
            title=r[3],
            heading=r[4],
            text=str(r[5]),
        )
        for r in rows
    }


# --- stats ----------------------------------------------------------------


@dataclass(frozen=True)
class IndexStats:
    pages: int
    chunks: int
    edges: int
    types: dict[str, int]


def stats(conn: sqlite3.Connection) -> IndexStats:
    pages = int(conn.execute("SELECT COUNT(*) FROM pages").fetchone()[0])
    chunks = int(conn.execute("SELECT COUNT(*) FROM chunks").fetchone()[0])
    edges = int(conn.execute("SELECT COUNT(*) FROM links").fetchone()[0])
    types: dict[str, int] = {}
    for row in conn.execute(
        "SELECT COALESCE(type, '<untyped>') AS t, COUNT(*) AS n "
        "FROM pages GROUP BY t ORDER BY n DESC"
    ):
        types[str(row["t"])] = int(row["n"])
    return IndexStats(pages=pages, chunks=chunks, edges=edges, types=types)


def transaction(conn: sqlite3.Connection) -> Any:
    """Public transaction context manager."""
    return _txn(conn)
