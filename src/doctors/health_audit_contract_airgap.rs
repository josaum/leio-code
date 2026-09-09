//! Health-audit air-gapped contract-extraction egress doctor.
//!
//! Locks the air-gapped egress invariant the `/contracts` ingestion path must
//! satisfy on the customer appliance (see `deploy/stamp/APPLIANCE.md`): the
//! route-reachable structured contract-extraction LLM must NEVER make an
//! external network call, and the refusal must happen BEFORE any LLM client is
//! ever constructed (egress must not be reachable-then-refused).
//!
//! Canonical evidence path: `cartridges/health_audit/router.py`,
//! `_run_structured_contract_extraction` — the only route-reachable LLM call
//! site (reached via `_attach_structured_contract_extraction` ← the `/contracts`
//! ingestion handlers). The doctor asserts three things:
//!
//!   1. **Guard helpers exist.** The three air-gapped helpers are present as
//!      `def`s in router.py: `_air_gapped_enabled`, `_is_local_llm_base_url`,
//!      `_air_gapped_blocks_external_llm`. If any is missing, the guard was
//!      deleted and air-gapped egress is no longer decidable.
//!
//!   2. **Guard precedes client construction.** Inside
//!      `_run_structured_contract_extraction` the guard call
//!      `_air_gapped_blocks_external_llm(` appears, is immediately followed
//!      (within a few lines) by a `return None`, AND appears strictly BEFORE
//!      the first `AsyncOpenAI(` construction in that function. The ordering is
//!      the crux: the refusal must precede any client construction so egress is
//!      never reachable-then-refused. The doctor computes the byte offsets of
//!      both markers within the function span and asserts `guard < client`.
//!
//!   3. **External Claude path stays out of the route graph.** The external
//!      Claude extractor `_extract_structured_via_claude` is confined to the
//!      CLI-only tool `cartridges/health_audit/scripts/extract_scanned_pdf.py`.
//!      It must NOT be referenced from `cartridges/health_audit/router.py` nor
//!      from any file under `cartridges/health_audit/routes/`. Any route-side
//!      reference to `_extract_structured_via_claude`, `extract_scanned_pdf`,
//!      or `extract_bundle_scanned` means the external Claude extractor became
//!      route-reachable — a FAIL.
//!
//! Warnings emitted by this doctor carry the `[health-audit-contract-airgap]`
//! prefix so the ledger can account for any future regression.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct HealthAuditContractAirgapDoctor;

impl Doctor for HealthAuditContractAirgapDoctor {
    fn name(&self) -> &'static str {
        "health-audit-contract-airgap"
    }

    fn description(&self) -> &'static str {
        "Locks the air-gapped /contracts egress invariant: the route-reachable contract-extraction LLM refuses external endpoints before any client is constructed, and the external Claude path stays out of the route graph."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_contract_airgap(root)
    }
}

const ROUTER_PATH: &str = "cartridges/health_audit/router.py";
const ROUTES_DIR: &str = "cartridges/health_audit/routes";
const SCRIPT_PATH: &str = "cartridges/health_audit/scripts/extract_scanned_pdf.py";

const PREFIX: &str = "[health-audit-contract-airgap]";

/// The three air-gapped helper `def`s that together make the egress decision.
const GUARD_HELPERS: &[&str] = &[
    "def _air_gapped_enabled(",
    "def _is_local_llm_base_url(",
    "def _air_gapped_blocks_external_llm(",
];

const GUARD_CALL: &str = "_air_gapped_blocks_external_llm(";
const CLIENT_CTOR: &str = "AsyncOpenAI(";
const FN_DEF: &str = "def _run_structured_contract_extraction(";

/// Route-side references that would prove the external Claude extractor became
/// route-reachable. Any of these inside router.py or routes/ is a FAIL.
const EXTERNAL_CLAUDE_MARKERS: &[&str] = &[
    "_extract_structured_via_claude",
    "extract_scanned_pdf",
    "extract_bundle_scanned",
];

/// Find the half-open byte span of the `_run_structured_contract_extraction`
/// function: from the `def` line to the next top-level `def `/`class ` (a line
/// starting in column 0 with `def ` or `class `) or end-of-file.
fn function_span(src: &str) -> Option<(usize, usize)> {
    let start = src.find(FN_DEF)?;
    // The body starts after the def line; scan subsequent lines for the next
    // top-level (column-0) `def `/`class ` which terminates this function.
    let after_def = start + FN_DEF.len();
    let mut cursor = src[after_def..]
        .find('\n')
        .map(|nl| after_def + nl + 1)
        .unwrap_or(src.len());
    let body_start = cursor;
    while cursor < src.len() {
        let line_end = src[cursor..]
            .find('\n')
            .map(|nl| cursor + nl)
            .unwrap_or(src.len());
        let line = &src[cursor..line_end];
        if line.starts_with("def ") || line.starts_with("class ") {
            return Some((start, cursor));
        }
        cursor = if line_end < src.len() {
            line_end + 1
        } else {
            src.len()
        };
    }
    let _ = body_start;
    Some((start, src.len()))
}

pub fn doctor_health_audit_contract_airgap(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let router_full = root.join(ROUTER_PATH);
    let mut io_warnings = Vec::new();
    let router_src = read_text(&router_full, &mut io_warnings);

    let mut helpers_present = false;
    let mut guard_before_client = false;
    let mut guard_returns_none = false;

    match router_src.as_deref() {
        None => {
            warnings.push(format!(
                "{PREFIX} canonical evidence file missing or unreadable: {ROUTER_PATH}"
            ));
            for w in io_warnings {
                warnings.push(format!("{PREFIX} {w}"));
            }
            evidence.push(EvidenceItem {
                kind: "health_audit_contract_airgap_missing_file".to_string(),
                path: ROUTER_PATH.to_string(),
                line: None,
                detail: "router.py is the only route-reachable LLM call site".to_string(),
            });
        }
        Some(src) => {
            // (1) the three air-gapped helper defs must exist.
            let missing_helpers: Vec<&str> = GUARD_HELPERS
                .iter()
                .filter(|needle| !src.contains(**needle))
                .copied()
                .collect();
            helpers_present = missing_helpers.is_empty();
            if !helpers_present {
                warnings.push(format!(
                    "{PREFIX} air-gapped guard helper(s) missing in {ROUTER_PATH}: {} — the egress guard was deleted",
                    missing_helpers.join(", ")
                ));
                evidence.push(EvidenceItem {
                    kind: "health_audit_contract_airgap_missing_helper".to_string(),
                    path: ROUTER_PATH.to_string(),
                    line: None,
                    detail: format!("missing: {}", missing_helpers.join(", ")),
                });
            } else {
                for needle in GUARD_HELPERS {
                    if let Some(line) = find_line(src, needle) {
                        evidence.push(EvidenceItem {
                            kind: "health_audit_contract_airgap_helper".to_string(),
                            path: ROUTER_PATH.to_string(),
                            line: Some(line),
                            detail: (*needle).to_string(),
                        });
                    }
                }
            }

            // (2) ordering: guard call must precede the first AsyncOpenAI(
            // construction, within the _run_structured_contract_extraction span,
            // and be immediately followed by a `return None`.
            match function_span(src) {
                None => {
                    warnings.push(format!(
                        "{PREFIX} {ROUTER_PATH}: `{FN_DEF}` not found — the route-reachable LLM call site moved or was renamed; the ordering guard can no longer be verified"
                    ));
                    evidence.push(EvidenceItem {
                        kind: "health_audit_contract_airgap_fn_missing".to_string(),
                        path: ROUTER_PATH.to_string(),
                        line: None,
                        detail: FN_DEF.to_string(),
                    });
                }
                Some((span_start, span_end)) => {
                    let span = &src[span_start..span_end];
                    let guard_rel = span.find(GUARD_CALL);
                    let client_rel = span.find(CLIENT_CTOR);

                    // Line number of the def for evidence anchoring.
                    let fn_line = find_line(src, FN_DEF);

                    match (guard_rel, client_rel) {
                        (None, _) => {
                            warnings.push(format!(
                                "{PREFIX} {ROUTER_PATH}: `_run_structured_contract_extraction` no longer calls `{GUARD_CALL}` — the air-gapped refusal was removed from the route-reachable LLM call site"
                            ));
                            evidence.push(EvidenceItem {
                                kind: "health_audit_contract_airgap_guard_call_missing".to_string(),
                                path: ROUTER_PATH.to_string(),
                                line: fn_line,
                                detail: format!(
                                    "expected guard call `{GUARD_CALL}` in function body"
                                ),
                            });
                        }
                        (Some(g), None) => {
                            // Guard present, no client at all in this function:
                            // egress cannot occur. Treat as ordering-safe but
                            // still verify the return-None follow-through below.
                            guard_before_client = true;
                            let _ = g;
                        }
                        (Some(g), Some(c)) => {
                            guard_before_client = g < c;
                            if !guard_before_client {
                                let guard_line = byte_to_line(src, span_start + g);
                                let client_line = byte_to_line(src, span_start + c);
                                warnings.push(format!(
                                    "{PREFIX} {ROUTER_PATH}:{guard_line}: air-gapped guard `{GUARD_CALL}` is constructed AFTER `{CLIENT_CTOR}` (line {client_line}) inside `_run_structured_contract_extraction` — egress is reachable-then-refused; the refusal must precede any client construction"
                                ));
                                evidence.push(EvidenceItem {
                                    kind: "health_audit_contract_airgap_ordering".to_string(),
                                    path: ROUTER_PATH.to_string(),
                                    line: Some(guard_line),
                                    detail: format!(
                                        "guard at line {guard_line} must precede AsyncOpenAI( at line {client_line}"
                                    ),
                                });
                            } else {
                                let guard_line = byte_to_line(src, span_start + g);
                                let client_line = byte_to_line(src, span_start + c);
                                evidence.push(EvidenceItem {
                                    kind: "health_audit_contract_airgap_ordering_ok".to_string(),
                                    path: ROUTER_PATH.to_string(),
                                    line: Some(guard_line),
                                    detail: format!(
                                        "guard at line {guard_line} precedes AsyncOpenAI( at line {client_line}"
                                    ),
                                });
                            }
                        }
                    }

                    // The guard call must be followed (within a few lines) by a
                    // `return None`, otherwise the refusal does not actually stop
                    // egress.
                    if let Some(g) = guard_rel {
                        let tail = &span[g..];
                        let window: String = tail.lines().take(12).collect::<Vec<_>>().join("\n");
                        guard_returns_none = window.contains("return None");
                        if !guard_returns_none {
                            let guard_line = byte_to_line(src, span_start + g);
                            warnings.push(format!(
                                "{PREFIX} {ROUTER_PATH}:{guard_line}: air-gapped guard `{GUARD_CALL}` is not immediately followed by `return None` — a refusal that does not return cannot block egress"
                            ));
                            evidence.push(EvidenceItem {
                                kind: "health_audit_contract_airgap_no_return".to_string(),
                                path: ROUTER_PATH.to_string(),
                                line: Some(guard_line),
                                detail: "expected `return None` within ~12 lines of the guard call"
                                    .to_string(),
                            });
                        }
                    }
                }
            }

            // (3) the external Claude path must not be referenced in router.py.
            for marker in EXTERNAL_CLAUDE_MARKERS {
                if let Some(line) = find_line(src, marker) {
                    warnings.push(format!(
                        "{PREFIX} {ROUTER_PATH}:{line}: route-side reference to external Claude extractor `{marker}` — the CLI-only extract_scanned_pdf path became route-reachable; egress is no longer confined to the appliance-safe LLM call site"
                    ));
                    evidence.push(EvidenceItem {
                        kind: "health_audit_contract_airgap_route_claude_ref".to_string(),
                        path: ROUTER_PATH.to_string(),
                        line: Some(line),
                        detail: format!("forbidden route-side reference: {marker}"),
                    });
                }
            }
        }
    }

    // (3, cont.) scan every file under routes/ for the external Claude markers.
    let routes_full = root.join(ROUTES_DIR);
    let mut routes_files_scanned = 0usize;
    if routes_full.is_dir() {
        for marker_hit in scan_routes_for_markers(&routes_full, EXTERNAL_CLAUDE_MARKERS) {
            routes_files_scanned = routes_files_scanned.max(marker_hit.files_scanned);
            warnings.push(format!(
                "{PREFIX} {}:{}: route-side reference to external Claude extractor `{}` — the CLI-only extract_scanned_pdf path became route-reachable",
                marker_hit.rel_path, marker_hit.line, marker_hit.marker
            ));
            evidence.push(EvidenceItem {
                kind: "health_audit_contract_airgap_route_claude_ref".to_string(),
                path: marker_hit.rel_path.clone(),
                line: Some(marker_hit.line),
                detail: format!("forbidden route-side reference: {}", marker_hit.marker),
            });
        }
        if routes_files_scanned == 0 {
            // count files even when no hits, for the entity record.
            routes_files_scanned = count_py_files(&routes_full);
        }
    }

    let claude_confined = warnings
        .iter()
        .all(|w| !w.contains("route-side reference to external Claude extractor"));

    entities.push(json!({
        "doctor": "health-audit-contract-airgap",
        "canonical_evidence": format!("{ROUTER_PATH}::_run_structured_contract_extraction"),
        "appliance_doc": "deploy/stamp/APPLIANCE.md",
        "guard_helpers_present": helpers_present,
        "guard_precedes_client": guard_before_client,
        "guard_returns_none": guard_returns_none,
        "external_claude_confined_to_cli": claude_confined,
        "cli_only_path": SCRIPT_PATH,
        "routes_files_scanned": routes_files_scanned,
        "invariant": "air-gapped /contracts ingestion must refuse external LLM endpoints BEFORE constructing any AsyncOpenAI client; the external Claude extractor stays CLI-only and out of the route graph",
        "forbidden_pattern": "constructing AsyncOpenAI( before the _air_gapped_blocks_external_llm guard returns; importing/calling _extract_structured_via_claude / extract_bundle_scanned / extract_scanned_pdf from router.py or routes/",
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_health_audit_contract_airgap"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "air-gapped /contracts egress invariant intact: guard helpers present, refusal precedes client construction, external Claude extractor confined to CLI".to_string()
        } else {
            format!(
                "health-audit air-gapped contract egress drift: {} warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.93 } else { 0.60 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "tenant": "health_audit",
            "env_flag": "HEALTH_AUDIT_AIR_GAPPED",
            "appliance_doc": "deploy/stamp/APPLIANCE.md",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// 1-based line number for a byte offset into `src`.
fn byte_to_line(src: &str, offset: usize) -> usize {
    let clamped = offset.min(src.len());
    src[..clamped].bytes().filter(|&b| b == b'\n').count() + 1
}

struct MarkerHit {
    rel_path: String,
    line: usize,
    marker: String,
    files_scanned: usize,
}

fn count_py_files(dir: &Path) -> usize {
    let mut n = 0usize;
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("py") {
                n += 1;
            }
        }
    }
    n
}

/// Scan every `*.py` directly under `routes/` for any of the forbidden markers.
fn scan_routes_for_markers(routes_dir: &Path, markers: &[&str]) -> Vec<MarkerHit> {
    let mut hits = Vec::new();
    let mut files_scanned = 0usize;
    let Ok(read) = std::fs::read_dir(routes_dir) else {
        return hits;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("py") {
            continue;
        }
        files_scanned += 1;
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel_path = format!(
            "{ROUTES_DIR}/{}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<unknown>")
        );
        for marker in markers {
            if let Some(line) = find_line(&body, marker) {
                hits.push(MarkerHit {
                    rel_path: rel_path.clone(),
                    line,
                    marker: (*marker).to_string(),
                    files_scanned,
                });
            }
        }
    }
    // Stamp the total files-scanned count onto every hit (best-effort metric).
    for hit in hits.iter_mut() {
        hit.files_scanned = files_scanned;
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_ha_contract_airgap_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    /// A minimal router.py that satisfies the invariant: all three helper defs,
    /// guard-before-client ordering with a return None, and no route-side Claude
    /// reference.
    const GOOD_ROUTER: &str = r#"import os


def _air_gapped_enabled() -> bool:
    return os.environ.get("HEALTH_AUDIT_AIR_GAPPED", "") in {"1", "true"}


def _is_local_llm_base_url(base_url):
    return False


def _air_gapped_blocks_external_llm(base_url, *, use_gemini):
    if not _air_gapped_enabled():
        return False
    return use_gemini or not _is_local_llm_base_url(base_url)


def _run_structured_contract_extraction(*, text, filename):
    openai_base_url = os.environ.get("OPENAI_BASE_URL")
    use_gemini = False
    if _air_gapped_blocks_external_llm(openai_base_url, use_gemini=use_gemini):
        logger.warning("[air-gapped] refusing external LLM")
        return None
    from openai import AsyncOpenAI

    client = AsyncOpenAI(base_url=openai_base_url)
    return client


def _attach_structured_contract_extraction(extraction, *, filename):
    return _run_structured_contract_extraction(text="", filename=filename)
"#;

    #[test]
    fn clean_router_passes() {
        let root = temp_root("clean");
        write(&root, ROUTER_PATH, GOOD_ROUTER);
        write(
            &root,
            SCRIPT_PATH,
            "def _extract_structured_via_claude():\n    pass\n",
        );
        // A benign routes file with no forbidden marker.
        write(
            &root,
            "cartridges/health_audit/routes/contracts.py",
            "from cartridges.health_audit.router import router\n",
        );
        let env = doctor_health_audit_contract_airgap(&root);
        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
        assert!(
            env.summary
                .contains("air-gapped /contracts egress invariant intact"),
            "summary: {}",
            env.summary
        );
    }

    #[test]
    fn missing_guard_helper_flags() {
        let root = temp_root("no_helper");
        // Drop _air_gapped_blocks_external_llm def by renaming it away, but keep
        // a call (simulating a half-deleted guard).
        let body = GOOD_ROUTER.replace(
            "def _air_gapped_blocks_external_llm(base_url, *, use_gemini):",
            "def _SOMETHING_ELSE(base_url, *, use_gemini):",
        );
        write(&root, ROUTER_PATH, &body);
        write(&root, SCRIPT_PATH, "pass\n");
        let env = doctor_health_audit_contract_airgap(&root);
        assert!(
            env.warnings.iter().any(|w| w.starts_with(PREFIX)
                && w.contains("air-gapped guard helper(s) missing")
                && w.contains("_air_gapped_blocks_external_llm")),
            "expected missing-helper warning, got: {:?}",
            env.warnings
        );
    }

    #[test]
    fn guard_after_client_flags_ordering() {
        let root = temp_root("bad_order");
        // Reorder: construct AsyncOpenAI( BEFORE the guard — egress
        // reachable-then-refused.
        let bad_fn = r#"def _run_structured_contract_extraction(*, text, filename):
    openai_base_url = os.environ.get("OPENAI_BASE_URL")
    use_gemini = False
    from openai import AsyncOpenAI

    client = AsyncOpenAI(base_url=openai_base_url)
    if _air_gapped_blocks_external_llm(openai_base_url, use_gemini=use_gemini):
        logger.warning("[air-gapped] refusing external LLM")
        return None
    return client
"#;
        let body = format!(
            r#"import os


def _air_gapped_enabled() -> bool:
    return False


def _is_local_llm_base_url(base_url):
    return False


def _air_gapped_blocks_external_llm(base_url, *, use_gemini):
    return False


{bad_fn}

def _attach(extraction):
    return None
"#
        );
        write(&root, ROUTER_PATH, &body);
        write(&root, SCRIPT_PATH, "pass\n");
        let env = doctor_health_audit_contract_airgap(&root);
        assert!(
            env.warnings.iter().any(|w| w.starts_with(PREFIX)
                && w.contains("is constructed AFTER")
                && w.contains("reachable-then-refused")),
            "expected ordering warning, got: {:?}",
            env.warnings
        );
    }

    #[test]
    fn route_side_claude_reference_flags() {
        let root = temp_root("route_claude");
        write(&root, ROUTER_PATH, GOOD_ROUTER);
        write(&root, SCRIPT_PATH, "pass\n");
        // A routes file that imports the external Claude path → route-reachable.
        write(
            &root,
            "cartridges/health_audit/routes/contracts.py",
            "from cartridges.health_audit.scripts.extract_scanned_pdf import extract_bundle_scanned\n",
        );
        let env = doctor_health_audit_contract_airgap(&root);
        assert!(
            env.warnings.iter().any(|w| w.starts_with(PREFIX)
                && w.contains("route-side reference to external Claude extractor")
                && w.contains("extract_bundle_scanned")),
            "expected route-side claude reference warning, got: {:?}",
            env.warnings
        );
    }

    #[test]
    fn guard_without_return_none_flags() {
        let root = temp_root("no_return");
        // Guard present and before client, but no `return None` follow-through.
        let body = GOOD_ROUTER.replace(
            "        logger.warning(\"[air-gapped] refusing external LLM\")\n        return None\n",
            "        logger.warning(\"[air-gapped] refusing external LLM\")\n        pass\n",
        );
        write(&root, ROUTER_PATH, &body);
        write(&root, SCRIPT_PATH, "pass\n");
        let env = doctor_health_audit_contract_airgap(&root);
        assert!(
            env.warnings.iter().any(|w| w.starts_with(PREFIX)
                && w.contains("not immediately followed by `return None`")),
            "expected missing-return warning, got: {:?}",
            env.warnings
        );
    }
}
