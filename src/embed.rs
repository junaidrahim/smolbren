//! The `embed` command: chunk + embed note bodies into embeddings.lance.
//! Incremental by diffing each note's content_hash against what was
//! embedded last time; a no-op run never loads the model.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use arrow_array::cast::AsArray;
use rayon::prelude::*;
use serde::Serialize;

use crate::chunker;
use crate::embedder;
use crate::error::{Result, SmolbrenError};
use crate::store::embeddings::{self, EmbedMeta};
use crate::store::schema::EmbeddingRow;
use crate::store::{self, sql_in_list};
use crate::vault::Vault;

/// Notes fetched per `id IN (...)` scan, bounding filter size and the
/// bodies held in memory at once.
const FETCH_SLICE: usize = 500;

#[derive(Debug, Serialize)]
pub struct EmbedStats {
    pub scanned: usize,
    pub unchanged: usize,
    pub embedded: usize,
    pub removed: usize,
    pub chunks_written: usize,
    pub chunks_total: usize,
    pub model: String,
    pub duration_ms: u128,
}

struct NoteDoc {
    id: String,
    title: String,
    body: String,
    content_hash: String,
}

pub async fn run(vault: &Vault, models_dir: &Path, mut full: bool) -> Result<EmbedStats> {
    let t0 = Instant::now();
    let model_id = embedder::configured_id();
    let current_meta = EmbedMeta {
        model: model_id.to_string(),
        dim: embedder::EXPECTED_DIM,
        chunk_max_bytes: chunker::MAX_CHUNK_BYTES,
        overlap_bytes: chunker::OVERLAP_BYTES,
    };
    if !full && vault.has_embeddings() {
        let stored = EmbedMeta::load(&vault.embeddings_meta_path());
        if stored.as_ref() != Some(&current_meta) {
            eprintln!("note: embedding model or chunk params changed — re-embedding all notes");
            full = true;
        }
    }

    // Diff: what notes.lance holds now vs what was embedded last time.
    let notes_state = store::notes::load_state(vault).await?;
    let scanned = notes_state.len();
    let embedded_state: HashMap<String, String> =
        if full { HashMap::new() } else { embeddings::load_state(vault).await? };

    let mut changed_ids: Vec<String> = notes_state
        .iter()
        .filter(|(id, meta)| embedded_state.get(*id) != Some(&meta.content_hash))
        .map(|(id, _)| id.clone())
        .collect();
    changed_ids.sort();
    let removed_ids: Vec<String> = embedded_state
        .keys()
        .filter(|id| !notes_state.contains_key(*id))
        .cloned()
        .collect();

    if changed_ids.is_empty() && removed_ids.is_empty() {
        return Ok(EmbedStats {
            scanned,
            unchanged: scanned,
            embedded: 0,
            removed: 0,
            chunks_written: 0,
            chunks_total: embeddings::count(vault).await?,
            model: model_id.to_string(),
            duration_ms: t0.elapsed().as_millis(),
        });
    }

    let docs = fetch_docs(vault, &changed_ids).await?;

    // Chunk in parallel; empty bodies fall back to a title-only chunk so
    // every live note is findable by similarity search.
    let chunked: Vec<(String, i32, String, String, String)> = docs
        .par_iter()
        .flat_map_iter(|doc| {
            let mut chunks = chunker::chunk_markdown(&doc.body);
            if chunks.is_empty() {
                chunks.push(chunker::Chunk { seq: 0, text: doc.title.clone() });
            }
            chunks.into_iter().map(|c| {
                (doc.id.clone(), c.seq, doc.title.clone(), c.text, doc.content_hash.clone())
            })
        })
        .collect();

    // Model work is sync + CPU-bound; keep it off the async runtime.
    let models_dir = models_dir.to_path_buf();
    let pairs: Vec<(String, String)> =
        chunked.iter().map(|(_, _, title, text, _)| (title.clone(), text.clone())).collect();
    let vectors = tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>> {
        let mut model = embedder::create(&models_dir)?;
        let vectors = model.embed_docs(&pairs).map_err(SmolbrenError::Model)?;
        if let Some(v) = vectors.first() {
            if v.len() != model.dim() || model.dim() != embedder::EXPECTED_DIM {
                return Err(SmolbrenError::Model(anyhow::anyhow!(
                    "model produced {}-dim vectors, expected {}",
                    v.len(),
                    embedder::EXPECTED_DIM
                )));
            }
        }
        Ok(vectors)
    })
    .await
    .context("embedding task panicked")??;

    let embedded_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let rows: Vec<EmbeddingRow> = chunked
        .into_iter()
        .zip(vectors)
        .map(|((note_id, seq, _, chunk_text, content_hash), vector)| EmbeddingRow {
            note_id,
            seq,
            content_hash,
            chunk_text,
            vector,
            embedded_at_ms,
        })
        .collect();
    let chunks_written = rows.len();
    let dim = embedder::EXPECTED_DIM as i32;

    if full {
        embeddings::overwrite(vault, rows, dim).await?;
    } else {
        let mut owners = changed_ids.clone();
        owners.extend(removed_ids.iter().cloned());
        embeddings::replace_for(vault, &owners, rows, dim).await?;
    }
    embeddings::refresh_index(vault).await?;
    current_meta.save(&vault.embeddings_meta_path())?;

    Ok(EmbedStats {
        scanned,
        unchanged: scanned - changed_ids.len(),
        embedded: changed_ids.len(),
        removed: removed_ids.len(),
        chunks_written,
        chunks_total: embeddings::count(vault).await?,
        model: model_id.to_string(),
        duration_ms: t0.elapsed().as_millis(),
    })
}

/// id, title, body, content_hash for the given notes, fetched in slices.
async fn fetch_docs(vault: &Vault, ids: &[String]) -> Result<Vec<NoteDoc>> {
    let ds = store::open_dataset(&vault.notes_uri()).await?;
    let mut docs = Vec::with_capacity(ids.len());
    for slice in ids.chunks(FETCH_SLICE) {
        let mut scan = ds.scan();
        scan.project(&["id", "title", "body", "content_hash"])
            .context("projecting note docs")?;
        scan.filter(&format!("id IN ({})", sql_in_list(slice)))
            .context("filtering note docs")?;
        let batch = scan.try_into_batch().await.context("fetching note docs")?;
        let id_col = batch.column(0).as_string::<i32>();
        let titles = batch.column(1).as_string::<i32>();
        let bodies = batch.column(2).as_string::<i32>();
        let hashes = batch.column(3).as_string::<i32>();
        for i in 0..batch.num_rows() {
            docs.push(NoteDoc {
                id: id_col.value(i).to_string(),
                title: titles.value(i).to_string(),
                body: bodies.value(i).to_string(),
                content_hash: hashes.value(i).to_string(),
            });
        }
    }
    Ok(docs)
}
