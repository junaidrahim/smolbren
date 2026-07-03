use anyhow::Result;
use arrow::json::writer::JsonArray;
use arrow::json::WriterBuilder;
use arrow_array::RecordBatch;

/// Convert a RecordBatch into JSON row objects via arrow's JSON writer.
/// Explicit nulls keep output shapes stable for agents (e.g. "type": null).
pub fn batch_to_rows(batch: &RecordBatch) -> Result<Vec<serde_json::Value>> {
    let mut buf = Vec::new();
    {
        let mut writer = WriterBuilder::new()
            .with_explicit_nulls(true)
            .build::<_, JsonArray>(&mut buf);
        writer.write(batch)?;
        writer.finish()?;
    }
    if buf.is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&buf)?)
}

/// All stdout is single-line JSON — this CLI is for agents, pipe to jq for eyes.
pub fn print_json<T: serde::Serialize>(value: &T) {
    println!("{}", serde_json::to_string(value).expect("value serializes"));
}
