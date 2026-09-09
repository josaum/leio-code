use anyhow::{Context, Result};
use arrow_array::{Array, BinaryArray, Int64Array, RecordBatch, StringArray, UInt64Array};
use arrow_buffer::{Buffer, NullBuffer, OffsetBuffer, ScalarBuffer};
use arrow_ipc::reader::FileReader;
use arrow_ipc::writer::FileWriter;
use arrow_schema::{DataType, Field, Schema};
use memmap2::Mmap;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

pub fn event_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("seq", DataType::UInt64, false),
        Field::new("timestamp_ms", DataType::Int64, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("stream", DataType::Utf8, false),
        Field::new("payload", DataType::Binary, false),
        Field::new("payload_sha256", DataType::Utf8, false),
    ]))
}

/// Streaming IPC writer: one single-row batch per event. Payload ownership is
/// moved into the Arrow buffer (`Buffer::from_vec`) — no payload copies.
pub struct EventStreamWriter {
    writer: FileWriter<File>,
    schema: Arc<Schema>,
}

pub struct StreamEvent {
    pub seq: u64,
    pub timestamp_ms: i64,
    pub kind: String,
    pub stream: String,
    pub payload: Vec<u8>,
}

impl EventStreamWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let schema = event_schema();
        let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
        let writer = FileWriter::try_new(file, &schema)?;
        Ok(Self { writer, schema })
    }

    pub fn write_event(&mut self, event: StreamEvent) -> Result<()> {
        self.writer.write(&event_batch(&self.schema, event)?)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.writer.finish()?;
        Ok(())
    }
}

fn event_batch(schema: &Arc<Schema>, event: StreamEvent) -> Result<RecordBatch> {
    let payload_len = event.payload.len() as i32;
    let mut digest = Sha256::new();
    digest.update(&event.payload);
    let payload_sha256 = hex::encode(digest.finalize());
    // Move the payload allocation into an Arrow buffer (zero-copy transfer).
    let values = Buffer::from_vec(event.payload);
    let offsets = OffsetBuffer::new(ScalarBuffer::new(
        Buffer::from_vec(vec![0i32, payload_len]),
        0,
        2,
    ));
    let payload_array = BinaryArray::new(offsets, values, None::<NullBuffer>);
    RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(UInt64Array::from(vec![event.seq])),
            Arc::new(Int64Array::from(vec![event.timestamp_ms])),
            Arc::new(StringArray::from(vec![event.kind.as_str()])),
            Arc::new(StringArray::from(vec![event.stream.as_str()])),
            Arc::new(payload_array),
            Arc::new(StringArray::from(vec![payload_sha256.as_str()])),
        ],
    )
    .map_err(Into::into)
}

#[derive(Debug, Serialize)]
pub struct ArrowSummary {
    pub batches: usize,
    pub rows: usize,
    pub payload_bytes: usize,
    pub kinds: Vec<String>,
}

pub fn inspect_mmap(path: &Path) -> Result<ArrowSummary> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    // SAFETY: the mapping is read-only and lives for the full reader traversal.
    let mmap = unsafe { Mmap::map(&file)? };
    let cursor = Cursor::new(mmap.as_ref());
    let reader = FileReader::try_new(cursor, None)?;
    let mut batches = 0;
    let mut rows = 0;
    let mut payload_bytes = 0;
    let mut kinds = std::collections::BTreeSet::new();
    for batch in reader {
        let batch = batch?;
        batches += 1;
        rows += batch.num_rows();
        let kind = batch
            .column_by_name("kind")
            .context("missing kind column")?
            .as_any()
            .downcast_ref::<StringArray>()
            .context("kind column type")?;
        let payload = batch
            .column_by_name("payload")
            .context("missing payload column")?
            .as_any()
            .downcast_ref::<BinaryArray>()
            .context("payload column type")?;
        for index in 0..batch.num_rows() {
            kinds.insert(kind.value(index).to_owned());
            payload_bytes += payload.value(index).len();
        }
    }
    Ok(ArrowSummary {
        batches,
        rows,
        payload_bytes,
        kinds: kinds.into_iter().collect(),
    })
}
