//! Pacto webhook -> Postgres drain dependency doctor.
//!
//! Drift surfaced 2026-05-28: the `example.jaipay_pacto_webhook_drain` task
//! silently dead-lettered every event because `psycopg2` was not installed in
//! the worker/api images, and Supabase/Prisma pooler DSNs carry libpq-hostile
//! params (`pgbouncer=true`) that must be stripped before
//! `psycopg2.connect(...)`.
//!
//! This doctor asserts that fix cannot silently regress:
//!   - `example-api/pyproject.toml` declares `psycopg2-binary`.
//!   - `pacto_webhook_ingest.py` defines `_libpq_safe_dsn`, that helper strips
//!     `pgbouncer`, and `_pg_connection` routes the DSN through it.
//!
//! Needle-based (model: `plusoft_routing_contract.rs`).

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct PactoDrainDependencyDoctor;

impl Doctor for PactoDrainDependencyDoctor {
    fn name(&self) -> &'static str {
        "pacto-drain-dependency"
    }

    fn description(&self) -> &'static str {
        "Checks that the Pacto webhook->Postgres drain fix (psycopg2-binary dependency + libpq-safe DSN that strips pgbouncer) is present so the drain cannot silently dead-letter every event."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_pacto_drain_dependency(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
}

pub fn doctor_pacto_drain_dependency(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let checks = [
        Check {
            path: "example-api/pyproject.toml",
            label: "psycopg2-binary dependency",
            needles: &["psycopg2-binary"],
        },
        Check {
            path: "example-api/example/integrations/jaipay/pacto_webhook_ingest.py",
            label: "Pacto webhook libpq-safe DSN + connection",
            needles: &["def _libpq_safe_dsn", "pgbouncer", "_libpq_safe_dsn(dsn)"],
        },
    ];

    let mut passed = 0usize;
    let checks_total = checks.len();
    for check in checks {
        let full_path = root.join(check.path);
        let mut io_warnings = Vec::new();
        let Some(body) = read_text(&full_path, &mut io_warnings) else {
            warnings.push(format!(
                "{} missing or unreadable: {}",
                check.label, check.path
            ));
            warnings.extend(io_warnings);
            evidence.push(EvidenceItem {
                kind: "pacto_drain_dependency_missing_file".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: check.label.to_string(),
            });
            continue;
        };

        let missing = check
            .needles
            .iter()
            .filter(|needle| !body.contains(**needle))
            .copied()
            .collect::<Vec<_>>();

        if missing.is_empty() {
            passed += 1;
            continue;
        }

        warnings.push(format!(
            "{} drift in {}: missing {}",
            check.label,
            check.path,
            missing.join(", ")
        ));
        evidence.push(EvidenceItem {
            kind: "pacto_drain_dependency_missing_anchor".to_string(),
            path: check.path.to_string(),
            line: None,
            detail: format!("missing: {}", missing.join(", ")),
        });
    }

    entities.push(json!({
        "doctor": "pacto-drain-dependency",
        "checks_passed": passed,
        "checks_total": checks_total,
        "task": "example.jaipay_pacto_webhook_drain",
        "dependency": "psycopg2-binary",
        "dsn_helper": "_libpq_safe_dsn (strips pgbouncer)",
        "source": "Pacto webhook -> Postgres drain broken 2026-05-28",
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_pacto_drain_dependency"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "Pacto webhook drain dependency intact: psycopg2-binary declared + libpq-safe DSN strips pgbouncer".to_string()
        } else {
            format!(
                "Pacto webhook drain dependency drift: {} warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.95 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "tenant": "jaipay",
            "integration": "pacto",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_pacto_drain_dependency_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_valid(root: &Path) {
        write(
            root,
            "example-api/pyproject.toml",
            "dependencies = [\n    \"psycopg2-binary>=2.9.9\",\n]\n",
        );
        write(
            root,
            "example-api/example/integrations/jaipay/pacto_webhook_ingest.py",
            "def _libpq_safe_dsn(dsn: str) -> str:\n    # strip pgbouncer params\n    return dsn\n\ndef _pg_connection():\n    return psycopg2.connect(_libpq_safe_dsn(dsn))\n",
        );
    }

    #[test]
    fn flags_missing_psycopg2_dependency() {
        let root = temp_root("drift-dep");
        write_valid(&root);
        write(
            &root,
            "example-api/pyproject.toml",
            "dependencies = [\n    \"fastapi\",\n]\n",
        );
        let envelope = doctor_pacto_drain_dependency(&root);
        assert!(!envelope.warnings.is_empty());
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("psycopg2-binary")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn flags_dsn_not_routed_through_safe_helper() {
        let root = temp_root("drift-dsn");
        write_valid(&root);
        // _libpq_safe_dsn defined but _pg_connection bypasses it (and no pgbouncer).
        write(
            &root,
            "example-api/example/integrations/jaipay/pacto_webhook_ingest.py",
            "def _libpq_safe_dsn(dsn: str) -> str:\n    return dsn\n\ndef _pg_connection():\n    return psycopg2.connect(dsn)\n",
        );
        let envelope = doctor_pacto_drain_dependency(&root);
        assert!(!envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }

    #[test]
    fn accepts_valid_contract() {
        let root = temp_root("valid");
        write_valid(&root);
        let envelope = doctor_pacto_drain_dependency(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }
}
