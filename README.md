# smolbren

Local-first second-brain CLI for Obsidian vaults.

Turns a folder of Markdown into a queryable knowledge base:

- **Hybrid search** — vector (sqlite-vec) + keyword (FTS5/BM25), fused with Reciprocal Rank Fusion
- **Typed knowledge graph** — self-wired from frontmatter relations, wikilinks, and regex patterns
- **Embedding cache** keyed by content hash — renaming or moving chunks costs zero embedding calls
- **Watch mode** — re-ingests on file changes with per-file debouncing

## Install

Requires Python 3.12+ and a running [Ollama](https://ollama.com) with the embedding model pulled:

```bash
ollama pull nomic-embed-text
```

From PyPI (once published):

```bash
uv tool install smolbren
```

From source:

```bash
git clone https://github.com/junaidrahim/smolbren && cd smolbren
uv sync
uv run smolbren --help
```

## Quickstart

```bash
cd /path/to/vault
smolbren init                          # writes .smolbren/config.toml + db
smolbren ingest                        # parse → chunk → upsert → embed
smolbren search "who's on call?"
smolbren graph neighbors people/jane --depth 2
smolbren stats
```

Run `smolbren ingest --watch` to keep the index live while you edit.

## Commands

| Command | What it does |
|---|---|
| `init` | Scaffold `.smolbren/` and write the default config |
| `ingest [--watch] [--no-embed]` | Parse, chunk, upsert, embed. `--watch` stays running with debounced re-ingest. |
| `embed` | Embed any chunks that don't yet have a vector (cache-aware) |
| `search QUERY [--mode hybrid\|vector\|keyword] [--top-k N]` | Semantic / keyword / hybrid search |
| `stats` | Page / chunk / edge counts and type distribution |
| `graph neighbors SLUG [--type T] [--depth N] [--direction out\|in\|both]` | BFS reachable neighbors |
| `graph path SRC DST` | Shortest path between two slugs |
| `graph stats [--top N]` | Node/edge counts, components, top-degree nodes |

Every command accepts `--vault PATH` (defaults to `$SMOLBREN_VAULT`, then cwd) and `--json` for machine-readable output.

## How it works

1. **Ingest** — Markdown is parsed with frontmatter, chunked by H2 with a token-window overlap, and stored in SQLite (WAL). Code fences are stripped before edge extraction so `` `[[fake]]` `` doesn't pollute the graph.
2. **Embed** — Chunks without a vector are embedded via Ollama (`nomic-embed-text`, 768d, L2-normalized). The cache is keyed on `(content_hash, model)` so chunks that move between files don't re-hit the model.
3. **Search** — Vector via sqlite-vec; keyword via FTS5 with BM25. Hybrid overfetches both branches, fuses with RRF (`score = weight / (k + rank)`), then applies a multiplicative backlink boost (`score × (1 + boost·log(1+backlinks))`) before slicing to top-k. Boost coefficient lives in config; set to `0` to disable.
4. **Graph** — Edges come from frontmatter relations (allow-listed types: `works_on`, `owns`, `member_of`, `reports_to`, `depends_on`, `attended`, `decided_in`, `references`, `mentions`), wikilinks (as `mentions`), and a small set of regex patterns (`X works at [[Y]]`, `X owns [[Y]]`, `attended [[Z]]`, `depends on [[W]]`, `decided in [[V]]`). Loaded into a NetworkX `MultiDiGraph` and cached process-locally; the cache invalidates on any edge mutation via a DB-side version counter.

## Config

`.smolbren/config.toml`:

```toml
[embeddings]
model = "nomic-embed-text"
ollama_url = "http://localhost:11434"

[chunking]
strategy = "h2"
max_chunk_tokens = 512
overlap_tokens = 50

[search]
rrf_k = 60
backlink_boost = 0.15
hybrid_weights = [1.0, 1.0]

[ignore]
patterns = ["**/.obsidian/**", "**/node_modules/**"]
```

## Development

```bash
uv sync
uv run pytest
uv run ruff check
uv run mypy
```

## Release process

Releases are fully automated. Every push to `main` is parsed for
[Conventional Commits](https://www.conventionalcommits.org/); if any
commit since the last tag is `feat:`, `fix:`, `perf:`, or contains
`BREAKING CHANGE`, [`python-semantic-release`](https://python-semantic-release.readthedocs.io)
bumps the version, commits the bump back to `main` (with `[skip ci]`),
tags it, creates a GitHub Release, and publishes the wheel + sdist to
PyPI via the `pypi` trusted-publisher environment.

Bump rules:

| Prefix | Bump |
|---|---|
| `feat:` | minor |
| `fix:`, `perf:` | patch |
| any commit body with `BREAKING CHANGE:` | major |
| `chore:`, `docs:`, `ci:`, `style:`, `test:`, `refactor:`, `build:` | none |

Non-conforming commit messages are ignored (no version bump). Reference
implementation: `.github/workflows/release.yml` and the
`[tool.semantic_release]` block in `pyproject.toml`.
