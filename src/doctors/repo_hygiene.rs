//! Repository hygiene doctor.
//!
//! Catches committed noise that no compiler or test will ever flag, scoped
//! tightly to high-signal / near-zero-false-positive patterns over
//! git-tracked files only (so we never nag about local build output):
//!
//! 1. **Merge-conflict leftovers**: tracked `*.orig` / `*.rej` files — the
//!    residue of a `git merge`/`git rebase` conflict that got committed by
//!    accident.
//! 2. **Tracked-but-ignore-worthy noise**: tracked paths matching the noise
//!    patterns we just added to the root `.gitignore` (build-artifacts,
//!    coverage data, notebook checkpoints, editor swap/tmp/bak files). A
//!    tracked match means the file slipped in before the ignore rule existed
//!    — i.e. real drift.
//! 3. **Byte-identical large duplicates**: among tracked files larger than
//!    5 MB, any size-group with more than one file that is byte-for-byte
//!    identical is reported once as a dedup candidate. We only hash/compare
//!    files over the threshold (there are ~16 in this repo), so the check
//!    stays cheap.
//!
//! Deliberately NOT checked: a generic "this file is large" rule. Legitimate
//! large reference data (ontologies, model wheels) must not be nagged
//! forever; only exact duplicates of such files are actionable.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{git_tracked_files, query_id};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Files larger than this are eligible for byte-identical duplicate detection.
const LARGE_FILE_THRESHOLD_BYTES: u64 = 5 * 1024 * 1024;

/// Byte-identical large-file sets that are intentionally kept duplicated, each
/// with its justification. The duplicate check skips a cluster when all its
/// members are covered by one of these sets, so a deliberate, reasoned
/// duplication doesn't nag on every audit — while still being counted in `meta`
/// for transparency. Add an entry ONLY with a real reason: an unexplained large
/// duplicate is exactly the drift this check exists to surface.
const ACCEPTED_DUPLICATES: &[&[&str]] = &[
    // The pacto OpenAPI spec is consumed by two independent surfaces: the runtime
    // cartridge (`cartridges/pacto/router.py`) and the self-contained
    // `skills/pacto-api` skill bundle, which must stay portable. Deduping would
    // couple the skill to runtime layout, so the copies are kept on purpose.
    &[
        "cartridges/pacto/pacto_vendas_openapi_3_1.yaml",
        "skills/pacto-api/pacto_vendas_openapi_3_1.yaml",
    ],
];

/// True when every member of `cluster` is named in one accepted-duplicate set.
fn is_accepted_duplicate(cluster: &[String]) -> bool {
    ACCEPTED_DUPLICATES.iter().any(|accepted| {
        cluster
            .iter()
            .all(|member| accepted.contains(&member.as_str()))
    })
}

pub struct RepoHygieneDoctor;

impl Doctor for RepoHygieneDoctor {
    fn name(&self) -> &'static str {
        "repo-hygiene"
    }

    fn description(&self) -> &'static str {
        "Flags committed repository noise over git-tracked files only: merge-conflict leftovers (*.orig/*.rej), tracked-but-ignore-worthy artifacts (build-artifacts/, coverage data, notebook checkpoints, *.swp/*.tmp/*.bak), and byte-identical large (>5MB) duplicate files. Near-zero false positives; intentionally excludes a generic 'file is large' check so legitimate reference data is never nagged."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_repo_hygiene(root)
    }
}

pub fn doctor_repo_hygiene(root: &Path) -> QueryEnvelope {
    let started = Instant::now();

    let tracked = match git_tracked_files(root) {
        Some(files) => files,
        None => {
            // git unavailable (not a repo, no git binary, command failed). We
            // cannot enumerate committed files, so report a low-confidence
            // envelope with a warning rather than panicking.
            return QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.to_string(),
                query_id: query_id("doctor_repo_hygiene"),
                kind: "doctor".to_string(),
                summary: "repo-hygiene: could not enumerate git-tracked files (git unavailable or not a repository); skipped".to_string(),
                confidence: 0.2,
                entities: Vec::new(),
                evidence: Vec::new(),
                warnings: vec![
                    "repo-hygiene: `git ls-files` did not run at the repo root; cannot audit tracked files".to_string(),
                ],
                meta: Some(json!({ "reason": "git_unavailable" })),
                timing_ms: started.elapsed().as_millis(),
            };
        }
    };

    audit_tracked_files(root, &tracked, started)
}

/// Core auditing logic, separated from git enumeration so tests can inject a
/// deterministic file list without shelling out.
fn audit_tracked_files(root: &Path, tracked: &[String], started: Instant) -> QueryEnvelope {
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let mut conflict_count = 0usize;
    let mut noise_count = 0usize;
    let mut duplicate_set_count = 0usize;
    let mut accepted_duplicate_set_count = 0usize;

    // ---- Checks 1 & 2: per-file pattern matches ----
    for rel in tracked {
        if is_conflict_leftover(rel) {
            conflict_count += 1;
            warnings.push(format!(
                "{rel} is a committed merge-conflict leftover (*.orig/*.rej); delete it"
            ));
            entities.push(json!({
                "doctor": "repo-hygiene",
                "category": "conflict_leftover",
                "file": rel,
            }));
            evidence.push(EvidenceItem {
                kind: "conflict_leftover".to_string(),
                path: rel.clone(),
                line: None,
                detail: "tracked merge-conflict residue (*.orig/*.rej); remove it".to_string(),
            });
            continue;
        }

        if let Some(reason) = ignore_worthy_noise_reason(rel) {
            noise_count += 1;
            warnings.push(format!(
                "{rel} is tracked but ignore-worthy ({reason}); untrack it (`git rm --cached`) — it matches a root .gitignore noise rule"
            ));
            entities.push(json!({
                "doctor": "repo-hygiene",
                "category": "tracked_noise",
                "file": rel,
                "reason": reason,
            }));
            evidence.push(EvidenceItem {
                kind: "tracked_noise".to_string(),
                path: rel.clone(),
                line: None,
                detail: format!("tracked file matches ignore-worthy pattern: {reason}"),
            });
        }
    }

    // ---- Check 3: byte-identical large duplicates ----
    // Group tracked files >5MB by exact size, then byte-compare within each
    // size group. Only files over the threshold are stat'd/hashed so the check
    // stays cheap.
    let mut by_size: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    for rel in tracked {
        let abs = root.join(rel);
        if let Ok(meta) = std::fs::metadata(&abs)
            && meta.is_file()
            && meta.len() > LARGE_FILE_THRESHOLD_BYTES
        {
            by_size.entry(meta.len()).or_default().push(rel.clone());
        }
    }

    for (size, candidates) in &by_size {
        if candidates.len() < 2 {
            continue;
        }
        // Within a same-size group, cluster by identical content. We compare
        // each file against the representative of an existing cluster.
        let mut clusters: Vec<Vec<String>> = Vec::new();
        for rel in candidates {
            let mut placed = false;
            for cluster in clusters.iter_mut() {
                let representative = &cluster[0];
                if files_identical(&root.join(representative), &root.join(rel)) {
                    cluster.push(rel.clone());
                    placed = true;
                    break;
                }
            }
            if !placed {
                clusters.push(vec![rel.clone()]);
            }
        }

        for cluster in clusters {
            if cluster.len() < 2 {
                continue;
            }
            if is_accepted_duplicate(&cluster) {
                // Knowingly-duplicated set with a documented justification —
                // counted for transparency but not warned (keeps audit --strict
                // green without hiding the fact that a dup exists).
                accepted_duplicate_set_count += 1;
                continue;
            }
            duplicate_set_count += 1;
            let mut members = cluster.clone();
            members.sort();
            let joined = members.join(", ");
            warnings.push(format!(
                "byte-identical large-file duplicate set ({size} bytes each): {joined} — keep one and dedup the rest"
            ));
            entities.push(json!({
                "doctor": "repo-hygiene",
                "category": "duplicate_large_files",
                "size_bytes": size,
                "files": members,
            }));
            evidence.push(EvidenceItem {
                kind: "duplicate_large_files".to_string(),
                path: members[0].clone(),
                line: None,
                detail: format!(
                    "{} byte-identical files of {size} bytes: {joined}",
                    members.len()
                ),
            });
        }
    }

    let summary = format!(
        "repo-hygiene: {} conflict leftover(s), {} tracked-noise file(s), {} duplicate-large-file set(s)",
        conflict_count, noise_count, duplicate_set_count
    );

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_repo_hygiene"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.78 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "tracked_files_scanned": tracked.len(),
            "conflict_leftovers": conflict_count,
            "tracked_noise": noise_count,
            "duplicate_large_file_sets": duplicate_set_count,
            "accepted_duplicate_large_file_sets": accepted_duplicate_set_count,
            "large_file_threshold_bytes": LARGE_FILE_THRESHOLD_BYTES,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Check 1: tracked merge-conflict residue.
fn is_conflict_leftover(rel: &str) -> bool {
    rel.ends_with(".orig") || rel.ends_with(".rej")
}

/// Check 2: tracked-but-ignore-worthy noise. Returns a human-readable reason
/// when the path matches one of the root `.gitignore` noise rules.
fn ignore_worthy_noise_reason(rel: &str) -> Option<&'static str> {
    let basename = rel.rsplit('/').next().unwrap_or(rel);

    if rel.starts_with("build-artifacts/") || rel.contains("/build-artifacts/") {
        return Some("under build-artifacts/");
    }
    if rel.contains("/.ipynb_checkpoints/") || rel.starts_with(".ipynb_checkpoints/") {
        return Some("under .ipynb_checkpoints/");
    }
    if basename == ".coverage" || basename.starts_with(".coverage.") {
        return Some("coverage data file");
    }
    if basename.ends_with(".swp") {
        return Some("editor swap file (*.swp)");
    }
    if basename.ends_with(".tmp") {
        return Some("temporary file (*.tmp)");
    }
    if basename.ends_with(".bak") {
        return Some("backup file (*.bak)");
    }
    None
}

/// Byte-compare two files. Returns `false` on any read error (treated as
/// "not provably identical" — we never emit a false dedup warning).
fn files_identical(a: &Path, b: &Path) -> bool {
    let (file_a, file_b) = match (std::fs::File::open(a), std::fs::File::open(b)) {
        (Ok(fa), Ok(fb)) => (fa, fb),
        _ => return false,
    };
    let mut reader_a = std::io::BufReader::new(file_a);
    let mut reader_b = std::io::BufReader::new(file_b);
    let mut buf_a = [0u8; 64 * 1024];
    let mut buf_b = [0u8; 64 * 1024];
    loop {
        let n_a = match reader_a.read(&mut buf_a) {
            Ok(n) => n,
            Err(_) => return false,
        };
        let n_b = match reader_b.read(&mut buf_b) {
            Ok(n) => n,
            Err(_) => return false,
        };
        if n_a != n_b {
            return false;
        }
        if n_a == 0 {
            return true;
        }
        if buf_a[..n_a] != buf_b[..n_b] {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-repo-hygiene-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture");
    }

    /// Initialize a git repo and stage every file under `dir`, returning the
    /// tracked-file list the doctor would see. Used to exercise the real
    /// `git ls-files` path end-to-end. Returns `None` if git is unavailable.
    fn git_init_and_add(dir: &Path) -> Option<Vec<String>> {
        let init = Command::new("git").arg("-C").arg(dir).arg("init").output();
        if init.map(|o| !o.status.success()).unwrap_or(true) {
            return None;
        }
        // `git add -A` stages tracked files; ls-files then reports them even
        // without a commit.
        let add = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["add", "-A", "-f"])
            .output();
        if add.map(|o| !o.status.success()).unwrap_or(true) {
            return None;
        }
        git_tracked_files(dir)
    }

    #[test]
    fn flags_tracked_orig_conflict_leftover() {
        let dir = unique_tempdir("orig");
        write_file(&dir.join("src/foo.rs"), b"fn main() {}\n");
        write_file(&dir.join("src/foo.rs.orig"), b"<<<<<<< HEAD\n");

        let tracked = vec!["src/foo.rs".to_string(), "src/foo.rs.orig".to_string()];
        let envelope = audit_tracked_files(&dir, &tracked, Instant::now());

        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("src/foo.rs.orig") && w.contains("merge-conflict")),
            "expected foo.rs.orig flagged as conflict leftover, got: {:?}",
            envelope.warnings
        );
        assert!(
            !envelope.warnings.iter().any(|w| w.contains("src/foo.rs ")),
            "the real source file must not be flagged: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_file_under_build_artifacts() {
        let dir = unique_tempdir("build-artifacts");
        write_file(&dir.join("build-artifacts/report.bin"), b"junk\n");

        let tracked = vec!["build-artifacts/report.bin".to_string()];
        let envelope = audit_tracked_files(&dir, &tracked, Instant::now());

        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("build-artifacts/report.bin") && w.contains("ignore-worthy")),
            "expected build-artifacts file flagged as tracked noise, got: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reports_identical_large_files_as_duplicate_set() {
        let dir = unique_tempdir("dup-large");
        // Two byte-identical files just over the 5MB threshold.
        let big = vec![0x5au8; (LARGE_FILE_THRESHOLD_BYTES as usize) + 1024];
        write_file(&dir.join("data/a.bin"), &big);
        write_file(&dir.join("data/b.bin"), &big);

        let tracked = vec!["data/a.bin".to_string(), "data/b.bin".to_string()];
        let envelope = audit_tracked_files(&dir, &tracked, Instant::now());

        let dup_warnings: Vec<&String> = envelope
            .warnings
            .iter()
            .filter(|w| w.contains("byte-identical large-file duplicate set"))
            .collect();
        assert_eq!(
            dup_warnings.len(),
            1,
            "expected exactly ONE duplicate-set warning naming both files, got: {:?}",
            envelope.warnings
        );
        assert!(
            dup_warnings[0].contains("data/a.bin") && dup_warnings[0].contains("data/b.bin"),
            "duplicate warning must name both files: {dup_warnings:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_repo_produces_zero_warnings() {
        let dir = unique_tempdir("clean");
        write_file(&dir.join("src/main.rs"), b"fn main() {}\n");
        write_file(&dir.join("README.md"), b"# hi\n");

        let tracked = vec!["src/main.rs".to_string(), "README.md".to_string()];
        let envelope = audit_tracked_files(&dir, &tracked, Instant::now());

        assert!(
            envelope.warnings.is_empty(),
            "expected a clean repo to produce zero warnings, got: {:?}",
            envelope.warnings
        );
        assert!(envelope.confidence > 0.9);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_large_file_without_twin_is_not_flagged() {
        let dir = unique_tempdir("single-large");
        let big = vec![0x11u8; (LARGE_FILE_THRESHOLD_BYTES as usize) + 4096];
        write_file(&dir.join("models/weights.bin"), &big);

        let tracked = vec!["models/weights.bin".to_string()];
        let envelope = audit_tracked_files(&dir, &tracked, Instant::now());

        assert!(
            envelope.warnings.is_empty(),
            "a lone large file must NOT be flagged (no generic large-file check): {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_size_but_different_content_is_not_a_duplicate() {
        let dir = unique_tempdir("same-size-diff");
        let mut a = vec![0u8; (LARGE_FILE_THRESHOLD_BYTES as usize) + 100];
        let mut b = a.clone();
        a[0] = 1; // make the two differ while keeping identical length
        b[0] = 2;
        write_file(&dir.join("a.bin"), &a);
        write_file(&dir.join("b.bin"), &b);

        let tracked = vec!["a.bin".to_string(), "b.bin".to_string()];
        let envelope = audit_tracked_files(&dir, &tracked, Instant::now());

        assert!(
            envelope.warnings.is_empty(),
            "same-size but differing content must not be reported as duplicate: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepted_duplicate_pair_is_not_flagged_but_is_counted() {
        let dir = unique_tempdir("accepted-dup");
        // Recreate the documented ACCEPTED_DUPLICATES pacto pair as two identical
        // >5MB files: the doctor must NOT warn (so audit --strict stays green) but
        // must still count the set in meta for transparency.
        let big = vec![0x7eu8; (LARGE_FILE_THRESHOLD_BYTES as usize) + 2048];
        write_file(
            &dir.join("cartridges/pacto/pacto_vendas_openapi_3_1.yaml"),
            &big,
        );
        write_file(
            &dir.join("skills/pacto-api/pacto_vendas_openapi_3_1.yaml"),
            &big,
        );

        let tracked = vec![
            "cartridges/pacto/pacto_vendas_openapi_3_1.yaml".to_string(),
            "skills/pacto-api/pacto_vendas_openapi_3_1.yaml".to_string(),
        ];
        let envelope = audit_tracked_files(&dir, &tracked, Instant::now());

        assert!(
            !envelope
                .warnings
                .iter()
                .any(|w| w.contains("duplicate set")),
            "accepted duplicate pair must not warn: {:?}",
            envelope.warnings
        );
        assert_eq!(
            envelope
                .meta
                .as_ref()
                .and_then(|m| m.get("accepted_duplicate_large_file_sets"))
                .and_then(|v| v.as_u64()),
            Some(1),
            "accepted duplicate must be counted in meta"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// End-to-end check that the real `git ls-files` enumeration path feeds the
    /// audit. Hermetic: a temp repo with one conflict leftover.
    #[test]
    fn git_ls_files_enumeration_flags_conflict_leftover() {
        let dir = unique_tempdir("git-e2e");
        write_file(&dir.join("keep.rs"), b"fn main() {}\n");
        write_file(&dir.join("keep.rs.orig"), b"<<<<<<< HEAD\n");

        match git_init_and_add(&dir) {
            Some(_tracked) => {
                let envelope = doctor_repo_hygiene(&dir);
                assert!(
                    envelope
                        .warnings
                        .iter()
                        .any(|w| w.contains("keep.rs.orig") && w.contains("merge-conflict")),
                    "expected git-enumerated conflict leftover to be flagged, got: {:?}",
                    envelope.warnings
                );
            }
            None => {
                // git not available in this environment; skip silently rather
                // than fail a hermetic unit test.
            }
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
