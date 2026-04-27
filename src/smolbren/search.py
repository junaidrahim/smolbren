"""Vector + keyword + RRF hybrid search and ranking.

Owns:
- Vector k-NN via sqlite-vec.
- Keyword search via FTS5.
- Reciprocal Rank Fusion of multiple rankings.
- Backlink boost integration.

Implemented in Milestones 2 (vector) and 3 (keyword + RRF).
"""
