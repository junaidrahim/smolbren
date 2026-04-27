"""Knowledge graph queries over the `links` table.

Owns:
- Loading the link table into NetworkX on demand (cache + invalidate on writes).
- Neighbor / path / stats queries.
- Backlink counts exposed for ranking.

Implemented in Milestone 5.
"""
