# smolbren

A portable brain for all us mere mortals trying to create a second brain.

`smolbren` turns a folder of markdown files — an Obsidian vault, a notes directory,
anything with frontmatter — into a local, queryable knowledge graph with full-text
search on top. It doesn't impose a schema; it **discovers** one from the frontmatter
you already write, then lets you (or your agent) query it with Cypher and BM25.

- **Ontology-first.** Tools like `qmd` approach vault search embeddings-first.
  smolbren instead starts from the structure your notes already encode: the
  frontmatter `type` key becomes a node type, and every frontmatter key holding
  `[[wikilinks]]` becomes a typed edge in a graph.
- **Built for agents.** Every command prints single-line JSON to stdout, errors are
  JSON on stderr with stable exit codes, and nothing is interactive. Humans pipe to
  `jq`; agents parse directly. There's a [ready-made skill](#agent-skill) to teach
  your agent the CLI.
- **Fully local and fast.** Storage is [Lance](https://lancedb.github.io/lance/) on
  disk under `~/.smolbren/`. Indexing is incremental (blake3 content hashes, mtime+size
  fast path) and parses files in parallel across all cores.

## How it works

Everything starts from a note like this:

```markdown
---
type: book
status: reading
started: 2026-06-01
author: "[[people/ursula-k-le-guin]]"
themes: ["[[topics/anarchism]]", "[[topics/utopia]]"]
related: ["[[books/the-left-hand-of-darkness]]"]
---

# The Dispossessed

An ambiguous utopia: two worlds, one wall, and the physicist who tries to
unbuild it.
```

From each file, `smolbren index` derives:

- **id** — the vault-relative path without `.md` (`books/the-dispossessed`).
  This is the same shape wikilink targets use, so ids and links line up for free.
- **type** — the frontmatter `type` key (`book`). Each type becomes a Cypher node
  label; `Note` is a catch-all label matching every note, typed or not.
- **title** — the first `# heading`, falling back to the filename stem.
- **edges** — every frontmatter key (except `type`) whose string values contain
  wikilinks becomes an edge type: here `author`, `themes`, and `related`.
  Each `[[link]]` is one directed edge, with its list position preserved.
  Scalar keys like `status` and `started` never become edges — they're kept in the
  note's `frontmatter` object instead.

Wikilink targets are resolved Obsidian-style: exact id match first, then unique
basename (`[[utopia]]` → `topics/utopia` if unambiguous). Aliases
(`[[target|alias]]`) are captured and heading/block anchors (`#section`, `^block`)
are stripped. Ambiguous or missing targets are kept but flagged `resolved: false`
rather than guessed.

The union of everything discovered — which types exist, which edge types exist, with
counts — is the vault's **ontology**, and it's what makes the graph queryable without
any configuration.

Indexing is incremental: files whose mtime and size are unchanged are skipped without
being read; the rest are read, hashed, and re-parsed in parallel, and only notes whose
content actually changed get rewritten. Hidden directories (`.obsidian/`, `.git/`,
`.trash/`) are skipped and `.gitignore` is honored.

## Install

From [crates.io](https://crates.io/crates/smolbren):

```sh
cargo install smolbren
```

`cargo install` builds from source, so you need Rust (edition 2024) and `protoc` on
PATH — Lance compiles protobuf definitions at build time:

```sh
brew install protobuf           # macOS
# apt-get install protobuf-compiler   # Debian/Ubuntu
```

Or build from a checkout:

```sh
git clone https://github.com/junaidrahim/smolbren
cd smolbren
cargo build --release           # binary at target/release/smolbren
```

## Quickstart

```sh
# 1. register a vault (the first one becomes the default)
smolbren vault add personal ~/notes
# {"default":true,"name":"personal","path":"/Users/you/notes"}

# 2. index it (incremental — rerun any time, only changed files are re-read)
smolbren index
# {"scanned":9,"unchanged":0,"added":9,"updated":0,"removed":0,"edges":15,"unresolved_edges":0,"duration_ms":83}

# 3. see what ontology was discovered
smolbren types      # [{"count":3,"type":"blog"},{"count":2,"type":"journal"},...]
smolbren edges      # [{"count":3,"edge_type":"derives_from"},{"count":6,"edge_type":"mentions"},...]

# 4. search, fetch, traverse
smolbren search "context engineering" --type blog --limit 5
smolbren get blogs/context-engineering --body
smolbren links blogs/context-engineering --type mentions
smolbren backlinks projects/prism

# 5. query the graph with Cypher
smolbren query "MATCH (b:blog)-[:mentions]->(n:Note) RETURN b.id, n.id"
```

## Command reference

Global flags on every command: `--vault <name>` (defaults to the configured default
vault) and `--config <path>` (defaults to `~/.smolbren/config.json`).

### `vault add <name> <path> [--default]`

Register a vault. The first vault registered becomes the default; `--default` makes a
later one the default.

### `vault list`

```json
[{"default":true,"indexed_at_ms":1783171064843,"name":"personal","path":"/Users/you/notes"}]
```

`indexed_at_ms` is `null` until the vault is first indexed.

### `vault remove <name>`

Unregister a vault and delete its index data (the source markdown is untouched).

### `index [--full]`

Incrementally index the vault. `--full` rebuilds from scratch, which also re-resolves
every wikilink (see [limitations](#current-limitations)).

```json
{"scanned":9,"unchanged":0,"added":9,"updated":0,"removed":0,"edges":15,"unresolved_edges":0,"duration_ms":83}
```

### `search <query> [--type <note_type>] [--limit <n>]`

BM25 full-text search over note titles and bodies, best match first.

```json
[{"id":"blogs/context-engineering","path":"blogs/context-engineering.md","score":2.5400267,"title":"Context engineering","type":"blog"},...]
```

### `get <id> [--body]`

Fetch one note by id. The full frontmatter is returned as a JSON object; `--body`
includes the markdown body.

```json
{"frontmatter":{"created":"2026-05-10","status":"draft",...},"id":"blogs/context-engineering","path":"blogs/context-engineering.md","title":"Context engineering","type":"blog"}
```

### `links <id> [--type <edge_type>]`

Outgoing edges of a note, ordered by (edge_type, position — i.e. the order links
appear in the frontmatter).

```json
[{"edge_type":"mentions","position":0,"resolved":true,"to_alias":null,"to_id":"projects/prism"},...]
```

### `backlinks <id> [--type <edge_type>]`

Incoming edges, each joined with the source note's type and title.

```json
[{"edge_type":"mentions","from_id":"Journal/2026, June 01","from_title":"2026, June 01","from_type":"journal"},...]
```

### `query <cypher> [--param k=v]`

Run a Cypher query over the note graph via
[lance-graph](https://crates.io/crates/lance-graph).

- Node labels are your note types plus the catch-all `Note`.
- Relationship types are your edge types.
- Addressable node properties: `id`, `path`, `type`, `title`.
- `--param` is repeatable; values parse as JSON first (numbers, bools), then string.

```sh
smolbren query "MATCH (b:blog)-[:merged_from]->(x:Note) RETURN b.id, x.id"
# {"columns":["b.id","x.id"],"rows":[{"b.id":"blogs/context-engineering","x.id":"blogs/context-development-lifecycle"},...]}

smolbren query 'MATCH (n:Note) WHERE n.id = $id RETURN n.title' --param id=projects/prism
```

### `types` / `edges`

The discovered ontology with counts:

```json
[{"count":3,"type":"blog"},{"count":2,"type":"journal"},{"count":1,"type":"project"},...]
[{"count":3,"edge_type":"derives_from"},{"count":6,"edge_type":"mentions"},...]
```

## Output contract

All stdout is single-line JSON (pipe to `jq` for eyes). Errors go to stderr as
`{"error":"...","code":"..."}` and the process exits non-zero:

| Exit code | `code` | Meaning |
|-----------|--------|---------|
| 0 | — | ok |
| 1 | `internal` | unexpected internal error |
| 2 | — | usage error (bad flags/arguments) |
| 3 | `vault_not_found` | unknown vault name, or no vault configured |
| 4 | `note_not_found` | no note with that id |
| 5 | `index_missing` | vault registered but never indexed — run `smolbren index` |

## Configuration

```
~/.smolbren/
├── config.json          # {"vaults": {"personal": "/path/to/vault"}, "default_vault": "personal"}
└── vaults/<name>/
    ├── notes.lance      # id, path, type, title, frontmatter_json, body, hashes (FTS + scalar indices)
    ├── edges.lance      # from_id, edge_type, to_id, to_alias, resolved, position
    └── ontology.json    # discovered types + edge types with counts
```

Config is layered: the JSON file first, then `SMOLBREN_*` environment variables on
top — e.g. `SMOLBREN_DEFAULT_VAULT=work smolbren search "..."` for a one-off
override. `--config <path>` relocates everything (vault data lives next to the config
file), which is also how the test suite isolates itself.

## Agent skill

This CLI is designed to be driven by agents, and [`skills/smolbren/SKILL.md`](skills/smolbren/SKILL.md)
is a ready-made [Agent Skill](https://code.claude.com/docs/en/skills) that teaches an
agent the output contract, the explore-the-ontology-first workflow, Cypher rules, and
the common gotchas.

For Claude Code, install it for yourself (available in every project):

```sh
mkdir -p ~/.claude/skills/smolbren
curl -fsSL https://raw.githubusercontent.com/junaidrahim/smolbren/main/skills/smolbren/SKILL.md \
  -o ~/.claude/skills/smolbren/SKILL.md
```

or drop the same file into a project at `.claude/skills/smolbren/SKILL.md` to share it
with everyone working in that repo. Any other agent runtime that supports the Agent
Skills format can load the same file.

## Current limitations

- Cypher can filter/return only physical columns (`id`, `path`, `type`, `title`) —
  arbitrary frontmatter scalars like `status` are in `get`'s `frontmatter` object but
  not Cypher-addressable yet.
- Wikilink targets are resolved when the *source* note is indexed; deleting a target
  leaves stale `resolved` flags on unchanged notes until `index --full`.
- Embeddings + hybrid (BM25 + vector) search are phase 2; the schema is designed so a
  vector column slots in via lance schema evolution without a rewrite.

## Development

Requires Rust (edition 2024) and `protoc` on PATH (`brew install protobuf`).

```sh
cargo build
cargo test            # includes an end-to-end CLI test over tests/fixture_vault
```

> **Version lock:** lance-graph 0.5.4 pins `lance ^1.0` / `arrow 56.2` / `datafusion 50.3`.
> Do not bump `lance` past 1.x until lance-graph tracks a newer release. Verify with `cargo tree -d`.

Releases are automated: every push to `main` runs
[release-plz](https://release-plz.dev/), which computes the next semver from
[Conventional Commits](https://www.conventionalcommits.org/), updates the changelog,
publishes to crates.io, and tags a GitHub release. Use conventional commit messages
(`feat:`, `fix:`, …) so your change lands in the right version bump.

## License

[MIT](LICENSE)
