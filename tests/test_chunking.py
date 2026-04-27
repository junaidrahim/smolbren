"""Chunking unit tests."""

from __future__ import annotations

import pytest

from smolbren.errors import IngestError
from smolbren.ingest import chunk_markdown, split_h2_sections


def test_split_h2_basic() -> None:
    md = "intro text\n\n## A\n\nbody A\n\n## B\n\nbody B\n"
    sections = split_h2_sections(md)
    assert sections == [
        (None, "intro text"),
        ("A", "body A"),
        ("B", "body B"),
    ]


def test_split_h2_ignores_headings_in_code_fences() -> None:
    md = (
        "## Real heading\n\n"
        "body\n\n"
        "```\n"
        "## fake heading\n"
        "more code\n"
        "```\n\n"
        "## Another real\n\n"
        "more body\n"
    )
    sections = split_h2_sections(md)
    headings = [h for h, _ in sections]
    assert headings == ["Real heading", "Another real"]
    fake_section = next(body for h, body in sections if h == "Real heading")
    assert "## fake heading" in fake_section


def test_chunk_markdown_short_section_one_chunk() -> None:
    md = "## A\n\nshort body\n"
    chunks = chunk_markdown(md, max_tokens=100, overlap_tokens=10)
    assert len(chunks) == 1
    assert chunks[0][0] == "A"


def test_chunk_markdown_long_section_windows_with_overlap() -> None:
    body_words = " ".join(f"w{i}" for i in range(250))
    md = f"## Big\n\n{body_words}\n"
    chunks = chunk_markdown(md, max_tokens=100, overlap_tokens=20)
    assert len(chunks) >= 3
    assert all(h == "Big" for h, _ in chunks)
    first = chunks[0][1].split()
    second = chunks[1][1].split()
    overlap = set(first[-20:]) & set(second[:20])
    assert len(overlap) >= 10


def test_chunk_overlap_must_be_less_than_max() -> None:
    with pytest.raises(IngestError):
        chunk_markdown("## A\n\n" + "w " * 200, max_tokens=10, overlap_tokens=10)
