# CHANGELOG


## v0.0.0 (2026-04-28)

### Documentation

- Add docs/design.md architecture overview
  ([`5630b97`](https://github.com/junaidrahim/smolbren/commit/5630b979845c03003919627a0da5da3912911a61))

Agent-onboarding handbook covering modules, the SQLite/FTS5/vec0 schema with migration history,
  pipeline walkthroughs (ingest / embed / search / graph), the load-bearing invariants across
  modules, the testing setup (synthetic vault, FakeEmbedder, gated Ollama eval), common gotchas, and
  the documented follow-ups (MCP, eval, ontology wiring).

Grounded in the current code, not the spec — claims should be verifiable against src/.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>

- Add README and PyPI trusted-publisher workflow
  ([`90a4977`](https://github.com/junaidrahim/smolbren/commit/90a4977f7565a3b3324e7586b3bc5fa3a53da923))

README documents install, quickstart, commands, and the hybrid-search / graph internals. The publish
  workflow builds with uv on release and uploads to PyPI via OIDC (no API token); requires a `pypi`
  environment configured as a trusted publisher on PyPI.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
