---
name: smolbren
description: Search and traverse the user's markdown/Obsidian vault with the smolbren CLI — graph queries (Cypher), BM25 full-text search, semantic similarity search with local embeddings, links and backlinks over frontmatter-defined note types. Use when the user asks about their notes, vault, second brain, knowledge base, backlinks, or connections between notes.
license: MIT
compatibility: Requires the smolbren CLI on PATH (cargo install smolbren)
metadata:
  author: junaidrahim
  repository: https://github.com/junaidrahim/smolbren
---

# smolbren

`smolbren` indexes a folder of markdown files into a local knowledge graph. Each note's
frontmatter `type` becomes a node label; every frontmatter key whose values contain
`[[wikilinks]]` becomes an edge type. On top of the graph there is BM25 full-text search
over note titles and bodies, plus semantic similarity search (`similar`) and hybrid
BM25+vector search (`search --hybrid`) once `smolbren embed` has run.

## Output contract

- Every command prints **single-line JSON to stdout**. Parse it; never scrape prose.
- Exceptions are explicit text modes: `docs --agent` emits this Markdown document and
  `get --body --format text` emits the raw body.
- Errors go to stderr as `{"error": "...", "code": "..."}` with a meaningful exit code:

| Exit | Meaning | What to do |
|------|---------|------------|
| 0 | ok | — |
| 1 | internal error | Read `error`, report it |
| 2 | usage error | Fix the arguments |
| 3 | vault not found / none configured | `smolbren vault add <name> <path>` |
| 4 | note not found | Check the id with `search` |
| 5 | index missing | Run `smolbren index` |
| 6 | embeddings missing | Run `smolbren embed` (needed by `similar` and `search --hybrid`) |
| 7 | model error | Embedding model download/init failed — needs network on first use; report to the user |

## Before you query

1. `smolbren vault list` — confirm a vault is registered (`[]` means none: ask the user
   for their notes path, then `smolbren vault add <name> <path>`). If
   `embeddings_stale` is true, run `smolbren embed` before semantic or hybrid search.
2. `smolbren index` — incremental and cheap (unchanged files are skipped by mtime+size),
   so run it at the start of a session and again after any note files change.
3. `smolbren types` and `smolbren edges` — learn the vault's ontology **before** writing
   Cypher. Types and edge types are user-defined; never assume a label exists.

All commands accept `--vault <name>` to target a non-default vault.

## Note ids

A note's id is its vault-relative path without `.md`, e.g. `blogs/context-engineering`
or `Journal/2026, June 01` (quote ids containing spaces). `search` results include ids;
use those rather than guessing.

## Commands

```sh
smolbren vault add <name> <path> [--default]   # register a vault (first one becomes default)
smolbren vault list                            # includes indexed/embedded counts and embedding lag
smolbren vault remove <name>                   # unregister + delete its index

smolbren index [--full]                        # {"scanned","unchanged","added","updated","removed","edges","unresolved_edges","duration_ms"}
smolbren repair                                # transactional full rebuild; preserves the last good index on failure
smolbren docs --agent                          # canonical Agent Skill markdown from this binary

smolbren types                                 # [{"type","count"}]
smolbren edges                                 # [{"edge_type","count"}]

smolbren search "<query>" [--type t] [--path prefix] [--limit n] # BM25 hits include snippets
smolbren search "<query>" --hybrid                 # BM25+vector RRF; adds "bm25_score","similarity","snippet" (needs embed)
smolbren similar "<query>" [--type t] [--path prefix] [--limit n] # semantic hits with snippets (needs embed)
smolbren embed [--full]                            # {"scanned","unchanged","embedded","removed","chunks_written","chunks_total","model","duration_ms"}
smolbren get <id> [--body] [--format json|text]    # text emits the raw Markdown body
smolbren links <id> [--type edge_type]             # [{"edge_type","to_id","to_alias","resolved","position"}]
smolbren backlinks <id> [--type edge_type]         # [{"edge_type","from_id","from_type","from_title"}]
smolbren unresolved [--type edge_type] [--limit n] # dangling-link grooming queue

smolbren query "<cypher>" [--param k=v]            # {"columns":[...],"rows":[{...}]}
```

## Cypher rules

- Node labels = the values of `smolbren types`, plus `Note` which matches every note.
- Relationship types = the values of `smolbren edges`.
- `id`, `path`, `type`, and `title` plus every scalar frontmatter key with a valid
  identifier (`status`, `created`, `updated`, booleans, and numbers) are addressable
  as case-insensitive node properties. Lists/maps remain available through `get`,
  not Cypher.
- Canonical ISO date strings compare chronologically, so recency filters can use
  `WHERE n.updated >= $cutoff` with `--param cutoff=2026-08-01`.
- Parameters: `--param min=30`, referenced as `$min` in the query.

```sh
smolbren query "MATCH (b:blog)-[:mentions]->(n:Note) RETURN b.id, n.id"
smolbren query 'MATCH (n:Note)-[:derives_from]->(j:journal) WHERE n.id = $id RETURN j.id' --param id=blogs/context-engineering
smolbren query 'MATCH (b:blog) WHERE b.status = $status AND b.updated >= $cutoff RETURN b.id, b.updated' --param status=draft --param cutoff=2026-08-01
```

## Recipes

- **"What do I have on X?"** — `smolbren search "X" --hybrid --limit 10` (falls back:
  exit 6 → run `smolbren embed` or use plain `search`), then `get <id> --body` on the
  best hits.
- **Conceptual/vague questions** — `smolbren similar "<full question>"` embeds meaning,
  so phrase it as a sentence, not keywords. Best when the user's words probably don't
  appear verbatim in their notes.
- **"What links to this note?"** — `smolbren backlinks <id>`, optionally
  `--type <edge_type>` to narrow.
- **Finished notes only** — add `--path Notes/` to keyword, semantic, or hybrid
  search to exclude Journal/Inbox/Notebook source material.
- **Groom missing pages** — `smolbren unresolved` enumerates every dangling edge;
  filter with `--type` when working one relation at a time.
- **Explore a note's neighborhood** — `get <id>` for its frontmatter, `links <id>` for
  outgoing edges, `backlinks <id>` for incoming.
- **Structured questions across types** — check `types`/`edges` first, then one Cypher
  query beats N `links` calls.

## Gotchas

- `links` rows with `"resolved": false` point at notes that don't exist (yet) — the
  wikilink target is preserved in `to_id` but `get` on it will fail.
- After the user deletes notes, resolved flags on unrelated notes can go stale;
  `smolbren index --full` rebuilds everything and re-resolves.
- Plain `search` is BM25 keyword matching; `similar` and `search --hybrid` are
  semantic but only see notes embedded by the last `smolbren embed`. After notes
  change, run `index` then `embed` (both incremental and cheap; embedding is a no-op
  when nothing changed).
- The first `embed` (or `similar`) downloads a ~300MB local model from Hugging Face —
  expect it to take a few minutes once; afterwards everything is offline.
