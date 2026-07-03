use anyhow::Context;
use lance_index::scalar::FullTextSearchQuery;

use crate::error::Result;
use crate::output;
use crate::store::{self, sql_str};
use crate::vault::Vault;

/// BM25 full-text search over note titles and bodies. Returns
/// [{"id","path","type","title","score"}] ranked by descending score.
pub async fn run(
    vault: &Vault,
    query: &str,
    note_type: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let ds = store::open_dataset(&vault.notes_uri()).await?;
    let fts = FullTextSearchQuery::new(query.to_string()).limit(Some(limit as i64));
    let mut scan = ds.scan();
    scan.full_text_search(fts).context("building full text search")?;
    if let Some(t) = note_type {
        scan.filter(&format!("type = {}", sql_str(t))).context("filtering by type")?;
    }
    scan.project(&["id", "path", "type", "title", "_score"])
        .context("projecting search columns")?;
    scan.limit(Some(limit as i64), None).context("limiting search")?;
    let batch = scan.try_into_batch().await.context("running search")?;

    let mut rows = output::batch_to_rows(&batch)?;
    for row in &mut rows {
        if let Some(obj) = row.as_object_mut() {
            let score = obj.remove("_score").unwrap_or(serde_json::Value::Null);
            obj.insert("score".to_string(), score);
        }
    }
    Ok(rows)
}
