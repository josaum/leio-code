//! Zero-copy Arrow IPC decode over a read-only memory map.
//!
//! Follows the Apache Arrow **IPC streaming format**
//! (<https://arrow.apache.org/docs/format/Columnar.html#ipc-streaming-format>):
//!
//! ```text
//! <continuation: int32 = 0xFFFFFFFF>
//! <metadata_size: int32 LE>          // 0 => EOS
//! <Message Flatbuffer, 8-byte pad>
//! <body, 8-byte pad>
//! ```
//!
//! The File format (`ARROW1` magic + footer) is seekable; this crate's node
//! export is a stream. [`arrow_ipc::reader::StreamDecoder`] walks that framing
//! and, when a body buffer is already aligned, keeps it as a view of the map
//! instead of copying into a new allocation.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use arrow_array::RecordBatch;
use arrow_buffer::Buffer;
use arrow_ipc::reader::StreamDecoder;
use bytes::Bytes;
use memmap2::Mmap;

/// Memory-map `path` as an immutable byte owner.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or mapped.
pub fn map_file(path: &Path) -> Result<Bytes> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    // Safety: the mapping is read-only (`PROT_READ` / `PAGE_READONLY`).
    // Export writes a sibling `*.tmp` and `rename`s over `path`, so this
    // inode stays stable for the life of the map. Readers never write.
    let mapped = unsafe { Mmap::map(&file) }
        .with_context(|| format!("failed to mmap {}", path.display()))?;
    Ok(Bytes::from_owner(mapped))
}

/// Decode an Arrow IPC **stream** from an already-mapped buffer.
///
/// Aligned record-batch bodies stay views of `bytes`. Misaligned bodies
/// are copied by `StreamDecoder` (the Arrow default).
///
/// # Errors
///
/// Returns an error if the bytes are not a complete IPC stream.
pub fn decode_ipc_stream(bytes: Bytes) -> Result<Vec<RecordBatch>> {
    let mut decoder = StreamDecoder::new();
    let mut buffer = Buffer::from(bytes);
    let mut batches = Vec::new();
    while !buffer.is_empty() {
        match decoder
            .decode(&mut buffer)
            .context("failed to decode Arrow IPC stream message")?
        {
            Some(batch) => batches.push(batch),
            None => break,
        }
    }
    decoder
        .finish()
        .context("Arrow IPC stream ended mid-message")?;
    Ok(batches)
}

/// Map `path` and decode it as an Arrow IPC stream.
///
/// # Errors
///
/// Returns an error if the file cannot be mapped or decoded.
pub fn read_ipc_stream_path(path: &Path) -> Result<Vec<RecordBatch>> {
    decode_ipc_stream(map_file(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Int32Array, RecordBatch};
    use arrow_ipc::writer::StreamWriter;
    use arrow_schema::{DataType, Field, Schema};

    fn sample_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int32, false)]));
        RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(vec![1, 2, 3])) as ArrayRef],
        )
        .expect("batch")
    }

    #[test]
    fn stream_decoder_round_trips_without_eos_error() {
        let batch = sample_batch();
        let mut encoded = Vec::new();
        {
            let mut writer =
                StreamWriter::try_new(Cursor::new(&mut encoded), &batch.schema()).expect("writer");
            writer.write(&batch).expect("write");
            writer.finish().expect("finish");
        }
        assert_eq!(encoded[..4], [0xff, 0xff, 0xff, 0xff], "IPC continuation");
        let decoded = decode_ipc_stream(Bytes::from(encoded)).expect("decode");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].num_rows(), 3);
    }

    #[test]
    fn empty_buffer_yields_no_batches() {
        let decoded = decode_ipc_stream(Bytes::new()).expect("empty stream");
        assert!(decoded.is_empty());
    }

    #[test]
    fn truncated_stream_is_an_error() {
        let batch = sample_batch();
        let mut encoded = Vec::new();
        {
            let mut writer =
                StreamWriter::try_new(Cursor::new(&mut encoded), &batch.schema()).expect("writer");
            writer.write(&batch).expect("write");
            writer.finish().expect("finish");
        }
        encoded.truncate(encoded.len() / 2);
        let err = decode_ipc_stream(Bytes::from(encoded)).expect_err("truncated");
        let message = err.to_string();
        assert!(
            message.contains("mid-message") || message.contains("decode"),
            "{message}"
        );
    }

    #[test]
    fn map_and_decode_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nodes.arrow");
        let batch = sample_batch();
        {
            let file = File::create(&path).expect("create");
            let mut writer = StreamWriter::try_new(file, &batch.schema()).expect("writer");
            writer.write(&batch).expect("write");
            writer.finish().expect("finish");
        }
        let decoded = read_ipc_stream_path(&path).expect("mmap decode");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].num_rows(), 3);
    }
}
