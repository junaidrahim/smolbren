//! Vector similarity search (`similar`) and BM25+vector hybrid search
//! (`search --hybrid`) over the chunk embeddings in embeddings.lance.
//!
//! Chunk hits are deduped to the best chunk per note, then joined with
//! note metadata the same way edges::backlinks joins source notes. With
//! no vector index this is exact flat KNN — plenty at personal-vault
//! scale (see store::embeddings::refresh_index).

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use arrow_array::cast::AsArray;
use arrow_array::types::{Float32Type, Int32Type};
use arrow_array::{Array, Float32Array};
use lance_linalg::distance::DistanceType;

use crate::embedder;
use crate::error::{Result, SmolbrenError};
use crate::search;
use crate::store::{self, sql_in_list};
use crate::vault::Vault;

const RRF_K: f32 = 60.0;
const SNIPPET_BYTES: usize = 280;

struct VectorHit {
    note_id: String,
    seq: i32,
    chunk_text: String,
    similarity: f32,
}

struct NoteMeta {
    path: String,
    note_type: Option<String>,
    title: String,
}

/// Semantic similarity search. Returns
/// [{"id","path","type","title","score","chunk_seq","snippet"}].
pub async fn similar(
    vault: &Vault,
    models_dir: &Path,
    query: &str,
    note_type: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let qvec = embed_query(models_dir, query).await?;
    // Overfetch: chunk→note dedupe and the post-join type filter both
    // shrink the candidate set.
    let k = (limit * 8).clamp(50, 512);
    let hits = vector_search(vault, qvec, k).await?;
    let ids: Vec<String> = hits.iter().map(|h| h.note_id.clone()).collect();
    let meta = join_notes(vault, &ids).await?;

    let mut rows = Vec::with_capacity(limit);
    for h in &hits {
        // Notes deleted since the last `embed` don't join; drop them.
        let Some(m) = meta.get(&h.note_id) else { continue };
        if let Some(t) = note_type {
            if m.note_type.as_deref() != Some(t) {
                continue;
            }
        }
        rows.push(serde_json::json!({
            "id": h.note_id,
            "path": m.path,
            "type": m.note_type,
            "title": m.title,
            "score": h.similarity,
            "chunk_seq": h.seq,
            "snippet": snippet(&h.chunk_text),
        }));
        if rows.len() == limit {
            break;
        }
    }
    Ok(rows)
}

/// Hybrid search: BM25 and vector rankings fused with Reciprocal Rank
/// Fusion (k=60). Output keeps `score` = RRF plus the per-side component
/// scores (null when a note appeared on one side only).
pub async fn hybrid(
    vault: &Vault,
    models_dir: &Path,
    query: &str,
    note_type: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let depth = (limit * 5).max(50);
    let bm25_rows = search::bm25(vault, query, note_type, depth).await?;

    let qvec = embed_query(models_dir, query).await?;
    let hits = vector_search(vault, qvec, depth.clamp(50, 512)).await?;
    let ids: Vec<String> = hits.iter().map(|h| h.note_id.clone()).collect();
    let meta = join_notes(vault, &ids).await?;
    // Filter the vector list by type *before* ranking so both fused
    // lists are filtered consistently (BM25 filters in its scan).
    let vec_hits: Vec<&VectorHit> = hits
        .iter()
        .filter(|h| match (note_type, meta.get(&h.note_id)) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(t), Some(m)) => m.note_type.as_deref() == Some(t),
        })
        .take(depth)
        .collect();

    let bm25_ranked: Vec<String> = bm25_rows
        .iter()
        .filter_map(|r| r["id"].as_str().map(String::from))
        .collect();
    let vec_ranked: Vec<String> = vec_hits.iter().map(|h| h.note_id.clone()).collect();
    let mut fused: Vec<(String, f32)> =
        rrf_fuse(&[bm25_ranked, vec_ranked], RRF_K).into_iter().collect();
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let bm25_by_id: HashMap<&str, &serde_json::Value> =
        bm25_rows.iter().filter_map(|r| r["id"].as_str().map(|id| (id, r))).collect();
    let vec_by_id: HashMap<&str, &&VectorHit> =
        vec_hits.iter().map(|h| (h.note_id.as_str(), h)).collect();

    let mut rows = Vec::with_capacity(limit);
    for (id, rrf_score) in fused.into_iter().take(limit) {
        let bm25_row = bm25_by_id.get(id.as_str());
        let vec_hit = vec_by_id.get(id.as_str());
        let (path, note_ty, title) = if let Some(r) = bm25_row {
            (r["path"].clone(), r["type"].clone(), r["title"].clone())
        } else if let Some(m) = meta.get(&id) {
            (
                m.path.clone().into(),
                m.note_type.clone().map_or(serde_json::Value::Null, Into::into),
                m.title.clone().into(),
            )
        } else {
            continue;
        };
        rows.push(serde_json::json!({
            "id": id,
            "path": path,
            "type": note_ty,
            "title": title,
            "score": rrf_score,
            "bm25_score": bm25_row.map(|r| r["score"].clone()).unwrap_or(serde_json::Value::Null),
            "similarity": vec_hit.map(|h| h.similarity.into()).unwrap_or(serde_json::Value::Null),
            "snippet": vec_hit.map(|h| snippet(&h.chunk_text).into()).unwrap_or(serde_json::Value::Null),
        }));
    }
    Ok(rows)
}

/// score(id) = Σ over lists containing id of 1/(k + rank), rank 1-based.
fn rrf_fuse(lists: &[Vec<String>], k: f32) -> HashMap<String, f32> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    for list in lists {
        for (i, id) in list.iter().enumerate() {
            *scores.entry(id.clone()).or_default() += 1.0 / (k + i as f32 + 1.0);
        }
    }
    scores
}

async fn embed_query(models_dir: &Path, query: &str) -> Result<Vec<f32>> {
    let models_dir = models_dir.to_path_buf();
    let query = query.to_string();
    tokio::task::spawn_blocking(move || -> Result<Vec<f32>> {
        let mut model = embedder::create(&models_dir)?;
        model.embed_query(&query).map_err(SmolbrenError::Model)
    })
    .await
    .context("query embedding task panicked")?
}

/// Exact KNN over chunks, deduped to the best (most similar) chunk per
/// note, ordered by descending similarity.
async fn vector_search(vault: &Vault, qvec: Vec<f32>, k: usize) -> Result<Vec<VectorHit>> {
    let ds = store::open_dataset(&vault.embeddings_uri()).await?;
    let qarr = Float32Array::from(qvec);
    let mut scan = ds.scan();
    scan.nearest("vector", &qarr, k).context("building vector search")?;
    scan.distance_metric(DistanceType::Cosine);
    scan.project(&["note_id", "seq", "chunk_text", "_distance"])
        .context("projecting vector search columns")?;
    let batch = scan.try_into_batch().await.context("running vector search")?;

    let ids = batch.column_by_name("note_id").context("note_id column")?.as_string::<i32>();
    let seqs = batch.column_by_name("seq").context("seq column")?.as_primitive::<Int32Type>();
    let texts =
        batch.column_by_name("chunk_text").context("chunk_text column")?.as_string::<i32>();
    let dists =
        batch.column_by_name("_distance").context("_distance column")?.as_primitive::<Float32Type>();

    let mut best: HashMap<&str, VectorHit> = HashMap::new();
    for i in 0..batch.num_rows() {
        let id = ids.value(i);
        // Cosine distance ∈ [0, 2] → similarity ∈ [-1, 1].
        let similarity = 1.0 - dists.value(i);
        let better = best.get(id).is_none_or(|h| similarity > h.similarity);
        if better {
            best.insert(
                id,
                VectorHit {
                    note_id: id.to_string(),
                    seq: seqs.value(i),
                    chunk_text: texts.value(i).to_string(),
                    similarity,
                },
            );
        }
    }
    let mut hits: Vec<VectorHit> = best.into_values().collect();
    hits.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.note_id.cmp(&b.note_id))
    });
    Ok(hits)
}

/// id -> (path, type, title), the same join-scan edges::backlinks uses.
async fn join_notes(vault: &Vault, ids: &[String]) -> Result<HashMap<String, NoteMeta>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ds = store::open_dataset(&vault.notes_uri()).await?;
    let mut scan = ds.scan();
    scan.project(&["id", "path", "type", "title"]).context("projecting note columns")?;
    scan.filter(&format!("id IN ({})", sql_in_list(ids))).context("filtering notes")?;
    let batch = scan.try_into_batch().await.context("fetching note metadata")?;
    let id_col = batch.column(0).as_string::<i32>();
    let paths = batch.column(1).as_string::<i32>();
    let types = batch.column(2).as_string::<i32>();
    let titles = batch.column(3).as_string::<i32>();
    let mut map = HashMap::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        map.insert(
            id_col.value(i).to_string(),
            NoteMeta {
                path: paths.value(i).to_string(),
                note_type: types.is_valid(i).then(|| types.value(i).to_string()),
                title: titles.value(i).to_string(),
            },
        );
    }
    Ok(map)
}

/// Chunk text trimmed to a UTF-8-safe preview.
fn snippet(text: &str) -> String {
    let clean = text.trim();
    if clean.len() <= SNIPPET_BYTES {
        return clean.to_string();
    }
    let mut end = SNIPPET_BYTES;
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &clean[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_overlap_beats_single_list() {
        let bm25 = vec!["a".to_string(), "b".to_string()];
        let vector = vec!["b".to_string(), "c".to_string()];
        let scores = rrf_fuse(&[bm25, vector], 60.0);
        // b: 1/62 + 1/61 beats a: 1/61 and c: 1/62.
        assert!(scores["b"] > scores["a"]);
        assert!(scores["a"] > scores["c"]);
    }

    #[test]
    fn rrf_math_is_one_over_k_plus_rank() {
        let scores = rrf_fuse(&[vec!["x".to_string()]], 60.0);
        assert!((scores["x"] - 1.0 / 61.0).abs() < 1e-7);
    }

    #[test]
    fn rrf_empty_lists_are_empty() {
        assert!(rrf_fuse(&[], 60.0).is_empty());
        assert!(rrf_fuse(&[vec![], vec![]], 60.0).is_empty());
    }

    #[test]
    fn snippet_truncates_on_char_boundary() {
        let long = "é".repeat(300);
        let s = snippet(&long);
        assert!(s.ends_with('…'));
        assert!(s.len() <= SNIPPET_BYTES + '…'.len_utf8());
        assert!(snippet("short") == "short");
    }
}
