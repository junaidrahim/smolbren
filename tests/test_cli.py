"""CLI smoke tests via typer's CliRunner."""

from __future__ import annotations

import json
from pathlib import Path

from typer.testing import CliRunner

from smolbren.cli import app

from .synthetic_vault import build_vault


def test_init_then_ingest_then_stats_json(tmp_path: Path) -> None:
    runner = CliRunner()
    build_vault(tmp_path, n_pages=5)

    init = runner.invoke(app, ["init", "--vault", str(tmp_path), "--json"])
    assert init.exit_code == 0, init.output
    init_data = json.loads(init.output)
    assert init_data["vault"]
    assert init_data["config"].endswith("config.toml")

    ingest = runner.invoke(app, ["ingest", "--vault", str(tmp_path), "--no-embed", "--json"])
    assert ingest.exit_code == 0, ingest.output
    ingest_data = json.loads(ingest.output)
    assert ingest_data["processed"] == 5
    assert ingest_data["upserted"] == 5
    assert ingest_data["chunks_written"] > 0

    stats = runner.invoke(app, ["stats", "--vault", str(tmp_path), "--json"])
    assert stats.exit_code == 0, stats.output
    stats_data = json.loads(stats.output)
    assert stats_data["pages"] == 5
    assert stats_data["chunks"] > 0


def test_ingest_before_init_errors(tmp_path: Path) -> None:
    runner = CliRunner()
    result = runner.invoke(app, ["ingest", "--vault", str(tmp_path), "--no-embed"])
    assert result.exit_code == 1
    assert "not initialized" in result.output.lower()


def test_double_init_errors(tmp_path: Path) -> None:
    runner = CliRunner()
    first = runner.invoke(app, ["init", "--vault", str(tmp_path)])
    assert first.exit_code == 0
    second = runner.invoke(app, ["init", "--vault", str(tmp_path)])
    assert second.exit_code == 1
    assert "already initialized" in second.output.lower()


def test_cli_version_flag() -> None:
    runner = CliRunner()
    result = runner.invoke(app, ["--version"])
    assert result.exit_code == 0
    assert result.stdout.strip()


def test_stats_human_readable(tmp_path: Path) -> None:
    runner = CliRunner()
    build_vault(tmp_path, n_pages=2)
    runner.invoke(app, ["init", "--vault", str(tmp_path)])
    runner.invoke(app, ["ingest", "--vault", str(tmp_path), "--no-embed"])
    result = runner.invoke(app, ["stats", "--vault", str(tmp_path)])
    assert result.exit_code == 0
    assert "pages" in result.output
    assert "chunks" in result.output
