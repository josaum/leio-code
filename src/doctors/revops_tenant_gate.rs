//! revops-tenant-gate doctor.
//!
//! Catches the exact class of bug surfaced 2026-05-28: a Pratique-tenant
//! viewer would see Fitness Exclusive data because the example-ops revops UI
//! had a hardcoded fitness gym ID as a `process.env.X ?? "<real-id>"`
//! default, and the new `/api/revops/[...path]` proxy forwarded blindly
//! to jai-pay without checking the viewer's tenant.
//!
//! The pattern this enforces is documented in
//! `example-ops/CLAUDE.md` → "Multi-Tenant Isolation" and in
//! `memory/feedback_tenant_isolation.md`. Three independent leak vectors,
//! three checks:
//!
//!   1. **Hardcoded fitness gym IDs** anywhere in example-ops source.
//!   2. **Revops page** that does not call `requireRevopsTenant()`.
//!   3. **`/api/revops/*` proxy** that does not import the tenant gate.
//!
//! Scope: this is example-ops-specific; jcube / hubspot / other frontends are
//! out of scope unless they grow their own revops surfaces.

use std::path::Path;
use std::time::Instant;

use ignore::WalkBuilder;
use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct RevopsTenantGateDoctor;

impl Doctor for RevopsTenantGateDoctor {
    fn name(&self) -> &'static str {
        "revops-tenant-gate"
    }

    fn description(&self) -> &'static str {
        "Gym-ops revops surface must (a) never hardcode fitness gym IDs, (b) call requireRevopsTenant() in every revops page, (c) decode tenant in every /api/revops/* proxy. Catches the 2026-05-28 Pratique→fitness leak class."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_revops_tenant_gate(root)
    }
}

// The hardcoded gym ID seen in the leak. Anyone introducing this string
// anywhere in source is reintroducing the leak.
const KNOWN_FITNESS_GYM_ID: &str = "cmmr4ypsf0000ar0ro2unv7cf";

// Surfaces this doctor inspects.
const EXAMPLE_OPS_ROOT: &str = "example-ops/src";
const REVOPS_PAGES_DIR: &str = "example-ops/src/app/(ops)/revops";
const REVOPS_API_DIR: &str = "example-ops/src/app/api/revops";

fn read_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

pub fn doctor_revops_tenant_gate(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut entities: Vec<serde_json::Value> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let example_ops_root = root.join(EXAMPLE_OPS_ROOT);
    if !example_ops_root.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_revops_tenant_gate"),
            kind: "doctor".to_string(),
            summary: "revops-tenant-gate: example-ops/src not present — doctor skipped".to_string(),
            confidence: 0.95,
            entities,
            evidence,
            warnings,
            meta: Some(json!({"reason": "no_example_ops_src"})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    // 1. Scan all .ts/.tsx in example-ops/src for the known fitness gym ID.
    let mut hardcoded_hits = 0usize;
    let walker = WalkBuilder::new(&example_ops_root)
        .hidden(false)
        .git_ignore(true)
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext_ok = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e, "ts" | "tsx"))
            .unwrap_or(false);
        if !ext_ok {
            continue;
        }
        // Skip the doctor's own fixtures / tests if any.
        let rel = match relative(root, path) {
            Some(r) => r,
            None => continue,
        };
        let contents = match read_file(path) {
            Some(s) => s,
            None => continue,
        };
        for (idx, line) in contents.lines().enumerate() {
            if line.contains(KNOWN_FITNESS_GYM_ID) {
                hardcoded_hits += 1;
                warnings.push(format!(
                    "hardcoded fitness gym ID at {rel}:{} — Pratique viewers would see fitness data",
                    idx + 1
                ));
                entities.push(json!({
                    "doctor": "revops-tenant-gate",
                    "kind": "hardcoded_gym_id",
                    "file": rel,
                    "line": idx + 1,
                }));
                evidence.push(EvidenceItem {
                    kind: "hardcoded_fitness_gym_id".to_string(),
                    path: rel.clone(),
                    line: Some(idx + 1),
                    detail: format!("references `{KNOWN_FITNESS_GYM_ID}`"),
                });
            }
        }
    }

    // 2. Every revops page (under app/(ops)/revops/) must call requireRevopsTenant.
    let revops_pages_root = root.join(REVOPS_PAGES_DIR);
    let mut pages_checked = 0usize;
    if revops_pages_root.exists() {
        let walker = WalkBuilder::new(&revops_pages_root)
            .hidden(false)
            .git_ignore(true)
            .build();
        for entry in walker.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !matches!(name, "page.tsx" | "page.ts") {
                continue;
            }
            pages_checked += 1;
            let rel = match relative(root, path) {
                Some(r) => r,
                None => continue,
            };
            let contents = match read_file(path) {
                Some(s) => s,
                None => continue,
            };
            if !contents.contains("requireRevopsTenant") {
                warnings.push(format!(
                    "revops page {rel} does not call requireRevopsTenant() — leaks if viewer is on a different tenant",
                ));
                entities.push(json!({
                    "doctor": "revops-tenant-gate",
                    "kind": "page_missing_gate",
                    "file": rel,
                }));
                evidence.push(EvidenceItem {
                    kind: "revops_page_missing_gate".to_string(),
                    path: rel.clone(),
                    line: None,
                    detail: "expected `await requireRevopsTenant()` from @/lib/revops/tenant-gate"
                        .to_string(),
                });
            }
        }
    }

    // 3. /api/revops/[...path]/route.ts (the catch-all proxy) must check tenant.
    //    Specific routes under /api/revops/warehouse/* are local-only and exempt.
    let proxy_route = root.join(REVOPS_API_DIR).join("[...path]").join("route.ts");
    let mut proxy_checked = false;
    if proxy_route.exists() {
        proxy_checked = true;
        let rel = relative(root, &proxy_route).unwrap_or_else(|| proxy_route.display().to_string());
        let contents = read_file(&proxy_route).unwrap_or_default();
        let imports_gate = contents.contains("verifyToken")
            || contents.contains("requireRevopsTenant")
            || contents.contains("getViewerTenant");
        let has_allowlist =
            contents.contains("ALLOWED_NAMESPACES") || contents.contains("REVOPS_TENANTS");
        if !imports_gate || !has_allowlist {
            warnings.push(format!(
                "{rel} does not decode tenant from cookie + check allowlist — proxy forwards blindly",
            ));
            entities.push(json!({
                "doctor": "revops-tenant-gate",
                "kind": "proxy_missing_gate",
                "file": rel,
            }));
            evidence.push(EvidenceItem {
                kind: "revops_proxy_missing_gate".to_string(),
                path: rel.clone(),
                line: None,
                detail: "expected verifyToken() + tenant allowlist + 403 on mismatch".to_string(),
            });
        }
    }

    let summary = if warnings.is_empty() {
        format!(
            "revops-tenant-gate: 0 hardcoded gym IDs, {pages_checked} revops page(s) gated, proxy_checked={proxy_checked}",
        )
    } else {
        format!(
            "revops-tenant-gate: {} warning(s) — {hardcoded_hits} hardcoded gym ID(s) + page/proxy gate gaps",
            warnings.len(),
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_revops_tenant_gate"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.88 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "hardcoded_hits": hardcoded_hits,
            "pages_checked": pages_checked,
            "proxy_checked": proxy_checked,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-revops-gate-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn skips_when_no_example_ops() {
        let dir = unique_tempdir("no-example-ops");
        let envelope = doctor_revops_tenant_gate(&dir);
        assert!(envelope.warnings.is_empty());
        assert!(envelope.summary.contains("skipped"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_hardcoded_fitness_gym_id() {
        let dir = unique_tempdir("hardcoded");
        write(
            &dir.join("example-ops/src/app/(ops)/dashboard/page.tsx"),
            r#"
const DEFAULT_GYM_ID = process.env.X ?? "cmmr4ypsf0000ar0ro2unv7cf";
export default async function Page() { return null; }
"#,
        );
        let envelope = doctor_revops_tenant_gate(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("hardcoded fitness gym ID")),
            "expected hardcoded warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_revops_page_without_gate() {
        let dir = unique_tempdir("ungated-page");
        write(
            &dir.join("example-ops/src/app/(ops)/revops/journey/page.tsx"),
            "export default async function Page() { return null; }\n",
        );
        let envelope = doctor_revops_tenant_gate(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("requireRevopsTenant")),
            "expected ungated-page warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_flag_gated_page() {
        let dir = unique_tempdir("gated-page");
        write(
            &dir.join("example-ops/src/app/(ops)/revops/journey/page.tsx"),
            r#"
import { requireRevopsTenant } from "@/lib/revops/tenant-gate";
export default async function Page() { await requireRevopsTenant(); return null; }
"#,
        );
        let envelope = doctor_revops_tenant_gate(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected no warnings, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_proxy_without_gate() {
        let dir = unique_tempdir("ungated-proxy");
        write(
            &dir.join("example-ops/src/app/api/revops/[...path]/route.ts"),
            "export const GET = async () => new Response('ok');\n",
        );
        let envelope = doctor_revops_tenant_gate(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("forwards blindly")),
            "expected ungated-proxy warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_flag_gated_proxy() {
        let dir = unique_tempdir("gated-proxy");
        write(
            &dir.join("example-ops/src/app/api/revops/[...path]/route.ts"),
            r#"
import { verifyToken } from "@/lib/auth";
const ALLOWED_NAMESPACES = new Set(["fitness_exclusive"]);
export const GET = async () => new Response('ok');
"#,
        );
        let envelope = doctor_revops_tenant_gate(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected no warnings, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
