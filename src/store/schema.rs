use std::sync::Arc;

use anyhow::Result;
use arrow_array::{
    ArrayRef, BooleanArray, FixedSizeListArray, Float32Array, Int32Array, Int64Array, RecordBatch,
    StringArray,
};
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

/// One embedded chunk of a note's body. `content_hash` is the owning
/// note's hash at embed time, so `smolbren embed` can diff incrementally
/// against notes.lance without touching bodies.
#[derive(Debug, Clone)]
pub struct EmbeddingRow {
    pub note_id: String,
    pub seq: i32,
    pub content_hash: String,
    pub chunk_text: String,
    pub vector: Vec<f32>,
    pub embedded_at_ms: i64,
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

fn vector_item_field() -> Arc<Field> {
    Arc::new(Field::new("item", DataType::Float32, true))
}

pub fn embeddings_schema(dim: i32) -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("note_id", DataType::Utf8, false),
        Field::new("seq", DataType::Int32, false),
        Field::new("content_hash", DataType::Utf8, false),
        Field::new("chunk_text", DataType::Utf8, false),
        Field::new("vector", DataType::FixedSizeList(vector_item_field(), dim), false),
        Field::new("embedded_at_ms", DataType::Int64, false),
    ]))
}

pub fn embeddings_batch(rows: &[EmbeddingRow], dim: i32) -> Result<RecordBatch> {
    let mut flat: Vec<f32> = Vec::with_capacity(rows.len() * dim as usize);
    for r in rows {
        anyhow::ensure!(
            r.vector.len() == dim as usize,
            "embedding dim mismatch for note {} chunk {}: got {}, expected {dim}",
            r.note_id,
            r.seq,
            r.vector.len()
        );
        flat.extend_from_slice(&r.vector);
    }
    let vectors = FixedSizeListArray::try_new(
        vector_item_field(),
        dim,
        Arc::new(Float32Array::from(flat)),
        None,
    )?;
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.note_id.as_str()))),
        Arc::new(Int32Array::from_iter_values(rows.iter().map(|r| r.seq))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.content_hash.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.chunk_text.as_str()))),
        Arc::new(vectors),
        Arc::new(Int64Array::from_iter_values(rows.iter().map(|r| r.embedded_at_ms))),
    ];
    Ok(RecordBatch::try_new(embeddings_schema(dim), cols)?)
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
