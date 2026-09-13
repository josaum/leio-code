//! Local compiled-wiki store for `leio-code knowledge`.
//!
//! Query used to require a remote vector service plus a `uv` Python helper. This module
//! writes `.leio-code/exports/knowledge-v1/articles.search` (packed strings)
//! and a matching Arrow IPC stream so adaptive/text/status work offline.
//! Ranking is BM25 over a packed inverted index of heading sections, with
//! phrase/title boosts and match-centered snippets. Optional BGE-M3 cosine
//! reranks the shortlist when `LEIO_CODE_EMBED_URL` is set. Compile reuses
//! unchanged files from the previous sidecar. The packed header stores
//! source identity so a stale store recompiles. The store is fully local.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result, bail};
use arrow_array::{RecordBatch, StringArray, UInt32Array};
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema};
use bytes::Bytes;
use ignore::WalkBuilder;
use memmap2::Mmap;
use serde_json::json;

use crate::model::{EvidenceItem, QueryEnvelope, SCHEMA_VERSION};

const MAGIC: &[u8; 8] = b"LEIOKB01";
/// Sidecar layout v4: inverted postings after the string blob. Older files rebuild.
const VERSION: u32 = 4;
/// Bump when heading-split rules change so incremental compile does not reuse stale sections.
const SPLIT_REV: u32 = 2;
const HEADER_LEN: usize = 128;
/// 4-byte line number plus six packed string fields (off+len each).
const ROW_WIDTH: usize = 52;
const F_TITLE: usize = 4;
const F_BODY: usize = 12;
const F_TOPIC: usize = 20;
const F_PATH: usize = 28;
const F_KIND: usize = 36;
const F_HEADING: usize = 44;
/// Skip generated/vendor blobs; agent wikis stay well under this.
const MAX_FILE_BYTES: u64 = 512 * 1024;
const BODY_CAP: usize = 256 * 1024;
/// Characters kept on each side of the first match. Wider windows bury the hit.
const SNIPPET_RADIUS: usize = 140;
/// Body-only hits need this many query terms (or a phrase). Stops README noise.
const MIN_BODY_TERMS: usize = 2;
const TITLE_EXACT: f64 = 100.0;
const TITLE_PHRASE: f64 = 50.0;
const HEADING_EXACT: f64 = 80.0;
const HEADING_PHRASE: f64 = 40.0;
const BODY_PHRASE: f64 = 18.0;
const TITLE_TERM: f64 = 8.0;
const HEADING_TERM: f64 = 6.0;
const TOPIC_TERM: f64 = 4.0;
const PATH_TERM: f64 = 2.0;
const BODY_TERM: f64 = 1.0;
/// README / GEMINI / CLAUDE pages match every generic query; keep them last.
const GENERIC_SCALE: f64 = 0.35;
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "api", "app", "by", "for", "from", "in", "of", "on", "or", "the", "this",
    "to", "with",
];
const HDR_SOURCE_COUNT: usize = 32;
const HDR_MAX_MTIME: usize = 36;
const HDR_POSTINGS_OFF: usize = 44;
const HDR_POSTINGS_LEN: usize = 52;
const HDR_AVG_DL: usize = 60;
const MASK_TITLE: u16 = 1;
const MASK_HEADING: u16 = 2;
const MASK_TOPIC: u16 = 4;
const MASK_PATH: u16 = 8;
const MASK_BODY: u16 = 16;
/// Classic BM25 saturation. Higher values treat extra term frequency more linearly.
const BM25_K1: f64 = 1.2;
/// Classic BM25 length norm. 0 ignores length; 1 fully scales by dl/avgdl.
const BM25_B: f64 = 0.75;
/// Cosine is ~0.3–0.8; this keeps hybrid from beating an exact title hit.
const EMBED_WEIGHT: f64 = 15.0;
/// Two sections from the same file is enough; more is the same page twice.
const MAX_SECTIONS_PER_FILE: usize = 2;
const TERM_DIR_WIDTH: usize = 20;
const POST_WIDTH: usize = 8;

/// Packed local wiki index.
pub struct WikiIndex {
    bytes: Bytes,
    n: usize,
    blob_off: usize,
    source_count: u32,
    max_source_mtime: u64,
    postings_off: usize,
    postings_len: usize,
    avg_dl: f32,
}

impl WikiIndex {
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn article(&self, row: usize) -> Option<Article<'_>> {
        if row >= self.n {
            return None;
        }
        let base = HEADER_LEN.checked_add(row.checked_mul(ROW_WIDTH)?)?;
        let line = u32::from_le_bytes(self.bytes.get(base..base + 4)?.try_into().ok()?);
        Some(Article {
            title: self.field(base, F_TITLE)?,
            body: self.field(base, F_BODY)?,
            topic: self.field(base, F_TOPIC)?,
            source_path: self.field(base, F_PATH)?,
            kind: self.field(base, F_KIND)?,
            heading_path: self.field(base, F_HEADING)?,
            line,
        })
    }

    fn field(&self, row_base: usize, field_off: usize) -> Option<&str> {
        let off_pos = row_base.checked_add(field_off)?;
        let len_pos = off_pos.checked_add(4)?;
        let rel = u32::from_le_bytes(self.bytes.get(off_pos..off_pos + 4)?.try_into().ok()?);
        let len = u32::from_le_bytes(self.bytes.get(len_pos..len_pos + 4)?.try_into().ok()?);
        let start = self.blob_off.checked_add(rel as usize)?;
        let end = start.checked_add(len as usize)?;
        std::str::from_utf8(self.bytes.get(start..end)?).ok()
    }

    fn posting_rows(&self, terms: &[String]) -> Vec<usize> {
        if terms.is_empty() || self.postings_len == 0 {
            return (0..self.n).collect();
        }
        let mut seen = vec![false; self.n];
        let mut any = false;
        for term in terms {
            let Some((_, posts)) = self.term_postings(term) else {
                continue;
            };
            any = true;
            for chunk in posts.chunks_exact(POST_WIDTH) {
                let row = u32::from_le_bytes(chunk[0..4].try_into().unwrap_or([0; 4])) as usize;
                if row < seen.len() {
                    seen[row] = true;
                }
            }
        }
        if !any {
            return Vec::new();
        }
        seen.into_iter()
            .enumerate()
            .filter_map(|(i, hit)| hit.then_some(i))
            .collect()
    }

    fn term_postings(&self, term: &str) -> Option<(u32, &[u8])> {
        let postings = self
            .bytes
            .get(self.postings_off..self.postings_off + self.postings_len)?;
        if postings.len() < 24 {
            return None;
        }
        let term_count = u32::from_le_bytes(postings[0..4].try_into().ok()?) as usize;
        let dl_bytes = u32::from_le_bytes(postings[12..16].try_into().ok()?) as usize;
        let dir_bytes = u32::from_le_bytes(postings[16..20].try_into().ok()?) as usize;
        let names_bytes = u32::from_le_bytes(postings[20..24].try_into().ok()?) as usize;
        let dir_off = 24usize.checked_add(dl_bytes)?;
        let names_off = dir_off.checked_add(dir_bytes)?;
        let posts_off = names_off.checked_add(names_bytes)?;
        let dir = postings.get(dir_off..dir_off + dir_bytes)?;
        let names = postings.get(names_off..names_off + names_bytes)?;
        let posts = postings.get(posts_off..)?;
        let mut lo = 0usize;
        let mut hi = term_count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let entry = dir.get(mid * TERM_DIR_WIDTH..(mid + 1) * TERM_DIR_WIDTH)?;
            let name_off = u32::from_le_bytes(entry[0..4].try_into().ok()?) as usize;
            let name_len = u32::from_le_bytes(entry[4..8].try_into().ok()?) as usize;
            let name = std::str::from_utf8(names.get(name_off..name_off + name_len)?).ok()?;
            match name.cmp(term) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let df = u32::from_le_bytes(entry[8..12].try_into().ok()?);
                    let post_rel = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
                    let post_len = u32::from_le_bytes(entry[16..20].try_into().ok()?) as usize;
                    let slice = posts.get(post_rel..post_rel + post_len)?;
                    return Some((df, slice));
                }
            }
        }
        None
    }

    fn posting_hit(&self, term: &str, row: u32) -> Option<(u16, u16)> {
        let (_, posts) = self.term_postings(term)?;
        for chunk in posts.chunks_exact(POST_WIDTH) {
            let found = u32::from_le_bytes(chunk[0..4].try_into().ok()?);
            if found == row {
                let tf = u16::from_le_bytes(chunk[4..6].try_into().ok()?);
                let mask = u16::from_le_bytes(chunk[6..8].try_into().ok()?);
                return Some((tf, mask));
            }
        }
        None
    }

    fn doc_len(&self, row: usize) -> u16 {
        let postings = match self
            .bytes
            .get(self.postings_off..self.postings_off + self.postings_len)
        {
            Some(bytes) if bytes.len() >= 16 => bytes,
            _ => return 0,
        };
        let dl_bytes = u32::from_le_bytes(postings[12..16].try_into().unwrap_or([0; 4])) as usize;
        let dls = postings.get(24..24 + dl_bytes).unwrap_or(&[]);
        let off = row.saturating_mul(2);
        dls.get(off..off + 2)
            .and_then(|b| b.try_into().ok())
            .map(u16::from_le_bytes)
            .unwrap_or(0)
    }
}

impl fmt::Debug for WikiIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WikiIndex")
            .field("n", &self.n)
            .field("source_count", &self.source_count)
            .field("max_source_mtime", &self.max_source_mtime)
            .field("postings_len", &self.postings_len)
            .finish_non_exhaustive()
    }
}

/// One compiled wiki section.
#[derive(Debug, Clone, Copy)]
pub struct Article<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub topic: &'a str,
    pub source_path: &'a str,
    pub kind: &'a str,
    pub heading_path: &'a str,
    pub line: u32,
}

/// Default export directory under the repo's `.leio-code` tree.
pub fn default_knowledge_dir(repo_root: &Path) -> PathBuf {
    repo_root
        .join(".leio-code")
        .join("exports")
        .join("knowledge-v1")
}

/// Packed sidecar path.
pub fn sidecar_path(dir: &Path) -> PathBuf {
    dir.join("articles.search")
}

/// Arrow IPC stream path.
pub fn arrow_path(dir: &Path) -> PathBuf {
    dir.join("articles.arrow")
}

/// Compile markdown/text into the local wiki store.
///
/// # Errors
///
/// Returns an error if the walk or write fails.
pub fn compile_knowledge(repo_root: &Path) -> Result<QueryEnvelope> {
    let _lock = crate::sidecar::acquire_lock(repo_root, "knowledge")?;
    let started = Instant::now();
    let batch = collect_articles(repo_root)?;
    let dir = default_knowledge_dir(repo_root);
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    write_arrow(&dir, &batch.articles)?;
    write_sidecar(&dir, &batch.articles, &batch.identity)?;
    write_manifest(&dir, repo_root, batch.articles.len(), &batch.identity)?;
    write_sources(&dir, &batch.stamps)?;
    let mut envelope = status_from_articles(
        "knowledge_compile",
        &format!(
            "compiled {} wiki sections from {} files",
            batch.articles.len(),
            batch.identity.count
        ),
        &batch.articles,
        started,
        "compiled",
    );
    if let Some(serde_json::Value::Object(meta)) = envelope.meta.as_mut() {
        meta.insert(
            "compile".to_string(),
            json!({
                "sections": batch.articles.len(),
                "files": batch.identity.count,
                "reused_files": batch.reused_files,
                "parsed_files": batch.parsed_files,
            }),
        );
    }
    if let Some(entity) = envelope.entities.first_mut() {
        entity["files"] = json!(batch.identity.count);
        entity["sections"] = json!(batch.articles.len());
    }
    match crate::knowledge_graph::compile_formal_graph(repo_root) {
        Ok(stats) => {
            if let Some(meta) = envelope.meta.as_mut() {
                meta["formal"] = json!(stats);
            }
        }
        Err(err) => envelope
            .warnings
            .push(format!("formal graph compile failed: {err}")),
    }
    Ok(envelope)
}

/// Wiki section rows for formal-context incidence (file + heading + topic).
pub(crate) struct WikiSectionInc {
    pub title: String,
    pub topic: String,
    pub source_path: String,
    pub heading_path: String,
    pub line: u32,
}

impl WikiSectionInc {
    pub(crate) fn object_id(&self) -> String {
        format!("section:{}#{}", self.source_path, self.line)
    }
}

/// Heading-stack segments (`Root > Child` → `["Root", "Child"]`).
pub(crate) fn heading_segments(path: &str) -> Vec<&str> {
    path.split(" > ")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// Walk the repo the same way `knowledge compile` does.
pub(crate) fn wiki_section_incidences(repo_root: &Path) -> Vec<WikiSectionInc> {
    let Ok(batch) = collect_articles(repo_root) else {
        return Vec::new();
    };
    batch
        .articles
        .into_iter()
        .map(|article| WikiSectionInc {
            title: article.title,
            topic: article.topic,
            source_path: article.source_path,
            heading_path: article.heading_path,
            line: article.line,
        })
        .collect()
}

/// Lexical search over the local store, compiling first if missing.
pub fn search_local(repo_root: &Path, needle: &str, limit: usize) -> Option<QueryEnvelope> {
    let started = Instant::now();
    let index = open_or_compile(repo_root).ok()?;
    if index.is_empty() {
        return None;
    }
    let ranked = rank_and_maybe_embed(&index, repo_root, needle, limit);
    Some(hits_to_envelope(
        "knowledge_text",
        &format!("local wiki search for `{needle}`"),
        &ranked,
        started,
        "local_lexical",
    ))
}

/// Adaptive local search: title, then topic, then body.
pub fn search_local_adaptive(
    repo_root: &Path,
    needle: &str,
    limit: usize,
) -> Option<QueryEnvelope> {
    let started = Instant::now();
    let index = open_or_compile(repo_root).ok()?;
    if index.is_empty() {
        return None;
    }
    let (stage, ranked) = rank_adaptive(&index, repo_root, needle, limit);
    Some(hits_to_envelope(
        "knowledge_adaptive",
        &format!("local wiki query for `{needle}`"),
        &ranked,
        started,
        stage,
    ))
}

/// Local store health. `None` when no compiled wiki exists yet.
pub fn status_local(repo_root: &Path) -> Option<QueryEnvelope> {
    let started = Instant::now();
    let path = sidecar_path(&default_knowledge_dir(repo_root));
    if !path.is_file() {
        return None;
    }
    let index = open_sidecar(&path).ok()?;
    let articles: Vec<Article<'_>> = (0..index.len()).filter_map(|i| index.article(i)).collect();
    let formal = crate::knowledge_graph::formal_graph_status(repo_root);
    let triples = formal
        .get("triples")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let mut envelope = status_from_articles(
        "knowledge_status",
        &format!(
            "local wiki: {} sections from {} files · formal {triples} triples",
            articles.len(),
            index.source_count
        ),
        &articles,
        started,
        "local_store",
    );
    if let Some(entity) = envelope.entities.first_mut() {
        entity["files"] = json!(index.source_count);
        entity["sections"] = json!(articles.len());
        entity["formal_files"] = formal.get("files_ok").cloned().unwrap_or(json!(0));
        entity["formal_triples"] = json!(triples);
        entity["formal_citations"] = formal.get("citations").cloned().unwrap_or(json!(0));
        entity["formal_prefixes"] = json!(
            formal
                .get("prefixes")
                .and_then(serde_json::Value::as_object)
                .map_or(0, serde_json::Map::len)
        );
        entity["formal_fresh"] = formal.get("fresh").cloned().unwrap_or(json!(false));
        entity["formal_cache"] = formal.get("cache").cloned().unwrap_or(json!(""));
    }
    if let Some(meta) = envelope.meta.as_mut() {
        meta["formal"] = formal;
    }
    Some(envelope)
}

/// Envelope for a missing or empty compiled store.
///
/// The local fns auto-compile on demand, so reaching this means compilation
/// was impossible (unreadable wiki, empty repo) — point the operator at the
/// explicit compile verb instead of failing silently.
fn missing_store_envelope(kind: &str, started: Instant) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: format!(
            "{kind}-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: kind.to_string(),
        summary: "no compiled wiki available — run `leio-code knowledge compile`".to_string(),
        confidence: 0.0,
        entities: Vec::new(),
        evidence: Vec::new(),
        warnings: vec!["knowledge store missing or empty".to_string()],
        meta: Some(json!({"store": "local"})),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Local-first lexical wiki search (CLI `knowledge text`).
pub fn search_knowledge_text(repo_root: &Path, needle: &str, limit: usize) -> QueryEnvelope {
    let started = Instant::now();
    search_local(repo_root, needle, limit)
        .unwrap_or_else(|| missing_store_envelope("knowledge_text", started))
}

/// Local-first adaptive wiki search (CLI `knowledge adaptive`).
pub fn search_knowledge_adaptive(repo_root: &Path, needle: &str, limit: usize) -> QueryEnvelope {
    let started = Instant::now();
    search_local_adaptive(repo_root, needle, limit)
        .unwrap_or_else(|| missing_store_envelope("knowledge_adaptive", started))
}

/// Local store health (CLI `knowledge status`).
pub fn knowledge_status(repo_root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    status_local(repo_root).unwrap_or_else(|| missing_store_envelope("knowledge_status", started))
}

fn open_or_compile(repo_root: &Path) -> Result<WikiIndex> {
    let path = sidecar_path(&default_knowledge_dir(repo_root));
    let identity = source_identity(repo_root);
    if path.is_file()
        && let Ok(index) = open_sidecar(&path)
    {
        let identity_ok = index.source_count == identity.count
            && index.max_source_mtime == identity.max_mtime
            && (index.n == 0 || index.source_count > 0);
        if identity_ok {
            return Ok(index);
        }
    }
    compile_knowledge(repo_root)?;
    open_sidecar(&path)
}

struct SourceIdentity {
    count: u32,
    max_mtime: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileStamp {
    pub(crate) mtime: u64,
    size: u64,
}

pub(crate) struct CompileBatch {
    pub(crate) articles: Vec<OwnedArticle>,
    identity: SourceIdentity,
    pub(crate) stamps: BTreeMap<String, FileStamp>,
    reused_files: u32,
    parsed_files: u32,
}

fn sources_path(dir: &Path) -> PathBuf {
    dir.join("sources.json")
}

fn stamp_from_meta(meta: &std::fs::Metadata) -> FileStamp {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    FileStamp {
        mtime,
        size: meta.len(),
    }
}

fn load_previous(dir: &Path) -> Option<(WikiIndex, BTreeMap<String, FileStamp>)> {
    let index = open_sidecar(&sidecar_path(dir)).ok()?;
    let raw = fs::read(sources_path(dir)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    if value.get("version").and_then(|v| v.as_u64()) != Some(u64::from(VERSION))
        || value.get("split").and_then(|v| v.as_u64()) != Some(u64::from(SPLIT_REV))
    {
        return None;
    }
    let mut stamps = BTreeMap::new();
    for (path, stamp) in value.get("files")?.as_object()? {
        let mtime = stamp.get("mtime")?.as_u64()?;
        let size = stamp.get("size")?.as_u64()?;
        stamps.insert(path.clone(), FileStamp { mtime, size });
    }
    Some((index, stamps))
}

fn previous_by_path(index: &WikiIndex) -> BTreeMap<String, Vec<OwnedArticle>> {
    let mut by_path = BTreeMap::new();
    for i in 0..index.len() {
        let Some(article) = index.article(i) else {
            continue;
        };
        by_path
            .entry(article.source_path.to_string())
            .or_insert_with(Vec::new)
            .push(OwnedArticle::from(article));
    }
    by_path
}

fn write_sources(dir: &Path, stamps: &BTreeMap<String, FileStamp>) -> Result<()> {
    let mut files = serde_json::Map::new();
    for (path, stamp) in stamps {
        files.insert(
            path.clone(),
            json!({ "mtime": stamp.mtime, "size": stamp.size }),
        );
    }
    crate::sidecar::write_atomic(
        &sources_path(dir),
        &serde_json::to_vec_pretty(&json!({
            "version": VERSION,
            "split": SPLIT_REV,
            "files": files,
        }))?,
    )?;
    Ok(())
}

fn source_identity(repo_root: &Path) -> SourceIdentity {
    let mut count = 0u32;
    let mut max_mtime = 0u64;
    for dent in knowledge_walker(repo_root).build().flatten() {
        let Some(meta) = accepted_wiki_meta(&dent) else {
            continue;
        };
        count = count.saturating_add(1);
        if let Ok(modified) = meta.modified()
            && let Ok(secs) = modified.duration_since(SystemTime::UNIX_EPOCH)
        {
            max_mtime = max_mtime.max(secs.as_secs());
        }
    }
    SourceIdentity { count, max_mtime }
}

fn knowledge_walker(repo_root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(repo_root);
    builder.hidden(true);
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);
    builder.filter_entry(|entry| {
        let Some(name) = entry.file_name().to_str() else {
            return true;
        };
        if entry.path().is_dir() {
            return !matches!(
                name,
                "target"
                    | "node_modules"
                    | ".git"
                    | ".leio-code"
                    | "dist"
                    | "vendor"
                    | ".venv"
                    | "__pycache__"
                    | "fixtures"
                    | ".next"
                    | ".turbo"
                    | ".pnpm-store"
                    | "coverage"
            );
        }
        true
    });
    builder
}

fn is_wiki_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "md" | "mdx" | "txt" | "rst"
    )
}

fn is_noise_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        "CHANGELOG.md"
            | "CHANGELOG"
            | "LICENSE"
            | "LICENSE.md"
            | "CODE_OF_CONDUCT.md"
            | "SECURITY.md"
            | "NOTICE"
    )
}

fn accepted_wiki_meta(dent: &ignore::DirEntry) -> Option<std::fs::Metadata> {
    let path = dent.path();
    if !dent.file_type().is_some_and(|t| t.is_file()) {
        return None;
    }
    if !is_wiki_file(path) || is_noise_file(path) {
        return None;
    }
    let meta = dent.metadata().ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }
    Some(meta)
}

pub(crate) fn collect_articles(repo_root: &Path) -> Result<CompileBatch> {
    let dir = default_knowledge_dir(repo_root);
    let previous = load_previous(&dir);
    let cached = previous
        .as_ref()
        .map(|(index, _)| previous_by_path(index))
        .unwrap_or_default();
    let prev_stamps = previous.as_ref().map(|(_, stamps)| stamps);

    let mut articles = Vec::new();
    let mut stamps = BTreeMap::new();
    let mut max_mtime = 0u64;
    let mut file_count = 0u32;
    let mut reused_files = 0u32;
    let mut parsed_files = 0u32;

    for dent in knowledge_walker(repo_root).build().flatten() {
        let Some(meta) = accepted_wiki_meta(&dent) else {
            continue;
        };
        let path = dent.path();
        let file_kind = match path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "md" | "mdx" => "markdown",
            "txt" | "rst" => "text",
            _ => continue,
        };
        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let stamp = stamp_from_meta(&meta);
        max_mtime = max_mtime.max(stamp.mtime);
        file_count = file_count.saturating_add(1);
        stamps.insert(rel.clone(), stamp);

        let reuse = prev_stamps
            .and_then(|map| map.get(&rel))
            .is_some_and(|prev| *prev == stamp)
            .then(|| cached.get(&rel))
            .flatten();
        if let Some(sections) = reuse {
            articles.extend(sections.iter().cloned());
            reused_files = reused_files.saturating_add(1);
            continue;
        }

        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        articles.extend(split_sections(&raw, &rel, file_kind));
        parsed_files = parsed_files.saturating_add(1);
    }
    articles.sort_by(|a, b| {
        a.source_path
            .cmp(&b.source_path)
            .then(a.line.cmp(&b.line))
            .then(a.heading_path.cmp(&b.heading_path))
    });
    Ok(CompileBatch {
        articles,
        identity: SourceIdentity {
            count: file_count,
            max_mtime,
        },
        stamps,
        reused_files,
        parsed_files,
    })
}

#[derive(Clone)]
pub(crate) struct OwnedArticle {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) topic: String,
    pub(crate) source_path: String,
    pub(crate) kind: String,
    pub(crate) heading_path: String,
    pub(crate) line: u32,
}

impl From<Article<'_>> for OwnedArticle {
    fn from(article: Article<'_>) -> Self {
        Self {
            title: article.title.to_string(),
            body: article.body.to_string(),
            topic: article.topic.to_string(),
            source_path: article.source_path.to_string(),
            kind: article.kind.to_string(),
            heading_path: article.heading_path.to_string(),
            line: article.line,
        }
    }
}

fn file_stem_title(rel: &str) -> String {
    Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(rel)
        .replace(['-', '_'], " ")
}

fn heading_level(line: &str) -> Option<(u8, &str)> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('#')?;
    let mut level = 1u8;
    let mut body = rest;
    while let Some(next) = body.strip_prefix('#') {
        level = level.saturating_add(1);
        if level > 6 {
            return None;
        }
        body = next;
    }
    let title = body.strip_prefix(' ')?.trim();
    if title.is_empty() {
        return None;
    }
    Some((level, title))
}

fn split_sections(raw: &str, rel: &str, file_kind: &str) -> Vec<OwnedArticle> {
    let yaml = yaml_title(raw);
    let body = strip_frontmatter(raw);
    let file_topic = topic_from_path(rel);
    let fallback = yaml.clone().unwrap_or_else(|| file_stem_title(rel));
    let mut stack: Vec<(u8, String)> = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    let mut start_line = 1u32;
    let mut out = Vec::new();

    let flush = |stack: &[(u8, String)],
                 lines: &mut Vec<&str>,
                 start_line: u32,
                 first: &mut bool,
                 out: &mut Vec<OwnedArticle>| {
        let text = lines.join("\n");
        lines.clear();
        let text = text.trim();
        let titles: Vec<&str> = stack.iter().map(|(_, title)| title.as_str()).collect();
        if text.is_empty() && titles.is_empty() {
            return;
        }
        let title = if *first {
            *first = false;
            yaml.clone()
                .or_else(|| titles.last().map(|t| (*t).to_string()))
                .unwrap_or_else(|| fallback.clone())
        } else {
            titles
                .last()
                .map(|t| (*t).to_string())
                .unwrap_or_else(|| fallback.clone())
        };
        if text.is_empty() && title.is_empty() {
            return;
        }
        let heading_path = if titles.is_empty() {
            title.clone()
        } else {
            titles.join(" > ")
        };
        let topic = file_topic.clone();
        let kind = if file_kind == "text" {
            "text".to_string()
        } else if titles.len() <= 1 {
            "markdown".to_string()
        } else {
            "markdown-section".to_string()
        };
        out.push(OwnedArticle {
            title,
            body: truncate_body(text),
            topic,
            source_path: rel.to_string(),
            kind,
            heading_path,
            line: start_line,
        });
    };

    let mut first = true;
    let mut in_fence = false;
    for (idx, line) in body.lines().enumerate() {
        let line_no = u32::try_from(idx + 1).unwrap_or(u32::MAX);
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            lines.push(line);
            continue;
        }
        if !in_fence && let Some((level, title)) = heading_level(line) {
            flush(&stack, &mut lines, start_line, &mut first, &mut out);
            while stack.last().is_some_and(|(seen, _)| *seen >= level) {
                stack.pop();
            }
            stack.push((level, title.to_string()));
            start_line = line_no;
            continue;
        }
        lines.push(line);
    }
    flush(&stack, &mut lines, start_line, &mut first, &mut out);
    if out.is_empty() {
        out.push(OwnedArticle {
            title: fallback,
            body: truncate_body(body),
            topic: file_topic,
            source_path: rel.to_string(),
            kind: file_kind.to_string(),
            heading_path: yaml.unwrap_or_else(|| file_stem_title(rel)),
            line: 1,
        });
    }
    out
}

fn yaml_title(raw: &str) -> Option<String> {
    let rest = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---").or_else(|| rest.find("\r\n---"))?;
    for line in rest[..end].lines() {
        let Some(value) = line.trim().strip_prefix("title:") else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'').trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn strip_frontmatter(raw: &str) -> &str {
    let Some(rest) = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))
    else {
        return raw;
    };
    let Some(idx) = rest.find("\n---").or_else(|| rest.find("\r\n---")) else {
        return raw;
    };
    let after = &rest[idx + 4..];
    after
        .strip_prefix('\n')
        .or_else(|| after.strip_prefix("\r\n"))
        .unwrap_or(after)
}

fn topic_from_path(rel: &str) -> String {
    rel.split('/')
        .next()
        .filter(|part| !part.is_empty() && *part != rel)
        .unwrap_or("root")
        .to_string()
}

fn truncate_body(raw: &str) -> String {
    if raw.len() <= BODY_CAP {
        return raw.to_string();
    }
    let end = raw
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= BODY_CAP)
        .last()
        .unwrap_or(0);
    raw[..end].to_string()
}

fn write_arrow(dir: &Path, articles: &[OwnedArticle]) -> Result<()> {
    let schema = Schema::new(vec![
        Field::new("title", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("topic", DataType::Utf8, false),
        Field::new("source_path", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("heading_path", DataType::Utf8, false),
        Field::new("line", DataType::UInt32, false),
    ]);
    let batch = RecordBatch::try_new(
        std::sync::Arc::new(schema.clone()),
        vec![
            std::sync::Arc::new(StringArray::from(
                articles
                    .iter()
                    .map(|a| a.title.as_str())
                    .collect::<Vec<_>>(),
            )),
            std::sync::Arc::new(StringArray::from(
                articles.iter().map(|a| a.body.as_str()).collect::<Vec<_>>(),
            )),
            std::sync::Arc::new(StringArray::from(
                articles
                    .iter()
                    .map(|a| a.topic.as_str())
                    .collect::<Vec<_>>(),
            )),
            std::sync::Arc::new(StringArray::from(
                articles
                    .iter()
                    .map(|a| a.source_path.as_str())
                    .collect::<Vec<_>>(),
            )),
            std::sync::Arc::new(StringArray::from(
                articles.iter().map(|a| a.kind.as_str()).collect::<Vec<_>>(),
            )),
            std::sync::Arc::new(StringArray::from(
                articles
                    .iter()
                    .map(|a| a.heading_path.as_str())
                    .collect::<Vec<_>>(),
            )),
            std::sync::Arc::new(UInt32Array::from(
                articles.iter().map(|a| a.line).collect::<Vec<_>>(),
            )),
        ],
    )?;
    let path = arrow_path(dir);
    let tmp = path.with_extension("arrow.tmp");
    let file = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    let mut writer = StreamWriter::try_new(BufWriter::new(file), &schema)?;
    writer.write(&batch)?;
    writer.finish()?;
    fs::rename(&tmp, &path).with_context(|| format!("publish {}", path.display()))?;
    Ok(())
}

fn write_sidecar(dir: &Path, articles: &[OwnedArticle], identity: &SourceIdentity) -> Result<()> {
    let mut blob = Vec::new();
    let mut table = Vec::with_capacity(articles.len() * ROW_WIDTH);
    for article in articles {
        table.extend_from_slice(&article.line.to_le_bytes());
        push_field(&mut table, &mut blob, &article.title)?;
        push_field(&mut table, &mut blob, &article.body)?;
        push_field(&mut table, &mut blob, &article.topic)?;
        push_field(&mut table, &mut blob, &article.source_path)?;
        push_field(&mut table, &mut blob, &article.kind)?;
        push_field(&mut table, &mut blob, &article.heading_path)?;
    }

    let n = u32::try_from(articles.len()).context("too many wiki articles")?;
    let blob_off = HEADER_LEN
        .checked_add(table.len())
        .context("sidecar table overflow")?;
    let mut header = vec![0u8; HEADER_LEN];
    header[..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&VERSION.to_le_bytes());
    header[12..16].copy_from_slice(&n.to_le_bytes());
    header[16..24].copy_from_slice(&(blob_off as u64).to_le_bytes());
    header[24..32].copy_from_slice(&(blob.len() as u64).to_le_bytes());
    header[HDR_SOURCE_COUNT..HDR_SOURCE_COUNT + 4].copy_from_slice(&identity.count.to_le_bytes());
    header[HDR_MAX_MTIME..HDR_MAX_MTIME + 8].copy_from_slice(&identity.max_mtime.to_le_bytes());
    let (postings, avg_dl) = pack_postings(articles)?;
    let postings_off = blob_off
        .checked_add(blob.len())
        .context("postings offset overflow")?;
    header[HDR_POSTINGS_OFF..HDR_POSTINGS_OFF + 8]
        .copy_from_slice(&(postings_off as u64).to_le_bytes());
    header[HDR_POSTINGS_LEN..HDR_POSTINGS_LEN + 8]
        .copy_from_slice(&(postings.len() as u64).to_le_bytes());
    header[HDR_AVG_DL..HDR_AVG_DL + 4].copy_from_slice(&avg_dl.to_le_bytes());

    let path = sidecar_path(dir);
    let tmp = path.with_extension("search.tmp");
    {
        let mut out = BufWriter::new(File::create(&tmp)?);
        out.write_all(&header)?;
        out.write_all(&table)?;
        out.write_all(&blob)?;
        out.write_all(&postings)?;
        out.flush()?;
    }
    if let Err(err) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("publish {}", path.display()));
    }
    Ok(())
}

fn tokenize_stream(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 3)
        .map(|token| token.to_ascii_lowercase())
        .filter(|token| !STOPWORDS.contains(&token.as_str()))
        .collect()
}

fn pack_postings(articles: &[OwnedArticle]) -> Result<(Vec<u8>, f32)> {
    let mut inv: BTreeMap<String, BTreeMap<u32, PostingAcc>> = BTreeMap::new();
    let mut dls = Vec::with_capacity(articles.len());
    let mut dl_sum = 0u64;
    for (i, article) in articles.iter().enumerate() {
        let row = u32::try_from(i).context("too many wiki sections")?;
        let mut dl = 0u32;
        add_posting(&mut inv, &mut dl, row, &article.title, MASK_TITLE);
        add_posting(&mut inv, &mut dl, row, &article.heading_path, MASK_HEADING);
        add_posting(&mut inv, &mut dl, row, &article.topic, MASK_TOPIC);
        add_posting(&mut inv, &mut dl, row, &article.source_path, MASK_PATH);
        add_posting(&mut inv, &mut dl, row, &article.body, MASK_BODY);
        dls.push(u16::try_from(dl.min(u32::from(u16::MAX))).unwrap_or(u16::MAX));
        dl_sum = dl_sum.saturating_add(u64::from(dl));
    }
    let n = articles.len();
    let avg_dl = if n == 0 {
        0.0
    } else {
        dl_sum as f32 / n as f32
    };

    let mut names = Vec::new();
    let mut posts = Vec::new();
    let mut dir = Vec::with_capacity(inv.len() * TERM_DIR_WIDTH);
    for (term, rows) in &inv {
        let name_off = u32::try_from(names.len()).context("term dictionary overflow")?;
        let name_len = u32::try_from(term.len()).context("term too long")?;
        names.extend_from_slice(term.as_bytes());
        let post_off = u32::try_from(posts.len()).context("postings overflow")?;
        for (row, acc) in rows {
            posts.extend_from_slice(&row.to_le_bytes());
            posts.extend_from_slice(&acc.tf.to_le_bytes());
            posts.extend_from_slice(&acc.mask.to_le_bytes());
        }
        let post_len =
            u32::try_from(posts.len() - post_off as usize).context("posting list overflow")?;
        let df = u32::try_from(rows.len()).context("df overflow")?;
        dir.extend_from_slice(&name_off.to_le_bytes());
        dir.extend_from_slice(&name_len.to_le_bytes());
        dir.extend_from_slice(&df.to_le_bytes());
        dir.extend_from_slice(&post_off.to_le_bytes());
        dir.extend_from_slice(&post_len.to_le_bytes());
    }

    let mut dl_bytes = Vec::with_capacity(dls.len() * 2 + 2);
    for dl in &dls {
        dl_bytes.extend_from_slice(&dl.to_le_bytes());
    }
    while dl_bytes.len() % 4 != 0 {
        dl_bytes.push(0);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&(u32::try_from(inv.len()).context("too many terms")?).to_le_bytes());
    out.extend_from_slice(&(u32::try_from(n).context("too many docs")?).to_le_bytes());
    out.extend_from_slice(&avg_dl.to_le_bytes());
    out.extend_from_slice(&(u32::try_from(dl_bytes.len())?).to_le_bytes());
    out.extend_from_slice(&(u32::try_from(dir.len())?).to_le_bytes());
    out.extend_from_slice(&(u32::try_from(names.len())?).to_le_bytes());
    out.extend_from_slice(&dl_bytes);
    out.extend_from_slice(&dir);
    out.extend_from_slice(&names);
    out.extend_from_slice(&posts);
    Ok((out, avg_dl))
}

#[derive(Default)]
struct PostingAcc {
    tf: u16,
    mask: u16,
}

fn add_posting(
    inv: &mut BTreeMap<String, BTreeMap<u32, PostingAcc>>,
    dl: &mut u32,
    row: u32,
    text: &str,
    mask: u16,
) {
    for token in tokenize_stream(text) {
        *dl = dl.saturating_add(1);
        let acc = inv.entry(token).or_default().entry(row).or_default();
        acc.tf = acc.tf.saturating_add(1);
        acc.mask |= mask;
    }
}

fn push_field(table: &mut Vec<u8>, blob: &mut Vec<u8>, value: &str) -> Result<()> {
    let off = u32::try_from(blob.len()).context("wiki blob too large")?;
    let len = u32::try_from(value.len()).context("wiki field too large")?;
    table.extend_from_slice(&off.to_le_bytes());
    table.extend_from_slice(&len.to_le_bytes());
    blob.extend_from_slice(value.as_bytes());
    Ok(())
}

fn write_manifest(dir: &Path, repo_root: &Path, n: usize, identity: &SourceIdentity) -> Result<()> {
    let payload = json!({
        "version": VERSION,
        "repo_root": repo_root.display().to_string(),
        "articles": n,
        "source_count": identity.count,
        "max_source_mtime": identity.max_mtime,
        "sections": n,
        "format": "LEIOKB01",
    });
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&payload)?,
    )?;
    Ok(())
}

fn open_sidecar(path: &Path) -> Result<WikiIndex> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    // Safety: read-only map of an atomically published sidecar.
    let mapped = unsafe { Mmap::map(&file) }.with_context(|| format!("mmap {}", path.display()))?;
    let bytes = Bytes::from_owner(mapped);
    if bytes.len() < HEADER_LEN {
        bail!("knowledge sidecar truncated");
    }
    if &bytes[..8] != MAGIC {
        bail!("knowledge sidecar magic mismatch");
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into()?);
    if version != VERSION {
        bail!("knowledge sidecar version {version} unsupported");
    }
    let n = u32::from_le_bytes(bytes[12..16].try_into()?) as usize;
    let blob_off = u64::from_le_bytes(bytes[16..24].try_into()?) as usize;
    let blob_len = u64::from_le_bytes(bytes[24..32].try_into()?) as usize;
    let table_end = HEADER_LEN
        .checked_add(n.checked_mul(ROW_WIDTH).context("row table overflow")?)
        .context("table end overflow")?;
    if blob_off != table_end {
        bail!("knowledge sidecar table/blob overlap");
    }
    let end = blob_off
        .checked_add(blob_len)
        .context("blob end overflow")?;
    if end > bytes.len() {
        bail!("knowledge sidecar truncated at blob");
    }
    let source_count =
        u32::from_le_bytes(bytes[HDR_SOURCE_COUNT..HDR_SOURCE_COUNT + 4].try_into()?);
    let max_source_mtime = u64::from_le_bytes(bytes[HDR_MAX_MTIME..HDR_MAX_MTIME + 8].try_into()?);
    let postings_off =
        u64::from_le_bytes(bytes[HDR_POSTINGS_OFF..HDR_POSTINGS_OFF + 8].try_into()?) as usize;
    let postings_len =
        u64::from_le_bytes(bytes[HDR_POSTINGS_LEN..HDR_POSTINGS_LEN + 8].try_into()?) as usize;
    let avg_dl = f32::from_le_bytes(bytes[HDR_AVG_DL..HDR_AVG_DL + 4].try_into()?);
    if postings_off != end {
        bail!("knowledge sidecar postings do not follow blob");
    }
    let postings_end = postings_off
        .checked_add(postings_len)
        .context("postings end overflow")?;
    if postings_end > bytes.len() {
        bail!("knowledge sidecar truncated at postings");
    }
    Ok(WikiIndex {
        bytes,
        n,
        blob_off,
        source_count,
        max_source_mtime,
        postings_off,
        postings_len,
        avg_dl,
    })
}

#[derive(Clone)]
struct Hit {
    title: String,
    body: String,
    topic: String,
    source_path: String,
    kind: String,
    heading_path: String,
    line: u32,
    score: f64,
    stage: &'static str,
}

fn rank_articles(index: &WikiIndex, needle: &str, limit: usize) -> Vec<Hit> {
    let terms = tokens(needle);
    let phrase = needle.trim().to_ascii_lowercase();
    if terms.is_empty() && phrase.is_empty() {
        return Vec::new();
    }
    let n = index.len();
    let candidates = if terms.is_empty() {
        (0..n).collect::<Vec<_>>()
    } else {
        index.posting_rows(&terms)
    };
    let mut df = vec![0u32; terms.len()];
    for (j, term) in terms.iter().enumerate() {
        if let Some((term_df, _)) = index.term_postings(term) {
            df[j] = term_df;
        }
    }
    let mut lowered = Vec::with_capacity(candidates.len());
    for i in candidates {
        let Some(article) = index.article(i) else {
            continue;
        };
        let row = u32::try_from(i).unwrap_or(u32::MAX);
        let fields = LoweredArticle {
            title: article.title.to_ascii_lowercase(),
            topic: article.topic.to_ascii_lowercase(),
            body: article.body.to_ascii_lowercase(),
            path: article.source_path.to_ascii_lowercase(),
            title_raw: article.title,
            body_raw: article.body,
            topic_raw: article.topic,
            path_raw: article.source_path,
            kind: article.kind,
            heading_path: article.heading_path,
            line: article.line,
            row,
        };
        lowered.push(fields);
    }
    let idf: Vec<f64> = df.iter().map(|count| smoothed_idf(n, *count)).collect();
    let min_terms = min_match_terms(terms.len());
    let mut hits = Vec::new();
    for fields in lowered {
        let heading_l = heading_text(fields.body_raw, fields.title_raw).to_ascii_lowercase();
        let title_phrase = !phrase.is_empty() && fields.title.contains(&phrase);
        let heading_phrase = !phrase.is_empty() && heading_l.contains(&phrase);
        let heading_exact = !phrase.is_empty() && heading_l.lines().any(|line| line == phrase);
        let body_phrase = !phrase.is_empty() && fields.body.contains(&phrase);
        let title_exact = !phrase.is_empty() && fields.title == phrase;
        let mut title_hits = 0usize;
        let mut heading_hits = 0usize;
        let mut topic_hits = 0usize;
        let mut path_hits = 0usize;
        let mut body_hits = 0usize;
        let mut score = 0.0;
        if title_exact {
            score += TITLE_EXACT;
        }
        if title_phrase {
            score += TITLE_PHRASE;
        }
        if heading_exact {
            score += HEADING_EXACT;
        }
        if heading_phrase {
            score += HEADING_PHRASE;
        }
        if body_phrase {
            score += BODY_PHRASE;
        }
        let dl = f64::from(index.doc_len(fields.row as usize));
        for (term, weight) in terms.iter().zip(idf.iter()) {
            if let Some((tf, mask)) = index.posting_hit(term, fields.row) {
                score += bm25(
                    f64::from(tf),
                    df_for(term, &terms, &df),
                    n,
                    dl,
                    f64::from(index.avg_dl),
                );
                if mask & MASK_TITLE != 0 {
                    title_hits += 1;
                    score += TITLE_TERM * weight;
                }
                if mask & MASK_HEADING != 0 {
                    heading_hits += 1;
                    score += HEADING_TERM * weight;
                }
                if mask & MASK_TOPIC != 0 {
                    topic_hits += 1;
                    score += TOPIC_TERM * weight;
                }
                if mask & MASK_PATH != 0 {
                    path_hits += 1;
                    score += PATH_TERM * weight;
                }
                if mask & MASK_BODY != 0 {
                    body_hits += 1;
                    score += BODY_TERM * weight;
                }
            } else {
                if fields.title.contains(term) {
                    title_hits += 1;
                    score += TITLE_TERM * weight;
                }
                if heading_l.contains(term) {
                    heading_hits += 1;
                    score += HEADING_TERM * weight;
                }
                if fields.topic.contains(term) {
                    topic_hits += 1;
                    score += TOPIC_TERM * weight;
                }
                if fields.path.contains(term) {
                    path_hits += 1;
                    score += PATH_TERM * weight;
                }
                if fields.body.contains(term) {
                    body_hits += 1;
                    score += BODY_TERM * weight;
                }
            }
        }
        let structural = title_hits + heading_hits + topic_hits + path_hits;
        let eligible = title_exact
            || title_phrase
            || heading_exact
            || heading_phrase
            || body_phrase
            || title_hits > 0
            || heading_hits > 0
            || (structural > 0 && structural >= min_terms)
            || (structural == 0 && terms.len() >= MIN_BODY_TERMS && body_hits >= MIN_BODY_TERMS);
        if !eligible || score <= 0.0 {
            continue;
        }
        let strong_heading = heading_exact || heading_phrase || title_exact || title_phrase;
        if is_generic_article(&fields.title, &fields.path) && !strong_heading {
            score *= GENERIC_SCALE;
        }
        // Heading hits are title-grade evidence, same as an exact title match.
        let stage = if title_exact
            || title_phrase
            || heading_exact
            || heading_phrase
            || title_hits > 0
            || heading_hits > 0
        {
            "title"
        } else if topic_hits > 0 {
            "topic"
        } else {
            "text"
        };
        hits.push(Hit {
            title: fields.title_raw.to_string(),
            body: match_snippet(fields.body_raw, &phrase, &terms),
            topic: fields.topic_raw.to_string(),
            source_path: fields.path_raw.to_string(),
            kind: fields.kind.to_string(),
            heading_path: fields.heading_path.to_string(),
            line: fields.line,
            score,
            stage,
        });
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.cmp(&b.title))
    });
    collapse_siblings(hits, limit)
}

struct LoweredArticle<'a> {
    title: String,
    topic: String,
    body: String,
    path: String,
    title_raw: &'a str,
    body_raw: &'a str,
    topic_raw: &'a str,
    path_raw: &'a str,
    kind: &'a str,
    heading_path: &'a str,
    line: u32,
    row: u32,
}

fn rank_and_maybe_embed(
    index: &WikiIndex,
    repo_root: &Path,
    needle: &str,
    limit: usize,
) -> Vec<Hit> {
    let mut hits = rank_articles(index, needle, limit);
    maybe_embed_rerank(repo_root, needle, &mut hits);
    hits
}

fn rank_adaptive(
    index: &WikiIndex,
    repo_root: &Path,
    needle: &str,
    limit: usize,
) -> (&'static str, Vec<Hit>) {
    let hits = rank_and_maybe_embed(index, repo_root, needle, limit.saturating_mul(4).max(8));
    let title: Vec<Hit> = hits
        .iter()
        .filter(|h| h.stage == "title")
        .cloned()
        .collect();
    if !title.is_empty() {
        return ("title", truncate_hits(title, limit));
    }
    let topic: Vec<Hit> = hits
        .iter()
        .filter(|h| h.stage == "topic")
        .cloned()
        .collect();
    if !topic.is_empty() {
        return ("topic", truncate_hits(topic, limit));
    }
    ("text", truncate_hits(hits, limit))
}

fn truncate_hits(mut hits: Vec<Hit>, limit: usize) -> Vec<Hit> {
    hits.truncate(limit.max(1));
    hits
}

fn tokens(needle: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in needle.split(|c: char| !c.is_ascii_alphanumeric()) {
        if raw.len() < 3 {
            continue;
        }
        let term = raw.to_ascii_lowercase();
        if STOPWORDS.contains(&term.as_str()) {
            continue;
        }
        if !out.iter().any(|existing| existing == &term) {
            out.push(term);
        }
    }
    out
}

fn min_match_terms(term_count: usize) -> usize {
    match term_count {
        0 | 1 => 1,
        2 | 3 => 2,
        _ => 3,
    }
}

fn smoothed_idf(n: usize, df: u32) -> f64 {
    // Rare title terms stay > 1; words in every article collapse toward 1.
    ((n as f64 + 1.0) / (f64::from(df) + 1.0)).ln() + 1.0
}

fn df_for(term: &str, terms: &[String], df: &[u32]) -> u32 {
    terms
        .iter()
        .position(|item| item == term)
        .and_then(|i| df.get(i).copied())
        .unwrap_or(1)
}

fn bm25(tf: f64, df: u32, n: usize, dl: f64, avg_dl: f64) -> f64 {
    if tf <= 0.0 || n == 0 {
        return 0.0;
    }
    let idf = ((n as f64 - f64::from(df) + 0.5) / (f64::from(df) + 0.5) + 1.0).ln();
    let denom = tf + BM25_K1 * (1.0 - BM25_B + BM25_B * dl / avg_dl.max(1.0));
    idf * (tf * (BM25_K1 + 1.0)) / denom.max(f64::EPSILON)
}

fn collapse_siblings(hits: Vec<Hit>, limit: usize) -> Vec<Hit> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = Vec::new();
    for hit in hits {
        let count = seen.entry(hit.source_path.clone()).or_insert(0);
        if *count >= MAX_SECTIONS_PER_FILE {
            continue;
        }
        *count += 1;
        out.push(hit);
        if out.len() >= limit.max(1) {
            break;
        }
    }
    out
}

fn maybe_embed_rerank(repo_root: &Path, needle: &str, hits: &mut [Hit]) {
    if hits.is_empty() || crate::config::embed_query_url().is_none() {
        return;
    }
    let Ok(query) = crate::embed::embed_query(repo_root, needle) else {
        return;
    };
    let texts = hits
        .iter()
        .map(|hit| format!("{}\n{}\n{}", hit.title, hit.heading_path, hit.body))
        .collect::<Vec<_>>();
    let Ok(vectors) = crate::embed::embed_texts(repo_root, &texts) else {
        return;
    };
    for (hit, vector) in hits.iter_mut().zip(vectors) {
        hit.score += EMBED_WEIGHT * f64::from(cosine(&query, &vector));
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.cmp(&b.title))
    });
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut left_norm = 0.0f32;
    let mut right_norm = 0.0f32;
    for (a, b) in left.iter().zip(right.iter()) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    let denom = left_norm.sqrt() * right_norm.sqrt();
    if denom <= f32::EPSILON {
        0.0
    } else {
        dot / denom
    }
}

fn heading_text(body: &str, title: &str) -> String {
    let title_l = title.to_ascii_lowercase();
    let mut out = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        let rest = trimmed
            .strip_prefix("### ")
            .or_else(|| trimmed.strip_prefix("## "))
            .or_else(|| trimmed.strip_prefix("# "));
        let Some(rest) = rest else {
            continue;
        };
        let heading = rest.trim();
        if heading.is_empty() || heading.to_ascii_lowercase() == title_l {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(heading);
    }
    out
}

fn is_generic_article(title: &str, _path: &str) -> bool {
    matches!(
        title,
        "readme"
            | "changelog"
            | "license"
            | "gemini"
            | "agents"
            | "claude"
            | "contributing"
            | "index"
            | "overview"
            | "leio code"
    )
}

fn match_snippet(body: &str, phrase: &str, terms: &[String]) -> String {
    let lower = body.to_ascii_lowercase();
    let found = if !phrase.is_empty() {
        lower.find(phrase)
    } else {
        None
    }
    .or_else(|| terms.iter().find_map(|term| lower.find(term.as_str())));
    let Some(pos) = found else {
        return body.chars().take(200).collect();
    };
    let line_start = body
        .get(..pos)
        .and_then(|prefix| prefix.rfind('\n').map(|i| i + 1))
        .unwrap_or(0);
    let at_heading = body
        .get(line_start..)
        .is_some_and(|rest| rest.starts_with('#'));
    let start = if at_heading {
        line_start
    } else {
        body.floor_char_boundary(pos.saturating_sub(SNIPPET_RADIUS))
    };
    let raw_end = pos
        .saturating_add(phrase.len().max(8))
        .saturating_add(SNIPPET_RADIUS)
        .min(body.len());
    let end = body.ceil_char_boundary(raw_end);
    let mut out = String::new();
    if start > 0 && !at_heading {
        out.push('…');
    }
    out.push_str(body[start..end].trim());
    if end < body.len() {
        out.push('…');
    }
    out
}

fn hits_to_envelope(
    query_prefix: &str,
    summary: &str,
    hits: &[Hit],
    started: Instant,
    strategy: &str,
) -> QueryEnvelope {
    let entities = hits
        .iter()
        .map(|hit| {
            json!({
                "id": format!("{}#{}", hit.source_path, hit.line),
                "kind": hit.kind,
                "title": hit.title,
                "body": hit.body,
                "snippet": hit.body,
                "topic": hit.topic,
                "source_path": hit.source_path,
                "heading_path": hit.heading_path,
                "line": hit.line,
                "_search": { "score": hit.score, "stage": hit.stage }
            })
        })
        .collect::<Vec<_>>();
    let evidence = hits
        .iter()
        .map(|hit| EvidenceItem {
            kind: hit.kind.clone(),
            path: hit.source_path.clone(),
            line: (hit.line > 0).then_some(hit.line as usize),
            detail: format!(
                "{} {} [{}] [stage:{}] [score:{:.3}] {}",
                hit.kind, hit.title, hit.heading_path, hit.stage, hit.score, hit.body
            ),
        })
        .collect();
    QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: format!(
            "{query_prefix}-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: query_prefix.to_string(),
        summary: summary.to_string(),
        confidence: if hits.is_empty() { 0.2 } else { 0.82 },
        entities,
        evidence,
        warnings: vec![format!("search_strategy: {strategy}")],
        meta: Some(json!({
            "transport": "local_arrow",
            "knowledge_base": { "mode": "local_compiled_wiki" },
            "search": { "strategy": strategy, "ranker": "bm25_postings" }
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn status_from_articles(
    query_prefix: &str,
    summary: &str,
    articles: &[impl ArticleLike],
    started: Instant,
    strategy: &str,
) -> QueryEnvelope {
    let mut by_topic: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for article in articles {
        *by_topic.entry(article.topic().to_string()).or_insert(0) += 1;
        *by_kind.entry(article.kind().to_string()).or_insert(0) += 1;
    }
    QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: format!(
            "{query_prefix}-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: query_prefix.to_string(),
        summary: summary.to_string(),
        confidence: 1.0,
        entities: vec![json!({
            "articles": articles.len(),
            "by_topic": by_topic,
            "by_kind": by_kind,
        })],
        evidence: Vec::new(),
        warnings: Vec::new(),
        meta: Some(json!({
            "transport": "local_arrow",
            "knowledge_base": { "mode": "local_compiled_wiki" },
            "search": { "strategy": strategy }
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

trait ArticleLike {
    fn topic(&self) -> &str;
    fn kind(&self) -> &str;
}

impl ArticleLike for OwnedArticle {
    fn topic(&self) -> &str {
        &self.topic
    }
    fn kind(&self) -> &str {
        &self.kind
    }
}

impl ArticleLike for Article<'_> {
    fn topic(&self) -> &str {
        self.topic
    }
    fn kind(&self) -> &str {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arrow_ipc::read_ipc_stream_path;
    use std::time::SystemTime;

    fn temp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "leio-kb-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(dir.join("docs")).unwrap();
        fs::create_dir_all(dir.join("fixtures")).unwrap();
        fs::write(
            dir.join("README.md"),
            "# LEIO Code\n\nAgent-first compiled markdown wiki notes for operators.\n",
        )
        .unwrap();
        fs::write(
            dir.join("GEMINI.md"),
            "# Gemini\n\nThis page mentions a compiled markdown wiki only as an aside.\n",
        )
        .unwrap();
        fs::write(
            dir.join("CHANGELOG.md"),
            "# Changelog\n\nknowledge wiki should not be indexed from changelog.\n",
        )
        .unwrap();
        fs::write(
            dir.join("fixtures/noise.md"),
            "# Fixture only\n\nzzzxxyy fixtureonly phrase.\n",
        )
        .unwrap();
        fs::write(
            dir.join("docs/knowledge.md"),
            "# Knowledge wiki\n\nCompiled markdown wiki articles for adaptive search.\n",
        )
        .unwrap();
        fs::write(
            dir.join("docs/frontmatter.md"),
            "---\ntitle: Local wiki ranking\n---\n\n# Ignored heading\n\nYAML title wins.\n",
        )
        .unwrap();
        dir
    }

    fn first_title(envelope: &QueryEnvelope) -> Option<&str> {
        envelope
            .entities
            .first()
            .and_then(|row| row.get("title"))?
            .as_str()
    }

    #[test]
    fn compile_and_search_recovers_heading() {
        let repo = temp_repo();
        let compiled = compile_knowledge(&repo).unwrap();
        assert!(
            compiled.summary.contains("wiki sections from 4 files"),
            "{}",
            compiled.summary
        );
        assert_eq!(compiled.entities[0]["files"], 4);
        assert!(compiled.entities[0]["sections"].as_u64().unwrap() >= 4);
        let hits = search_local(&repo, "knowledge wiki", 5).unwrap();
        assert_eq!(
            first_title(&hits),
            Some("Knowledge wiki"),
            "{:?}",
            hits.entities
        );
        assert_eq!(
            hits.entities[0].get("source_path").and_then(|v| v.as_str()),
            Some("docs/knowledge.md"),
            "{:?}",
            hits.entities
        );
        let snippet = hits.entities[0]
            .get("snippet")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            snippet
                .to_ascii_lowercase()
                .contains("compiled markdown wiki"),
            "{snippet}"
        );
        let status = status_local(&repo).unwrap();
        assert_eq!(status.entities[0]["files"], 4);
        assert!(status.summary.contains("formal"), "{}", status.summary);
        assert!(status.meta.as_ref().unwrap().get("formal").is_some());
        assert!(status.entities[0].get("formal_triples").is_some());
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn phrase_query_outranks_generic_readme() {
        let repo = temp_repo();
        compile_knowledge(&repo).unwrap();
        let hits = search_local(&repo, "compiled markdown wiki", 5).unwrap();
        assert_eq!(
            first_title(&hits),
            Some("Knowledge wiki"),
            "{:?}",
            hits.entities
        );
        let titles: Vec<&str> = hits
            .entities
            .iter()
            .filter_map(|row| row.get("title").and_then(|v| v.as_str()))
            .collect();
        assert!(!titles.contains(&"Changelog"), "{titles:?}");
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn yaml_frontmatter_title_and_adaptive_stage() {
        let repo = temp_repo();
        compile_knowledge(&repo).unwrap();
        let hits = search_local_adaptive(&repo, "local wiki ranking", 5).unwrap();
        assert_eq!(
            first_title(&hits),
            Some("Local wiki ranking"),
            "{:?}",
            hits.entities
        );
        assert!(
            hits.warnings.iter().any(|w| w.contains("title")),
            "{:?}",
            hits.warnings
        );
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn section_heading_ranks_generic_readme() {
        let repo = temp_repo();
        fs::write(
            repo.join("README.md"),
            "# LEIO Code\n\n## Knowledge wiki\n\nLocal Arrow articles live here.\n",
        )
        .unwrap();
        fs::remove_file(repo.join("docs/knowledge.md")).unwrap();
        compile_knowledge(&repo).unwrap();
        let hits = search_local_adaptive(&repo, "knowledge wiki", 5).unwrap();
        assert_eq!(
            first_title(&hits),
            Some("Knowledge wiki"),
            "{:?}",
            hits.entities
        );
        assert_eq!(
            hits.entities[0]
                .get("heading_path")
                .and_then(|v| v.as_str()),
            Some("LEIO Code > Knowledge wiki"),
            "{:?}",
            hits.entities
        );
        assert_eq!(
            hits.entities[0].get("source_path").and_then(|v| v.as_str()),
            Some("README.md"),
            "{:?}",
            hits.entities
        );
        assert!(
            hits.warnings.iter().any(|w| w.contains("title")),
            "{:?}",
            hits.warnings
        );
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn skips_fixtures_and_rebuilds_when_sources_change() {
        let repo = temp_repo();
        compile_knowledge(&repo).unwrap();
        let missed = search_local(&repo, "zzzxxyy fixtureonly", 5).unwrap();
        assert!(missed.entities.is_empty(), "{:?}", missed.entities);
        fs::write(
            repo.join("docs/late.md"),
            "# Late article\n\nbrand-new wiki page.\n",
        )
        .unwrap();
        let hits = search_local(&repo, "late article", 5).unwrap();
        assert_eq!(
            first_title(&hits),
            Some("Late article"),
            "{:?}",
            hits.entities
        );
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn snippet_starts_at_matching_heading() {
        let prefix = "intro paragraph that would otherwise fill the snippet window. ".repeat(8);
        let body = format!("{prefix}\n## Knowledge wiki\n\nLocal Arrow articles live here.\n");
        let snippet = match_snippet(
            &body,
            "knowledge wiki",
            &["knowledge".to_string(), "wiki".to_string()],
        );
        assert!(snippet.starts_with("## Knowledge wiki"), "{snippet}");
        assert!(snippet.contains("Local Arrow"), "{snippet}");
    }

    #[test]
    fn sidecar_rejects_bad_magic() {
        let dir = std::env::temp_dir().join(format!("leio-kb-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(sidecar_path(&dir), b"NOTAKB01").unwrap();
        assert!(open_sidecar(&sidecar_path(&dir)).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn decode_arrow_round_trip_counts() {
        let repo = temp_repo();
        compile_knowledge(&repo).unwrap();
        let batches = read_ipc_stream_path(&arrow_path(&default_knowledge_dir(&repo))).unwrap();
        assert!(batches[0].num_rows() >= 4);
        assert!(
            batches[0]
                .schema()
                .fields()
                .iter()
                .any(|field| field.name() == "heading_path")
        );
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn incremental_compile_reuses_unchanged_files() {
        let repo = temp_repo();
        let first = compile_knowledge(&repo).unwrap();
        assert_eq!(first.meta.as_ref().unwrap()["compile"]["parsed_files"], 4);
        assert_eq!(first.meta.as_ref().unwrap()["compile"]["reused_files"], 0);
        let second = compile_knowledge(&repo).unwrap();
        assert_eq!(second.meta.as_ref().unwrap()["compile"]["parsed_files"], 0);
        assert_eq!(second.meta.as_ref().unwrap()["compile"]["reused_files"], 4);
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn split_sections_keeps_heading_stack() {
        let sections = split_sections(
            "# Root\n\nintro\n\n## Child\n\nbody\n",
            "docs/page.md",
            "markdown",
        );
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "Root");
        assert_eq!(sections[0].kind, "markdown");
        assert_eq!(sections[1].title, "Child");
        assert_eq!(sections[1].heading_path, "Root > Child");
        assert_eq!(
            heading_segments(&sections[1].heading_path),
            ["Root", "Child"]
        );
        assert_eq!(sections[1].kind, "markdown-section");
        assert!(sections[1].body.contains("body"));
        assert_eq!(sections[1].topic, "docs");
    }

    #[test]
    fn postings_retrieve_title_section() {
        let repo = temp_repo();
        compile_knowledge(&repo).unwrap();
        let index = open_sidecar(&sidecar_path(&default_knowledge_dir(&repo))).unwrap();
        assert!(index.postings_len > 0);
        let rows = index.posting_rows(&["knowledge".into(), "wiki".into()]);
        let titles: Vec<&str> = rows
            .iter()
            .filter_map(|row| index.article(*row).map(|a| a.title))
            .collect();
        assert!(titles.contains(&"Knowledge wiki"), "{titles:?}");
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn collapse_siblings_keeps_two_per_file() {
        let hits = (0..5)
            .map(|i| Hit {
                title: format!("S{i}"),
                body: String::new(),
                topic: "docs".into(),
                source_path: "README.md".into(),
                kind: "markdown-section".into(),
                heading_path: format!("S{i}"),
                line: i,
                score: 10.0 - f64::from(i),
                stage: "text",
            })
            .collect();
        let kept = collapse_siblings(hits, 10);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].title, "S0");
        assert_eq!(kept[1].title, "S1");
    }

    #[test]
    fn split_sections_ignores_fenced_headings() {
        let sections = split_sections(
            "# Real\n\n```bash\n# Fake heading\nexit 0\n```\n\n## Child\n\nok\n",
            "docs/page.md",
            "markdown",
        );
        assert_eq!(
            sections
                .iter()
                .map(|s| s.title.as_str())
                .collect::<Vec<_>>(),
            ["Real", "Child"]
        );
        assert!(!sections.iter().any(|s| s.title.contains("Fake")));
    }
}
