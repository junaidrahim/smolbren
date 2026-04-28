"""Edge extraction: frontmatter relations + wikilinks + regex patterns.

Three sources contribute to the typed knowledge graph:

1. **Frontmatter relations** — keys that name a known relation type
   (``works_on``, ``owns``, ``member_of``, ``reports_to``, ``depends_on``,
   ``attended``, ``decided_in``, ``references``, ``mentions``) become typed
   edges from the page slug to each value (a string or list of strings).

2. **Wikilinks** — `[[Target]]` in body text becomes a ``mentions`` edge.

3. **Regex patterns** — the starter set from the spec covers `X works at
   [[Y]]`, `X owns [[Y]]`, `attended [[Y]]`, `depends on [[Y]]`,
   `decided in [[Y]]`. Some patterns capture the source from the prose
   (`src_from_capture=True`); the rest treat the page itself as the source.

Code fences (``` ``` ```) and inline code (`` ` ``) are stripped from the body
before regex / wikilink extraction so edges aren't fabricated from code
samples — that pitfall is called out in the spec.
"""

from __future__ import annotations

import logging
import re
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any

log = logging.getLogger(__name__)


# --- types -----------------------------------------------------------------


@dataclass(frozen=True)
class Edge:
    src_slug: str
    dst_slug: str
    relation_type: str
    confidence: float


@dataclass(frozen=True)
class _Pattern:
    regex: re.Pattern[str]
    relation_type: str
    confidence: float
    src_from_capture: bool  # True → src = group(1); False → src = page itself
    dst_group: int  # which capture group holds the dst


# --- regex assets ----------------------------------------------------------


# Match runs of `` ` ``-delimited code (any length, including ```` ``` ```` blocks).
_FENCED = re.compile(r"`+[\s\S]*?`+")
# Match wikilinks. The target stops at `]]`, `|`, `#`, or newline. We capture
# the raw target text and normalize separately.
_WIKILINK = re.compile(r"\[\[([^\[\]\n]+?)\]\]")

KNOWN_FRONTMATTER_RELATIONS: frozenset[str] = frozenset(
    {
        "works_on",
        "owns",
        "member_of",
        "reports_to",
        "depends_on",
        "attended",
        "decided_in",
        "references",
        "mentions",
    }
)

# Frontmatter keys that are *not* edges and should be ignored when scanning
# for relation arrays. Anything else with a list/string value gets dropped
# silently — relation membership is by allow-list, not deny-list.
_FRONTMATTER_NON_RELATION_KEYS: frozenset[str] = frozenset(
    {"type", "title", "tags", "aliases", "id", "created", "updated", "summary",
     "description", "draft", "extra"}
)


_PATTERNS: tuple[_Pattern, ...] = (
    # Capture-as-source patterns. The name capture is restricted to
    # `[\w \t]` so it can't span newlines — otherwise text like
    # "## Role\n\nJane owns ..." captures "Role\n\nJane" as the source.
    _Pattern(
        re.compile(r"\b(\w[\w \t]+?)\s+works\s+at\s+\[\[([^\]\n]+?)\]\]", re.IGNORECASE),
        "works_at",
        0.9,
        src_from_capture=True,
        dst_group=2,
    ),
    _Pattern(
        re.compile(r"\b(\w[\w \t]+?)\s+owns\s+\[\[([^\]\n]+?)\]\]", re.IGNORECASE),
        "owns",
        0.9,
        src_from_capture=True,
        dst_group=2,
    ),
    _Pattern(
        re.compile(r"\battended\s+\[\[([^\]]+?)\]\]", re.IGNORECASE),
        "attended",
        0.95,
        src_from_capture=False,
        dst_group=1,
    ),
    _Pattern(
        re.compile(r"\bdepends?\s+on\s+\[\[([^\]]+?)\]\]", re.IGNORECASE),
        "depends_on",
        0.85,
        src_from_capture=False,
        dst_group=1,
    ),
    _Pattern(
        re.compile(r"\bdecided\s+in\s+\[\[([^\]]+?)\]\]", re.IGNORECASE),
        "decided_in",
        0.95,
        src_from_capture=False,
        dst_group=1,
    ),
)


# --- helpers ---------------------------------------------------------------


def strip_code(text: str) -> str:
    """Remove fenced and inline code spans from markdown.

    Pitfall mitigation: regex patterns and wikilink scanning must not pick up
    text inside code blocks, or the graph fills with garbage edges from code
    samples. A single backtick run handles both inline code and fenced blocks.
    """
    return _FENCED.sub(" ", text)


def normalize_slug(raw: str) -> str:
    """Normalize a wikilink target or capture into canonical slug form.

    Strips ``|alias`` and ``#anchor`` suffixes, lowercases, replaces runs of
    whitespace with a single hyphen, and trims. Returns ``""`` if the input
    has no usable content.
    """
    raw = raw.strip()
    if not raw:
        return ""
    if "|" in raw:
        raw = raw.split("|", 1)[0]
    if "#" in raw:
        raw = raw.split("#", 1)[0]
    raw = raw.strip().lower()
    raw = re.sub(r"\s+", "-", raw)
    return raw


def extract_wikilinks(body: str) -> list[str]:
    """Return all wikilink targets in body text, normalized.

    Caller is responsible for stripping code first via `strip_code`.
    """
    return [normalize_slug(m.group(1)) for m in _WIKILINK.finditer(body)]


# --- main entry point ------------------------------------------------------


def extract_edges(
    *,
    slug: str,
    frontmatter: dict[str, Any],
    body: str,
) -> list[Edge]:
    """Combine frontmatter, wikilinks, and regex sources into a deduped edge list.

    Identical (src, dst, type) triples are collapsed; on conflict, the highest
    confidence wins. Self-loops (src == dst == page slug) are dropped.
    """
    edges: list[Edge] = []

    # 1. Frontmatter relations.
    for key, value in frontmatter.items():
        if key in _FRONTMATTER_NON_RELATION_KEYS:
            continue
        if key not in KNOWN_FRONTMATTER_RELATIONS:
            continue
        for raw in _coerce_targets(value):
            dst = normalize_slug(raw)
            if dst:
                edges.append(Edge(slug, dst, key, 1.0))

    # 2. Body extraction (with code stripped).
    clean = strip_code(body)

    # 2a. Wikilinks → mentions
    for dst in extract_wikilinks(clean):
        if dst:
            edges.append(Edge(slug, dst, "mentions", 1.0))

    # 2b. Typed regex patterns
    for pat in _PATTERNS:
        for m in pat.regex.finditer(clean):
            dst = normalize_slug(m.group(pat.dst_group))
            src = (
                normalize_slug(m.group(1))
                if pat.src_from_capture
                else slug
            )
            if not src or not dst:
                continue
            edges.append(Edge(src, dst, pat.relation_type, pat.confidence))

    return _dedupe(edges, page_slug=slug)


# --- internals -------------------------------------------------------------


def _coerce_targets(value: Any) -> list[str]:
    if isinstance(value, str):
        return [value]
    if isinstance(value, list):
        return [str(v) for v in value if isinstance(v, str | int | float)]
    return []


def _dedupe(edges: Iterable[Edge], *, page_slug: str) -> list[Edge]:
    by_key: dict[tuple[str, str, str], Edge] = {}
    for e in edges:
        # Drop self-loops on the page itself — `[[my-own-slug]]` from a page
        # to itself is noise, not a relation.
        if e.src_slug == page_slug and e.dst_slug == page_slug:
            continue
        key = (e.src_slug, e.dst_slug, e.relation_type)
        existing = by_key.get(key)
        if existing is None or e.confidence > existing.confidence:
            by_key[key] = e
    return list(by_key.values())
