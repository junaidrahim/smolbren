"""Typed error hierarchy. Never raise bare Exception in smolbren."""

from __future__ import annotations


class SmolbrenError(Exception):
    """Base for all smolbren errors."""


class ConfigError(SmolbrenError):
    """Bad / missing config, or vault not initialized."""


class IndexError(SmolbrenError):
    """SQLite index corruption, schema mismatch, migration failure."""


class IngestError(SmolbrenError):
    """File parsing / chunking / upsert failure."""


class EmbedError(SmolbrenError):
    """Ollama embedding failure."""


class SearchError(SmolbrenError):
    """Search / ranking failure."""


class GraphError(SmolbrenError):
    """Graph load / query failure."""
