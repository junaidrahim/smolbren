//! Real embedder: EmbeddingGemma-300M (quantized ONNX) via fastembed.
//! The model (~300MB) is fetched from Hugging Face into the smolbren
//! models dir on first use and loaded from cache afterwards.

use std::path::Path;

use anyhow::Context;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use super::{doc_prompt, query_prompt, Embedder, MAX_TOKENS};

pub const ID: &str = "embeddinggemma-300m-onnx-q4";

// Q4, NOT Q8 (EmbeddingGemma300MQ): fastembed marks the Q8 build as
// dynamically quantized, which both rejects batched embedding outright
// and makes vectors from separate invocations incompatible — queries
// are always embedded in a different process than documents here.
const MODEL: EmbeddingModel = EmbeddingModel::EmbeddingGemma300MQ4;
const BATCH_SIZE: usize = 32;

pub struct FastEmbedder {
    model: TextEmbedding,
}

impl FastEmbedder {
    pub fn new(models_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(models_dir)
            .with_context(|| format!("creating models dir {}", models_dir.display()))?;
        // hf-hub caches repos as `models--<org>--<name>` under the cache dir.
        let cached = models_dir
            .join("models--onnx-community--embeddinggemma-300m-ONNX")
            .exists();
        if !cached {
            eprintln!(
                "downloading embedding model (~300MB, one-time) to {}",
                models_dir.display()
            );
        }
        let options = TextInitOptions::new(MODEL)
            .with_cache_dir(models_dir.to_path_buf())
            .with_show_download_progress(true)
            .with_max_length(MAX_TOKENS);
        let model = TextEmbedding::try_new(options).context(
            "initializing embedding model — it downloads from Hugging Face on first use; \
             check network access, or delete the models dir to clear a corrupt cache",
        )?;
        Ok(Self { model })
    }
}

impl Embedder for FastEmbedder {
    fn dim(&self) -> usize {
        super::EXPECTED_DIM
    }

    fn embed_docs(&mut self, pairs: &[(String, String)]) -> anyhow::Result<Vec<Vec<f32>>> {
        let texts: Vec<String> =
            pairs.iter().map(|(title, text)| doc_prompt(title, text)).collect();
        self.model.embed(&texts, Some(BATCH_SIZE)).context("embedding chunks")
    }

    fn embed_query(&mut self, query: &str) -> anyhow::Result<Vec<f32>> {
        let out = self
            .model
            .embed(&[query_prompt(query)], None)
            .context("embedding query")?;
        out.into_iter().next().context("model returned no embedding")
    }
}
