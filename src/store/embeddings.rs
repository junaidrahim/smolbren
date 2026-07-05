//! Chunk-embedding dataset (`embeddings.lance`), maintained by `smolbren
//! embed` independently of the notes/edges index. Same delete+append
//! replacement strategy as edges: chunk counts vary per note, so a merge
//! on a composite key buys nothing.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use arrow_array::cast::AsArray;
use arrow_array::RecordBatchIterator;
use lance::dataset::{WriteMode, WriteParams};
use lance::Dataset;
use lance_index::scalar::ScalarIndexParams;
use lance_index::{DatasetIndexExt, IndexType};
use serde::{Deserialize, Serialize};

use super::schema::{embeddings_batch, embeddings_schema, EmbeddingRow};
use super::{open_dataset, sql_in_list};
use crate::error::Result;
use crate::vault::Vault;

/// ~1024 rows * 768 dims * 4B ≈ 3MB per written batch.
const WRITE_BATCH_ROWS: usize = 1024;

fn exists(vault: &Vault) -> bool {
    vault.data_dir.join("embeddings.lance").exists()
}

/// Embedding-run parameters recorded per vault (embeddings_meta.json).
/// Any drift means stored vectors are incomparable with new ones, so the
/// embed pipeline escalates to a full re-embed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedMeta {
    pub model: String,
    pub dim: usize,
    pub chunk_max_bytes: usize,
    pub overlap_bytes: usize,
}

impl EmbedMeta {
    pub fn load(path: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = serde_json::to_string_pretty(self).context("serializing embed meta")?;
        std::fs::write(path, raw)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// note_id -> content_hash at embed time. All chunks of a note share the
/// note's hash, so duplicates collapse into one entry.
pub async fn load_state(vault: &Vault) -> Result<HashMap<String, String>> {
    if !exists(vault) {
        return Ok(HashMap::new());
    }
    let ds = open_dataset(&vault.embeddings_uri()).await?;
    let mut scan = ds.scan();
    scan.project(&["note_id", "content_hash"])
        .context("projecting embedding state columns")?;
    let batch = scan.try_into_batch().await.context("scanning embedding state")?;
    let ids = batch.column(0).as_string::<i32>();
    let hashes = batch.column(1).as_string::<i32>();
    let mut map = HashMap::new();
    for i in 0..batch.num_rows() {
        map.insert(ids.value(i).to_string(), hashes.value(i).to_string());
    }
    Ok(map)
}

fn batches(rows: &[EmbeddingRow], dim: i32) -> Result<Vec<arrow_array::RecordBatch>> {
    Ok(rows
        .chunks(WRITE_BATCH_ROWS)
        .map(|c| embeddings_batch(c, dim))
        .collect::<anyhow::Result<Vec<_>>>()?)
}

/// Replace all chunks owned by `note_ids` with `rows` (delete + append).
pub async fn replace_for(
    vault: &Vault,
    note_ids: &[String],
    rows: Vec<EmbeddingRow>,
    dim: i32,
) -> Result<()> {
    if !exists(vault) {
        if rows.is_empty() {
            return Ok(());
        }
        return overwrite(vault, rows, dim).await;
    }
    if !note_ids.is_empty() {
        let mut ds = open_dataset(&vault.embeddings_uri()).await?;
        ds.delete(&format!("note_id IN ({})", sql_in_list(note_ids)))
            .await
            .context("deleting stale embeddings")?;
    }
    if !rows.is_empty() {
        let b = batches(&rows, dim)?;
        let reader = RecordBatchIterator::new(b.into_iter().map(Ok), embeddings_schema(dim));
        let params = WriteParams { mode: WriteMode::Append, ..Default::default() };
        Dataset::write(reader, vault.embeddings_uri().as_str(), Some(params))
            .await
            .context("appending embeddings")?;
    }
    Ok(())
}

pub async fn overwrite(vault: &Vault, rows: Vec<EmbeddingRow>, dim: i32) -> Result<()> {
    std::fs::create_dir_all(&vault.data_dir).context("creating vault data dir")?;
    let b = batches(&rows, dim)?;
    let reader = RecordBatchIterator::new(b.into_iter().map(Ok), embeddings_schema(dim));
    let params = WriteParams { mode: WriteMode::Overwrite, ..Default::default() };
    Dataset::write(reader, vault.embeddings_uri().as_str(), Some(params))
        .await
        .context("overwriting embeddings dataset")?;
    Ok(())
}

/// Scalar index on note_id for the delete/join paths. No vector index:
/// at personal-vault scale flat exact KNN is fast and lossless; add
/// IVF_FLAT here (VectorIndexParams::ivf_flat, Cosine) if chunk counts
/// ever reach ~50k+.
pub async fn refresh_index(vault: &Vault) -> Result<()> {
    if !exists(vault) {
        return Ok(());
    }
    let mut ds = open_dataset(&vault.embeddings_uri()).await?;
    if ds.count_rows(None).await.context("counting embeddings")? > 0 {
        let btree = ScalarIndexParams::default();
        ds.create_index(&["note_id"], IndexType::BTree, None, &btree, true)
            .await
            .context("creating note_id index")?;
    }
    Ok(())
}

pub async fn count(vault: &Vault) -> Result<usize> {
    if !exists(vault) {
        return Ok(0);
    }
    let ds = open_dataset(&vault.embeddings_uri()).await?;
    Ok(ds.count_rows(None).await.context("counting embeddings")?)
}
