"""Eval harness.

Owns:
- Loading `queries.json`.
- Computing P@1, P@5, Recall@5, MRR, nDCG@5.
- Tag-grouped reports and run-over-run diffs in `.smolbren/eval-history/`.

Implemented in Milestone 7.
"""
