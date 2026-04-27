"""Config loading tests."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from smolbren.config import (
    is_initialized,
    load_config,
    resolve_vault,
    write_default_config,
)
from smolbren.errors import ConfigError


def test_resolve_vault_explicit(tmp_path: Path) -> None:
    assert resolve_vault(tmp_path) == tmp_path.resolve()


def test_resolve_vault_env(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SMOLBREN_VAULT", str(tmp_path))
    assert resolve_vault(None) == tmp_path.resolve()


def test_resolve_vault_cwd(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("SMOLBREN_VAULT", raising=False)
    monkeypatch.chdir(tmp_path)
    assert resolve_vault(None) == tmp_path.resolve()


def test_init_creates_config_and_loads(tmp_path: Path) -> None:
    assert not is_initialized(tmp_path)
    write_default_config(tmp_path)
    assert is_initialized(tmp_path)
    cfg = load_config(tmp_path)
    assert cfg.embeddings.model == "nomic-embed-text"
    assert cfg.chunking.max_chunk_tokens == 512
    assert cfg.search.hybrid_weights == (1.0, 1.0)


def test_double_init_errors(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    with pytest.raises(ConfigError):
        write_default_config(tmp_path)


def test_load_uninitialized_errors(tmp_path: Path) -> None:
    with pytest.raises(ConfigError):
        load_config(tmp_path)


def test_invalid_hybrid_weights_rejected(tmp_path: Path) -> None:
    write_default_config(tmp_path)
    cfg_path = tmp_path / ".smolbren" / "config.toml"
    cfg_path.write_text(
        cfg_path.read_text().replace("hybrid_weights = [1.0, 1.0]", "hybrid_weights = [1.0]"),
        encoding="utf-8",
    )
    with pytest.raises(ConfigError):
        load_config(tmp_path)


@pytest.fixture(autouse=True)
def _clean_env(monkeypatch: pytest.MonkeyPatch) -> None:
    if "SMOLBREN_VAULT" in os.environ:
        monkeypatch.delenv("SMOLBREN_VAULT", raising=False)
