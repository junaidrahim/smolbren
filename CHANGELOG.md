# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.6](https://github.com/junaidrahim/smolbren/compare/v0.1.5...v0.1.6) - 2026-09-03

### Fixed

- *(ci)* repair release builds and cache reuse ([#13](https://github.com/junaidrahim/smolbren/pull/13))

## [0.1.5](https://github.com/junaidrahim/smolbren/compare/v0.1.4...v0.1.5) - 2026-09-02

### Fixed

- *(graph)* support anonymous Cypher patterns ([#12](https://github.com/junaidrahim/smolbren/pull/12))

### Other

- configure Amp orb lifecycle

### Fixed

- Cypher queries can now scan anonymous nodes and traverse anonymous
  relationships across the full vault graph ([#9](https://github.com/junaidrahim/smolbren/issues/9)).

## [0.1.4](https://github.com/junaidrahim/smolbren/compare/v0.1.3...v0.1.4) - 2026-08-11

### Added

- add progress logging and prebuilt Homebrew releases ([#11](https://github.com/junaidrahim/smolbren/pull/11))

## [0.1.3](https://github.com/junaidrahim/smolbren/compare/v0.1.2...v0.1.3) - 2026-08-07

### Added

- ship smolbren stabilization backlog ([#10](https://github.com/junaidrahim/smolbren/pull/10))

### Added

- Canonical agent documentation via `smolbren docs --agent`.
- Embedding freshness counts and `embeddings_stale` in `vault list`.
- Scalar frontmatter properties, including ISO dates, in Cypher queries.
- `smolbren unresolved` for dangling-link grooming.
- BM25 context snippets and `--path` filters across keyword, semantic, and hybrid search.
- Raw Markdown output through `get --format text`.
- Transactional `index --full` rebuilds and the `smolbren repair` recovery command.

### Fixed

- Index failures now retain the underlying Lance error and identify the dataset,
  operation, and candidate note paths.
- Failed full rebuilds no longer overwrite the last readable index.

## [0.1.2](https://github.com/junaidrahim/smolbren/compare/v0.1.1...v0.1.2) - 2026-07-05

### Added

- *(search)* local embedding pipeline, similar command and hybrid search ([#8](https://github.com/junaidrahim/smolbren/pull/8))

### Other

- *(readme)* link docs at smolbren.com custom domain ([#7](https://github.com/junaidrahim/smolbren/pull/7))

## [0.1.1](https://github.com/junaidrahim/smolbren/compare/v0.1.0...v0.1.1) - 2026-07-04

### Added

- *(cli)* add ascii art banner to help menu ([#6](https://github.com/junaidrahim/smolbren/pull/6))

### Other

- rewrite readme as full explainer and add installable agent skill ([#4](https://github.com/junaidrahim/smolbren/pull/4))
- *(release)* pass --git-token to release-plz release ([#3](https://github.com/junaidrahim/smolbren/pull/3))
- setup mintlify ([#5](https://github.com/junaidrahim/smolbren/pull/5))
- *(release)* automate semver bumps from conventional commits with release-plz ([#2](https://github.com/junaidrahim/smolbren/pull/2))
- *(config)* load config via the config crate ([#1](https://github.com/junaidrahim/smolbren/pull/1))
