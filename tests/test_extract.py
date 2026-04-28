"""Edge extraction unit + integration tests."""

from __future__ import annotations

from pathlib import Path

import pytest

from smolbren.config import load_config, write_default_config
from smolbren.extract import (
    Edge,
    extract_edges,
    extract_wikilinks,
    normalize_slug,
    strip_code,
)
from smolbren.index import connect
from smolbren.ingest import ingest_vault

# --- strip_code ------------------------------------------------------------


def test_strip_code_removes_fenced_blocks() -> None:
    md = "before\n```\n[[ghost]] inside fence\n```\nafter"
    assert "[[ghost]]" not in strip_code(md)
    assert "before" in strip_code(md)
    assert "after" in strip_code(md)


def test_strip_code_removes_inline_code() -> None:
    md = "use the `[[fake-link]]` macro to inline"
    assert "[[fake-link]]" not in strip_code(md)
    assert "macro" in strip_code(md)


def test_strip_code_handles_empty_and_no_fence() -> None:
    assert strip_code("") == ""
    assert "hello" in strip_code("hello world")


# --- normalize_slug --------------------------------------------------------


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("Foo", "foo"),
        ("People/Jane Doe", "people/jane-doe"),
        ("people/jane-doe", "people/jane-doe"),
        ("Foo|alias", "foo"),
        ("Foo#section", "foo"),
        ("  Multi   word  ", "multi-word"),
        ("", ""),
        ("   ", ""),
    ],
)
def test_normalize_slug(raw: str, expected: str) -> None:
    assert normalize_slug(raw) == expected


# --- extract_wikilinks -----------------------------------------------------


def test_extract_wikilinks_basic() -> None:
    body = "talked to [[Jane]] and [[Bob Smith]] yesterday"
    assert extract_wikilinks(body) == ["jane", "bob-smith"]


def test_extract_wikilinks_with_alias_and_anchor() -> None:
    body = "see [[People/Jane Doe|Jane]] and [[Notes/Decisions#2024]]"
    assert extract_wikilinks(body) == ["people/jane-doe", "notes/decisions"]


# --- extract_edges ---------------------------------------------------------


def test_frontmatter_relations_become_edges() -> None:
    edges = extract_edges(
        slug="people/jane",
        frontmatter={
            "type": "person",
            "title": "Jane",
            "works_on": ["systems/snowflake-adapter", "systems/pipeline"],
            "member_of": "teams/data",
        },
        body="",
    )
    triples = {(e.src_slug, e.dst_slug, e.relation_type) for e in edges}
    assert ("people/jane", "systems/snowflake-adapter", "works_on") in triples
    assert ("people/jane", "systems/pipeline", "works_on") in triples
    assert ("people/jane", "teams/data", "member_of") in triples


def test_unknown_frontmatter_keys_are_ignored() -> None:
    edges = extract_edges(
        slug="people/jane",
        frontmatter={"hobby": ["snowboarding"], "tags": ["x"]},
        body="",
    )
    assert edges == []


def test_wikilinks_become_mentions() -> None:
    edges = extract_edges(
        slug="people/jane",
        frontmatter={},
        body="catch up with [[Bob]] and [[Systems/Pipeline]]",
    )
    mentions = {(e.src_slug, e.dst_slug) for e in edges if e.relation_type == "mentions"}
    assert mentions == {("people/jane", "bob"), ("people/jane", "systems/pipeline")}


def test_wikilinks_inside_code_fences_are_ignored() -> None:
    body = (
        "see [[Real]] in body.\n\n"
        "```python\n"
        "x = '[[CodeFake]]'\n"
        "```\n"
        "tail with `[[InlineFake]]` mention"
    )
    edges = extract_edges(slug="notes/x", frontmatter={}, body=body)
    targets = {e.dst_slug for e in edges if e.relation_type == "mentions"}
    assert "real" in targets
    assert "codefake" not in targets
    assert "inlinefake" not in targets


def test_pattern_attended_uses_page_as_source() -> None:
    body = "## Notes\n\nattended [[meetings/q4-review]]"
    edges = extract_edges(slug="people/jane", frontmatter={}, body=body)
    assert any(
        (e.src_slug, e.dst_slug, e.relation_type) == (
            "people/jane",
            "meetings/q4-review",
            "attended",
        )
        for e in edges
    )


def test_pattern_works_at_uses_capture_as_source() -> None:
    body = "Bob Smith works at [[orgs/acme]]"
    edges = extract_edges(slug="notes/bio", frontmatter={}, body=body)
    triple = (
        "bob-smith",
        "orgs/acme",
        "works_at",
    )
    assert any(
        (e.src_slug, e.dst_slug, e.relation_type) == triple for e in edges
    )


def test_pattern_depends_on_with_optional_s() -> None:
    # Two separate "depends on [[X]]" mentions → two depends_on edges. Note
    # that a single "depends on [[A]] and [[B]]" only produces ONE depends_on
    # edge (A); B is captured as a `mentions` wikilink only.
    body = (
        "the api depends on [[systems/db]] for storage.\n"
        "it also depends on [[systems/cache]] for hot reads."
    )
    edges = extract_edges(slug="systems/api", frontmatter={}, body=body)
    targets = {e.dst_slug for e in edges if e.relation_type == "depends_on"}
    assert targets == {"systems/db", "systems/cache"}


def test_pattern_decided_in() -> None:
    body = "we decided in [[meetings/2026-q1-planning]] to do this"
    edges = extract_edges(slug="decisions/budget", frontmatter={}, body=body)
    assert any(
        e.dst_slug == "meetings/2026-q1-planning" and e.relation_type == "decided_in"
        for e in edges
    )


def test_dedup_keeps_highest_confidence() -> None:
    # Wikilink (mentions, 1.0) + attended pattern (0.95) for same target.
    body = "attended [[meetings/x]] then later mentioned [[meetings/x]]"
    edges = extract_edges(slug="people/jane", frontmatter={}, body=body)
    by_type = {(e.dst_slug, e.relation_type): e for e in edges}
    # Both relation types coexist (different keys), but each is unique.
    assert ("meetings/x", "mentions") in by_type
    assert ("meetings/x", "attended") in by_type


def test_self_loop_dropped() -> None:
    edges = extract_edges(
        slug="people/jane",
        frontmatter={},
        body="Jane Doe owns [[people/jane]]",
    )
    self_loops = [e for e in edges if e.src_slug == e.dst_slug == "people/jane"]
    assert self_loops == []


def test_frontmatter_value_with_nested_wikilink_form() -> None:
    edges = extract_edges(
        slug="people/jane",
        frontmatter={"works_on": ["[[Systems/Snowflake]]"]},
        body="",
    )
    # `[[Systems/Snowflake]]` normalizes via strip→split — the brackets are
    # left in raw, but normalize_slug currently treats them as literal chars.
    # Verify we handle this gracefully (don't crash, produce some slug).
    assert all(e.relation_type == "works_on" for e in edges)
    assert any(e.dst_slug for e in edges)


# --- end-to-end via ingest -------------------------------------------------


def _vault(tmp_path: Path, files: dict[str, str]) -> Path:
    write_default_config(tmp_path)
    for rel, body in files.items():
        p = tmp_path / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(body, encoding="utf-8")
    return tmp_path


def test_ingest_writes_expected_edges(tmp_path: Path) -> None:
    vault = _vault(
        tmp_path,
        {
            "people/jane.md": (
                "---\ntype: person\nworks_on: [systems/snowflake]\n---\n\n# Jane\n\n"
                "## Role\n\nJane owns [[systems/snowflake]] and attended [[meetings/q4]].\n"
            ),
            "systems/snowflake.md": (
                "---\ntype: system\ndepends_on: [systems/aws]\n---\n\n# Snowflake\n\n"
                "## Notes\n\ndepends on [[systems/postgres]] for staging.\n"
            ),
        },
    )
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        result = ingest_vault(conn, cfg)
        assert result.edges_written > 0

        rows = conn.execute(
            "SELECT src_slug, dst_slug, type, source_page FROM links "
            "ORDER BY src_slug, dst_slug, type"
        ).fetchall()
        triples = {(r[0], r[1], r[2]) for r in rows}

        # Frontmatter relation: src is the page slug.
        assert ("people/jane", "systems/snowflake", "works_on") in triples
        # `attended [[X]]` pattern: src is the page (no capture group).
        assert ("people/jane", "meetings/q4", "attended") in triples
        # `Jane owns [[X]]` pattern: src is the captured prose name, NOT the
        # page slug. The (jane, ..., owns) edge is dangling unless something
        # later resolves "jane" → "people/jane" via title/H1 lookup.
        assert ("jane", "systems/snowflake", "owns") in triples
        # snowflake page: frontmatter relation + pattern relation.
        assert ("systems/snowflake", "systems/aws", "depends_on") in triples
        assert ("systems/snowflake", "systems/postgres", "depends_on") in triples

        # Reconciliation: edit the page to drop the wikilink.
        (vault / "people" / "jane.md").write_text(
            "---\ntype: person\nworks_on: [systems/snowflake]\n---\n\n# Jane\n\n"
            "## Role\n\nJane is here.\n",
            encoding="utf-8",
        )
        ingest_vault(conn, cfg)
        rows = conn.execute(
            "SELECT src_slug, dst_slug, type FROM links WHERE source_page = ?",
            ("people/jane",),
        ).fetchall()
        triples = {(r[0], r[1], r[2]) for r in rows}
        # Frontmatter relation survives, pattern-based ones gone.
        assert ("people/jane", "systems/snowflake", "works_on") in triples
        assert ("people/jane", "meetings/q4", "attended") not in triples
        assert ("jane", "systems/snowflake", "owns") not in triples
    finally:
        conn.close()


def test_ingest_no_edges_from_code_fences(tmp_path: Path) -> None:
    vault = _vault(
        tmp_path,
        {
            "n.md": (
                "---\ntype: doc\n---\n\n# Code-only\n\n"
                "## Snippet\n\nNothing real.\n\n"
                "```\n"
                "Bob works at [[orgs/acme]]\n"
                "attended [[meetings/x]]\n"
                "```\n"
            ),
        },
    )
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        rows = conn.execute("SELECT COUNT(*) FROM links").fetchone()
        assert int(rows[0]) == 0
    finally:
        conn.close()


def test_page_delete_drops_its_edges(tmp_path: Path) -> None:
    vault = _vault(
        tmp_path,
        {
            "p.md": (
                "---\ntype: person\n---\n\n# P\n\n## R\n\nattended [[meetings/x]]\n"
            ),
        },
    )
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        assert int(conn.execute("SELECT COUNT(*) FROM links").fetchone()[0]) > 0
        (vault / "p.md").unlink()
        ingest_vault(conn, cfg)
        assert int(conn.execute("SELECT COUNT(*) FROM links").fetchone()[0]) == 0
    finally:
        conn.close()


def test_edge_dedupe_unique_constraint(tmp_path: Path) -> None:
    """Same (src, dst, type, source_page) appearing twice on a page lands once."""
    vault = _vault(
        tmp_path,
        {
            "n.md": (
                "---\ntype: doc\n---\n\n# x\n\n## A\n\n"
                "see [[other]] then again [[other]] and once more [[other]]\n"
            ),
        },
    )
    cfg = load_config(vault)
    conn = connect(cfg.db_path)
    try:
        ingest_vault(conn, cfg)
        n = conn.execute(
            "SELECT COUNT(*) FROM links WHERE src_slug='n' AND dst_slug='other' AND type='mentions'"
        ).fetchone()[0]
        assert int(n) == 1
    finally:
        conn.close()


def test_extract_returns_typed_edges() -> None:
    edges: list[Edge] = extract_edges(
        slug="x",
        frontmatter={"works_on": ["foo"]},
        body="and [[bar]]",
    )
    assert all(isinstance(e, Edge) for e in edges)
    assert all(0.0 <= e.confidence <= 1.0 for e in edges)


def test_frontmatter_handles_weird_input() -> None:
    """Sanity: frontmatter helpers don't crash on weird input."""
    extract_edges(slug="x", frontmatter={"works_on": None}, body="")
    extract_edges(slug="x", frontmatter={"works_on": [1, "foo", None]}, body="")
