# smolbren — Design & Architecture

This document is the agent-onboarding handbook for smolbren. It explains
*what each module is responsible for*, *what invariants hold across them*,
and *how to test changes safely*. Everything here reflects the code that
ships — not aspirations. If a claim diverges from the code, the code wins
and this doc should be updated.

## What it is

A single-binary CLI that turns an Obsidian-style Markdown vault into a
queryable knowledge base:

- **Hybrid search** — sqlite-vec dense vectors + FTS5/BM25 keyword, fused
  with Reciprocal Rank Fusion, with a multiplicative backlink boost.
- **Typed knowledge graph** — extracted from frontmatter relations,
  wikilinks, and a small set of regex patterns; queryable as a NetworkX
  `MultiDiGraph`.
- **Watch-mode ingest** — `watchdog` with per-file debouncing.

Everything is local-first: SQLite (WAL) on disk, embeddings via a local
[Ollama](https://ollama.com) server, no network calls otherwise.

## Locked tech stack

| Concern | Choice | Why |
|---|---|---|
| Language | Python 3.12+ | Modern type syntax, `tomllib`, structural matches in tests. |
| CLI | typer | Auto-helps + good Rich rendering; no Click ergonomics churn. |
| Storage | SQLite (WAL) + `sqlite-vec` (`vec0`) + FTS5 | One file, no daemon. `vec0` is a virtual table, not a separate DB. |
| Embedder | `ollama` Python client → `nomic-embed-text` | 768d, runs offline, deterministic per text. |
| Graph | `networkx` `MultiDiGraph` | Parallel edges keyed by relation type for "free". |
| Watcher | `watchdog` | Cross-platform; we layer per-file debouncing on top. |
| Tooling | `uv`, `ruff` (E/F/W/I/B/UP), `mypy --strict`, `pytest` (+ `pytest-cov`) | Same toolchain across CI and dev. |

## Repository layout

```
smolbren/
├── pyproject.toml                # uv_build backend; entry: smolbren = smolbren.cli:app
├── README.md
├── docs/design.md                # this file
├── .github/workflows/publish.yml # PyPI trusted-publisher (OIDC, release-triggered)
├── src/smolbren/
│   ├── cli.py                    # typer entry: init / ingest / embed / search / stats / graph
│   ├── config.py                 # vault resolution + .smolbren/config.toml loader
│   ├── errors.py                 # SmolbrenError + 6 subclasses; never raise bare Exception
│   ├── index.py                  # SQLite schema, migrations, low-level read/write helpers
│   ├── ingest.py                 # walk → parse → chunk → upsert + watchdog watcher
│   ├── extract.py                # frontmatter + wikilink + regex edge extraction
│   ├── embed.py                  # Ollama client + content-hash embedding cache
│   ├── search.py                 # vector / keyword / RRF-hybrid search
│   ├── graph.py                  # NetworkX cache + neighbors / path / stats / backlink_counts
│   ├── eval.py                   # (present, not wired to CLI yet — see "follow-ups")
│   ├── mcp_server.py             # (present, not wired to CLI yet — see "follow-ups")
│   ├── ontology.py               # (present, not wired to CLI yet — see "follow-ups")
│   └── py.typed                  # PEP 561 marker
└── tests/
    ├── synthetic_vault.py        # build_vault(root, n_pages, seed) for perf/integration
    ├── fake_embedder.py          # deterministic SHA1-bucketed 768d embedder
    └── test_*.py                 # one module per src module (+ test_rrf, test_keyword_and_hybrid)
```

CLI surface today (verify with `smolbren --help`): `init`, `ingest`,
`embed`, `search`, `stats`, `graph {neighbors,path,stats}`. `eval` /
`serve` *modules* exist but aren't registered as commands.

## Module dependency graph

```
cli ──► config, index, ingest, embed, search, graph
ingest ──► config, extract, index
embed ──► config, index
search ──► config, embed, index
        └──► graph (lazy, only for backlink_counts — keeps the import one-way)
graph ──► index (for graph_state version + backlink SQL)
extract ──► (pure; no smolbren deps)
config, errors, index ──► (leaves)
```

Two rules to preserve:

1. **`extract` stays pure.** No SQLite, no config, no I/O — just text in,
   `Edge` objects out. This makes it trivial to unit-test and lets ingest
   feed the same function from anywhere.
2. **`graph` does not import `search`.** The reverse import (`search →
   graph`) is *lazy* — `from .graph import backlink_counts` lives inside
   `hybrid_search` so the static dependency direction stays one-way.

## Data model

The on-disk schema is defined as an ordered list of migrations in
`src/smolbren/index.py::MIGRATIONS`. They run idempotently at every
`connect()`; `schema_version` tracks the highest applied number.

| # | What it adds | Notes |
|---|---|---|
| 1 | `pages`, `chunks`, `links` + indices | `pages.slug` is unique. `chunks.content_hash` is SHA-256 of chunk text. `links` UNIQUE(src, dst, type, source_page) — same edge from two pages stays as two rows. |
| 2 | `vec_chunks` (vec0 `FLOAT[768]`), `fts_chunks` (FTS5) | `vec_chunks.chunk_id` is the only PK; vec0 doesn't get FK cascades. Initial FTS schema had a bogus `slug UNINDEXED` col — fixed in migration 4. |
| 3 | `embedding_cache(content_hash, model)` | Survives chunk deletions. Renaming a file or recreating identical text → 0 Ollama calls. |
| 4 | Drops + recreates `fts_chunks`; adds AI/AD/AU triggers; backfills | `content=chunks, content_rowid=id`; triggers keep FTS in lockstep with `chunks`. |
| 5 | `graph_state(id=1, version)` single-row counter | Bumped on every edge mutation; used for cross-process graph cache invalidation. |

`pages.frontmatter` is JSON (parsed back to a dict on read).
`links.confidence` is a float in [0, 1] — frontmatter relations get
`1.0`, regex patterns get 0.85–0.95 depending on specificity. `links`
deduplicates by `UNIQUE(src, dst, type, source_page)` — but
**`extract.extract_edges` also collapses duplicates within a single
source** before insert (highest-confidence wins on conflict).

### Critical schema constraints

- `chunks(page_id) ON DELETE CASCADE` cleans pages → chunks. **`vec_chunks`
  has no FK cascade** (virtual tables don't get them) — `_delete_vec_rows`
  handles it manually whenever chunks are deleted.
- Migration 4's triggers (`chunks_ai/ad/au`) are how FTS5 stays current.
  Don't bypass them with raw `INSERT INTO fts_chunks(...)` — go through
  `chunks` and let the triggers fire.
- `executescript` *implicitly commits*. Don't wrap migrations in
  `BEGIN/COMMIT` — they'll fail with "no transaction is active". Migration
  bodies use `IF NOT EXISTS` to stay re-runnable on the off chance.

## Pipeline walkthroughs

### Ingest (`ingest_vault → ingest_file`)

1. Walk vault for `*.md` (`iter_markdown_files`), respecting
   `ignore.patterns` (fnmatch, with `**` recursion emulated).
2. For each file: SHA-256 the bytes (`file_hash`). If `pages.content_hash`
   already matches, skip — early exit, no parse, no embed touch. This is
   the "skipped_unchanged" case in `IngestResult`.
3. Otherwise, parse frontmatter (`python-frontmatter`), pull `title`
   (frontmatter > first H1 > slug tail), pull `type`.
4. Chunk the body via `chunk_markdown` (H2 strategy) — split on `^## ` lines,
   then for each section split into token windows of `max_chunk_tokens`
   with `overlap_tokens` overlap. **Code-fence-aware**: H2 inside a fenced
   block is not a section boundary.
5. `extract_edges(slug, frontmatter, body)` returns a deduped `Edge` list.
6. **One transaction**: `upsert_page` → `replace_chunks` →
   `replace_edges_for_source`. `replace_edges_for_source` bumps
   `graph_state.version` if anything changed.
7. After per-file work, full ingest reconciles deletes: any slug present
   in DB but not seen this run gets `delete_pages_by_slugs`'d (which also
   trims links and bumps the version).

`ingest_file` is idempotent: same content → same final state. Re-running
without changes touches no rows.

### Watch mode

`watch_vault` registers a `_DebouncedHandler` against `Observer`. Each
file path has its own `threading.Timer`; new events on the same path
*push the deadline back* by `debounce_s` (default 0.5s). When the timer
fires, the action runs in a background thread. The CLI shares a single
SQLite connection across threads (`check_same_thread=False`) and
serializes handler calls with a lock.

### Embed (`embed_pending`)

1. `chunks_without_embedding(conn)` — left-join `vec_chunks` to find
   chunks that have no vector yet.
2. Inside one transaction: probe `embedding_cache` for each
   `(content_hash, model)`. Hits → write `vec_chunks` from cache, count
   as `cache_hits`. Misses → buffer.
3. For misses: batch (default 32) → `embedder.embed(texts)` → L2-normalize
   → write both `vec_chunks` and `embedding_cache` in one transaction per
   batch.

Vectors are unit-normalized so the L2 distance vec0 returns is convertible
to cosine: `cos_sim ≈ 1 - L2² / 2` (used by `vector_search`).

### Search (`vector_search` / `keyword_search` / `hybrid_search`)

- `vector_search`: `MATCH ? AND k = ?` against `vec_chunks` ordered by
  distance, hydrate via `get_chunk_contexts`, score = cosine.
- `keyword_search`: `bm25(fts_chunks)`. BM25 returns lower-is-better; we
  *negate* so `SearchHit.score` is consistently higher-is-better.
- `build_fts_query` is the safety layer: preserves `"phrases"`, quotes
  every other token individually so FTS5 specials (`*` `(` `:` `?`) can't
  break parsing, and OR-joins for recall (BM25 + RRF tighten precision).
- `hybrid_search`: overfetches both branches at `top_k * overfetch`
  (default 3), fuses via `rrf` (`score = weight / (k + rank)`, rank
  0-indexed), then applies a multiplicative backlink boost
  `score × (1 + boost · log(1 + backlinks))` *before* slicing to top-k.
  Boost reads `config.search.backlink_boost`; set to `0` to disable.
  Slicing-after-boost lets a popular page knock a less-cited but slightly
  higher-RRF page off the list — that's intentional.

Why multiplicative boost? Spec said `0.15 × log(1+50) ≈ 0.59`, which would
*dominate* RRF scores in the `1/60` range. Multiplicative gives ~1.6× on
50 backlinks while leaving 0-backlink scores untouched. This is a
deliberate spec interpretation; if it's revisited, change it in
`hybrid_search` and update the test in `test_search.py`.

### Graph (`load_graph` / `neighbors` / `shortest_path` / `graph_stats`)

- Process-local cache: `_cached_version`, `_cached_graph` guarded by a
  `threading.Lock`. `load_graph` reads `get_graph_version(conn)`; if it
  matches, returns the cached graph. Otherwise rebuilds from `links`.
- `_build_graph` walks `SELECT … FROM links` and emits `MultiDiGraph` edges
  keyed by relation type, attributing `source_page` and `confidence`.
- `neighbors`: BFS with depth/direction/edge_type filters. Self-loops on
  the source slug are dropped during traversal.
- `backlink_counts(conn, slugs)` runs a direct SQL aggregate
  (`COUNT(DISTINCT source_page)`) — **no graph load needed**. Distinct
  source_page is the meaningful popularity signal: duplicate `mentions`
  rows from one page (e.g. wikilink + regex hit on the same target) don't
  inflate the count.

### Edge extraction (`extract.extract_edges`)

Three sources, all combined and deduped:

1. **Frontmatter relations** — keys in `KNOWN_FRONTMATTER_RELATIONS`
   (`works_on`, `owns`, `member_of`, `reports_to`, `depends_on`,
   `attended`, `decided_in`, `references`, `mentions`). Value can be a
   string or list. Confidence = 1.0.
2. **Wikilinks** — `[[Target]]` → `mentions`, confidence 1.0.
3. **Regex patterns** (`_PATTERNS`) — `X works at [[Y]]`, `X owns [[Y]]`,
   `attended [[Z]]`, `depends on [[W]]`, `decided in [[V]]`. Some patterns
   capture the source from the prose (`src_from_capture=True`); the rest
   use the page itself. Confidences 0.85–0.95.

**`strip_code` runs before regex / wikilink scanning.** A single regex
(`` `+[\s\S]*?`+ ``) handles both fenced (```` ``` ````) and inline (`` `
``) spans. This is the "don't fabricate edges from code samples" rule —
the synthetic vault deliberately includes a `## not a heading` inside a
fenced block to test it.

**Slug normalization** (`normalize_slug`): strip `|alias` and `#anchor`
suffixes, lowercase, replace whitespace runs with `-`. Returns `""` for
unusable input (callers drop those edges).

**Self-loops are dropped** when src == dst == page slug.

## Key invariants

These are the load-bearing rules across the codebase. Violating any of
them silently corrupts state — read/preserve them when changing code.

1. **Idempotent ingest.** Running `ingest_vault` twice on an unchanged
   vault writes zero rows and returns `upserted=0`. Tests rely on this.
2. **`pages.content_hash` short-circuits work.** If the file hash matches,
   nothing else runs. Don't add side-effects after the early return.
3. **Edge writes bump `graph_state.version`.** `replace_edges_for_source`,
   `delete_edges_for_source`, and `delete_page_by_slug` (when it
   actually removes link rows) all bump. The graph cache trusts this.
4. **`vec_chunks` rows are deleted whenever their `chunks` rows are.**
   Virtual tables don't cascade — `_delete_vec_rows` is the manual
   cleanup. `replace_chunks` and `delete_page_by_slug` both call it.
5. **`extract.extract_edges` is pure.** No DB. No config. Same input →
   same `Edge` list, every time.
6. **Embeddings are L2-normalized at the embedder boundary.** Downstream
   code assumes unit vectors. `OllamaEmbedder` normalizes before
   returning; `FakeEmbedder` does too.
7. **Embedder output is validated** for count and dim before any DB
   write. Wrong dim → `EmbedError`, no partial state.
8. **`SearchHit.score` is always higher-is-better.** BM25 is negated.
   This makes sorting and display uniform across modes; absolute values
   aren't comparable across modes.
9. **Errors are typed.** Never raise bare `Exception`. The hierarchy is
   `SmolbrenError` → `{Config,Index,Ingest,Embed,Search,Graph}Error`.
   The CLI catches `SmolbrenError` at the top.
10. **No global state except `typer.Typer`** in `cli.py`. The graph
    cache is *module-local* and gated by the version counter — that is
    not "global" in the harmful sense.

## Configuration

`<vault>/.smolbren/config.toml`:

```toml
[embeddings]
model = "nomic-embed-text"
ollama_url = "http://localhost:11434"

[chunking]
strategy = "h2"               # only "h2" today
overlap_tokens = 50
max_chunk_tokens = 512        # both must satisfy 0 < overlap < max

[search]
rrf_k = 60                    # RRF constant
backlink_boost = 0.15         # 0 disables the boost
hybrid_weights = [1.0, 1.0]   # [vector, keyword]

[ignore]
patterns = [".trash/**", "templates/**", ".smolbren/**", ".git/**", ".obsidian/**"]
```

Vault resolution precedence (in `config.resolve_vault`): `--vault PATH`
> `$SMOLBREN_VAULT` > current working directory.

`load_config` raises `ConfigError` on missing init or bad TOML. Defaults
are baked into the dataclasses, so a minimal `config.toml` (`[embeddings]
model = "..."`) is fine.

## Testing

### How to run

```bash
uv sync
uv run pytest                          # all unit + integration tests
uv run pytest -k graph                 # focus
uv run pytest tests/test_graph.py::test_neighbors_basic
OLLAMA_URL=http://localhost:11434 uv run pytest -k hybrid_beats   # gated MRR test
uv run ruff check                      # lint
uv run mypy                            # strict type-check
uv run pytest --cov=src/smolbren       # coverage (≥80% on core modules expected)
```

### Test architecture

- **Per-module unit tests** — `tests/test_<module>.py` mirrors
  `src/smolbren/<module>.py`. Add new tests in the matching file.
- **Synthetic vault** — `tests/synthetic_vault.py::build_vault(root,
  n_pages, seed)` generates Markdown that includes a code fence with
  fake `## headings` so chunking and edge extraction get exercised.
- **Fake embedder** — `tests/fake_embedder.py::FakeEmbedder` is a
  deterministic, hash-bucketed 768d embedder. Use it everywhere instead
  of touching Ollama. It tracks `call_log` and `total_texts_embedded`,
  which let cache tests assert *zero* embedder calls on a re-ingest.
- **CLI tests** — `tests/test_cli.py` uses Typer's `CliRunner`. They pass
  `--no-embed` to avoid hitting Ollama in the suite.
- **Mocking Ollama** — for tests that exercise `OllamaEmbedder` itself,
  monkeypatch via the string form to keep mypy happy:
  `monkeypatch.setattr("smolbren.embed.ollama.Client", factory)`.

### Gated tests

- `test_hybrid_beats_or_matches_pure_modes_on_eval_set` in
  `test_keyword_and_hybrid.py` is **skipped unless `OLLAMA_URL` is set**.
  `FakeEmbedder` is essentially bag-of-words, so vector ≈ keyword and the
  comparison is meaningless. Run with a real Ollama for the regression
  guard.

### Performance bars to preserve

These come from past measurements on the synthetic vault — don't regress:

- 100 pages → first ingest (parse + chunk + upsert + edges, no embed) on
  the order of a few hundred ms.
- 415 chunks → embed via Ollama in ~5s; subsequent re-ingest with no edits
  → 0 cache misses, ~ms-level total.
- 10k-edge graph → `load_graph` < 50 ms; `neighbors`/`shortest_path` are
  microseconds once cached.

## Common gotchas (and the fixes)

| Symptom | Cause | Fix |
|---|---|---|
| `cannot commit - no transaction is active` | Wrapping `executescript` in BEGIN/COMMIT. | Don't. `executescript` implicit-commits; rely on `IF NOT EXISTS`. |
| FTS5 returns nothing for known terms | Triggers missing or schema has bogus columns. | Migration 4 handles this. If editing `fts_chunks`, drop+rebuild+backfill in a new migration; never alter in place. |
| `vec_chunks` rows orphaned after chunk delete | Virtual tables don't get FK cascades. | Always go through `replace_chunks` / `delete_page_by_slug` (both call `_delete_vec_rows`). |
| Backlink boost dwarfs RRF scores | Read spec's `0.15·log(1+bl)` literally as additive. | Use multiplicative — `score × (1 + 0.15·log1p(bl))`. |
| `sqlite3.ProgrammingError: created in thread X` | Connection used from watchdog's worker thread. | `connect()` already passes `check_same_thread=False`; serialize cross-thread writes with a lock. |
| Edges captured from inside code blocks | Not stripping code before regex. | `extract.strip_code` first; the single-pass `_FENCED` regex handles both fenced and inline. |
| Capture-as-source regex spans heading break | Default `[\w\s]` matches newlines. | Use `[\w \t]` for name captures so they can't span `\n`. |
| `replace_all` edits collapse adjacent whitespace | tool quirk noted multiple times during M-iteration. | Prefer targeted Edit calls; if you must `replace_all`, re-verify the surrounding whitespace. |

## Known limitations / follow-ups

In priority order — pick these up when extending:

1. **Wire `mcp_server.py` into the CLI.** Module exists; needs a
   `smolbren serve` typer command exposing `search`, `graph_query`,
   `get_page`, `list_types` over MCP stdio.
2. **Wire `eval.py` into the CLI.** Module exists; needs `smolbren eval`
   that reads `queries.json`, runs all three modes, computes
   P@1/P@5/Recall@5/MRR/nDCG@5, and diffs against
   `.smolbren/eval-history/`.
3. **Wire `ontology.py` into the CLI.** Allow-list of types lives in
   `extract.KNOWN_FRONTMATTER_RELATIONS` today (hardcoded). `ontology.py`
   should support `smolbren ontology add-type` and warn on unknown types
   in frontmatter.
4. **Title/H1-based slug resolver.** Capture-as-source patterns produce
   slugs from prose names ("Jane" → `jane`) that don't always match the
   page slug (`people/jane`). A title→slug index would resolve these
   "dangling source" edges. Test `test_extract.py` documents the current
   behavior.
5. **Multi-target regex variants.** `depends on [[A]] and [[B]]` only
   catches the first link. A small follow-up: split on `, and`/`,`/`/and `
   before pattern-matching.

## Operational notes

- **Database lives at `<vault>/.smolbren/index.db`** (WAL, so two extra
  files alongside). Safe to delete to force a full rebuild on next
  ingest.
- **Embedding cache lives in the same DB** (`embedding_cache` table) —
  blowing away the DB blows away the cache.
- **Watch mode keeps a single connection alive** for the lifetime of the
  process. SIGINT/SIGTERM trigger a clean `observer.stop()`/`join()`.
- **JSON output mode (`--json`)** emits one JSON object/array per command.
  Watch mode in JSON emits one object per settled event.
- **PyPI publish** is GitHub-Actions-driven via OIDC trusted publisher
  (see `.github/workflows/publish.yml`). Tag + Release → workflow runs
  `uv build` → uploads. No tokens in CI.

## Quick reference for agents touching the code

- Adding a new edge type? Update `extract.KNOWN_FRONTMATTER_RELATIONS`
  *and* add a regex to `_PATTERNS` if it has prose form. Add a test in
  `test_extract.py`. No schema change needed (`links.type` is just TEXT).
- Changing the embedder? Implement `embed.Embedder` Protocol (`model`,
  `dim`, `embed(texts) → list[list[float]]`). L2-normalize before
  returning. Validate count and dim. Add a test with `FakeEmbedder` as
  the template.
- Adding a new search mode? Return `list[SearchHit]` with score
  higher-is-better. If you want to participate in hybrid, add a ranking
  to the `rrf([…])` call in `hybrid_search` (and a corresponding weight).
- Adding a migration? Append to `MIGRATIONS`. Use `IF NOT EXISTS`. Don't
  wrap in BEGIN/COMMIT. If you alter an FTS or vec0 virtual table, drop
  + recreate + backfill (see migration 4 as the template).
- Adding a CLI command? It goes in `cli.py`. Wire `--vault` and `--json`.
  Configure logging with `_configure_logging(json_out)`. Catch typed
  errors → `_die(...)`.
