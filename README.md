# smolbren

A portable brain for all us mere mortals trying to create a second brain.

## Design

So this is my idea: I want a tool that can do the following things over a body of markdown files (mostly managed by obsidian).

There are tools like `qmd` in the market that approach this problem of search by focusing on embeddings first search. I want to instead work 
on something that offers users what I call ontology first search over a body of markdown files using the markdown frontmatter.

And then layer on hybrid search (BM25 + Vector) on top of this.

This CLI is designed to be used by agents, not by humans, so I want it to be really simple.

The idea is that all markdown files will have a `type` key that would define the type of the note, any other key in the frontmatter that has wikilinks
to other notes would be edge types -- key being the edge and values being the outward going edge from the host note.

I want to build all of this on top of sqlite so it's fully local.

The CLI should also maintain an index of all the frontmatter hashes so it knows when to read the files and update it's index. I want to write it
in rust so it's heavily parallel and can use as much CPU as possible. 

```markdown
---
type: blog
created: 2026-05-10
updated: 2026-06-30
status: draft
for: "[[orgs/junaid-foo]]"
about: []
mentions: ["[[projects/prism]]", "[[repos/smolbren]]", "[[blogs/context-development-lifecycle]]", "[[blogs/context-platform-engineering]]"]
merged_from: ["[[blogs/context-development-lifecycle]]", "[[blogs/context-platform-engineering]]"]
derives_from: ["[[Journal/2026, June 01]]", "[[Journal/2026, June 04]]", "[[Journal/2026, June 10]]", "[[Journal/2026, June 12]]", "[[Journal/2026, June 22]]", "[[Journal/2026, June 29]]"]
published_url: ""
published_at:
---

# Context engineering

Draft thesis: prompt engineering changes the instruction; context engineering changes the system around the instruction. It is the work of deciding what an AI system knows, when it knows it, where that knowledge came from, how much space it deserves, whether the user is allowed to use it, and whether it actually improved the outcome.

This post should merge three threads into one argument:

- **Context engineering** - the distinction from prompt engineering.
- **Context development lifecycle** - the lifecycle for acquiring, shaping, delivering, evaluating, and evolving context.
- **Context platform engineering** - the platform layer agents talk to when context becomes production infrastructure.
```

^ this is how a sample note would look like in the vault. I want to build this whole thing using lance-graph, introduce a vault level namespacing, perhaps a new schema per vault.
Make this vault configurable via a ~/.smolbren/config.json.

## Build

Requires Rust (edition 2024) and `protoc` on PATH (`brew install protobuf`) — lance compiles
protobuf definitions at build time.

```sh
cargo build --release
```

> **Version lock:** lance-graph 0.5.4 pins `lance ^1.0` / `arrow 56.2` / `datafusion 50.3`.
> Do not bump `lance` past 1.x until lance-graph tracks a newer release. Verify with `cargo tree -d`.

## Usage

All stdout is single-line JSON (pipe to `jq` for eyes). Exit codes: `0` ok, `1` internal,
`2` usage, `3` vault not found, `4` note not found, `5` index missing.

```sh
# register a vault (first one becomes the default)
smolbren vault add personal ~/notes
smolbren vault list

# index (incremental — only changed files are re-read; --full rebuilds)
smolbren index
# {"scanned":9,"unchanged":0,"added":9,"updated":0,"removed":0,"edges":15,"unresolved_edges":0,"duration_ms":90}

# ontology
smolbren types                     # [{"type":"blog","count":3}, ...]
smolbren edges                     # [{"edge_type":"mentions","count":6}, ...]

# graph
smolbren links blogs/context-engineering --type mentions
smolbren backlinks projects/prism
smolbren query "MATCH (b:blog)-[:mentions]->(n:Note) RETURN b.id, n.id"

# notes + search
smolbren get blogs/context-engineering --body
smolbren search "context engineering" --type blog --limit 5
```

Every note's frontmatter `type` becomes a Cypher node label; `Note` is a catch-all label
matching every note. Every frontmatter key whose values contain wikilinks becomes a
relationship type. Cypher parameters: `--param min=30`.

### Storage layout

```
~/.smolbren/
├── config.json          # {"vaults": {"personal": "/path/to/vault"}, "default_vault": "personal"}
│                        # settings can be overridden per-invocation via SMOLBREN_* env vars,
│                        # e.g. SMOLBREN_DEFAULT_VAULT=work
└── vaults/<name>/
    ├── notes.lance      # id, path, type, title, frontmatter_json, body, hashes (FTS + scalar indices)
    ├── edges.lance      # from_id, edge_type, to_id, to_alias, resolved, position
    └── ontology.json    # discovered types + edge types with counts
```

### Current limitations

- Cypher can filter/return only physical columns (`id`, `path`, `type`, `title`) — arbitrary
  frontmatter scalars like `status` are in `get`'s `frontmatter` object but not Cypher-addressable yet.
- Wikilink targets are resolved when the *source* note is indexed; deleting a target leaves stale
  `resolved` flags on unchanged notes until `index --full`.
- Embeddings + hybrid (BM25 + vector) search are phase 2; the schema is designed so a vector
  column slots in via lance schema evolution without a rewrite.

