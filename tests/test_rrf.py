"""Reciprocal Rank Fusion unit tests."""

from __future__ import annotations

import pytest

from smolbren.errors import SearchError
from smolbren.search import rrf


def test_rrf_promotes_consensus() -> None:
    # Both rankings agree that 1 is best → 1 wins.
    fused = rrf([[1, 2, 3], [1, 4, 5]], k=60)
    assert fused[0][0] == 1


def test_rrf_unknown_to_one_ranking_still_scored() -> None:
    fused = rrf([[1, 2, 3], [4, 5, 6]], k=60)
    ids = [cid for cid, _ in fused]
    assert set(ids) == {1, 2, 3, 4, 5, 6}
    # Items at rank 0 in their ranking outscore items at rank 2 in theirs.
    assert fused[0][1] > fused[-1][1]


def test_rrf_formula_matches_spec() -> None:
    fused = rrf([[10, 20]], k=60)
    # rank 0 → 1/(60+0) = 1/60; rank 1 → 1/61
    assert fused[0][0] == 10
    assert fused[0][1] == pytest.approx(1 / 60)
    assert fused[1][0] == 20
    assert fused[1][1] == pytest.approx(1 / 61)


def test_rrf_zero_weight_ignores_branch() -> None:
    fused = rrf([[1, 2, 3], [9, 8, 7]], k=60, weights=[1.0, 0.0])
    ids = [cid for cid, _ in fused]
    # Branch with weight 0 contributes nothing.
    assert ids == [1, 2, 3]


def test_rrf_weights_shift_ordering() -> None:
    # Two single-item rankings with one strongly weighted.
    fused = rrf([[1], [2]], k=60, weights=[10.0, 1.0])
    assert fused[0][0] == 1
    assert fused[1][0] == 2


def test_rrf_weight_length_mismatch_raises() -> None:
    with pytest.raises(SearchError):
        rrf([[1, 2], [3, 4]], k=60, weights=[1.0])


def test_rrf_empty_rankings() -> None:
    assert rrf([[], []], k=60) == []
