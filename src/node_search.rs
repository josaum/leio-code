//! Packed search sidecar for local Arrow nodes.
//!
//! `nodes.arrow` is a 1 GiB+ IPC stream (three 1024-d lists + strings).
//! Every CLI process that decodes it pays a multi-second cold start.
//!
//! `nodes.search` is a single mmap-friendly file:
//!
//! ```text
//! header 128
//! norms    f32[n]            // little-endian
//! pad → 64
//! matrix   f32[n * dim]      // LE; semantic, else code; zeros if missing
//! meta     u32[n+1] + blob  // path, kind, symbol, snippet, relations
//! ```
//!
//! Freshness is the source `nodes.arrow` (len + mtime) stamped in the header.
//! Export writes this file; search rebuilds it if missing or stale.
//! LE hosts mmap the f32 sections zero-copy; BE hosts reject the file.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use arrow_array::{Array, Float32Array, ListArray, StringArray};
use bytes::Bytes;
use memmap2::Mmap;

use crate::arrow_ipc::read_ipc_stream_path;
use crate::export::default_search_sidecar_path;

const MAGIC: &[u8; 8] = b"LEIOVEC1";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 128;
const ALIGN: usize = 64;

mod col {
    pub const PATH: usize = 4;
    pub const KIND: usize = 6;
    pub const SYMBOL: usize = 7;
    pub const RELATIONS: usize = 9;
    pub const SNIPPET: usize = 11;
    pub const CODE_VEC: usize = 14;
    pub const SEMANTIC_VEC: usize = 15;
}

/// Memory-mapped packed search index.
pub struct PackedIndex {
    bytes: Bytes,
    n: usize,
    dim: usize,
    norms_off: usize,
    matrix_off: usize,
    meta_off: usize,
}

impl PackedIndex {
    pub fn n(&self) -> usize {
        self.n
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn norm(&self, row: usize) -> f32 {
        self.norms().get(row).copied().unwrap_or(0.0)
    }

    pub fn vector(&self, row: usize) -> Option<&[f32]> {
        if self.dim == 0 || row >= self.n || self.norm(row) <= f32::EPSILON {
            return None;
        }
        let stride = self.dim.checked_mul(4)?;
        let start = self.matrix_off.checked_add(row.checked_mul(stride)?)?;
        let end = start.checked_add(stride)?;
        f32s(self.bytes.get(start..end)?).ok()
    }

    pub fn path(&self, row: usize) -> &str {
        self.field(row, 0)
    }

    pub fn kind(&self, row: usize) -> &str {
        self.field(row, 1)
    }

    pub fn symbol(&self, row: usize) -> &str {
        self.field(row, 2)
    }

    pub fn snippet(&self, row: usize) -> &str {
        self.field(row, 3)
    }

    pub fn relations(&self, row: usize) -> &str {
        self.field(row, 4)
    }

    fn norms(&self) -> &[f32] {
        let Some(end) = self
            .n
            .checked_mul(4)
            .and_then(|len| self.norms_off.checked_add(len))
        else {
            return &[];
        };
        self.bytes
            .get(self.norms_off..end)
            .and_then(|slice| f32s(slice).ok())
            .unwrap_or(&[])
    }

    fn field(&self, row: usize, which: usize) -> &str {
        let Some(record) = self.record(row) else {
            return "";
        };
        record.split('\0').nth(which).unwrap_or("")
    }

    fn record(&self, row: usize) -> Option<&str> {
        if row >= self.n {
            return None;
        }
        let table = self.meta_off;
        let off = read_u32(&self.bytes, table.checked_add(row.checked_mul(4)?)?)? as usize;
        let end = read_u32(
            &self.bytes,
            table.checked_add(row.checked_add(1)?.checked_mul(4)?)?,
        )? as usize;
        if end < off {
            return None;
        }
        let blob = self
            .meta_off
            .checked_add(self.n.checked_add(1)?.checked_mul(4)?)?;
        let start = blob.checked_add(off)?;
        let stop = blob.checked_add(end)?;
        let bytes = self.bytes.get(start..stop)?;
        std::str::from_utf8(bytes).ok()
    }
}

/// Open `sidecar` if it matches `source` identity; otherwise rebuild it.
pub fn load_or_build(source: &Path, sidecar: &Path) -> Result<PackedIndex> {
    let ident = source_ident(source)?;
    if sidecar.is_file()
        && let Ok(packed) = open_sidecar(sidecar)
        && packed_matches(&packed, &ident)
    {
        return Ok(packed);
    }
    build_sidecar(source, sidecar, &ident)?;
    open_sidecar(sidecar)
}

/// Rebuild the sidecar next to a freshly written `nodes.arrow`.
pub fn rebuild_next_to_arrow(output_dir: &Path) -> Result<PathBuf> {
    let source = crate::export::default_arrow_nodes_rows_path(output_dir);
    let sidecar = default_search_sidecar_path(output_dir);
    let ident = source_ident(&source)?;
    build_sidecar(&source, &sidecar, &ident)?;
    Ok(sidecar)
}

struct SourceIdent {
    len: u64,
    mtime_secs: u64,
    mtime_nanos: u32,
}

fn source_ident(path: &Path) -> Result<SourceIdent> {
    let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let elapsed = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    Ok(SourceIdent {
        len: meta.len(),
        mtime_secs: elapsed.as_secs(),
        mtime_nanos: elapsed.subsec_nanos(),
    })
}

fn packed_matches(packed: &PackedIndex, ident: &SourceIdent) -> bool {
    read_u64(&packed.bytes, 32) == Some(ident.len)
        && read_u64(&packed.bytes, 40) == Some(ident.mtime_secs)
        && read_u32(&packed.bytes, 48) == Some(ident.mtime_nanos)
}

fn open_sidecar(path: &Path) -> Result<PackedIndex> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    // Safety: read-only map. Writers publish via rename onto a new inode.
    let mapped = unsafe { Mmap::map(&file) }.with_context(|| format!("mmap {}", path.display()))?;
    let bytes = Bytes::from_owner(mapped);
    if bytes.len() < HEADER_LEN {
        bail!("sidecar {} is shorter than the header", path.display());
    }
    if &bytes[..8] != MAGIC {
        bail!("sidecar {} has the wrong magic", path.display());
    }
    if read_u32(&bytes, 8) != Some(VERSION) {
        bail!("sidecar {} version is not {VERSION}", path.display());
    }
    let n = usize::try_from(read_u64(&bytes, 16).context("n")?).context("n overflows usize")?;
    let dim = read_u32(&bytes, 24).context("dim")? as usize;
    let norms_off = usize::try_from(read_u64(&bytes, 56).context("norms_off")?)
        .context("norms_off overflows usize")?;
    let matrix_off = usize::try_from(read_u64(&bytes, 64).context("matrix_off")?)
        .context("matrix_off overflows usize")?;
    let meta_off = usize::try_from(read_u64(&bytes, 72).context("meta_off")?)
        .context("meta_off overflows usize")?;
    let meta_len = usize::try_from(read_u64(&bytes, 80).context("meta_len")?)
        .context("meta_len overflows usize")?;
    validate_layout(
        bytes.len(),
        n,
        dim,
        norms_off,
        matrix_off,
        meta_off,
        meta_len,
    )
    .with_context(|| format!("sidecar {} failed layout checks", path.display()))?;
    Ok(PackedIndex {
        bytes,
        n,
        dim,
        norms_off,
        matrix_off,
        meta_off,
    })
}

/// Reject corrupt headers before any slice/`from_raw_parts` on the map.
fn validate_layout(
    file_len: usize,
    n: usize,
    dim: usize,
    norms_off: usize,
    matrix_off: usize,
    meta_off: usize,
    meta_len: usize,
) -> Result<()> {
    if norms_off < HEADER_LEN {
        bail!("norms_off sits inside the header");
    }
    if !norms_off.is_multiple_of(4) {
        bail!("norms_off is not 4-byte aligned");
    }
    if !matrix_off.is_multiple_of(4) {
        bail!("matrix_off is not 4-byte aligned");
    }
    if !meta_off.is_multiple_of(4) {
        bail!("meta_off is not 4-byte aligned");
    }
    let norms_bytes = n.checked_mul(4).context("norms size overflow")?;
    let matrix_bytes = n
        .checked_mul(dim)
        .and_then(|cells| cells.checked_mul(4))
        .context("matrix size overflow")?;
    let meta_table = n
        .checked_add(1)
        .and_then(|count| count.checked_mul(4))
        .context("meta table overflow")?;
    let norms_end = norms_off
        .checked_add(norms_bytes)
        .context("norms end overflow")?;
    if matrix_off < norms_end {
        bail!("matrix overlaps norms");
    }
    let matrix_end = matrix_off
        .checked_add(matrix_bytes)
        .context("matrix end overflow")?;
    if meta_off < matrix_end {
        bail!("meta overlaps matrix");
    }
    if meta_len < meta_table {
        bail!("meta table truncated");
    }
    let need = meta_off
        .checked_add(meta_len)
        .context("sidecar size overflow")?;
    if file_len < need {
        bail!("sidecar truncated (have {file_len}, need {need})");
    }
    Ok(())
}

fn build_sidecar(source: &Path, sidecar: &Path, ident: &SourceIdent) -> Result<()> {
    let batches = read_ipc_stream_path(source)?;
    let mut rows: Vec<OwnedRow> = Vec::new();
    let mut dim = 0usize;
    for batch in &batches {
        collect_batch(batch, &mut rows, &mut dim)?;
    }
    let n = rows.len();
    let norms_bytes = n.checked_mul(4).context("norms size overflow")?;
    let matrix_bytes = n
        .checked_mul(dim)
        .and_then(|cells| cells.checked_mul(4))
        .context("matrix size overflow")?;
    let norms_off = HEADER_LEN;
    let matrix_off = align_up(
        norms_off
            .checked_add(norms_bytes)
            .context("norms end overflow")?,
        ALIGN,
    )?;
    let meta_off = align_up(
        matrix_off
            .checked_add(matrix_bytes)
            .context("matrix end overflow")?,
        8,
    )?;
    let (meta_blob, meta_len) = encode_meta(&rows)?;

    let tmp = sidecar.with_extension(format!("search.tmp.{}", std::process::id()));
    let write_result = (|| -> Result<()> {
        let file = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        let mut out = BufWriter::new(file);
        let mut header = [0u8; HEADER_LEN];
        header[..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&VERSION.to_le_bytes());
        header[16..24].copy_from_slice(&(n as u64).to_le_bytes());
        header[24..28].copy_from_slice(&(dim as u32).to_le_bytes());
        header[32..40].copy_from_slice(&ident.len.to_le_bytes());
        header[40..48].copy_from_slice(&ident.mtime_secs.to_le_bytes());
        header[48..52].copy_from_slice(&ident.mtime_nanos.to_le_bytes());
        header[56..64].copy_from_slice(&(norms_off as u64).to_le_bytes());
        header[64..72].copy_from_slice(&(matrix_off as u64).to_le_bytes());
        header[72..80].copy_from_slice(&(meta_off as u64).to_le_bytes());
        header[80..88].copy_from_slice(&(meta_len as u64).to_le_bytes());
        out.write_all(&header)?;
        write_pad(&mut out, norms_off - HEADER_LEN)?;
        for row in &rows {
            out.write_all(&row.norm.to_le_bytes())?;
        }
        write_pad(&mut out, matrix_off - (norms_off + n * 4))?;
        for row in &rows {
            if dim == 0 {
                continue;
            }
            if row.vector.len() == dim {
                for value in &row.vector {
                    out.write_all(&value.to_le_bytes())?;
                }
            } else {
                for _ in 0..dim {
                    out.write_all(&0f32.to_le_bytes())?;
                }
            }
        }
        write_pad(&mut out, meta_off - (matrix_off + n * dim * 4))?;
        out.write_all(&meta_blob)?;
        out.flush()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = fs::rename(&tmp, sidecar) {
        let _ = fs::remove_file(&tmp);
        return Err(error)
            .with_context(|| format!("publish {} over {}", tmp.display(), sidecar.display()));
    }
    Ok(())
}

struct OwnedRow {
    path: String,
    kind: String,
    symbol: String,
    snippet: String,
    relations: String,
    vector: Vec<f32>,
    norm: f32,
}

fn collect_batch(
    batch: &arrow_array::RecordBatch,
    rows: &mut Vec<OwnedRow>,
    dim: &mut usize,
) -> Result<()> {
    if batch.num_columns() <= col::SEMANTIC_VEC {
        bail!(
            "nodes.arrow has {} columns; need at least {}",
            batch.num_columns(),
            col::SEMANTIC_VEC + 1
        );
    }
    let paths = utf8(batch, col::PATH, "path")?;
    let kinds = utf8(batch, col::KIND, "kind")?;
    let symbols = utf8(batch, col::SYMBOL, "symbol")?;
    let snippets = utf8(batch, col::SNIPPET, "text_snippet")?;
    let relations = batch
        .column(col::RELATIONS)
        .as_any()
        .downcast_ref::<ListArray>();
    let semantic = batch
        .column(col::SEMANTIC_VEC)
        .as_any()
        .downcast_ref::<ListArray>();
    let code = batch
        .column(col::CODE_VEC)
        .as_any()
        .downcast_ref::<ListArray>();
    for row in 0..batch.num_rows() {
        let vector = row_vector(semantic, row)
            .or_else(|| row_vector(code, row))
            .unwrap_or_default();
        if *dim == 0 && !vector.is_empty() {
            *dim = vector.len();
        }
        let norm = if vector.len() == *dim && *dim > 0 {
            vector.iter().map(|value| value * value).sum::<f32>().sqrt()
        } else {
            0.0
        };
        rows.push(OwnedRow {
            path: paths.value(row).to_string(),
            kind: kinds.value(row).to_string(),
            symbol: symbols.value(row).to_string(),
            snippet: snippets.value(row).chars().take(240).collect(),
            relations: join_relations(relations, row),
            vector,
            norm,
        });
    }
    Ok(())
}

fn join_relations(list: Option<&ListArray>, row: usize) -> String {
    let Some(list) = list else {
        return String::new();
    };
    if row >= list.len() || list.is_null(row) {
        return String::new();
    }
    let start = list
        .value_offsets()
        .get(row)
        .copied()
        .and_then(|off| usize::try_from(off).ok())
        .unwrap_or(0);
    let len = usize::try_from(list.value_length(row)).unwrap_or(0);
    let Some(end) = start.checked_add(len) else {
        return String::new();
    };
    let Some(values) = list.values().as_any().downcast_ref::<StringArray>() else {
        return String::new();
    };
    let mut out = String::new();
    for index in start..end {
        if index >= values.len() || values.is_null(index) {
            continue;
        }
        if !out.is_empty() {
            out.push('\u{1f}');
        }
        out.push_str(values.value(index));
    }
    out
}

fn encode_meta(rows: &[OwnedRow]) -> Result<(Vec<u8>, usize)> {
    let mut offsets = Vec::with_capacity(rows.len() + 1);
    let mut blob = Vec::new();
    offsets.push(0u32);
    for row in rows {
        for part in [
            row.path.as_str(),
            row.kind.as_str(),
            row.symbol.as_str(),
            row.snippet.as_str(),
            row.relations.as_str(),
        ] {
            blob.extend_from_slice(part.as_bytes());
            blob.push(0);
        }
        offsets.push(u32::try_from(blob.len()).context("meta blob exceeds 4 GiB")?);
    }
    let mut out = Vec::with_capacity(offsets.len() * 4 + blob.len());
    for off in offsets {
        out.extend_from_slice(&off.to_le_bytes());
    }
    out.extend_from_slice(&blob);
    let len = out.len();
    Ok((out, len))
}

fn utf8<'a>(
    batch: &'a arrow_array::RecordBatch,
    index: usize,
    name: &str,
) -> Result<&'a StringArray> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("column {index} ({name}) is not Utf8"))
}

fn row_vector(list: Option<&ListArray>, row: usize) -> Option<Vec<f32>> {
    let list = list?;
    if list.is_null(row) {
        return None;
    }
    let start = list.value_offsets()[row] as usize;
    let len = list.value_length(row) as usize;
    if len == 0 {
        return None;
    }
    let values = list.values().as_any().downcast_ref::<Float32Array>()?;
    let rel = start.checked_sub(values.offset())?;
    let slice = values.values().get(rel..rel + len)?;
    if !slice.iter().any(|value| value.abs() > f32::EPSILON) {
        return None;
    }
    Some(slice.to_vec())
}

fn write_pad(out: &mut BufWriter<File>, n: usize) -> Result<()> {
    if n == 0 {
        return Ok(());
    }
    out.write_all(&vec![0u8; n])?;
    Ok(())
}

fn align_up(value: usize, align: usize) -> Result<usize> {
    if align == 0 || !align.is_power_of_two() {
        bail!("alignment must be a power of two");
    }
    let mask = align - 1;
    let sum = value.checked_add(mask).context("alignment overflow")?;
    Ok(sum & !mask)
}

fn read_u32(bytes: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    bytes.get(off..end)?.try_into().ok().map(u32::from_le_bytes)
}

fn read_u64(bytes: &[u8], off: usize) -> Option<u64> {
    let end = off.checked_add(8)?;
    bytes.get(off..end)?.try_into().ok().map(u64::from_le_bytes)
}

fn f32s(bytes: &[u8]) -> Result<&[f32]> {
    if cfg!(target_endian = "big") {
        bail!("nodes.search stores little-endian f32; big-endian hosts are unsupported");
    }
    if !bytes.len().is_multiple_of(4) {
        bail!("f32 buffer length is not a multiple of 4");
    }
    if !(bytes.as_ptr() as usize).is_multiple_of(4) {
        bail!("f32 buffer is unaligned");
    }
    // Safety: LE host, length is a multiple of 4, pointer is 4-byte aligned.
    // The mapping is immutable for the life of `bytes`.
    Ok(unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<f32>(), bytes.len() / 4) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_up_is_64() {
        assert_eq!(align_up(128, 64).expect("align"), 128);
        assert_eq!(align_up(129, 64).expect("align"), 192);
        assert!(align_up(1, 0).is_err());
    }

    #[test]
    fn rejects_short_garbage() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("nodes.search");
        fs::write(&path, b"nope").expect("write");
        assert!(open_sidecar(&path).is_err());
    }

    #[test]
    fn f32s_rejects_odd_length() {
        assert!(f32s(&[0, 1, 2]).is_err());
    }

    #[test]
    fn f32s_rejects_unaligned() {
        let bytes = [0u8; 8];
        assert!(f32s(&bytes[1..5]).is_err());
    }

    // Test helper whose argument order deliberately mirrors the on-disk header
    // layout it writes, field for field. Renaming that into a struct would hide
    // the very correspondence these tests exercise.
    #[allow(clippy::too_many_arguments)]
    fn write_header(
        path: &Path,
        n: u64,
        dim: u32,
        norms_off: u64,
        matrix_off: u64,
        meta_off: u64,
        meta_len: u64,
        extra: usize,
    ) {
        let mut header = [0u8; HEADER_LEN];
        header[..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&VERSION.to_le_bytes());
        header[16..24].copy_from_slice(&n.to_le_bytes());
        header[24..28].copy_from_slice(&dim.to_le_bytes());
        header[56..64].copy_from_slice(&norms_off.to_le_bytes());
        header[64..72].copy_from_slice(&matrix_off.to_le_bytes());
        header[72..80].copy_from_slice(&meta_off.to_le_bytes());
        header[80..88].copy_from_slice(&meta_len.to_le_bytes());
        let mut bytes = header.to_vec();
        bytes.resize(HEADER_LEN + extra, 0);
        fs::write(path, bytes).expect("write sidecar");
    }

    #[test]
    fn rejects_inconsistent_header_offsets() {
        assert!(
            validate_layout(
                HEADER_LEN + 32,
                1,
                4,
                HEADER_LEN,
                HEADER_LEN,
                HEADER_LEN + 20,
                8
            )
            .is_err()
        );
        assert!(
            validate_layout(
                HEADER_LEN + 32,
                1,
                4,
                HEADER_LEN,
                HEADER_LEN + 4,
                HEADER_LEN + 4,
                8
            )
            .is_err()
        );
        assert!(validate_layout(HEADER_LEN + 4, 1, 4, 129, 192, 208, 8).is_err());
        assert!(validate_layout(usize::MAX, usize::MAX / 2, 4, HEADER_LEN, 256, 512, 8).is_err());
        assert!(
            validate_layout(HEADER_LEN + 4, 0, 0, HEADER_LEN, HEADER_LEN, HEADER_LEN, 4).is_ok()
        );
    }

    #[test]
    fn open_sidecar_rejects_magic_version_truncation_and_unaligned_matrix() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("nodes.search");
        fs::write(&path, [0u8; HEADER_LEN]).expect("write");
        let err = |p: &Path| {
            let error = open_sidecar(p).err().expect("expected sidecar error");
            format!("{error:#}")
        };
        assert!(err(&path).contains("magic"));

        write_header(&path, 1, 4, HEADER_LEN as u64, 192, 208, 8, 8);
        let mut bytes = fs::read(&path).expect("read");
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        fs::write(&path, &bytes).expect("write");
        assert!(err(&path).contains("version"));

        write_header(&path, 1, 4, HEADER_LEN as u64, 192, 208, 64, 0);
        assert!(err(&path).contains("truncated"), "{}", err(&path));

        write_header(&path, 1, 4, HEADER_LEN as u64, 129, 208, 8, 100);
        assert!(err(&path).contains("aligned"), "{}", err(&path));
    }

    #[test]
    fn collect_batch_rejects_short_schema() {
        use std::sync::Arc;

        use arrow_array::{ArrayRef, Int32Array, RecordBatch};
        use arrow_schema::{DataType, Field, Schema};

        let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int32, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(vec![1])) as ArrayRef],
        )
        .expect("batch");
        let mut rows = Vec::new();
        let mut dim = 0;
        assert!(collect_batch(&batch, &mut rows, &mut dim).is_err());
    }

    #[test]
    fn sidecar_round_trip_from_leio_batch() {
        use crate::node_rows::build_leio_row_batch;
        use arrow_ipc::writer::StreamWriter;
        use serde_json::json;

        let dir = tempfile::tempdir().expect("temp");
        let arrow_path = dir.path().join("nodes.arrow");
        let sidecar_path = dir.path().join("nodes.search");
        let entities = vec![json!({
            "node_id": "n1",
            "tenant_id": "t",
            "repo": "r",
            "rev": "1",
            "path": "src/nav.rs",
            "lang": "rust",
            "kind": "function",
            "symbol": "run_nav",
            "target": "",
            "relations": ["fcaFamily:nav"],
            "metadata": {},
            "text_snippet": "fn run_nav()",
            "embed_model": "BAAI/bge-m3",
            "embed_dim": 4,
            "code_vec": [1.0, 0.0, 0.0, 0.0],
            "semantic_vec": [0.6, 0.8, 0.0, 0.0],
            "ontology_vec": [0.0],
            "execution_vec_bin": [],
        })];
        let batch = build_leio_row_batch(&entities).expect("batch");
        {
            let file = File::create(&arrow_path).expect("create");
            let mut writer = StreamWriter::try_new(file, &batch.schema()).expect("writer");
            writer.write(&batch).expect("write");
            writer.finish().expect("finish");
        }
        let packed = load_or_build(&arrow_path, &sidecar_path).expect("sidecar");
        assert_eq!(packed.n(), 1);
        assert_eq!(packed.symbol(0), "run_nav");
        assert_eq!(packed.path(0), "src/nav.rs");
        assert_eq!(packed.kind(0), "function");
        assert!(packed.relations(0).contains("fcaFamily:nav"));
        assert_eq!(packed.vector(0), Some(&[0.6_f32, 0.8, 0.0, 0.0][..]));
        assert!((packed.norm(0) - 1.0).abs() < 1e-6);
        assert!(sidecar_path.is_file());
        let again = load_or_build(&arrow_path, &sidecar_path).expect("reuse");
        assert_eq!(again.symbol(0), "run_nav");
        assert_eq!(again.vector(0), packed.vector(0));
    }
}
