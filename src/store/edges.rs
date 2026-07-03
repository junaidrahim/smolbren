use std::collections::BTreeMap;

use anyhow::Context;
use arrow_array::cast::AsArray;
use arrow_array::{Array, RecordBatchIterator};
use lance::dataset::{WriteMode, WriteParams};
use lance::Dataset;

use super::schema::{edges_batch, edges_schema, EdgeRow};
use super::{open_dataset, sql_in_list, sql_str};
use crate::error::Result;
use crate::output;
use crate::parser;
use crate::vault::Vault;

fn exists(vault: &Vault) -> bool {
    vault.data_dir.join("edges.lance").exists()
}

/// Replace all edges owned by `from_ids` with `rows` (delete + append).
/// Simpler and safer than a composite-key merge for list-valued data.
pub async fn replace_for(vault: &Vault, from_ids: &[String], rows: Vec<EdgeRow>) -> Result<()> {
    if !exists(vault) {
        if rows.is_empty() {
            return Ok(());
        }
        return overwrite(vault, rows).await;
    }
    if !from_ids.is_empty() {
        let mut ds = open_dataset(&vault.edges_uri()).await?;
        ds.delete(&format!("from_id IN ({})", sql_in_list(from_ids)))
            .await
            .context("deleting stale edges")?;
    }
    if !rows.is_empty() {
        let batch = edges_batch(&rows)?;
        let reader = RecordBatchIterator::new(vec![batch].into_iter().map(Ok), edges_schema());
        let params = WriteParams { mode: WriteMode::Append, ..Default::default() };
        Dataset::write(reader, vault.edges_uri().as_str(), Some(params))
            .await
            .context("appending edges")?;
    }
    Ok(())
}

pub async fn overwrite(vault: &Vault, rows: Vec<EdgeRow>) -> Result<()> {
    std::fs::create_dir_all(&vault.data_dir).context("creating vault data dir")?;
    let batch = edges_batch(&rows)?;
    let reader = RecordBatchIterator::new(vec![batch].into_iter().map(Ok), edges_schema());
    let params = WriteParams { mode: WriteMode::Overwrite, ..Default::default() };
    Dataset::write(reader, vault.edges_uri().as_str(), Some(params))
        .await
        .context("overwriting edges dataset")?;
    Ok(())
}

/// Outgoing edges of a note, ordered by (edge_type, position).
pub async fn links(vault: &Vault, id: &str, edge_type: Option<&str>) -> Result<Vec<serde_json::Value>> {
    if !exists(vault) {
        return Ok(Vec::new());
    }
    let norm = parser::note_id(id);
    let ds = open_dataset(&vault.edges_uri()).await?;
    let mut filter = format!("from_id = {}", sql_str(&norm));
    if let Some(et) = edge_type {
        filter.push_str(&format!(" AND edge_type = {}", sql_str(et)));
    }
    let mut scan = ds.scan();
    scan.project(&["edge_type", "to_id", "to_alias", "resolved", "position"])
        .context("projecting edge columns")?;
    scan.filter(&filter).context("filtering edges")?;
    let batch = scan.try_into_batch().await.context("fetching links")?;
    let mut rows = output::batch_to_rows(&batch)?;
    rows.sort_by(|a, b| {
        let key = |v: &serde_json::Value| {
            (
                v["edge_type"].as_str().unwrap_or("").to_string(),
                v["position"].as_i64().unwrap_or(0),
            )
        };
        key(a).cmp(&key(b))
    });
    Ok(rows)
}

/// Incoming edges of a note, each joined with the source note's type/title.
pub async fn backlinks(vault: &Vault, id: &str, edge_type: Option<&str>) -> Result<Vec<serde_json::Value>> {
    if !exists(vault) {
        return Ok(Vec::new());
    }
    let norm = parser::note_id(id);
    let ds = open_dataset(&vault.edges_uri()).await?;
    let mut filter = format!("to_id = {}", sql_str(&norm));
    if let Some(et) = edge_type {
        filter.push_str(&format!(" AND edge_type = {}", sql_str(et)));
    }
    let mut scan = ds.scan();
    scan.project(&["edge_type", "from_id"]).context("projecting edge columns")?;
    scan.filter(&filter).context("filtering edges")?;
    let batch = scan.try_into_batch().await.context("fetching backlinks")?;
    let mut rows = output::batch_to_rows(&batch)?;

    // Join source note info (type, title) onto each backlink.
    let from_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r["from_id"].as_str().map(str::to_string))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if !from_ids.is_empty() {
        let notes = open_dataset(&vault.notes_uri()).await?;
        let mut nscan = notes.scan();
        nscan
            .project(&["id", "type", "title"])
            .context("projecting note columns")?;
        nscan
            .filter(&format!("id IN ({})", sql_in_list(&from_ids)))
            .context("filtering notes")?;
        let nbatch = nscan.try_into_batch().await.context("fetching source notes")?;
        let ids = nbatch.column(0).as_string::<i32>();
        let types = nbatch.column(1).as_string::<i32>();
        let titles = nbatch.column(2).as_string::<i32>();
        let mut info: std::collections::HashMap<&str, (Option<&str>, &str)> = Default::default();
        for i in 0..nbatch.num_rows() {
            info.insert(
                ids.value(i),
                (types.is_valid(i).then(|| types.value(i)), titles.value(i)),
            );
        }
        for row in &mut rows {
            let Some(from) = row["from_id"].as_str().map(str::to_string) else { continue };
            if let (Some(obj), Some((t, title))) = (row.as_object_mut(), info.get(from.as_str())) {
                obj.insert("from_type".to_string(), (*t).map_or(serde_json::Value::Null, |s| s.into()));
                obj.insert("from_title".to_string(), (*title).into());
            }
        }
    }
    rows.sort_by(|a, b| {
        let key = |v: &serde_json::Value| {
            (
                v["edge_type"].as_str().unwrap_or("").to_string(),
                v["from_id"].as_str().unwrap_or("").to_string(),
            )
        };
        key(a).cmp(&key(b))
    });
    Ok(rows)
}

/// (edge_type -> count, total edges, unresolved edges).
pub async fn edge_counts(vault: &Vault) -> Result<(BTreeMap<String, u64>, usize, usize)> {
    if !exists(vault) {
        return Ok((BTreeMap::new(), 0, 0));
    }
    let ds = open_dataset(&vault.edges_uri()).await?;
    let mut scan = ds.scan();
    scan.project(&["edge_type", "resolved"]).context("projecting edge columns")?;
    let batch = scan.try_into_batch().await.context("scanning edges")?;
    let types = batch.column(0).as_string::<i32>();
    let resolved = batch.column(1).as_boolean();
    let mut map: BTreeMap<String, u64> = BTreeMap::new();
    let mut unresolved = 0usize;
    for i in 0..batch.num_rows() {
        *map.entry(types.value(i).to_string()).or_default() += 1;
        if !resolved.value(i) {
            unresolved += 1;
        }
    }
    Ok((map, batch.num_rows(), unresolved))
}
