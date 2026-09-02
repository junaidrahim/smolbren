use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::Context;
use arrow_array::cast::AsArray;
use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use lance_graph::ast::{CypherQuery as CypherAst, GraphPattern, ReadingClause};
use lance_graph::{CypherQuery, GraphConfig, NodeMapping, RelationshipMapping};

use crate::error::{Result, SmolbrenError};
use crate::ontology::Ontology;
use crate::output;
use crate::store::{self, schema};
use crate::vault::Vault;

const NOTE_PROPERTIES: [&str; 3] = ["path", "type", "title"];
const EDGE_PROPERTIES: [&str; 3] = ["to_raw", "resolved", "position"];
const ALL_RELATIONSHIPS_TYPE: &str = "__SmolbrenAllRelationships";

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
    let parsed_query = CypherQuery::new(cypher)
        .map_err(|e| SmolbrenError::Other(anyhow::anyhow!("cypher parse error: {e}")))?;
    // lance-graph 0.5.4 defaults anonymous nodes to `Node`, but does not give
    // anonymous relationships a corresponding default type. Add an internal
    // catch-all type so patterns such as (a)-[r]->(b) traverse every edge.
    let anonymous_relationships = anonymous_relationship_count(parsed_query.ast());
    let query = if anonymous_relationships == 0 {
        parsed_query
    } else {
        let rewritten = label_anonymous_relationships(cypher, anonymous_relationships)
            .map_err(SmolbrenError::Other)?;
        CypherQuery::new(&rewritten).map_err(|e| {
            SmolbrenError::Other(anyhow::anyhow!(
                "cypher parse error after expanding anonymous relationships: {e}"
            ))
        })?
    };
    let node_labels = query.referenced_node_labels();
    let rel_types = query.referenced_relationship_types();
    let uses_all_relationships = anonymous_relationships > 0
        || rel_types
            .iter()
            .any(|rel| rel.eq_ignore_ascii_case(ALL_RELATIONSHIPS_TYPE));

    // Scalar frontmatter is promoted into typed in-memory Arrow columns for
    // graph execution. The persisted JSON remains the source of truth, so new
    // keys become queryable on the next index without a storage migration.
    let notes_ds = store::open_dataset(&vault.notes_uri()).await?;
    let mut nscan = notes_ds.scan();
    nscan
        .project(&["id", "path", "type", "title", "frontmatter_json"])
        .context("projecting notes and frontmatter for graph")?;
    let raw_notes = nscan.try_into_batch().await.context("loading notes for graph")?;
    let (notes, frontmatter_properties) = promote_frontmatter(&raw_notes)?;
    let node_properties: Vec<String> = NOTE_PROPERTIES
        .iter()
        .map(|s| s.to_string())
        .chain(frontmatter_properties.iter().cloned())
        .collect();

    // Config covers ontology labels plus anything the query references, so an
    // unknown label yields an empty result instead of an unknown-table error.
    let mut builder = GraphConfig::builder();
    let mut seen_labels: Vec<String> = Vec::new();
    for label in ontology
        .types
        .keys()
        .map(String::as_str)
        .chain(node_labels.iter().map(String::as_str))
        // lance-graph plans unlabeled node patterns as the reserved `Node`
        // label. Keep smolbren's public `Note` catch-all and register `Node`
        // as an internal alias for the same dataset.
        .chain(["Note", "Node"])
    {
        if seen_labels.iter().any(|l| l.eq_ignore_ascii_case(label)) {
            continue;
        }
        seen_labels.push(label.to_string());
        builder = builder.with_node_mapping(
            NodeMapping::new(label, "id")
                .with_properties(node_properties.clone()),
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
    if anonymous_relationships > 0
        && !seen_rels
            .iter()
            .any(|rel| rel.eq_ignore_ascii_case(ALL_RELATIONSHIPS_TYPE))
    {
        builder = builder.with_relationship_mapping(
            RelationshipMapping::new(ALL_RELATIONSHIPS_TYPE, "from_id", "to_id")
                .with_properties(EDGE_PROPERTIES.iter().map(|s| s.to_string()).collect()),
        );
    }
    let config = builder
        .build()
        .map_err(|e| SmolbrenError::Other(anyhow::anyhow!("graph config: {e}")))?;

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
        if label.eq_ignore_ascii_case("node") {
            continue;
        }
        let batch = if label.eq_ignore_ascii_case("note") {
            notes.clone()
        } else {
            filter_eq(&notes, "type", label)?
        };
        datasets.insert(label.clone(), batch);
    }
    for rel in &rel_types {
        if !rel.eq_ignore_ascii_case(ALL_RELATIONSHIPS_TYPE) {
            datasets.insert(rel.clone(), filter_eq(&edges, "edge_type", rel)?);
        }
    }
    if uses_all_relationships {
        datasets.insert(ALL_RELATIONSHIPS_TYPE.to_string(), edges.clone());
    }
    // `referenced_node_labels` only returns explicit labels. Anonymous node
    // patterns therefore need the catch-all dataset registered separately,
    // including when other node labels or relationship types are present.
    datasets.insert("Node".to_string(), notes.clone());

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

fn anonymous_relationship_count(ast: &CypherAst) -> usize {
    ast.reading_clauses
        .iter()
        .chain(&ast.post_with_reading_clauses)
        .filter_map(|clause| match clause {
            ReadingClause::Match(match_clause) => Some(&match_clause.patterns),
            ReadingClause::Unwind(_) => None,
        })
        .flatten()
        .map(|pattern| match pattern {
            GraphPattern::Node(_) => 0,
            GraphPattern::Path(path) => path
                .segments
                .iter()
                .filter(|segment| segment.relationship.types.is_empty())
                .count(),
        })
        .sum()
}

/// Give anonymous relationship patterns the internal catch-all type expected
/// by smolbren's graph configuration. The input has already been parsed, so
/// relationship syntax is known to be `-[...]->`, `<-[...]-`, or `-[...]-`.
fn label_anonymous_relationships(cypher: &str, expected: usize) -> anyhow::Result<String> {
    let bytes = cypher.as_bytes();
    let mut insertions = Vec::with_capacity(expected);
    let mut quote = None;
    let mut index = 0;

    while index < bytes.len() && insertions.len() < expected {
        match quote {
            Some(delimiter) if bytes[index] == delimiter => quote = None,
            Some(_) => {}
            None if matches!(bytes[index], b'\'' | b'"') => quote = Some(bytes[index]),
            None if bytes[index] == b'[' && index > 0 && bytes[index - 1] == b'-' => {
                let mut close = index + 1;
                let mut inner_quote = None;
                while close < bytes.len() {
                    match inner_quote {
                        Some(delimiter) if bytes[close] == delimiter => inner_quote = None,
                        Some(_) => {}
                        None if matches!(bytes[close], b'\'' | b'"') => {
                            inner_quote = Some(bytes[close]);
                        }
                        None if bytes[close] == b']' => break,
                        None => {}
                    }
                    close += 1;
                }

                if close < bytes.len()
                    && bytes.get(close + 1) == Some(&b'-')
                {
                    let mut content = index + 1;
                    while content < close {
                        let character = cypher[content..close].chars().next().unwrap();
                        if !character.is_whitespace() {
                            break;
                        }
                        content += character.len_utf8();
                    }
                    while content < close {
                        let character = cypher[content..close].chars().next().unwrap();
                        if !(character.is_alphanumeric() || character == '_') {
                            break;
                        }
                        content += character.len_utf8();
                    }
                    if bytes.get(content) != Some(&b':') {
                        insertions.push(content);
                    }
                    index = close;
                }
            }
            None => {}
        }
        index += 1;
    }

    if insertions.len() != expected {
        anyhow::bail!(
            "could not identify all anonymous relationships in parsed Cypher query (expected {expected}, found {})",
            insertions.len()
        );
    }

    let mut rewritten = String::with_capacity(
        cypher.len() + insertions.len() * (ALL_RELATIONSHIPS_TYPE.len() + 1),
    );
    let mut copied = 0;
    for insertion in insertions {
        rewritten.push_str(&cypher[copied..insertion]);
        rewritten.push(':');
        rewritten.push_str(ALL_RELATIONSHIPS_TYPE);
        copied = insertion;
    }
    rewritten.push_str(&cypher[copied..]);
    Ok(rewritten)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScalarKind {
    Bool,
    Integer,
    Float,
    String,
}

impl ScalarKind {
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Integer, Self::Float) | (Self::Float, Self::Integer) => Self::Float,
            (a, b) if a == b => a,
            _ => Self::String,
        }
    }
}

/// Append valid scalar frontmatter keys as nullable Arrow columns. ISO dates
/// stay UTF-8 strings, which preserves chronological ordering for the vault's
/// canonical `YYYY-MM-DD` format and works naturally with string parameters.
fn promote_frontmatter(batch: &RecordBatch) -> Result<(RecordBatch, Vec<String>)> {
    let json = batch
        .column_by_name("frontmatter_json")
        .context("missing frontmatter_json column")?
        .as_string::<i32>();
    let mut objects = Vec::with_capacity(batch.num_rows());
    let mut kinds: BTreeMap<String, ScalarKind> = BTreeMap::new();
    for raw in json.iter() {
        let value: serde_json::Value = raw
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(object) = value.as_object() {
            for (key, value) in object {
                let canonical = key.to_ascii_lowercase();
                if !valid_property_name(&canonical)
                    || NOTE_PROPERTIES.contains(&canonical.as_str())
                    || canonical == "id"
                {
                    continue;
                }
                let Some(kind) = scalar_kind(value) else { continue };
                kinds
                    .entry(canonical)
                    .and_modify(|current| *current = current.merge(kind))
                    .or_insert(kind);
            }
        }
        objects.push(value);
    }

    let properties: Vec<String> = kinds.keys().cloned().collect();
    let mut fields: Vec<Arc<Field>> = Vec::new();
    let mut columns: Vec<ArrayRef> = Vec::new();
    for name in ["id", "path", "type", "title"] {
        let index = batch.schema().index_of(name).with_context(|| format!("missing {name} column"))?;
        fields.push(Arc::new(batch.schema().field(index).clone()));
        columns.push(batch.column(index).clone());
    }
    for (name, kind) in &kinds {
        let values = objects.iter().map(|v| scalar_property(v, name));
        match kind {
            ScalarKind::Bool => {
                fields.push(Arc::new(Field::new(name, DataType::Boolean, true)));
                columns.push(Arc::new(BooleanArray::from_iter(
                    values.map(|v| v.and_then(serde_json::Value::as_bool)),
                )));
            }
            ScalarKind::Integer => {
                fields.push(Arc::new(Field::new(name, DataType::Int64, true)));
                columns.push(Arc::new(Int64Array::from_iter(
                    values.map(|v| v.and_then(serde_json::Value::as_i64)),
                )));
            }
            ScalarKind::Float => {
                fields.push(Arc::new(Field::new(name, DataType::Float64, true)));
                columns.push(Arc::new(Float64Array::from_iter(
                    values.map(|v| v.and_then(serde_json::Value::as_f64)),
                )));
            }
            ScalarKind::String => {
                fields.push(Arc::new(Field::new(name, DataType::Utf8, true)));
                columns.push(Arc::new(StringArray::from_iter(values.map(scalar_string))));
            }
        }
    }
    let schema = Arc::new(Schema::new(fields));
    Ok((RecordBatch::try_new(schema, columns).context("building graph note properties")?, properties))
}

fn scalar_property<'a>(
    value: &'a serde_json::Value,
    canonical: &str,
) -> Option<&'a serde_json::Value> {
    let object = value.as_object()?;
    object.get(canonical).or_else(|| {
        object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(canonical))
            .map(|(_, value)| value)
    })
}

fn scalar_kind(value: &serde_json::Value) -> Option<ScalarKind> {
    match value {
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => None,
        serde_json::Value::Bool(_) => Some(ScalarKind::Bool),
        serde_json::Value::Number(n) if n.is_i64() || n.is_u64() => Some(ScalarKind::Integer),
        serde_json::Value::Number(_) => Some(ScalarKind::Float),
        serde_json::Value::String(_) => Some(ScalarKind::String),
    }
}

fn scalar_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => None,
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn valid_property_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
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

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::StringArray;

    #[test]
    fn anonymous_relationships_receive_the_internal_catch_all_type() {
        let cypher = "MATCH (a)-[r]->(b), (b)<-[]-(c), (c)-[:typed]->(d) RETURN count(d)";
        let parsed = CypherQuery::new(cypher).unwrap();
        let count = anonymous_relationship_count(parsed.ast());
        assert_eq!(count, 2);
        assert_eq!(
            label_anonymous_relationships(cypher, count).unwrap(),
            format!(
                "MATCH (a)-[r:{0}]->(b), (b)<-[:{0}]-(c), (c)-[:typed]->(d) RETURN count(d)",
                ALL_RELATIONSHIPS_TYPE
            )
        );
    }

    #[test]
    fn anonymous_relationship_rewrite_handles_properties_and_quoted_brackets() {
        let cypher = "MATCH (a)-[rélation {note: 'keep ] intact'}]->(b) RETURN b";
        assert_eq!(
            label_anonymous_relationships(cypher, 1).unwrap(),
            format!(
                "MATCH (a)-[rélation:{ALL_RELATIONSHIPS_TYPE} {{note: 'keep ] intact'}}]->(b) RETURN b"
            )
        );
    }

    #[test]
    fn frontmatter_scalars_become_typed_columns() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("path", DataType::Utf8, false),
            Field::new("type", DataType::Utf8, true),
            Field::new("title", DataType::Utf8, false),
            Field::new("frontmatter_json", DataType::Utf8, false),
        ]));
        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(vec!["blogs/a"])),
            Arc::new(StringArray::from(vec!["blogs/a.md"])),
            Arc::new(StringArray::from(vec![Some("blog")])),
            Arc::new(StringArray::from(vec!["A"])),
            Arc::new(StringArray::from(vec![
                r#"{"status":"draft","Status":"published","created":"2026-08-07","score":3,"public":true,"tags":["x"]}"#,
            ])),
        ];
        let batch = RecordBatch::try_new(schema, columns).unwrap();
        let (promoted, names) = promote_frontmatter(&batch).unwrap();
        assert_eq!(names, vec!["created", "public", "score", "status"]);
        assert_eq!(promoted.schema().field_with_name("created").unwrap().data_type(), &DataType::Utf8);
        assert_eq!(promoted.schema().field_with_name("score").unwrap().data_type(), &DataType::Int64);
        assert_eq!(promoted.schema().field_with_name("public").unwrap().data_type(), &DataType::Boolean);
        assert!(promoted.schema().field_with_name("tags").is_err());
        assert_eq!(
            promoted
                .column_by_name("status")
                .unwrap()
                .as_string::<i32>()
                .value(0),
            "draft"
        );
        assert_eq!(
            promoted
                .schema()
                .fields()
                .iter()
                .filter(|field| field.name().eq_ignore_ascii_case("status"))
                .count(),
            1
        );
    }

    #[test]
    fn scalar_kinds_widen_numbers_and_fall_back_to_strings() {
        assert_eq!(
            ScalarKind::Integer.merge(ScalarKind::Float),
            ScalarKind::Float
        );
        assert_eq!(
            ScalarKind::Bool.merge(ScalarKind::String),
            ScalarKind::String
        );
        assert_eq!(
            ScalarKind::Integer.merge(ScalarKind::Integer),
            ScalarKind::Integer
        );
    }

    #[test]
    fn property_names_follow_cypher_identifier_rules() {
        assert!(valid_property_name("updated_at"));
        assert!(valid_property_name("_private"));
        assert!(!valid_property_name("updated-at"));
        assert!(!valid_property_name("2fa"));
        assert!(!valid_property_name(""));
    }
}
