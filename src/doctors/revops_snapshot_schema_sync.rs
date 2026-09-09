//! Ensures the Rust `DashboardSnapshot` writer stays aligned with the Prisma migration.

use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct RevopsSnapshotSchemaSyncDoctor;

impl Doctor for RevopsSnapshotSchemaSyncDoctor {
    fn name(&self) -> &'static str {
        "revops-snapshot-schema-sync"
    }

    fn description(&self) -> &'static str {
        "Diffs jai-pay DashboardSnapshot Prisma migration columns against the Rust writer contract in example-revops-snapshot/src/snapshots.rs."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_revops_snapshot_schema_sync(root)
    }
}

const RUST_COLUMNS: &[&str] = &[
    "id",
    "snapshotKey",
    "scopeId",
    "payload",
    "sourceStatus",
    "warning",
    "computedAt",
    "expiresAt",
    "createdAt",
    "updatedAt",
];

pub fn doctor_revops_snapshot_schema_sync(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    let migration_path =
        root.join("jai-pay/prisma/migrations/20260521120000_dashboard_snapshots/migration.sql");
    let rust_contract_path = root.join("example-revops-snapshot/src/snapshots.rs");

    let mut io_warnings = Vec::new();
    let migration_sql = match read_text(&migration_path, &mut io_warnings) {
        Some(body) => body,
        None => {
            warnings.extend(io_warnings);
            warnings.push(format!(
                "missing DashboardSnapshot migration at {}",
                migration_path.display()
            ));
            return finalize(started, warnings, evidence);
        }
    };

    let rust_src = match read_text(&rust_contract_path, &mut io_warnings) {
        Some(body) => body,
        None => {
            warnings.extend(io_warnings);
            warnings.push(format!(
                "missing Rust snapshot contract at {}",
                rust_contract_path.display()
            ));
            return finalize(started, warnings, evidence);
        }
    };

    warnings.extend(io_warnings);

    let sql_columns = parse_create_table_columns(&migration_sql);
    if sql_columns.is_empty() {
        warnings.push(format!(
            "could not parse CREATE TABLE columns from {}",
            migration_path.display()
        ));
    } else {
        evidence.push(EvidenceItem {
            kind: "schema".to_string(),
            path: migration_path.display().to_string(),
            line: None,
            detail: format!("Prisma migration defines {} columns", sql_columns.len()),
        });
    }

    if !rust_src.contains("DASHBOARD_SNAPSHOT_COLUMNS") {
        warnings.push(format!(
            "{} must declare DASHBOARD_SNAPSHOT_COLUMNS",
            rust_contract_path.display()
        ));
    }

    for col in RUST_COLUMNS {
        if !sql_columns.iter().any(|c| c == col) {
            warnings.push(format!(
                "Rust contract column `{col}` missing from Prisma migration"
            ));
        }
    }

    for col in &sql_columns {
        if !RUST_COLUMNS.contains(&col.as_str()) {
            warnings.push(format!(
                "Prisma migration column `{col}` not listed in Rust DASHBOARD_SNAPSHOT_COLUMNS — update example-revops-snapshot SQL"
            ));
        }
    }

    finalize(started, warnings, evidence)
}

fn parse_create_table_columns(sql: &str) -> Vec<String> {
    let re = Regex::new(r#"^\s*"(\w+)"\s+(TEXT|JSONB|TIMESTAMP|BOOLEAN|INTEGER)"#).expect("regex");
    sql.lines()
        .filter_map(|line| {
            re.captures(line)
                .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        })
        .collect()
}

fn finalize(started: Instant, warnings: Vec<String>, evidence: Vec<EvidenceItem>) -> QueryEnvelope {
    let summary = if warnings.is_empty() {
        "DashboardSnapshot Prisma migration matches Rust writer column contract".to_string()
    } else {
        format!(
            "DashboardSnapshot schema drift: {} warning(s)",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_revops_snapshot_schema_sync"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.98 } else { 0.72 },
        entities: vec![json!({
            "rust_columns": RUST_COLUMNS,
            "warning_count": warnings.len(),
        })],
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dashboard_snapshot_migration_columns() {
        let sql = r#"
CREATE TABLE "DashboardSnapshot" (
  "id" TEXT NOT NULL,
  "snapshotKey" TEXT NOT NULL,
  "scopeId" TEXT NOT NULL DEFAULT 'global',
  "payload" JSONB NOT NULL
);
"#;
        let cols = parse_create_table_columns(sql);
        assert!(cols.contains(&"id".to_string()));
        assert!(cols.contains(&"snapshotKey".to_string()));
    }
}
