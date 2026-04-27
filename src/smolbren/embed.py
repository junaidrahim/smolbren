"""Ollama client + embedding cache.

Owns:
- Talking to a local Ollama server for `nomic-embed-text` embeddings.
- Skipping re-embedding when a chunk's content_hash already has a row in `vec_chunks`.
- Batched embedding calls.

Implemented in Milestone 2.
"""
