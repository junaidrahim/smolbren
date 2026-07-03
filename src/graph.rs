use std::collections::HashMap;

use anyhow::Context;
use arrow_array::cast::AsArray;
use arrow_array::{BooleanArray, RecordBatch};
use lance_graph::{CypherQuery, GraphConfig, NodeMapping, RelationshipMapping};

use crate::error::{Result, SmolbrenError};
use crate::ontology::Ontology;
use crate::output;
use crate::store::{self, schema};
use crate::vault::Vault;

const NOTE_PROPERTIES: [&str; 3] = ["path", "type", "title"];
const EDGE_PROPERTIES: [&str; 3] = ["to_raw", "resolved", "position"];

/// Run a Cypher query over the vault's note graph.
///
/// lance-graph 0.5.4 never consumes NodeMapping filter_conditions, so
/// registering one notes batch under N labels with different filters does not
/// work. Instead we pre-partition the notes batch per referenced label (and
/// the edges batch per referenced relationship type) in Rust, and register
/// each partition under its label key. The `Note` label is a catch-all
/// matching every note.
pub async fn run_query(
    vault: &Vault,
    cypher: &str,
    params: &[(String, String)],
) -> Result<serde_json::Value> {
    let ontology = Ontology::load(&vault.ontology_path()).map_err(SmolbrenError::Other)?;
    let query = CypherQuery::new(cypher)
        .map_err(|e| SmolbrenError::Other(anyhow::anyhow!("cypher parse error: {e}")))?;
    let node_labels = query.referenced_node_labels();
    let rel_types = query.referenced_relationship_types();

    // Config covers ontology labels plus anything the query references, so an
    // unknown label yields an empty result instead of an unknown-table error.
    let mut builder = GraphConfig::builder();
    let mut seen_labels: Vec<String> = Vec::new();
    for label in ontology
        .types
        .keys()
        .map(String::as_str)
        .chain(node_labels.iter().map(String::as_str))
        .chain(["Note"])
    {
        if seen_labels.iter().any(|l| l.eq_ignore_ascii_case(label)) {
            continue;
        }
        seen_labels.push(label.to_string());
        builder = builder.with_node_mapping(
            NodeMapping::new(label, "id")
                .with_properties(NOTE_PROPERTIES.iter().map(|s| s.to_string()).collect()),
        );
    }
    let mut seen_rels: Vec<String> = Vec::new();
    for rel in ontology
        .edge_types
        .keys()
        .map(String::as_str)
        .chain(rel_types.iter().map(String::as_str))
    {
        if seen_rels.iter().any(|r| r.eq_ignore_ascii_case(rel)) {
            continue;
        }
        seen_rels.push(rel.to_string());
        builder = builder.with_relationship_mapping(
            RelationshipMapping::new(rel, "from_id", "to_id")
                .with_properties(EDGE_PROPERTIES.iter().map(|s| s.to_string()).collect()),
        );
    }
    let config = builder
        .build()
        .map_err(|e| SmolbrenError::Other(anyhow::anyhow!("graph config: {e}")))?;

    // Slim projections: no body/frontmatter_json in memory.
    let notes_ds = store::open_dataset(&vault.notes_uri()).await?;
    let mut nscan = notes_ds.scan();
    nscan
        .project(&["id", "path", "type", "title"])
        .context("projecting notes for graph")?;
    let notes = nscan.try_into_batch().await.context("loading notes for graph")?;

    let edges = if vault.data_dir.join("edges.lance").exists() {
        let edges_ds = store::open_dataset(&vault.edges_uri()).await?;
        let mut escan = edges_ds.scan();
        escan
            .project(&["from_id", "edge_type", "to_id", "to_raw", "resolved", "position"])
            .context("projecting edges for graph")?;
        escan.try_into_batch().await.context("loading edges for graph")?
    } else {
        RecordBatch::new_empty(schema::edges_schema())
    };

    let mut datasets: HashMap<String, RecordBatch> = HashMap::new();
    for label in &node_labels {
        let batch = if label.eq_ignore_ascii_case("note") {
            notes.clone()
        } else {
            filter_eq(&notes, "type", label)?
        };
        datasets.insert(label.clone(), batch);
    }
    for rel in &rel_types {
        datasets.insert(rel.clone(), filter_eq(&edges, "edge_type", rel)?);
    }

    let mut param_map: HashMap<String, serde_json::Value> = HashMap::new();
    for (k, v) in params {
        // Try JSON first so numbers/bools work; fall back to a plain string.
        let value = serde_json::from_str(v).unwrap_or_else(|_| serde_json::Value::String(v.clone()));
        param_map.insert(k.clone(), value);
    }

    let query = query.with_config(config).with_parameters(param_map);
    let result = query
        .execute(datasets, None)
        .await
        .map_err(|e| SmolbrenError::Other(anyhow::anyhow!("cypher execution: {e}")))?;

    let columns: Vec<String> = result
        .schema()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .collect();
    let rows = output::batch_to_rows(&result)?;
    Ok(serde_json::json!({"columns": columns, "rows": rows}))
}

/// Rows of `batch` where `column` equals `value` (case-insensitive, nulls excluded).
fn filter_eq(batch: &RecordBatch, column: &str, value: &str) -> Result<RecordBatch> {
    let col = batch
        .column_by_name(column)
        .with_context(|| format!("missing column {column}"))?
        .as_string::<i32>();
    let mask: BooleanArray = col
        .iter()
        .map(|v| Some(v.is_some_and(|s| s.eq_ignore_ascii_case(value))))
        .collect();
    Ok(arrow::compute::filter_record_batch(batch, &mask).context("filtering batch")?)
}
