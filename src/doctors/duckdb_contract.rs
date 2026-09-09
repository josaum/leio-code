// Rust guideline compliant 2026-02-21
use std::env;
use std::fs;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, git_tracked_files, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Tracked `*.duckdb` artifacts allowed to live in the repository.
///
/// Verified against `git ls-files`: exactly one tracked DuckDB file exists
/// today, the S1000D BREX registry. Every other warehouse/registry DuckDB
/// file must stay out of version control (built or synced at deploy time).
const ALLOWED_DUCKDB_FILES: &[&str] = &["cartridges/s1000d_align/ontology/brex_registry.duckdb"];

/// Path prefixes where direct DuckDB usage is sanctioned.
///
/// DuckDB is the embedded durable store of record owned jointly by the gateway
/// crate (whole crate: routing, sessions, CRM, events, agents, ops_console,
/// leases, semantic, ingest, analytics, reasoning, server, api, whatsapp,
/// verticals, plus the storage write queue), the platform crate (sanctioned
/// but currently DuckDB-free after PHASE B), the cartridges (health_audit /
/// assurant / revops / s1000d_align reference and warehouse read-stores, plus
/// the gohosptwin and vorcaro pipelines), the jcube digital-twin and report
/// surfaces, the control-plane `core/duckdb.py` and its `registry.py`
/// consumer, the report-generator skill scripts, and the one-off ops/ETL
/// script tiers. New control-plane or request-path code outside these prefixes
/// must not open DuckDB directly.
const SANCTIONED_PREFIXES: &[&str] = &[
    "example-gateway/",
    "example-platform/",
    "cartridges/",
    "jcube/",
    "example-api/example/core/duckdb.py",
    "example-api/example/cartridges/registry.py",
    "example-api/example/tests/",
    "example-api/scripts/",
    "example-api/sdks/python/example_rag/duckdb_rag.py",
    "scripts/",
    ".claude/skills/",
    ".agents/skills/",
    "tools/ha-semantic-merge/",
];

/// Sanctioned-but-tolerated prefixes flagged as cleanup candidates.
///
/// These are not regressions: they are PHASE-A survivors (the lone DuckDB RAG
/// example) and a dev-only spike (the HA semantic-merge sandbox). They stay
/// green so CI does not fail on pre-existing code, but the audit surfaces them
/// as removal candidates via low-severity evidence rows.
const TOLERATED_PENDING_REMOVAL: &[&str] = &[
    "example-api/sdks/python/example_rag/duckdb_rag.py",
    "tools/ha-semantic-merge/",
];

/// Eradicated targets that must stay free of `duckdb::` after PHASE B.
///
/// The platform crate was migrated off DuckDB in PHASE B and leio-code itself
/// computes over Arrow IPC; any reappearance of `duckdb::` here is a
/// regression, not new sanctioned ownership.
const NEGATIVE_INVARIANT_PREFIXES: &[&str] = &["example-platform/src/", "leio-code/src/"];

/// This doctor's own source path, exempt from the negative-invariant scan.
///
/// The grep needles (`duckdb::`, `duckdb.connect(`) appear here as string
/// literals and in documentation, so this file must not flag itself as a
/// regression under [`NEGATIVE_INVARIANT_PREFIXES`].
const DOCTOR_SELF_PATH: &str = "leio-code/src/doctors/duckdb_contract.rs";

pub struct DuckdbContractDoctor;

impl Doctor for DuckdbContractDoctor {
    fn name(&self) -> &'static str {
        "duckdb-contract"
    }

    fn description(&self) -> &'static str {
        "Checks the pinned DuckDB build contract, stable prebuilt cache bootstrap, and version drift across gateway/platform."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_duckdb_contract(root)
    }
}

pub fn doctor_duckdb_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let cargo_config_path = root.join(".cargo/config.toml");
    let ensure_script_path = root.join("scripts/ensure-duckdb.sh");
    let gateway_cargo_path = root.join("example-gateway/Cargo.toml");
    let gateway_makefile_path = root.join("example-gateway/Makefile");
    let gateway_start_path = root.join("example-gateway/start-gateway.sh");
    let chatbot_start_path = root.join("example-api/start-chatbot.sh");
    let platform_cargo_path = root.join("example-platform/example-server/Cargo.toml");
    let gateway_local_cargo_config_path = root.join("example-gateway/.cargo/config.toml");
    let platform_local_cargo_config_path = root.join("example-platform/.cargo/config.toml");
    let platform_lock_path = root.join("example-platform/Cargo.lock");
    let platform_docker_path = root.join("example-platform/Dockerfile.server");

    let cargo_config_src = read_text(&cargo_config_path, &mut warnings);
    let ensure_script_src = read_text(&ensure_script_path, &mut warnings);
    let gateway_cargo_src = read_text(&gateway_cargo_path, &mut warnings);
    let gateway_makefile_src = read_text(&gateway_makefile_path, &mut warnings);
    let gateway_start_src = read_text(&gateway_start_path, &mut warnings);
    let chatbot_start_src = read_text(&chatbot_start_path, &mut warnings);
    let platform_cargo_src = read_text(&platform_cargo_path, &mut warnings);
    let gateway_local_cargo_config_src = read_text(&gateway_local_cargo_config_path, &mut warnings);
    let platform_local_cargo_config_src =
        read_text(&platform_local_cargo_config_path, &mut warnings);
    let platform_lock_src = read_text(&platform_lock_path, &mut warnings);
    let platform_docker_src = read_text(&platform_docker_path, &mut warnings);

    let workspace_enables_prebuilt = cargo_config_src
        .as_deref()
        .is_some_and(|src| src.contains("[env]") && src.contains("DUCKDB_DOWNLOAD_LIB = \"1\""));
    let gateway_local_enables_prebuilt = gateway_local_cargo_config_src
        .as_deref()
        .is_some_and(|src| src.contains("[env]") && src.contains("DUCKDB_DOWNLOAD_LIB = \"1\""));
    let platform_local_enables_prebuilt = platform_local_cargo_config_src
        .as_deref()
        .is_some_and(|src| src.contains("[env]") && src.contains("DUCKDB_DOWNLOAD_LIB = \"1\""));
    let ensure_script_present = ensure_script_src.is_some();
    let gateway_makefile_bootstraps = gateway_makefile_src
        .as_deref()
        .is_some_and(|src| src.contains("ensure-duckdb.sh gateway"));
    let gateway_start_bootstraps = gateway_start_src
        .as_deref()
        .is_some_and(|src| src.contains("ensure-duckdb.sh") && src.contains("DUCKDB_LIB_DIR"));
    let chatbot_start_bootstraps = chatbot_start_src
        .as_deref()
        .is_some_and(|src| src.contains("ensure-duckdb.sh") && src.contains("DUCKDB_LIB_DIR"));
    let platform_docker_hardcodes_cache_path = platform_docker_src.as_deref().is_some_and(|src| {
        src.contains("target/duckdb-download/")
            && src.contains("libduckdb.so")
            && !src.contains("find target/duckdb-download -name 'libduckdb.so' -exec cp {} /tmp/libduckdb.so \\;")
    });

    let gateway_pkg_version = gateway_cargo_src
        .as_deref()
        .and_then(parse_gateway_duckdb_pkg_version);
    let gateway_duckdb_version = gateway_pkg_version
        .as_deref()
        .and_then(derive_duckdb_version_from_pkg_version);
    let gateway_comment_mentions = gateway_cargo_src
        .as_deref()
        .and_then(parse_gateway_duckdb_comment_version);
    let platform_duckdb_pkg_version = platform_lock_src
        .as_deref()
        .and_then(parse_platform_duckdb_lock_version);
    let platform_duckdb_version = platform_duckdb_pkg_version
        .as_deref()
        .and_then(derive_duckdb_version_from_pkg_version)
        .or(platform_duckdb_pkg_version.clone());
    let platform_uses_git_rev = platform_cargo_src.as_deref().is_some_and(|src| {
        src.contains("duckdb = { git = \"https://github.com/duckdb/duckdb-rs\"")
    });

    let env_duckdb_lib_dir = env::var("DUCKDB_LIB_DIR").ok();
    let env_duckdb_include_dir = env::var("DUCKDB_INCLUDE_DIR").ok();
    let env_lib_dir_missing = env_duckdb_lib_dir
        .as_deref()
        .is_some_and(|path| !Path::new(path).exists());
    let env_include_dir_missing = env_duckdb_include_dir
        .as_deref()
        .is_some_and(|path| !Path::new(path).exists());

    if !(workspace_enables_prebuilt
        || gateway_local_enables_prebuilt && platform_local_enables_prebuilt)
    {
        warnings.push(
            "DuckDB prebuilt bootstrap is no longer clearly declared either globally in .cargo/config.toml or locally in gateway/platform cargo configs"
                .to_string(),
        );
    }
    if !ensure_script_present {
        warnings.push(
            "scripts/ensure-duckdb.sh is missing; there is no stable workspace bootstrap for prebuilt DuckDB"
                .to_string(),
        );
    }
    if !gateway_makefile_bootstraps {
        warnings.push(
            "example-gateway/Makefile does not bootstrap the canonical DuckDB cache before build/test"
                .to_string(),
        );
    }
    if !gateway_start_bootstraps {
        warnings.push(
            "example-gateway/start-gateway.sh does not seed/export the canonical DuckDB lib/include dirs"
                .to_string(),
        );
    }
    if !chatbot_start_bootstraps {
        warnings.push(
            "example-api/start-chatbot.sh does not seed/export the canonical DuckDB lib/include dirs"
                .to_string(),
        );
    }
    if env_lib_dir_missing {
        warnings.push(format!(
            "DUCKDB_LIB_DIR points to a missing path in the current shell: {}",
            env_duckdb_lib_dir.as_deref().unwrap_or_default()
        ));
    }
    if env_include_dir_missing {
        warnings.push(format!(
            "DUCKDB_INCLUDE_DIR points to a missing path in the current shell: {}",
            env_duckdb_include_dir.as_deref().unwrap_or_default()
        ));
    }
    if let (Some(expected), Some(comment)) = (&gateway_duckdb_version, &gateway_comment_mentions)
        && expected != comment
    {
        warnings.push(format!(
                "example-gateway/Cargo.toml comment says DuckDB {comment}, but crate version {pkg} encodes DuckDB {expected}",
                pkg = gateway_pkg_version.as_deref().unwrap_or_default()
            ));
    }
    if let (Some(gateway), Some(platform)) = (&gateway_duckdb_version, &platform_duckdb_version)
        && gateway != platform
    {
        warnings.push(format!(
                "gateway pins DuckDB {gateway} while example-platform currently resolves DuckDB {platform}"
            ));
    }
    if platform_uses_git_rev {
        warnings.push(
            "example-platform/Cargo.toml still uses duckdb-rs from git; workspace policy now expects the pinned release aligned with the Arrow baseline"
                .to_string(),
        );
    }
    if platform_docker_hardcodes_cache_path {
        warnings.push(
            "example-platform/Dockerfile.server still hardcodes a DuckDB cache path instead of copying libduckdb.so generically"
                .to_string(),
        );
    }

    for (path, src, needle, detail, kind) in [
        (
            &cargo_config_path,
            cargo_config_src.as_ref(),
            "DUCKDB_DOWNLOAD_LIB = \"1\"",
            "workspace cargo config enables prebuilt DuckDB downloads",
            "duckdb_contract",
        ),
        (
            &gateway_local_cargo_config_path,
            gateway_local_cargo_config_src.as_ref(),
            "DUCKDB_DOWNLOAD_LIB = \"1\"",
            "gateway cargo config enables prebuilt DuckDB downloads locally",
            "duckdb_contract",
        ),
        (
            &platform_local_cargo_config_path,
            platform_local_cargo_config_src.as_ref(),
            "DUCKDB_DOWNLOAD_LIB = \"1\"",
            "platform cargo config enables prebuilt DuckDB downloads locally",
            "duckdb_contract",
        ),
        (
            &ensure_script_path,
            ensure_script_src.as_ref(),
            "archive_url=\"https://github.com/duckdb/duckdb/releases/download/v${duckdb_version}/${archive_name}\"",
            "stable bootstrap script derives and downloads the exact prebuilt release",
            "duckdb_contract",
        ),
        (
            &gateway_makefile_path,
            gateway_makefile_src.as_ref(),
            "ensure-duckdb.sh gateway",
            "gateway Makefile bootstraps the canonical DuckDB cache before cargo",
            "duckdb_contract",
        ),
        (
            &gateway_start_path,
            gateway_start_src.as_ref(),
            "DUCKDB_LIB_DIR",
            "gateway startup exports canonical DuckDB paths",
            "duckdb_contract",
        ),
        (
            &chatbot_start_path,
            chatbot_start_src.as_ref(),
            "DUCKDB_LIB_DIR",
            "chatbot launcher exports canonical DuckDB paths",
            "duckdb_contract",
        ),
        (
            &platform_docker_path,
            platform_docker_src.as_ref(),
            "find target/duckdb-download -name 'libduckdb.so' -exec cp {} /tmp/libduckdb.so \\;",
            "platform Dockerfile copies DuckDB without hardcoding target/version",
            "duckdb_contract",
        ),
    ] {
        if let Some(src) = src
            && let Some(line) = find_line(src, needle)
        {
            evidence.push(EvidenceItem {
                kind: kind.to_string(),
                path: path.display().to_string(),
                line: Some(line),
                detail: detail.to_string(),
            });
        }
    }

    entities.push(json!({
        "gateway_duckdb_pkg_version": gateway_pkg_version,
        "gateway_duckdb_version": gateway_duckdb_version,
        "gateway_comment_mentions": gateway_comment_mentions,
    }));
    entities.push(json!({
        "platform_duckdb_version": platform_duckdb_version,
        "platform_duckdb_pkg_version": platform_duckdb_pkg_version,
        "platform_uses_git_rev": platform_uses_git_rev,
    }));
    entities.push(json!({
        "workspace_enables_prebuilt": workspace_enables_prebuilt,
        "gateway_local_enables_prebuilt": gateway_local_enables_prebuilt,
        "platform_local_enables_prebuilt": platform_local_enables_prebuilt,
        "ensure_script_present": ensure_script_present,
        "gateway_makefile_bootstraps": gateway_makefile_bootstraps,
        "gateway_start_bootstraps": gateway_start_bootstraps,
        "chatbot_start_bootstraps": chatbot_start_bootstraps,
        "platform_docker_hardcodes_cache_path": platform_docker_hardcodes_cache_path,
    }));
    entities.push(json!({
        "env_duckdb_lib_dir": env_duckdb_lib_dir,
        "env_duckdb_include_dir": env_duckdb_include_dir,
        "env_lib_dir_missing": env_lib_dir_missing,
        "env_include_dir_missing": env_include_dir_missing,
    }));

    append_duckdb_anticreep_checks(root, &mut warnings, &mut entities, &mut evidence);

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_duckdb_contract"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked DuckDB build contract, stable prebuilt bootstrap, and workspace version drift; found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.72 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Append the anti-creep, allowlist, and negative-invariant DuckDB checks.
///
/// Runs three additive checks over git-tracked files and merges their findings
/// into the doctor's existing `warnings`, `entities`, and `evidence` vectors:
///
/// - CHECK A: no NEW tracked `*.duckdb` artifact outside [`ALLOWED_DUCKDB_FILES`].
/// - CHECK B: no DuckDB call site (`duckdb::` for Rust, `duckdb.connect(` for
///   Python) outside [`SANCTIONED_PREFIXES`].
/// - NEGATIVE INVARIANT: zero `duckdb::` under [`NEGATIVE_INVARIANT_PREFIXES`].
///
/// All checks fail open: when git is unavailable the artifact and usage checks
/// are skipped with a soft note rather than warned, matching the repo-hygiene
/// precedent. Files that cannot be read are skipped silently so a transient
/// read error never manufactures a spurious clean signal or warning.
fn append_duckdb_anticreep_checks(
    root: &Path,
    warnings: &mut Vec<String>,
    entities: &mut Vec<serde_json::Value>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let Some(tracked) = git_tracked_files(root) else {
        // Fail open: without git we cannot enumerate tracked files, so record a
        // soft note and do not warn (a sandboxed/shallow checkout is not drift).
        evidence.push(EvidenceItem {
            kind: "duckdb_anticreep".to_string(),
            path: root.display().to_string(),
            line: None,
            detail: "git unavailable; .duckdb artifact and usage checks skipped".to_string(),
        });
        entities.push(json!({
            "duckdb_anticreep_skipped": true,
            "reason": "git_unavailable",
        }));
        return;
    };

    // CHECK A: no unsanctioned tracked *.duckdb artifacts.
    let mut unsanctioned_artifact_count: usize = 0;
    for rel in tracked
        .iter()
        .filter(|rel| rel.ends_with(".duckdb"))
        .filter(|rel| !ALLOWED_DUCKDB_FILES.contains(&rel.as_str()))
    {
        unsanctioned_artifact_count += 1;
        warnings.push(format!(
            "Unsanctioned tracked DuckDB artifact: {rel} (not in ALLOWED_DUCKDB_FILES; DuckDB warehouse/registry files must not be committed except the s1000d brex registry)"
        ));
        evidence.push(EvidenceItem {
            kind: "duckdb_anticreep".to_string(),
            path: rel.clone(),
            line: None,
            detail: "tracked *.duckdb artifact outside ALLOWED_DUCKDB_FILES".to_string(),
        });
    }

    // CHECK B: no DuckDB call site outside the sanctioned prefixes.
    let mut unsanctioned_usage_count: usize = 0;
    for rel in tracked
        .iter()
        .filter(|rel| rel.ends_with(".rs") || rel.ends_with(".py"))
        .filter(|rel| rel.as_str() != DOCTOR_SELF_PATH)
        .filter(|rel| !SANCTIONED_PREFIXES.iter().any(|p| rel.starts_with(p)))
    {
        let Ok(src) = fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let Some((line, needle)) = duckdb_call_site(&src) else {
            continue;
        };
        unsanctioned_usage_count += 1;
        warnings.push(format!(
            "Unsanctioned DuckDB usage in {rel}: DuckDB is the embedded durable store of record owned by gateway/platform/cartridges; new control-plane or request-path code must not open DuckDB directly (use the gateway write queue / Arrow / Milvus)"
        ));
        evidence.push(EvidenceItem {
            kind: "duckdb_anticreep".to_string(),
            path: rel.clone(),
            line: Some(line),
            detail: format!("DuckDB call site `{needle}` outside SANCTIONED_PREFIXES"),
        });
    }

    // STEP 5: surface tolerated-pending-removal prefixes as cleanup candidates
    // (low-severity evidence only — never a warning, so CI stays green today).
    for tolerated in TOLERATED_PENDING_REMOVAL {
        if tracked.iter().any(|rel| rel.starts_with(tolerated)) {
            evidence.push(EvidenceItem {
                kind: "duckdb_anticreep".to_string(),
                path: (*tolerated).to_string(),
                line: None,
                detail:
                    "tolerated-pending-removal: sanctioned today but a DuckDB cleanup candidate"
                        .to_string(),
            });
        }
    }

    // NEGATIVE INVARIANT: the eradicated targets must stay free of `duckdb::`.
    let mut platform_clean = true;
    let mut leio_src_clean = true;
    for rel in tracked
        .iter()
        .filter(|rel| rel.ends_with(".rs"))
        .filter(|rel| rel.as_str() != DOCTOR_SELF_PATH)
        .filter(|rel| {
            NEGATIVE_INVARIANT_PREFIXES
                .iter()
                .any(|p| rel.starts_with(p))
        })
    {
        let Ok(src) = fs::read_to_string(root.join(rel)) else {
            continue;
        };
        if !src.contains("duckdb::") {
            continue;
        }
        if rel.starts_with("example-platform/src/") {
            platform_clean = false;
        }
        if rel.starts_with("leio-code/src/") {
            leio_src_clean = false;
        }
        let line = find_line(&src, "duckdb::").unwrap_or(1);
        warnings.push(format!(
            "DuckDB regression: {rel} re-introduced DuckDB into an eradicated target (PHASE B / leio-code Arrow IPC)"
        ));
        evidence.push(EvidenceItem {
            kind: "duckdb_anticreep".to_string(),
            path: rel.clone(),
            line: Some(line),
            detail: "`duckdb::` in a negative-invariant prefix".to_string(),
        });
    }

    entities.push(json!({
        "platform_clean": platform_clean,
        "leio_src_clean": leio_src_clean,
        "unsanctioned_usage_count": unsanctioned_usage_count,
        "unsanctioned_artifact_count": unsanctioned_artifact_count,
    }));
}

/// Detect the first DuckDB call site in a Rust or Python source string.
///
/// Returns the 1-based line number and the matched needle. Keys on real call
/// sites, not import lines: `duckdb::` for Rust (any path-qualified use), and
/// `duckdb.connect(` for Python. A bare `import duckdb` is intentionally not a
/// match — it misses guarded `try: import duckdb` / function-local imports and
/// over-counts files that import but never connect.
fn duckdb_call_site(src: &str) -> Option<(usize, &'static str)> {
    for (index, line) in src.lines().enumerate() {
        if line.contains("duckdb::") {
            return Some((index + 1, "duckdb::"));
        }
        if line.contains("duckdb.connect(") {
            return Some((index + 1, "duckdb.connect("));
        }
    }
    None
}

fn parse_gateway_duckdb_pkg_version(src: &str) -> Option<String> {
    src.lines()
        .find(|line| line.trim_start().starts_with("duckdb = { version = "))
        .and_then(|line| line.split('"').nth(1))
        .map(ToOwned::to_owned)
}

fn parse_gateway_duckdb_comment_version(src: &str) -> Option<String> {
    src.lines()
        .find(|line| line.trim_start().starts_with("duckdb = { version = "))
        .and_then(|line| line.split("DuckDB ").nth(1))
        .and_then(|tail| tail.split_whitespace().next())
        .map(ToOwned::to_owned)
}

fn derive_duckdb_version_from_pkg_version(pkg_version: &str) -> Option<String> {
    let encoded = pkg_version.split('.').nth(1)?.parse::<u32>().ok()?;
    let major = encoded / 10_000;
    let minor = (encoded / 100) % 100;
    let patch = encoded % 100;
    Some(format!("{major}.{minor}.{patch}"))
}

fn parse_platform_duckdb_lock_version(src: &str) -> Option<String> {
    let mut in_package = false;
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            in_package = false;
            continue;
        }
        if trimmed == "name = \"libduckdb-sys\"" {
            in_package = true;
            continue;
        }
        if in_package && trimmed.starts_with("version = ") {
            return trimmed.split('"').nth(1).map(ToOwned::to_owned);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempdir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-duckdb-contract-{label}-{}-{nanos}",
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

    /// Initialize a git repo and stage everything so `git ls-files` reports it.
    /// Returns `false` when git is unavailable, so tests skip rather than fail.
    fn git_init_and_add(dir: &Path) -> bool {
        let init = Command::new("git").arg("-C").arg(dir).arg("init").output();
        if init.map(|o| !o.status.success()).unwrap_or(true) {
            return false;
        }
        let add = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["add", "-A", "-f"])
            .output();
        add.map(|o| o.status.success()).unwrap_or(false)
    }

    #[test]
    fn call_site_matches_rust_path_usage_not_import_line() {
        // The bare Python import line must not match (guarded-import undercount
        // guard): only the `.connect(` call site counts.
        let py = "import duckdb\n# no connect here\n";
        assert_eq!(duckdb_call_site(py), None);

        let py_connect = "import duckdb\nconn = duckdb.connect('x.db')\n";
        assert_eq!(duckdb_call_site(py_connect), Some((2, "duckdb.connect(")));

        let rs = "fn f() {}\nlet c = duckdb::Connection::open(p)?;\n";
        assert_eq!(duckdb_call_site(rs), Some((2, "duckdb::")));
    }

    #[test]
    fn flags_unsanctioned_artifact_and_usage_but_allows_sanctioned() {
        let dir = unique_tempdir("anticreep");
        // Sanctioned: gateway crate-wide Rust usage — must NOT warn.
        write_file(
            &dir.join("example-gateway/src/routing.rs"),
            b"let c = duckdb::Connection::open(p)?;\n",
        );
        // Sanctioned tracked artifact — must NOT warn.
        write_file(
            &dir.join("cartridges/s1000d_align/ontology/brex_registry.duckdb"),
            b"\x00fake",
        );
        // Unsanctioned: a stray control-plane module opening DuckDB directly.
        write_file(
            &dir.join("some-app/src/leak.rs"),
            b"let c = duckdb::Connection::open(p)?;\n",
        );
        // Unsanctioned tracked artifact.
        write_file(&dir.join("data/warehouse.duckdb"), b"\x00fake");

        if !git_init_and_add(&dir) {
            return; // git unavailable: skip rather than fail (fail-open contract)
        }

        let mut warnings = Vec::new();
        let mut entities = Vec::new();
        let mut evidence = Vec::new();
        append_duckdb_anticreep_checks(&dir, &mut warnings, &mut entities, &mut evidence);

        // The stray usage and stray artifact each produce exactly one warning.
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("Unsanctioned DuckDB usage in some-app/src/leak.rs")),
            "expected unsanctioned usage warning, got {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("Unsanctioned tracked DuckDB artifact: data/warehouse.duckdb")),
            "expected unsanctioned artifact warning, got {warnings:?}"
        );
        // The sanctioned gateway usage must not appear in any warning.
        assert!(
            !warnings.iter().any(|w| w.contains("example-gateway/")),
            "sanctioned gateway usage must not warn, got {warnings:?}"
        );
        // The sanctioned brex registry must not appear in any warning.
        assert!(
            !warnings.iter().any(|w| w.contains("brex_registry.duckdb")),
            "sanctioned brex registry must not warn, got {warnings:?}"
        );
    }

    #[test]
    fn negative_invariant_flags_platform_regression() {
        let dir = unique_tempdir("neginv");
        // A regression: DuckDB reintroduced into the eradicated platform crate.
        write_file(
            &dir.join("example-platform/src/store.rs"),
            b"let c = duckdb::Connection::open(p)?;\n",
        );

        if !git_init_and_add(&dir) {
            return;
        }

        let mut warnings = Vec::new();
        let mut entities = Vec::new();
        let mut evidence = Vec::new();
        append_duckdb_anticreep_checks(&dir, &mut warnings, &mut entities, &mut evidence);

        assert!(
            warnings
                .iter()
                .any(|w| w.contains("DuckDB regression")
                    && w.contains("example-platform/src/store.rs")),
            "expected platform regression warning, got {warnings:?}"
        );
        // The summary entity must record the regression as unclean.
        let summary = entities
            .iter()
            .find(|e| e.get("platform_clean").is_some())
            .expect("summary entity present");
        assert_eq!(summary["platform_clean"], serde_json::json!(false));
    }

    #[test]
    fn tolerated_pending_removal_is_evidence_not_warning() {
        let dir = unique_tempdir("tolerated");
        write_file(
            &dir.join("tools/ha-semantic-merge/05_query.py"),
            b"import duckdb\nconn = duckdb.connect('x')\n",
        );

        if !git_init_and_add(&dir) {
            return;
        }

        let mut warnings = Vec::new();
        let mut entities = Vec::new();
        let mut evidence = Vec::new();
        append_duckdb_anticreep_checks(&dir, &mut warnings, &mut entities, &mut evidence);

        // Tolerated prefix must never warn (CI stays green today)...
        assert!(
            !warnings.iter().any(|w| w.contains("ha-semantic-merge")),
            "tolerated prefix must not warn, got {warnings:?}"
        );
        // ...but must surface as a cleanup-candidate evidence row.
        assert!(
            evidence
                .iter()
                .any(|e| e.detail.contains("tolerated-pending-removal")
                    && e.path.contains("ha-semantic-merge")),
            "expected tolerated-pending-removal evidence, got {evidence:?}"
        );
    }
}
