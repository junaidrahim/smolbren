use anyhow::Context;
use lance_index::scalar::FullTextSearchQuery;

use crate::error::Result;
use crate::output;
use crate::store::{self, sql_str};
use crate::vault::Vault;

/// BM25 full-text search over note titles and bodies. Returns
/// [{"id","path","type","title","score"}] ranked by descending score.
pub async fn bm25(
    vault: &Vault,
    query: &str,
    note_type: Option<&str>,
    path_prefix: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let ds = store::open_dataset(&vault.notes_uri()).await?;
    let fts = FullTextSearchQuery::new(query.to_string()).limit(Some(limit as i64));
    let mut scan = ds.scan();
    scan.full_text_search(fts).context("building full text search")?;
    if let Some(filter) = metadata_filter(note_type, path_prefix) {
        scan.filter(&filter).context("filtering search metadata")?;
    }
    scan.project(&["id", "path", "type", "title", "body", "_score"])
        .context("projecting search columns")?;
    scan.limit(Some(limit as i64), None).context("limiting search")?;
    let batch = scan.try_into_batch().await.context("running search")?;

    let mut rows = output::batch_to_rows(&batch)?;
    for row in &mut rows {
        if let Some(obj) = row.as_object_mut() {
            let score = obj.remove("_score").unwrap_or(serde_json::Value::Null);
            let body = obj.remove("body").and_then(|v| v.as_str().map(str::to_string));
            obj.insert("score".to_string(), score);
            obj.insert(
                "snippet".to_string(),
                body.map_or(serde_json::Value::Null, |body| snippet(query, &body).into()),
            );
        }
    }
    Ok(rows)
}

fn metadata_filter(note_type: Option<&str>, path_prefix: Option<&str>) -> Option<String> {
    let mut clauses = Vec::new();
    if let Some(note_type) = note_type {
        clauses.push(format!("type = {}", sql_str(note_type)));
    }
    if let Some(path) = path_prefix {
        let normalized = path.trim_start_matches("./").trim_start_matches('/');
        clauses.push(format!("path LIKE {}", sql_str(&format!("{normalized}%"))));
    }
    (!clauses.is_empty()).then(|| clauses.join(" AND "))
}

const SNIPPET_BYTES: usize = 280;

fn snippet(query: &str, body: &str) -> String {
    let clean = body.trim();
    if clean.len() <= SNIPPET_BYTES {
        return clean.to_string();
    }
    let needle = query
        .split_whitespace()
        .filter(|s| s.len() >= 3)
        .max_by_key(|s| s.len())
        .unwrap_or(query);
    let found = regex::RegexBuilder::new(&regex::escape(needle))
        .case_insensitive(true)
        .build()
        .ok()
        .and_then(|re| re.find(clean).map(|m| m.start()))
        .unwrap_or(0);
    let mut start = found.saturating_sub(SNIPPET_BYTES / 3);
    while start > 0 && !clean.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + SNIPPET_BYTES).min(clean.len());
    while end > start && !clean.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        &clean[start..end],
        if end < clean.len() { "…" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_centers_a_query_term_and_keeps_utf8_boundaries() {
        let body = format!("{}needle{}", "é".repeat(180), "z".repeat(400));
        let result = snippet("needle", &body);
        assert!(result.contains("needle"));
        assert!(result.starts_with('…'));
        assert!(result.ends_with('…'));
    }

    #[test]
    fn metadata_filters_combine_type_and_path() {
        assert_eq!(
            metadata_filter(Some("blog"), Some("./Notes/blogs/")),
            Some("type = 'blog' AND path LIKE 'Notes/blogs/%'".to_string())
        );
    }
}
