"""End-to-end ingest tests against a tmp_path vault."""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from smolbren.config import load_config, write_default_config
from smolbren.index import connect, get_page_by_slug, stats
from smolbren.ingest import ingest_file, ingest_vault, watch_vault

from .synthetic_vault import build_vault


def _init_vault(root: Path, n_pages: int = 5) -> Path:
    build_vault(root, n_pages=n_pages)
    write_default_config(root)
    return root


def test_ingest_writes_pages_and_chunks(tmp_path: Path) -> None:
    vault = _init_vault(tmp_path, n_pages=3)
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        result = ingest_vault(conn, cfg)
        assert result.processed == 3
        assert result.upserted == 3
        assert result.chunks_written > 0
        assert result.deleted == 0
        s = stats(conn)
        assert s.pages == 3
        assert s.chunks > 0
    finally:
        conn.close()


def test_ingest_is_idempotent(tmp_path: Path) -> None:
    vault = _init_vault(tmp_path, n_pages=3)
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        second = ingest_vault(conn, cfg)
        assert second.upserted == 0
        assert second.skipped_unchanged == 3
        assert second.chunks_written == 0
    finally:
        conn.close()


def test_ingest_re_ingests_only_changed_file(tmp_path: Path) -> None:
    vault = _init_vault(tmp_path, n_pages=4)
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)

        # Edit one file
        target = next(iter((vault / "notes").glob("*.md")))
        target.write_text(target.read_text() + "\n## New section\n\nnew body\n", encoding="utf-8")

        result = ingest_vault(conn, cfg)
        assert result.upserted == 1
        assert result.skipped_unchanged == 3
    finally:
        conn.close()


def test_ingest_deletes_missing_files(tmp_path: Path) -> None:
    vault = _init_vault(tmp_path, n_pages=4)
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        s_before = stats(conn)
        # remove one file from disk
        target = next(iter((vault / "notes").glob("*.md")))
        target.unlink()

        result = ingest_vault(conn, cfg)
        assert result.deleted == 1
        s_after = stats(conn)
        assert s_after.pages == s_before.pages - 1
    finally:
        conn.close()


def test_ingest_respects_ignore_patterns(tmp_path: Path) -> None:
    vault = _init_vault(tmp_path, n_pages=2)
    # Create a templates/ file that should be ignored
    (vault / "templates").mkdir()
    (vault / "templates" / "tpl.md").write_text(
        "---\ntype: doc\n---\n\n# tpl\n\n## A\n\nbody\n", encoding="utf-8"
    )
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        result = ingest_vault(conn, cfg)
        assert result.processed == 2  # templates ignored
    finally:
        conn.close()


def test_ingest_extracts_type_and_title(tmp_path: Path) -> None:
    vault = tmp_path
    write_default_config(vault)
    p = vault / "people" / "jane.md"
    p.parent.mkdir(parents=True)
    p.write_text(
        "---\ntype: person\ntitle: Jane Doe\n---\n\n# Jane Doe\n\n## Role\n\nbody\n",
        encoding="utf-8",
    )
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        page = get_page_by_slug(conn, "people/jane")
        assert page is not None
        assert page.type == "person"
        assert page.title == "Jane Doe"
    finally:
        conn.close()


def test_ingest_handles_untyped_page(tmp_path: Path) -> None:
    vault = tmp_path
    write_default_config(vault)
    p = vault / "notes" / "x.md"
    p.parent.mkdir(parents=True)
    p.write_text("# x\n\n## A\n\nbody\n", encoding="utf-8")
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        page = get_page_by_slug(conn, "notes/x")
        assert page is not None
        assert page.type is None
        s = stats(conn)
        assert s.types.get("<untyped>") == 1
    finally:
        conn.close()


def test_watch_reingests_on_modify(tmp_path: Path) -> None:
    pytest.importorskip("watchdog")
    vault = _init_vault(tmp_path, n_pages=2)
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    events: list[tuple[str, bool, int]] = []

    def on_event(path: Path, deleted: bool, was_upserted: bool, n: int) -> None:
        events.append((path.name, deleted, n))

    try:
        ingest_vault(conn, cfg)
        observer = watch_vault(conn, cfg, debounce_s=0.2, on_event=on_event)
        try:
            target = next(iter((vault / "notes").glob("*.md")))
            time.sleep(0.1)
            target.write_text(
                target.read_text() + "\n## Watched section\n\nfresh body\n", encoding="utf-8"
            )
            # wait for debounce + some slack
            deadline = time.time() + 5.0
            while time.time() < deadline and not events:
                time.sleep(0.1)
        finally:
            observer.stop()
            observer.join(timeout=3.0)
        assert events, "expected at least one watch event"
        assert any(not deleted for _, deleted, _ in events)
    finally:
        conn.close()


def test_ingest_skips_when_only_chunk_text_unchanged(tmp_path: Path) -> None:
    vault = tmp_path
    write_default_config(vault)
    p = vault / "n.md"
    p.write_text("---\ntype: doc\n---\n\n## A\n\nbody one\n", encoding="utf-8")
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        was_upserted, n = ingest_file(conn, cfg, p)
        assert was_upserted is True
        assert n >= 1
        was_upserted, n = ingest_file(conn, cfg, p)
        assert was_upserted is False
        assert n == 0
    finally:
        conn.close()
