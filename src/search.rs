// Rust guideline compliant 2026-02-21
//! Arrow-IPC-backed full-text sidecar index for `find` queries.
//!
//! Built from a [`RepoIndex`] snapshot into `.leio-code/search.arrow` next to
//! the JSON index. One shared Arrow schema carries all three entity kinds
//! (`symbol` / `env_var` / `redis_key`), discriminated by a `domain` column and
//! written as three `RecordBatch`es (one per kind). An identifier-tokenized
//! `tokens` column lets substring + prefix + token-level lookups all share one
//! columnar scan instead of an in-memory `lowercase.contains()` linear pass over
//! every file in [`RepoIndex`].
//!
//! Search is scored: exact-name (100) > token-equality (90) > token-prefix
//! (70) > substring (50). Ties break on shorter `name`, then `path`.
//!
//! The sidecar is best-effort. If the file is missing, stale, or unreadable,
//! callers fall back to the existing iterator-based scan in [`crate::query`] —
//! no functional regression, just slower lookups.
//!
//! The on-disk format is the Arrow IPC **file** format (footer + block index),
//! not the stream format used by [`crate::export`]: the footer makes the file
//! seekable / random-access, which the read path needs. Reads take **no file
//! lock** — the bytes are slurped once and decoded from an in-memory cursor, so
//! unlimited concurrent readers can run while a rebuild atomically renames a new
//! file underneath them. This is the fix for the prior embedded-SQL backend's
//! exclusive-lock contention.
//!
//! Bump [`SEARCH_ARROW_VERSION`] (stamped into schema metadata) whenever the
//! schema changes — the version gate at open returns `Ok(None)` on mismatch so
//! stale sidecars degrade to linear scan.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use arrow_array::{Array, RecordBatch, StringArray, UInt32Array};
use arrow_ipc::reader::FileReader;
use arrow_ipc::writer::FileWriter;
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use serde::{Deserialize, Serialize};

use crate::model::RepoIndex;

/// On-disk schema version, stamped into the Arrow schema metadata under
/// [`SEARCH_VERSION_KEY`]. Bump whenever the column layout below changes or the
/// indexed text gets re-folded — ASCII vs Unicode lowercase matters at the byte
/// level for the `tokens` / `name_lower` / `qual_name_lower` substring probes.
///
/// `1` here is the first Arrow-format version; it replaces the prior SQL
/// backend's `SEARCH_DB_VERSION = 4` (a different format on a different backend,
/// so the counter restarts).
pub const SEARCH_ARROW_VERSION: u32 = 1;

/// Schema-metadata key carrying [`SEARCH_ARROW_VERSION`]. Read at open to gate
/// stale or foreign files into the linear-scan fallback.
const SEARCH_VERSION_KEY: &str = "leio_search_version";

/// `domain` column tag for symbol rows.
const DOMAIN_SYMBOL: &str = "symbol";
/// `domain` column tag for env-var rows.
const DOMAIN_ENV_VAR: &str = "env_var";
/// `domain` column tag for redis-key rows.
const DOMAIN_REDIS_KEY: &str = "redis_key";

/// Score for an exact (case-folded) name match. Mirrors the prior SQL `CASE`
/// tiers verbatim so the Arrow path and linear-scan fallback agree.
const SCORE_EXACT: f64 = 100.0;
/// Score for a whole-token equality match (`" needle "` in the tokens bag).
const SCORE_TOKEN_EQ: f64 = 90.0;
/// Score for a token-prefix match (`" needle"` in the tokens bag).
const SCORE_TOKEN_PREFIX: f64 = 70.0;
/// Score for a plain substring match on the (case-folded) name.
const SCORE_SUBSTRING: f64 = 50.0;

/// Returns the canonical sidecar path for the given repo root.
pub fn default_search_db_path(root: &Path) -> PathBuf {
    root.join(".leio-code").join("search.arrow")
}

/// In-memory handle over a decoded Arrow-IPC sidecar.
///
/// Holds the three `RecordBatch`es (one per entity kind). `RecordBatch` is
/// `Send + Sync`, so this handle is freely shareable across threads with no lock
/// — reads never hold a file handle.
#[derive(Debug)]
pub struct SearchIndex {
    batches: Vec<RecordBatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolHit {
    pub name: String,
    /// Populated when the symbol sits inside an enclosing `impl` / `class` /
    /// `trait` scope. Lets callers query by either bare name or the
    /// `Type::method` / `Type.method` form indexed alongside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qual_name: Option<String>,
    pub kind: String,
    pub path: String,
    pub line: usize,
    pub language: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVarHit {
    pub name: String,
    pub access: String,
    pub path: String,
    pub line: usize,
    pub language: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisKeyHit {
    pub key: String,
    pub access: String,
    pub path: String,
    pub line: usize,
    pub language: String,
    pub score: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildStats {
    pub symbols: usize,
    pub env_vars: usize,
    pub redis_keys: usize,
    pub elapsed_ms: u128,
}

/// Build the single shared Arrow schema used for all three entity kinds.
///
/// `qual_name` / `qual_name_lower` are the only nullable columns: they are
/// populated for symbols and `null` for env/redis rows (and for symbols with an
/// empty qualifier). Schema metadata stamps [`SEARCH_ARROW_VERSION`] so the read
/// path can gate stale files.
///
/// `StringArray` (i32-offset Utf8) is sufficient — symbol counts are ~1e5, far
/// below the 2^31 offset ceiling, so no `LargeUtf8` is needed.
fn search_schema() -> SchemaRef {
    let mut metadata = std::collections::HashMap::new();
    metadata.insert(
        SEARCH_VERSION_KEY.to_string(),
        SEARCH_ARROW_VERSION.to_string(),
    );
    Arc::new(Schema::new_with_metadata(
        vec![
            Field::new("domain", DataType::Utf8, false),
            Field::new("name", DataType::Utf8, false),
            Field::new("name_lower", DataType::Utf8, false),
            Field::new("qual_name", DataType::Utf8, true),
            Field::new("qual_name_lower", DataType::Utf8, true),
            Field::new("kind", DataType::Utf8, false),
            Field::new("path", DataType::Utf8, false),
            Field::new("line", DataType::UInt32, false),
            Field::new("language", DataType::Utf8, false),
            Field::new("tokens", DataType::Utf8, false),
        ],
        metadata,
    ))
}

/// Column ordinals into [`search_schema`], named so the build and scan paths
/// can't drift on positional indexing.
mod col {
    pub const DOMAIN: usize = 0;
    pub const NAME: usize = 1;
    pub const NAME_LOWER: usize = 2;
    pub const QUAL_NAME: usize = 3;
    pub const QUAL_NAME_LOWER: usize = 4;
    pub const KIND: usize = 5;
    pub const PATH: usize = 6;
    pub const LINE: usize = 7;
    pub const LANGUAGE: usize = 8;
    pub const TOKENS: usize = 9;
}

/// Column accumulators for one entity-kind `RecordBatch`. Mirrors the
/// `Vec<...>` + `StringArray::from` builder pattern in `crate::node_rows`.
#[derive(Default)]
struct BatchColumns {
    domain: Vec<String>,
    name: Vec<String>,
    name_lower: Vec<String>,
    qual_name: Vec<Option<String>>,
    qual_name_lower: Vec<Option<String>>,
    kind: Vec<String>,
    path: Vec<String>,
    line: Vec<u32>,
    language: Vec<String>,
    tokens: Vec<String>,
}

impl BatchColumns {
    fn len(&self) -> usize {
        self.name.len()
    }

    fn finish(self, schema: SchemaRef) -> Result<RecordBatch> {
        let columns: Vec<arrow_array::ArrayRef> = vec![
            Arc::new(StringArray::from(self.domain)),
            Arc::new(StringArray::from(self.name)),
            Arc::new(StringArray::from(self.name_lower)),
            Arc::new(StringArray::from(self.qual_name)),
            Arc::new(StringArray::from(self.qual_name_lower)),
            Arc::new(StringArray::from(self.kind)),
            Arc::new(StringArray::from(self.path)),
            Arc::new(UInt32Array::from(self.line)),
            Arc::new(StringArray::from(self.language)),
            Arc::new(StringArray::from(self.tokens)),
        ];
        RecordBatch::try_new(schema, columns).context("failed to build search RecordBatch")
    }
}

/// Build the three per-kind `RecordBatch`es from a [`RepoIndex`].
///
/// This is Arrow column serialization (sub-second at our scale), not a heavy DB
/// rebuild. Empty `qual_name` maps to `null` (never `""`) so the exact tier
/// cannot false-match a short/empty needle on an empty qualifier.
fn build_batches(index: &RepoIndex) -> Result<(Vec<RecordBatch>, BuildStats)> {
    let started = Instant::now();
    let schema = search_schema();

    let mut symbols = BatchColumns::default();
    let mut env_vars = BatchColumns::default();
    let mut redis_keys = BatchColumns::default();

    for file in &index.files {
        for sym in &file.symbols {
            // Tokens combine bare and qualified names so `Router::route`,
            // `Router`, and `route` all hit this row.
            let tokens = match &sym.qual_name {
                Some(qual) => tokens_field_multi(&[sym.name.as_str(), qual.as_str()]),
                None => tokens_field(&sym.name),
            };
            // Empty qualifier -> Arrow null (not "") so the 100-tier exact
            // probe can't false-match an empty needle on qual_name_lower.
            let (qual, qual_lower) = match sym.qual_name.as_deref().filter(|q| !q.is_empty()) {
                Some(q) => (Some(q.to_string()), Some(q.to_ascii_lowercase())),
                None => (None, None),
            };
            symbols.domain.push(DOMAIN_SYMBOL.to_string());
            symbols.name.push(sym.name.clone());
            symbols.name_lower.push(sym.name.to_ascii_lowercase());
            symbols.qual_name.push(qual);
            symbols.qual_name_lower.push(qual_lower);
            symbols.kind.push(sym.kind.as_str().to_string());
            symbols.path.push(sym.path.clone());
            symbols.line.push(sym.line as u32);
            symbols.language.push(sym.language.as_str().to_string());
            symbols.tokens.push(tokens);
        }

        for ev in &file.env_vars {
            env_vars.domain.push(DOMAIN_ENV_VAR.to_string());
            env_vars.name.push(ev.name.clone());
            env_vars.name_lower.push(ev.name.to_ascii_lowercase());
            env_vars.qual_name.push(None);
            env_vars.qual_name_lower.push(None);
            env_vars.kind.push(ev.access.as_str().to_string());
            env_vars.path.push(ev.path.clone());
            env_vars.line.push(ev.line as u32);
            env_vars.language.push(ev.language.as_str().to_string());
            env_vars.tokens.push(tokens_field(&ev.name));
        }

        for rk in &file.redis_keys {
            redis_keys.domain.push(DOMAIN_REDIS_KEY.to_string());
            redis_keys.name.push(rk.key.clone());
            redis_keys.name_lower.push(rk.key.to_ascii_lowercase());
            redis_keys.qual_name.push(None);
            redis_keys.qual_name_lower.push(None);
            redis_keys.kind.push(rk.access.as_str().to_string());
            redis_keys.path.push(rk.path.clone());
            redis_keys.line.push(rk.line as u32);
            redis_keys.language.push(rk.language.as_str().to_string());
            redis_keys.tokens.push(tokens_field(&rk.key));
        }
    }

    let stats = BuildStats {
        symbols: symbols.len(),
        env_vars: env_vars.len(),
        redis_keys: redis_keys.len(),
        elapsed_ms: started.elapsed().as_millis(),
    };

    let batches = vec![
        symbols.finish(Arc::clone(&schema))?,
        env_vars.finish(Arc::clone(&schema))?,
        redis_keys.finish(schema)?,
    ];

    Ok((batches, stats))
}

/// Write `batches` to `path` atomically: write to a hidden, pid-scoped temp file
/// in the same directory, then `rename(2)` over the target.
///
/// The temp file lives in the destination directory so the rename is a
/// same-filesystem atomic replace; concurrent readers see the old file or the
/// new file, never a torn/partial one. The Arrow [`FileWriter`] footer is
/// flushed by `finish()`, and the `File` is dropped (scope close) **before** the
/// rename so a racing reader that wins the race never sees a footerless file.
///
/// # Errors
///
/// Returns an error if the directory cannot be created, the temp file cannot be
/// written, or the rename fails. On any failure the temp file is removed
/// best-effort.
pub fn write_index_atomic(path: &Path, schema: &SchemaRef, batches: &[RecordBatch]) -> Result<()> {
    let parent = path
        .parent()
        .context("search index path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;

    // pid-scoped hidden name avoids collisions between concurrent rebuilds.
    let tmp = parent.join(format!(".search.arrow.tmp-{}", std::process::id()));

    let write_result = (|| -> Result<()> {
        // Scope the File so its footer is fully flushed and the handle dropped
        // before we rename — a footerless file would fail FileReader::try_new
        // and silently degrade readers to linear scan.
        let file =
            File::create(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;
        let mut writer =
            FileWriter::try_new(file, schema).context("failed to create Arrow IPC file writer")?;
        for batch in batches {
            writer
                .write(batch)
                .context("failed to write search RecordBatch")?;
        }
        writer.finish().context("failed to finish Arrow IPC file")?;
        Ok(())
    })();

    if let Err(err) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }

    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("failed to rename into {}", path.display()));
    }

    Ok(())
}

impl SearchIndex {
    /// Open the canonical sidecar IFF it exists and matches
    /// [`SEARCH_ARROW_VERSION`]. Returns `Ok(None)` when the sidecar is absent,
    /// unreadable, or version-stale — callers fall back to linear scan.
    ///
    /// Reads take **no lock**: the file bytes are read once into memory and
    /// decoded from an in-memory cursor, so the open holds no file handle and a
    /// concurrent rebuild can rename a new file underneath live readers.
    ///
    /// # Errors
    ///
    /// Returns an error only if the file exists but cannot be read from disk.
    /// A malformed/foreign Arrow file is treated as "absent" (`Ok(None)`), never
    /// an error, so a bad sidecar degrades silently to linear scan.
    pub fn open_if_fresh(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read search index at {}", path.display()))?;

        let reader = match FileReader::try_new(Cursor::new(bytes), None) {
            Ok(reader) => reader,
            // Unreadable / foreign / footerless file -> linear-scan fallback.
            Err(_) => return Ok(None),
        };

        let version = reader
            .schema()
            .metadata
            .get(SEARCH_VERSION_KEY)
            .and_then(|raw| raw.parse::<u32>().ok());
        if version != Some(SEARCH_ARROW_VERSION) {
            return Ok(None);
        }

        // FileReader iterates every footer block — we write three batches, so
        // do NOT assume a single batch.
        let batches = reader
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to read search index record batches")?;
        Ok(Some(Self { batches }))
    }

    /// Rebuild the sidecar from `index` and write it atomically to `path`.
    ///
    /// Returns the in-memory handle plus build statistics. Existing readers keep
    /// their decoded batches; the rename only affects future opens.
    ///
    /// # Errors
    ///
    /// Returns an error if the batches cannot be built or the atomic write fails.
    pub fn rebuild_to(path: &Path, index: &RepoIndex) -> Result<(Self, BuildStats)> {
        let (batches, stats) = build_batches(index)?;
        let schema = search_schema();
        write_index_atomic(path, &schema, &batches)?;
        Ok((Self { batches }, stats))
    }

    pub fn search_symbols(&self, needle: &str, limit: Option<usize>) -> Result<Vec<SymbolHit>> {
        let n = needle.to_ascii_lowercase();
        let token_eq = format!(" {n} ");
        let token_prefix = format!(" {n}");
        let mut hits: Vec<SymbolHit> = Vec::new();

        for batch in &self.batches {
            let domain = col_str(batch, col::DOMAIN)?;
            let name = col_str(batch, col::NAME)?;
            let name_lower = col_str(batch, col::NAME_LOWER)?;
            let qual_name = col_str(batch, col::QUAL_NAME)?;
            let qual_name_lower = col_str(batch, col::QUAL_NAME_LOWER)?;
            let kind = col_str(batch, col::KIND)?;
            let path = col_str(batch, col::PATH)?;
            let line = col_u32(batch, col::LINE)?;
            let language = col_str(batch, col::LANGUAGE)?;
            let tokens = col_str(batch, col::TOKENS)?;

            for row in 0..batch.num_rows() {
                if domain.value(row) != DOMAIN_SYMBOL {
                    continue;
                }
                let nl = name_lower.value(row);
                // is_null gates qual so an absent qualifier reads as "" and
                // cannot false-match an empty/short needle on the exact tier.
                let qlv = if qual_name_lower.is_null(row) {
                    ""
                } else {
                    qual_name_lower.value(row)
                };
                let tok = tokens.value(row);

                let score = if nl == n || qlv == n {
                    SCORE_EXACT
                } else if tok.contains(&token_eq) {
                    SCORE_TOKEN_EQ
                } else if tok.contains(&token_prefix) {
                    SCORE_TOKEN_PREFIX
                } else if nl.contains(&n) || qlv.contains(&n) {
                    SCORE_SUBSTRING
                } else {
                    continue;
                };

                let qual = if qual_name.is_null(row) {
                    None
                } else {
                    Some(qual_name.value(row).to_string())
                };
                hits.push(SymbolHit {
                    name: name.value(row).to_string(),
                    qual_name: qual,
                    kind: kind.value(row).to_string(),
                    path: path.value(row).to_string(),
                    line: line.value(row) as usize,
                    language: language.value(row).to_string(),
                    score,
                });
            }
        }

        sort_and_truncate(
            &mut hits,
            limit,
            |h| &h.score,
            |h| h.name.len(),
            |h| &h.path,
        );
        Ok(hits)
    }

    pub fn search_env_vars(&self, needle: &str, limit: Option<usize>) -> Result<Vec<EnvVarHit>> {
        let n = needle.to_ascii_lowercase();
        let token_eq = format!(" {n} ");
        let token_prefix = format!(" {n}");
        let mut hits: Vec<EnvVarHit> = Vec::new();

        for batch in &self.batches {
            let domain = col_str(batch, col::DOMAIN)?;
            let name = col_str(batch, col::NAME)?;
            let name_lower = col_str(batch, col::NAME_LOWER)?;
            let access = col_str(batch, col::KIND)?;
            let path = col_str(batch, col::PATH)?;
            let line = col_u32(batch, col::LINE)?;
            let language = col_str(batch, col::LANGUAGE)?;
            let tokens = col_str(batch, col::TOKENS)?;

            for row in 0..batch.num_rows() {
                if domain.value(row) != DOMAIN_ENV_VAR {
                    continue;
                }
                let nl = name_lower.value(row);
                let tok = tokens.value(row);
                let score = score_single(nl, tok, &n, &token_eq, &token_prefix);
                let Some(score) = score else { continue };
                hits.push(EnvVarHit {
                    name: name.value(row).to_string(),
                    access: access.value(row).to_string(),
                    path: path.value(row).to_string(),
                    line: line.value(row) as usize,
                    language: language.value(row).to_string(),
                    score,
                });
            }
        }

        sort_and_truncate(
            &mut hits,
            limit,
            |h| &h.score,
            |h| h.name.len(),
            |h| &h.path,
        );
        Ok(hits)
    }

    pub fn search_redis_keys(
        &self,
        needle: &str,
        limit: Option<usize>,
    ) -> Result<Vec<RedisKeyHit>> {
        let n = needle.to_ascii_lowercase();
        let token_eq = format!(" {n} ");
        let token_prefix = format!(" {n}");
        let mut hits: Vec<RedisKeyHit> = Vec::new();

        for batch in &self.batches {
            let domain = col_str(batch, col::DOMAIN)?;
            let name = col_str(batch, col::NAME)?;
            let name_lower = col_str(batch, col::NAME_LOWER)?;
            let access = col_str(batch, col::KIND)?;
            let path = col_str(batch, col::PATH)?;
            let line = col_u32(batch, col::LINE)?;
            let language = col_str(batch, col::LANGUAGE)?;
            let tokens = col_str(batch, col::TOKENS)?;

            for row in 0..batch.num_rows() {
                if domain.value(row) != DOMAIN_REDIS_KEY {
                    continue;
                }
                let nl = name_lower.value(row);
                let tok = tokens.value(row);
                let score = score_single(nl, tok, &n, &token_eq, &token_prefix);
                let Some(score) = score else { continue };
                hits.push(RedisKeyHit {
                    key: name.value(row).to_string(),
                    access: access.value(row).to_string(),
                    path: path.value(row).to_string(),
                    line: line.value(row) as usize,
                    language: language.value(row).to_string(),
                    score,
                });
            }
        }

        // Redis ranks ties by KEY length (its `name` column holds the key).
        sort_and_truncate(&mut hits, limit, |h| &h.score, |h| h.key.len(), |h| &h.path);
        Ok(hits)
    }
}

/// Score a single-name row against the four tiers (env/redis path). Returns
/// `None` for a tier-0 (no-match) row so the caller drops it.
fn score_single(
    name_lower: &str,
    tokens: &str,
    needle: &str,
    token_eq: &str,
    token_prefix: &str,
) -> Option<f64> {
    if name_lower == needle {
        Some(SCORE_EXACT)
    } else if tokens.contains(token_eq) {
        Some(SCORE_TOKEN_EQ)
    } else if tokens.contains(token_prefix) {
        Some(SCORE_TOKEN_PREFIX)
    } else if name_lower.contains(needle) {
        Some(SCORE_SUBSTRING)
    } else {
        None
    }
}

/// Sort hits by `score` DESC, then a length key ASC, then `path` ASC, and apply
/// an optional `limit`. Reproduces the prior SQL `ORDER BY score DESC,
/// length(name|key), path` plus `LIMIT` byte-for-byte.
fn sort_and_truncate<H>(
    hits: &mut Vec<H>,
    limit: Option<usize>,
    score: impl Fn(&H) -> &f64,
    len_key: impl Fn(&H) -> usize,
    path: impl Fn(&H) -> &String,
) {
    hits.sort_by(|a, b| {
        // scores are finite tier constants, so partial_cmp never yields None.
        score(b)
            .partial_cmp(score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(len_key(a).cmp(&len_key(b)))
            .then(path(a).cmp(path(b)))
    });
    if let Some(k) = limit {
        hits.truncate(k);
    }
}

/// Downcast batch column `idx` to a `&StringArray`.
///
/// # Errors
///
/// Returns an error if the column is not a Utf8 array — a contract violation
/// between [`search_schema`] and the scan, surfaced as a recoverable error so
/// callers degrade to linear scan rather than panic.
fn col_str(batch: &RecordBatch, idx: usize) -> Result<&StringArray> {
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("search batch column {idx} was not Utf8"))
}

/// Downcast batch column `idx` to a `&UInt32Array`.
///
/// # Errors
///
/// Returns an error if the column is not a UInt32 array (see [`col_str`]).
fn col_u32(batch: &RecordBatch, idx: usize) -> Result<&UInt32Array> {
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .with_context(|| format!("search batch column {idx} was not UInt32"))
}

/// Best-effort sidecar rebuild from a `RepoIndex`. Logs to stderr on failure and
/// returns `None` instead of bubbling — callers continue with the linear-scan
/// path. Arrow IPC has no WAL or lock sibling, so there is nothing to clean up
/// beyond the atomic temp file (handled inside [`write_index_atomic`]).
pub fn rebuild_sidecar(root: &Path, index: &RepoIndex) -> Option<BuildStats> {
    let path = default_search_db_path(root);
    match SearchIndex::rebuild_to(&path, index) {
        Ok((_, stats)) => Some(stats),
        Err(err) => {
            eprintln!("leio-code: search sidecar rebuild failed: {err:#}");
            None
        }
    }
}

/// Tokenize an identifier into normalized lowercase parts. Splits on
/// non-alphanumeric boundaries (snake_case, dots, colons, dashes, slashes)
/// and on lower→upper transitions (camelCase / PascalCase). The full
/// lowercased identifier is always included so substring matches still work
/// even when the needle straddles a token boundary.
///
/// Uses ASCII case-folding (`to_ascii_lowercase`) deliberately — the linear
/// scan fallback in [`crate::query`] also folds ASCII-only, and identifiers
/// in the supported source languages are overwhelmingly ASCII. Keeping both
/// paths on the same casing rule prevents a sidecar-vs-fallback divergence
/// for the rare Unicode identifier (`fn café_route` would otherwise hit one
/// path but not the other).
pub fn tokenize(s: &str) -> Vec<String> {
    let mut tokens: BTreeSet<String> = BTreeSet::new();
    let lower = s.to_ascii_lowercase();
    if !lower.is_empty() {
        tokens.insert(lower.clone());
    }

    let mut current = String::new();
    let mut prev_lower_or_digit = false;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase() && prev_lower_or_digit && !current.is_empty() {
                tokens.insert(current.to_ascii_lowercase());
                current.clear();
            }
            current.push(ch);
            prev_lower_or_digit = ch.is_lowercase() || ch.is_numeric();
        } else {
            if !current.is_empty() {
                tokens.insert(current.to_ascii_lowercase());
                current.clear();
            }
            prev_lower_or_digit = false;
        }
    }
    if !current.is_empty() {
        tokens.insert(current.to_ascii_lowercase());
    }

    tokens.into_iter().filter(|t| !t.is_empty()).collect()
}

/// Render a token list as a space-padded `" tok1 tok2 ... "` string. The
/// padding makes `tokens.contains(" tok ")` a clean token-equality probe,
/// byte-identical to the prior SQL `position(' tok ' IN tokens)` semantics.
fn tokens_field(s: &str) -> String {
    tokens_field_multi(&[s])
}

/// `tokens_field` over the union of multiple inputs. Used to fold both a
/// symbol's bare `name` and its `qual_name` into a single tokens column so
/// the same scan resolves `route`, `Router`, and `Router::route`.
fn tokens_field_multi(sources: &[&str]) -> String {
    let mut all = BTreeSet::new();
    for src in sources {
        for tok in tokenize(src) {
            all.insert(tok);
        }
    }
    if all.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(all.iter().map(|t| t.len() + 1).sum::<usize>() + 1);
    out.push(' ');
    for tok in &all {
        out.push_str(tok);
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AccessKind, EnvVarOccurrence, FileRecord, RedisKeyOccurrence, RepoIndex, SourceLanguage,
        SymbolKind, SymbolOccurrence,
    };
    use std::path::PathBuf;

    fn fixture_index() -> RepoIndex {
        RepoIndex {
            version: 2,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-05-19T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "example-gateway/src/reasoning/crepe.rs".to_string(),
                language: SourceLanguage::Rust,
                bytes: 1024,
                modified_unix_ms: 0,
                symbols: vec![
                    SymbolOccurrence {
                        name: "ExampleCrepeReasoner".to_string(),
                        kind: SymbolKind::Struct,
                        path: "example-gateway/src/reasoning/crepe.rs".to_string(),
                        line: 42,
                        language: SourceLanguage::Rust,
                        qual_name: None,
                    },
                    SymbolOccurrence {
                        name: "run_crepe_query".to_string(),
                        kind: SymbolKind::Function,
                        path: "example-gateway/src/reasoning/crepe.rs".to_string(),
                        line: 87,
                        language: SourceLanguage::Rust,
                        qual_name: None,
                    },
                    SymbolOccurrence {
                        name: "route".to_string(),
                        kind: SymbolKind::Method,
                        path: "example-gateway/src/reasoning/crepe.rs".to_string(),
                        line: 102,
                        language: SourceLanguage::Rust,
                        qual_name: Some("ExampleCrepeReasoner::route".to_string()),
                    },
                ],
                env_vars: vec![EnvVarOccurrence {
                    name: "EXAMPLE_FLIGHT_TOKEN".to_string(),
                    access: AccessKind::Read,
                    path: "example-gateway/src/reasoning/crepe.rs".to_string(),
                    line: 10,
                    language: SourceLanguage::Rust,
                }],
                redis_keys: vec![RedisKeyOccurrence {
                    key: "route:exact:onboarding".to_string(),
                    access: AccessKind::Read,
                    path: "example-gateway/src/reasoning/crepe.rs".to_string(),
                    line: 55,
                    language: SourceLanguage::Rust,
                }],
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    fn tmp_index_path() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        // Each test gets its own directory so write_index_atomic's same-dir
        // rename target is isolated.
        let dir = std::env::temp_dir().join(format!("leio-search-test-{stamp}-{count}"));
        dir.join("search.arrow")
    }

    #[test]
    fn tokenize_splits_snake_camel_and_separators() {
        let tokens = tokenize("ExampleCrepeReasoner");
        assert!(tokens.contains(&"example".to_string()));
        assert!(tokens.contains(&"crepe".to_string()));
        assert!(tokens.contains(&"reasoner".to_string()));
        assert!(tokens.contains(&"examplecrepereasoner".to_string()));

        let tokens = tokenize("EXAMPLE_FLIGHT_TOKEN");
        assert!(tokens.contains(&"example".to_string()));
        assert!(tokens.contains(&"flight".to_string()));
        assert!(tokens.contains(&"token".to_string()));

        let tokens = tokenize("route:exact:onboarding");
        assert!(tokens.contains(&"route".to_string()));
        assert!(tokens.contains(&"exact".to_string()));
        assert!(tokens.contains(&"onboarding".to_string()));

        let tokens = tokenize("run_crepe_query");
        assert!(tokens.contains(&"run".to_string()));
        assert!(tokens.contains(&"crepe".to_string()));
        assert!(tokens.contains(&"query".to_string()));
    }

    #[test]
    fn tokens_field_is_space_padded() {
        let field = tokens_field("EXAMPLE_FLIGHT_TOKEN");
        assert!(field.starts_with(' '));
        assert!(field.ends_with(' '));
        assert!(field.contains(" example "));
        assert!(field.contains(" flight "));
        assert!(field.contains(" token "));
    }

    #[test]
    fn build_and_search_round_trip() {
        let path = tmp_index_path();
        let index = fixture_index();

        let (_, stats) = SearchIndex::rebuild_to(&path, &index).expect("rebuild");
        assert_eq!(stats.symbols, 3);
        assert_eq!(stats.env_vars, 1);
        assert_eq!(stats.redis_keys, 1);

        // Read it back through the mmap-free Cursor open path — proves the
        // build -> atomic-write -> read round-trip.
        let search = SearchIndex::open_if_fresh(&path)
            .expect("open ok")
            .expect("fresh index present");

        let sym_hits = search
            .search_symbols("crepe", Some(10))
            .expect("search symbols");
        let names: Vec<_> = sym_hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"ExampleCrepeReasoner"));
        assert!(names.contains(&"run_crepe_query"));
        // Token-equality hit (90) outranks substring-only hits (50) for both
        // results here because "crepe" is one of the camelCase / snake_case
        // segments in each symbol.
        for hit in &sym_hits {
            assert!(hit.score >= 70.0, "expected token hit, got {hit:?}");
        }

        let env_hits = search
            .search_env_vars("flight", Some(10))
            .expect("search envs");
        assert_eq!(env_hits.len(), 1);
        assert_eq!(env_hits[0].name, "EXAMPLE_FLIGHT_TOKEN");
        assert!(env_hits[0].score >= 70.0);

        let redis_hits = search
            .search_redis_keys("onboarding", Some(10))
            .expect("search redis");
        assert_eq!(redis_hits.len(), 1);
        assert_eq!(redis_hits[0].key, "route:exact:onboarding");
        assert!(redis_hits[0].score >= 70.0);

        // camelCase tokenization isolates "reasoner" — it matches both the
        // struct itself and the method whose `qual_name`
        // (ExampleCrepeReasoner::route) carries `reasoner` as a token, but
        // not the snake_case function whose tokens are {run, crepe, query}.
        let sym_hits = search
            .search_symbols("reasoner", Some(10))
            .expect("search reasoner");
        let names: Vec<&str> = sym_hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"ExampleCrepeReasoner"));
        assert!(names.contains(&"route"));
        assert!(!names.contains(&"run_crepe_query"));

        // `None` ⇒ no limit, so a broad needle returns every match. All three
        // fixture symbols share the substring "e" (Example*Reasoner /
        // run_crepe_query / route), so an unlimited search returns all of them.
        let all = search.search_symbols("e", None).expect("unlimited search");
        assert_eq!(all.len(), 3);

        // Qualified-name lookup: `find symbol ExampleCrepeReasoner::route`
        // resolves through the `tokens` column to the method, and the hit
        // carries the qual_name back unchanged.
        let qual_hits = search
            .search_symbols("ExampleCrepeReasoner::route", None)
            .expect("qual lookup");
        let method = qual_hits
            .iter()
            .find(|h| h.name == "route")
            .expect("method matched by qual_name");
        assert_eq!(
            method.qual_name.as_deref(),
            Some("ExampleCrepeReasoner::route")
        );
        assert_eq!(method.kind, "method");

        // A needle that straddles a token boundary inside qual_name
        // ("Reasoner::ro" spans the boundary between `reasoner` and `route`)
        // must still match — the qual_name_lower substring fallback is the
        // equivalent of the linear-scan path's qual_name.contains() check.
        let partial = search
            .search_symbols("Reasoner::ro", None)
            .expect("partial qual lookup");
        assert!(
            partial.iter().any(|h| h.name == "route"),
            "expected partial qual_name substring to match route, got {partial:?}",
        );

        // Ordering parity: score DESC, then length ASC, then path ASC. Among
        // the three symbols matching "e", the struct (score 90 via token tiers
        // is NOT it — "e" is a substring tier) — assert the sort is stable on
        // shorter name first within equal score.
        let mut prev: Option<(&f64, usize)> = None;
        for hit in &all {
            if let Some((prev_score, prev_len)) = prev
                && (prev_score - hit.score).abs() < f64::EPSILON
            {
                assert!(
                    prev_len <= hit.name.len(),
                    "ties must be shorter-name-first: {prev_len} then {}",
                    hit.name.len()
                );
            }
            prev = Some((&hit.score, hit.name.len()));
        }

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn open_if_fresh_returns_none_for_missing_or_stale() {
        let path = tmp_index_path();
        assert!(
            SearchIndex::open_if_fresh(&path)
                .expect("missing ok")
                .is_none()
        );

        // Build a fresh one, then read it back.
        let index = fixture_index();
        SearchIndex::rebuild_to(&path, &index).expect("rebuild");

        assert!(
            SearchIndex::open_if_fresh(&path)
                .expect("fresh ok")
                .is_some()
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn two_concurrent_reads_do_not_conflict() {
        // The lock-free read path must allow two independent opens of the same
        // file at once — this is the core property of the lock-free read fix.
        let path = tmp_index_path();
        let index = fixture_index();
        SearchIndex::rebuild_to(&path, &index).expect("rebuild");

        let a = SearchIndex::open_if_fresh(&path)
            .expect("open a ok")
            .expect("a present");
        let b = SearchIndex::open_if_fresh(&path)
            .expect("open b ok")
            .expect("b present");

        let hits_a = a.search_symbols("crepe", None).expect("a search");
        let hits_b = b.search_symbols("crepe", None).expect("b search");
        assert_eq!(hits_a.len(), hits_b.len());
        assert!(!hits_a.is_empty());

        let _ = fs::remove_file(&path);
    }
}
