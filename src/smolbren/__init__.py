"""smolbren — local-first second brain CLI for Obsidian vaults."""

from importlib.metadata import PackageNotFoundError, version

try:
    __version__ = version("smolbren")
except PackageNotFoundError:  # pragma: no cover (uninstalled / src checkout)
    __version__ = "unknown"
