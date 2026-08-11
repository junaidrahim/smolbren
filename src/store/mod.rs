pub mod edges;
pub mod embeddings;
pub mod notes;
pub mod schema;

use anyhow::Context;
use lance::Dataset;
use lance_index::scalar::{BuiltinIndexType, InvertedIndexParams, ScalarIndexParams};
use lance_index::{DatasetIndexExt, IndexType};

use crate::error::Result;
use crate::progress::Progress;
use crate::vault::Vault;

/// Per-note metadata from the last index run, used for incremental diffing.
#[derive(Debug)]
pub struct StoredMeta {
    pub content_hash: String,
    pub mtime_ms: i64,
    pub size_bytes: i64,
}

pub fn sql_quote(s: &str) -> String {
    s.replace('\'', "''")
}

pub fn sql_str(s: &str) -> String {
    format!("'{}'", sql_quote(s))
}

pub fn sql_in_list(values: &[String]) -> String {
    values.iter().map(|s| sql_str(s)).collect::<Vec<_>>().join(", ")
}

pub async fn open_dataset(uri: &str) -> Result<Dataset> {
    Ok(Dataset::open(uri)
        .await
        .with_context(|| format!("opening dataset {uri}"))?)
}

/// (Re)create all indices. FTS still works on unindexed rows via lance's
/// flat-search fallback, so this is a performance concern, not correctness;
/// replace=true keeps the logic simple at personal-vault scale.
pub async fn refresh_indices(vault: &Vault) -> Result<()> {
    let mut notes = open_dataset(&vault.notes_uri()).await?;
    let has_notes = notes.count_rows(None).await.context("counting notes")? > 0;
    let mut edges = if vault.data_dir.join("edges.lance").exists() {
        Some(open_dataset(&vault.edges_uri()).await?)
    } else {
        None
    };
    let has_edges = match edges.as_ref() {
        Some(edges) => edges.count_rows(None).await.context("counting edges")? > 0,
        None => false,
    };
    let total = usize::from(has_notes) * 4 + usize::from(has_edges) * 3;
    if total == 0 {
        eprintln!("index: no search indexes to refresh");
        return Ok(());
    }
    eprintln!("index: refreshing 0/{total} search indexes");
    let mut completed = 0usize;
    let mut progress = Progress::new("index: refreshed", "indexes", total);

    if has_notes {
        let inverted = InvertedIndexParams::default();
        let btree = ScalarIndexParams::default();
        let bitmap = ScalarIndexParams::for_builtin(BuiltinIndexType::Bitmap);
        notes
            .create_index(&["body"], IndexType::Inverted, None, &inverted, true)
            .await
            .context("creating body FTS index")?;
        completed += 1;
        progress.update(completed);
        notes
            .create_index(&["title"], IndexType::Inverted, None, &inverted, true)
            .await
            .context("creating title FTS index")?;
        completed += 1;
        progress.update(completed);
        notes
            .create_index(&["id"], IndexType::BTree, None, &btree, true)
            .await
            .context("creating id index")?;
        completed += 1;
        progress.update(completed);
        notes
            .create_index(&["type"], IndexType::Bitmap, None, &bitmap, true)
            .await
            .context("creating type index")?;
        completed += 1;
        progress.update(completed);
    }

    if has_edges
        && let Some(edges) = edges.as_mut()
    {
        let btree = ScalarIndexParams::default();
        let bitmap = ScalarIndexParams::for_builtin(BuiltinIndexType::Bitmap);
        edges
            .create_index(&["from_id"], IndexType::BTree, None, &btree, true)
            .await
            .context("creating from_id index")?;
        completed += 1;
        progress.update(completed);
        edges
            .create_index(&["to_id"], IndexType::BTree, None, &btree, true)
            .await
            .context("creating to_id index")?;
        completed += 1;
        progress.update(completed);
        edges
            .create_index(&["edge_type"], IndexType::Bitmap, None, &bitmap, true)
            .await
            .context("creating edge_type index")?;
        completed += 1;
        progress.update(completed);
    }
    Ok(())
}
