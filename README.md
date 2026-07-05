# smolbren

A portable brain for all us mere mortals trying to create a second brain.

`smolbren` turns a folder of markdown files — an Obsidian vault, a notes directory,
anything with frontmatter — into a local, queryable knowledge graph with full-text
search on top. It doesn't impose a schema; it **discovers** one from the frontmatter
you already write, then lets you (or your agent) query it with Cypher and BM25.

- **Ontology-first.** Tools like `qmd` approach vault search embeddings-first.
  smolbren instead starts from the structure your notes already encode: the
  frontmatter `type` key becomes a node type, and every frontmatter key holding
  `[[wikilinks]]` becomes a typed edge in a graph. Embeddings come on top —
  `smolbren embed` runs a local model (EmbeddingGemma-300M via ONNX, nothing
  leaves your machine) for semantic `similar` search and BM25+vector
  `search --hybrid`.
- **Built for agents.** Every command prints single-line JSON to stdout, errors are
  JSON on stderr with stable exit codes, and nothing is interactive. Humans pipe to
  `jq`; agents parse directly — there's a [ready-made skill](#agent-skill) too.
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

From each file, `smolbren index` derives an **id** (the vault-relative path without
`.md` — `books/the-dispossessed`, the same shape wikilink targets use), a **type**
(`book`, which becomes a Cypher node label alongside the catch-all `Note`), a
**title** (the first `# heading`), and **edges** — every frontmatter key whose values
contain wikilinks becomes a relationship type (here `author`, `themes`, and
`related`), while scalar keys like `status` stay on the note as plain frontmatter.
The discovered types and edge types are the vault's **ontology**: a graph schema you
never have to configure, queryable with Cypher and searchable with BM25.

## Install

From [crates.io](https://crates.io/crates/smolbren):

```sh
cargo install smolbren
```

`cargo install` builds from source, so you need Rust (edition 2024) and `protoc` on
PATH (`brew install protobuf`) — Lance compiles protobuf definitions at build time.

## Use it

```sh
smolbren vault add personal ~/notes    # register a vault
smolbren index                         # incremental index (rerun any time)
smolbren types                         # the discovered ontology
smolbren search "ambiguous utopia"     # BM25 full-text search
smolbren query "MATCH (b:book)-[:themes]->(t:Note) RETURN b.id, t.id"

smolbren embed                         # embed chunks with a local model (~300MB, one-time download)
smolbren similar "two worlds divided by ideology"   # semantic similarity search
smolbren search "utopia" --hybrid      # BM25 + vector, fused with RRF
```

Full documentation lives at **[smolbren.com](https://smolbren.com)**:
the [quickstart](https://smolbren.com/quickstart), core concepts
(vaults, ontology, indexing, search), guides for
[Obsidian setup](https://smolbren.com/guides/obsidian-setup),
[querying the graph](https://smolbren.com/guides/querying-graph), and
[scripting & agents](https://smolbren.com/guides/scripting-agents), plus the
complete [CLI reference](https://smolbren.com/cli/overview) with every flag,
output shape, and exit code.

## Agent skill

This CLI is designed to be driven by agents, and [`skills/smolbren/SKILL.md`](skills/smolbren/SKILL.md)
is a ready-made [Agent Skill](https://agentskills.io) that teaches an agent the output
contract, the explore-the-ontology-first workflow, Cypher rules, and the common
gotchas. Install it with the [skills CLI](https://github.com/vercel-labs/skills),
which detects your coding agents (Claude Code, Cursor, …) and installs it into each:

```sh
npx skills add junaidrahim/smolbren
```

Or install manually by copying the file into your agent's skills directory — for
Claude Code, `~/.claude/skills/smolbren/SKILL.md` (personal) or
`.claude/skills/smolbren/SKILL.md` (per-project).

## Current limitations

- Cypher can filter/return only physical columns (`id`, `path`, `type`, `title`) —
  arbitrary frontmatter scalars like `status` are in `get`'s `frontmatter` object but
  not Cypher-addressable yet.
- Wikilink targets are resolved when the *source* note is indexed; deleting a target
  leaves stale `resolved` flags on unchanged notes until `index --full`.
- `embed` is a separate step from `index` — new or edited notes are invisible to
  `similar`/`search --hybrid` until you run `smolbren embed` again (it's incremental,
  so rerunning is cheap).

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
(`feat:`, `fix:`, …) so your change lands in the right version bump. Docs live in
[`docs/`](docs/) as a Mintlify site and deploy on push to `main`.

## License

[MIT](LICENSE)
