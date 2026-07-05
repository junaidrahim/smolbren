//! Local embedding models behind a sync trait, so the store and search
//! layers never depend on a specific runtime. The real implementation is
//! fastembed (ONNX); a deterministic hash embedder keeps tests hermetic.

mod fastembed;
mod hash;

use std::path::Path;

use crate::error::{Result, SmolbrenError};

/// Output dimension of EmbeddingGemma-300M (and the hash fake, so test
/// datasets have the production shape).
pub const EXPECTED_DIM: usize = 768;

/// Tokenizer truncation length. Must comfortably exceed the chunker's
/// ~800-token chunks; EmbeddingGemma's context window is 2048.
pub const MAX_TOKENS: usize = 1024;

/// Sync by design: implementations are CPU-bound. Callers run them inside
/// `tokio::task::spawn_blocking`. The implementation's identity (for the
/// meta file / re-embed trigger) comes from `configured_id`, not the trait.
pub trait Embedder: Send {
    fn dim(&self) -> usize;
    fn embed_docs(&mut self, title_text_pairs: &[(String, String)]) -> anyhow::Result<Vec<Vec<f32>>>;
    fn embed_query(&mut self, query: &str) -> anyhow::Result<Vec<f32>>;
}

/// EmbeddingGemma prompt formats (fastembed applies none itself).
pub fn query_prompt(query: &str) -> String {
    format!("task: search result | query: {query}")
}

pub fn doc_prompt(title: &str, text: &str) -> String {
    format!("title: {title} | text: {text}")
}

/// Build the configured embedder. `SMOLBREN_EMBEDDER=hash` selects the
/// deterministic fake (used by the test suite); anything else gets the
/// real model, downloading it into `models_dir` on first use.
pub fn create(models_dir: &Path) -> Result<Box<dyn Embedder>> {
    match std::env::var("SMOLBREN_EMBEDDER").as_deref() {
        Ok("hash") => Ok(Box::new(hash::HashEmbedder)),
        _ => {
            let real = fastembed::FastEmbedder::new(models_dir).map_err(SmolbrenError::Model)?;
            Ok(Box::new(real))
        }
    }
}

/// Id of the embedder `create` would build, without constructing it —
/// lets the embed pipeline detect model drift (and no-op cheaply) before
/// paying for model load.
pub fn configured_id() -> &'static str {
    match std::env::var("SMOLBREN_EMBEDDER").as_deref() {
        Ok("hash") => hash::ID,
        _ => fastembed::ID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_formats_match_embeddinggemma_spec() {
        assert_eq!(
            query_prompt("rust lifetimes"),
            "task: search result | query: rust lifetimes"
        );
        assert_eq!(
            doc_prompt("My Note", "some body"),
            "title: My Note | text: some body"
        );
    }
}
