---
name: smolbren
description: Search and traverse the user's markdown/Obsidian vault with the smolbren CLI — graph queries (Cypher), BM25 full-text search, links and backlinks over frontmatter-defined note types. Use when the user asks about their notes, vault, second brain, knowledge base, backlinks, or connections between notes.
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
over note titles and bodies.

## Output contract

- Every command prints **single-line JSON to stdout**. Parse it; never scrape prose.
- Errors go to stderr as `{"error": "...", "code": "..."}` with a meaningful exit code:

| Exit | Meaning | What to do |
|------|---------|------------|
| 0 | ok | — |
| 1 | internal error | Read `error`, report it |
| 2 | usage error | Fix the arguments |
| 3 | vault not found / none configured | `smolbren vault add <name> <path>` |
| 4 | note not found | Check the id with `search` |
| 5 | index missing | Run `smolbren index` |

## Before you query

1. `smolbren vault list` — confirm a vault is registered (`[]` means none: ask the user
   for their notes path, then `smolbren vault add <name> <path>`).
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
smolbren vault list                            # [{"name","path","default","indexed_at_ms"}]
smolbren vault remove <name>                   # unregister + delete its index

smolbren index [--full]                        # {"scanned","unchanged","added","updated","removed","edges","unresolved_edges","duration_ms"}

smolbren types                                 # [{"type","count"}]
smolbren edges                                 # [{"edge_type","count"}]

smolbren search "<query>" [--type t] [--limit n]   # [{"id","path","type","title","score"}] best-first
smolbren get <id> [--body]                         # {"id","path","type","title","frontmatter"} (+"body")
smolbren links <id> [--type edge_type]             # [{"edge_type","to_id","to_alias","resolved","position"}]
smolbren backlinks <id> [--type edge_type]         # [{"edge_type","from_id","from_type","from_title"}]

smolbren query "<cypher>" [--param k=v]            # {"columns":[...],"rows":[{...}]}
```

## Cypher rules

- Node labels = the values of `smolbren types`, plus `Note` which matches every note.
- Relationship types = the values of `smolbren edges`.
- Only `id`, `path`, `type`, `title` are addressable as node properties. Other
  frontmatter keys (`status`, `created`, …) are **not** queryable in Cypher — fetch the
  note with `get` and filter its `frontmatter` object yourself.
- Parameters: `--param min=30`, referenced as `$min` in the query.

```sh
smolbren query "MATCH (b:blog)-[:mentions]->(n:Note) RETURN b.id, n.id"
smolbren query 'MATCH (n:Note)-[:derives_from]->(j:journal) WHERE n.id = $id RETURN j.id' --param id=blogs/context-engineering
```

## Recipes

- **"What do I have on X?"** — `smolbren search "X" --limit 10`, then `get <id> --body`
  on the best hits.
- **"What links to this note?"** — `smolbren backlinks <id>`, optionally
  `--type <edge_type>` to narrow.
- **Explore a note's neighborhood** — `get <id>` for its frontmatter, `links <id>` for
  outgoing edges, `backlinks <id>` for incoming.
- **Structured questions across types** — check `types`/`edges` first, then one Cypher
  query beats N `links` calls.

## Gotchas

- `links` rows with `"resolved": false` point at notes that don't exist (yet) — the
  wikilink target is preserved in `to_id` but `get` on it will fail.
- After the user deletes notes, resolved flags on unrelated notes can go stale;
  `smolbren index --full` rebuilds everything and re-resolves.
- Search is BM25 keyword matching, not semantic — try multiple phrasings before
  concluding a topic is absent.
