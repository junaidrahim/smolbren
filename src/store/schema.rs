use std::sync::Arc;

use anyhow::Result;
use arrow_array::{ArrayRef, BooleanArray, Int32Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};

#[derive(Debug, Clone)]
pub struct NoteRow {
    pub id: String,
    pub path: String,
    pub note_type: Option<String>,
    pub title: String,
    pub frontmatter_json: String,
    pub body: String,
    pub frontmatter_hash: String,
    pub content_hash: String,
    pub mtime_ms: i64,
    pub size_bytes: i64,
    pub indexed_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct EdgeRow {
    pub from_id: String,
    pub edge_type: String,
    pub to_id: String,
    pub to_raw: String,
    pub to_alias: Option<String>,
    pub resolved: bool,
    pub position: i32,
}

/// The `type` column is nullable — notes without frontmatter still get
/// indexed. An embedding column is added later via schema evolution
/// (`add_columns`), not reserved here.
pub fn notes_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("type", DataType::Utf8, true),
        Field::new("title", DataType::Utf8, false),
        Field::new("frontmatter_json", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("frontmatter_hash", DataType::Utf8, false),
        Field::new("content_hash", DataType::Utf8, false),
        Field::new("mtime_ms", DataType::Int64, false),
        Field::new("size_bytes", DataType::Int64, false),
        Field::new("indexed_at_ms", DataType::Int64, false),
    ]))
}

pub fn edges_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("from_id", DataType::Utf8, false),
        Field::new("edge_type", DataType::Utf8, false),
        Field::new("to_id", DataType::Utf8, false),
        Field::new("to_raw", DataType::Utf8, false),
        Field::new("to_alias", DataType::Utf8, true),
        Field::new("resolved", DataType::Boolean, false),
        Field::new("position", DataType::Int32, false),
    ]))
}

pub fn notes_batch(rows: &[NoteRow]) -> Result<RecordBatch> {
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.id.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.path.as_str()))),
        Arc::new(StringArray::from_iter(rows.iter().map(|r| r.note_type.as_deref()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.title.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.frontmatter_json.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.body.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.frontmatter_hash.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.content_hash.as_str()))),
        Arc::new(Int64Array::from_iter_values(rows.iter().map(|r| r.mtime_ms))),
        Arc::new(Int64Array::from_iter_values(rows.iter().map(|r| r.size_bytes))),
        Arc::new(Int64Array::from_iter_values(rows.iter().map(|r| r.indexed_at_ms))),
    ];
    Ok(RecordBatch::try_new(notes_schema(), cols)?)
}

pub fn edges_batch(rows: &[EdgeRow]) -> Result<RecordBatch> {
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.from_id.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.edge_type.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.to_id.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.to_raw.as_str()))),
        Arc::new(StringArray::from_iter(rows.iter().map(|r| r.to_alias.as_deref()))),
        Arc::new(BooleanArray::from_iter(rows.iter().map(|r| Some(r.resolved)))),
        Arc::new(Int32Array::from_iter_values(rows.iter().map(|r| r.position))),
    ];
    Ok(RecordBatch::try_new(edges_schema(), cols)?)
}
