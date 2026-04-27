"""Vault configuration.

Resolves the active vault root and loads `<vault>/.smolbren/config.toml`.
Defaults are baked in so a vault can be initialized with an empty config.
"""

from __future__ import annotations

import os
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .errors import ConfigError

CONFIG_DIRNAME = ".smolbren"
CONFIG_FILENAME = "config.toml"
DB_FILENAME = "index.db"

DEFAULT_CONFIG_TOML = """\
[embeddings]
model = "nomic-embed-text"
ollama_url = "http://localhost:11434"

[chunking]
strategy = "h2"
overlap_tokens = 50
max_chunk_tokens = 512

[search]
rrf_k = 60
backlink_boost = 0.15
hybrid_weights = [1.0, 1.0]

[ignore]
patterns = [".trash/**", "templates/**", ".smolbren/**", ".git/**", ".obsidian/**"]
"""


@dataclass(frozen=True)
class EmbeddingsConfig:
    model: str = "nomic-embed-text"
    ollama_url: str = "http://localhost:11434"


@dataclass(frozen=True)
class ChunkingConfig:
    strategy: str = "h2"
    overlap_tokens: int = 50
    max_chunk_tokens: int = 512


@dataclass(frozen=True)
class SearchConfig:
    rrf_k: int = 60
    backlink_boost: float = 0.15
    hybrid_weights: tuple[float, float] = (1.0, 1.0)


@dataclass(frozen=True)
class IgnoreConfig:
    patterns: tuple[str, ...] = (
        ".trash/**",
        "templates/**",
        ".smolbren/**",
        ".git/**",
        ".obsidian/**",
    )


@dataclass(frozen=True)
class Config:
    vault: Path
    embeddings: EmbeddingsConfig = field(default_factory=EmbeddingsConfig)
    chunking: ChunkingConfig = field(default_factory=ChunkingConfig)
    search: SearchConfig = field(default_factory=SearchConfig)
    ignore: IgnoreConfig = field(default_factory=IgnoreConfig)

    @property
    def smolbren_dir(self) -> Path:
        return self.vault / CONFIG_DIRNAME

    @property
    def config_path(self) -> Path:
        return self.smolbren_dir / CONFIG_FILENAME

    @property
    def db_path(self) -> Path:
        return self.smolbren_dir / DB_FILENAME


def resolve_vault(explicit: Path | None) -> Path:
    """Resolve the active vault root.

    Precedence: explicit `--vault` > `$SMOLBREN_VAULT` > current working dir.
    """
    if explicit is not None:
        return explicit.expanduser().resolve()
    env = os.environ.get("SMOLBREN_VAULT")
    if env:
        return Path(env).expanduser().resolve()
    return Path.cwd().resolve()


def is_initialized(vault: Path) -> bool:
    return (vault / CONFIG_DIRNAME / CONFIG_FILENAME).is_file()


def load_config(vault: Path) -> Config:
    """Load config from `<vault>/.smolbren/config.toml`.

    Raises ConfigError if the vault has not been initialized.
    """
    if not is_initialized(vault):
        raise ConfigError(
            f"Vault not initialized at {vault}. Run `smolbren init --vault {vault}` first."
        )
    raw = (vault / CONFIG_DIRNAME / CONFIG_FILENAME).read_bytes()
    try:
        data: dict[str, Any] = tomllib.loads(raw.decode("utf-8"))
    except tomllib.TOMLDecodeError as e:
        raise ConfigError(f"Invalid TOML in config: {e}") from e

    emb = data.get("embeddings", {})
    chunking = data.get("chunking", {})
    search = data.get("search", {})
    ignore = data.get("ignore", {})

    weights_raw = search.get("hybrid_weights", [1.0, 1.0])
    if not (isinstance(weights_raw, list) and len(weights_raw) == 2):
        raise ConfigError("search.hybrid_weights must be a 2-element array [vector, keyword]")

    return Config(
        vault=vault,
        embeddings=EmbeddingsConfig(
            model=str(emb.get("model", "nomic-embed-text")),
            ollama_url=str(emb.get("ollama_url", "http://localhost:11434")),
        ),
        chunking=ChunkingConfig(
            strategy=str(chunking.get("strategy", "h2")),
            overlap_tokens=int(chunking.get("overlap_tokens", 50)),
            max_chunk_tokens=int(chunking.get("max_chunk_tokens", 512)),
        ),
        search=SearchConfig(
            rrf_k=int(search.get("rrf_k", 60)),
            backlink_boost=float(search.get("backlink_boost", 0.15)),
            hybrid_weights=(float(weights_raw[0]), float(weights_raw[1])),
        ),
        ignore=IgnoreConfig(
            patterns=tuple(str(p) for p in ignore.get("patterns", IgnoreConfig().patterns)),
        ),
    )


def write_default_config(vault: Path) -> Path:
    """Create `<vault>/.smolbren/config.toml` with defaults if absent.

    Returns the path written. Raises ConfigError if it already exists.
    """
    smolbren_dir = vault / CONFIG_DIRNAME
    config_path = smolbren_dir / CONFIG_FILENAME
    if config_path.exists():
        raise ConfigError(f"Already initialized: {config_path}")
    smolbren_dir.mkdir(parents=True, exist_ok=True)
    config_path.write_text(DEFAULT_CONFIG_TOML, encoding="utf-8")
    return config_path
