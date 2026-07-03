use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::Context;
use arrow_array::cast::AsArray;
use arrow_array::types::Int64Type;
use arrow_array::RecordBatchIterator;
use lance::dataset::{MergeInsertBuilder, WhenMatched, WhenNotMatched, WriteMode, WriteParams};
use lance::Dataset;

use super::schema::{notes_batch, notes_schema, NoteRow};
use super::{open_dataset, sql_in_list, sql_str, StoredMeta};
use crate::error::{Result, SmolbrenError};
use crate::output;
use crate::parser;
use crate::vault::Vault;

fn exists(vault: &Vault) -> bool {
    vault.data_dir.join("notes.lance").exists()
}

/// id -> stored (content_hash, mtime, size) from the last index run.
pub async fn load_state(vault: &Vault) -> Result<HashMap<String, StoredMeta>> {
    if !exists(vault) {
        return Ok(HashMap::new());
    }
    let ds = open_dataset(&vault.notes_uri()).await?;
    let mut scan = ds.scan();
    scan.project(&["id", "content_hash", "mtime_ms", "size_bytes"])
        .context("projecting state columns")?;
    let batch = scan.try_into_batch().await.context("scanning note state")?;

    let ids = batch.column(0).as_string::<i32>();
    let hashes = batch.column(1).as_string::<i32>();
    let mtimes = batch.column(2).as_primitive::<Int64Type>();
    let sizes = batch.column(3).as_primitive::<Int64Type>();

    let mut map = HashMap::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        map.insert(
            ids.value(i).to_string(),
            StoredMeta {
                content_hash: hashes.value(i).to_string(),
                mtime_ms: mtimes.value(i),
                size_bytes: sizes.value(i),
            },
        );
    }
    Ok(map)
}

pub async fn upsert(vault: &Vault, rows: Vec<NoteRow>) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let batch = notes_batch(&rows)?;
    let reader = RecordBatchIterator::new(vec![batch].into_iter().map(Ok), notes_schema());
    if !exists(vault) {
        std::fs::create_dir_all(&vault.data_dir).context("creating vault data dir")?;
        Dataset::write(reader, vault.notes_uri().as_str(), Some(WriteParams::default()))
            .await
            .context("creating notes dataset")?;
        return Ok(());
    }
    let ds = Arc::new(open_dataset(&vault.notes_uri()).await?);
    MergeInsertBuilder::try_new(ds, vec!["id".to_string()])
        .context("building merge insert")?
        .when_matched(WhenMatched::UpdateAll)
        .when_not_matched(WhenNotMatched::InsertAll)
        .try_build()
        .context("building merge insert job")?
        .execute_reader(reader)
        .await
        .context("upserting notes")?;
    Ok(())
}

pub async fn overwrite(vault: &Vault, rows: Vec<NoteRow>) -> Result<()> {
    std::fs::create_dir_all(&vault.data_dir).context("creating vault data dir")?;
    let batch = notes_batch(&rows)?;
    let reader = RecordBatchIterator::new(vec![batch].into_iter().map(Ok), notes_schema());
    let params = WriteParams { mode: WriteMode::Overwrite, ..Default::default() };
    Dataset::write(reader, vault.notes_uri().as_str(), Some(params))
        .await
        .context("overwriting notes dataset")?;
    Ok(())
}

pub async fn delete_ids(vault: &Vault, ids: &[String]) -> Result<()> {
    if ids.is_empty() || !exists(vault) {
        return Ok(());
    }
    let mut ds = open_dataset(&vault.notes_uri()).await?;
    ds.delete(&format!("id IN ({})", sql_in_list(ids)))
        .await
        .context("deleting removed notes")?;
    Ok(())
}

pub async fn get(vault: &Vault, id: &str, include_body: bool) -> Result<serde_json::Value> {
    let norm = parser::note_id(id);
    let ds = open_dataset(&vault.notes_uri()).await?;
    let mut cols = vec!["id", "path", "type", "title", "frontmatter_json"];
    if include_body {
        cols.push("body");
    }
    let mut scan = ds.scan();
    scan.project(&cols).context("projecting note columns")?;
    scan.filter(&format!("id = {}", sql_str(&norm))).context("filtering by id")?;
    scan.limit(Some(1), None).context("limiting")?;
    let batch = scan.try_into_batch().await.context("fetching note")?;
    if batch.num_rows() == 0 {
        return Err(SmolbrenError::NoteNotFound(norm));
    }
    let mut row = output::batch_to_rows(&batch)?.remove(0);
    if let Some(obj) = row.as_object_mut() {
        let fm = obj
            .remove("frontmatter_json")
            .and_then(|v| v.as_str().and_then(|s| serde_json::from_str(s).ok()))
            .unwrap_or(serde_json::Value::Null);
        obj.insert("frontmatter".to_string(), fm);
    }
    Ok(row)
}

/// (type -> count, total notes). Untyped notes count toward the total only.
pub async fn type_counts(vault: &Vault) -> Result<(BTreeMap<String, u64>, usize)> {
    let ds = open_dataset(&vault.notes_uri()).await?;
    let mut scan = ds.scan();
    scan.project(&["type"]).context("projecting type column")?;
    let batch = scan.try_into_batch().await.context("scanning types")?;
    let types = batch.column(0).as_string::<i32>();
    let mut map: BTreeMap<String, u64> = BTreeMap::new();
    for v in types.iter().flatten() {
        *map.entry(v.to_string()).or_default() += 1;
    }
    Ok((map, batch.num_rows()))
}
