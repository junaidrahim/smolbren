"""Generate a synthetic Obsidian-style vault for tests and perf checks.

Usage from a script or test:
    from tests.synthetic_vault import build_vault
    build_vault(Path("/tmp/v"), n_pages=100)
"""

from __future__ import annotations

import random
from pathlib import Path

WORDS = (
    "warehouse adapter pipeline retry bucket cluster schema migration latency "
    "throughput dashboard alerting backfill replay snapshot fixture"
).split()

ENTITY_TYPES = ["person", "team", "system", "decision", "doc", "concept"]


def _para(rng: random.Random, n: int = 30) -> str:
    return " ".join(rng.choice(WORDS) for _ in range(n))


def _page(rng: random.Random, idx: int) -> tuple[str, str]:
    type_ = rng.choice(ENTITY_TYPES)
    title = f"Note {idx:04d}"
    n_sections = rng.randint(2, 4)
    sections: list[str] = []
    for s in range(n_sections):
        sections.append(f"## Section {s + 1}\n\n{_para(rng, 60)}\n")
    code = "```python\n## not a heading\nprint('hi')\n```\n"
    body = (
        f"---\ntype: {type_}\ntitle: {title}\n---\n\n"
        f"# {title}\n\nIntro paragraph.\n\n"
        + "\n".join(sections)
        + "\n"
        + code
    )
    rel = f"notes/note-{idx:04d}.md"
    return rel, body


def build_vault(root: Path, *, n_pages: int = 100, seed: int = 42) -> Path:
    """Create a vault rooted at `root` and return it."""
    rng = random.Random(seed)
    root.mkdir(parents=True, exist_ok=True)
    for i in range(n_pages):
        rel, body = _page(rng, i)
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")
    return root
