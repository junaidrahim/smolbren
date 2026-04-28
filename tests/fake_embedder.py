"""Deterministic fake embedder for tests — no Ollama dependency."""

from __future__ import annotations

import hashlib
import math
from dataclasses import dataclass, field

from smolbren.index import EMBEDDING_DIM


@dataclass
class FakeEmbedder:
    """Hash-bucketed embedder.

    Each text is mapped to a unit vector by deterministic hashing into
    EMBEDDING_DIM buckets. Identical texts produce identical vectors;
    similar wording produces overlapping nonzero coordinates so cosine
    similarity is meaningful for ranking tests.
    """

    model: str = "fake-768"
    dim: int = EMBEDDING_DIM
    call_log: list[list[str]] = field(default_factory=list)

    def embed(self, texts: list[str]) -> list[list[float]]:
        self.call_log.append(list(texts))
        return [self._encode(t) for t in texts]

    @property
    def total_texts_embedded(self) -> int:
        return sum(len(c) for c in self.call_log)

    def _encode(self, text: str) -> list[float]:
        v = [0.0] * self.dim
        # Token unigrams + bigrams of normalized words → bucket-add weight.
        words = [w.lower() for w in text.split() if w.strip()]
        ngrams: list[str] = list(words) + [
            f"{a}_{b}" for a, b in zip(words, words[1:], strict=False)
        ]
        for ng in ngrams:
            digest = hashlib.sha1(ng.encode("utf-8")).digest()
            idx = int.from_bytes(digest[:4], "big") % self.dim
            sign = 1.0 if digest[4] & 1 else -1.0
            v[idx] += sign
        norm = math.sqrt(sum(x * x for x in v)) or 1.0
        return [x / norm for x in v]
