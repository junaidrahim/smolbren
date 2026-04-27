"""Frontmatter + regex auto-linker.

Owns:
- Pulling explicit edges from frontmatter arrays and `[[wikilinks]]`.
- Pulling implicit edges via regex over body text (with code fences stripped first).
- Reconciling stale edges (diff old vs new, delete removed).
- Writing rows into `links`.

Implemented in Milestone 4.
"""
