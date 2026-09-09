//! Jai Pay Supabase Session Pooler contract doctor.
//!
//! Regression class: Vercel runtime or Prisma CLI drifting back to Supabase's
//! transaction pooler, losing the Vercel pool lifecycle hook, or failing to
//! classify pooler connection timeouts as backpressure instead of 500s.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct JaipaySupabaseSessionPoolerDoctor;

impl Doctor for JaipaySupabaseSessionPoolerDoctor {
    fn name(&self) -> &'static str {
        "jaipay-supabase-session-pooler"
    }

    fn description(&self) -> &'static str {
        "Ensures Jai Pay rewrites Supabase transaction-pooler URLs to the Session Pooler, keeps Vercel pool lifecycle hooks, and degrades pooler timeouts as DB backpressure."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_jaipay_supabase_session_pooler(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
}

pub fn doctor_jaipay_supabase_session_pooler(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let checks = [
        Check {
            path: "jai-pay/src/lib/prisma.ts",
            label: "Jai Pay runtime Prisma pool",
            needles: &[
                "function isSupabaseHost",
                "export function normalizeDatabaseUrlForRuntime",
                "url.port === \"6543\"",
                "url.searchParams.get(\"pgbouncer\") === \"true\"",
                "url.port = \"5432\"",
                "url.searchParams.delete(\"pgbouncer\")",
                "url.searchParams.set(\"sslmode\", \"no-verify\")",
                "const databaseUrl = normalizeDatabaseUrlForRuntime(process.env.DATABASE_URL)",
                "connectionString: databaseUrl",
                "shouldUseDatabaseTls(databaseUrl)",
                "rejectUnauthorized: false",
                "const defaultPoolMax = process.env.VERCEL ? 1 : 10",
                "process.env.VERCEL ? 1_000 : 30_000",
                "attachDatabasePool(pool)",
                "connection terminated due to connection timeout",
                "timeout expired",
                "unable to check out connection from the pool",
            ],
        },
        Check {
            path: "jai-pay/prisma.config.ts",
            label: "Jai Pay Prisma CLI datasource",
            needles: &[
                "function isSupabaseHost",
                "function normalizeDatabaseUrlForPrisma",
                "url.port === \"6543\"",
                "url.searchParams.get(\"pgbouncer\") === \"true\"",
                "url.port = \"5432\"",
                "url.searchParams.delete(\"pgbouncer\")",
                "url.searchParams.set(\"sslmode\", \"require\")",
                "const directStr = process.env.DIRECT_URL?.trim()",
                "normalizeDatabaseUrlForPrisma(directStr)",
                "normalizeDatabaseUrlForPrisma(env(\"DATABASE_URL\"))",
            ],
        },
        Check {
            path: "jai-pay/src/lib/prisma.test.ts",
            label: "Jai Pay Prisma pool regression tests",
            needles: &[
                "attaches the pg pool to the Vercel function lifecycle",
                "does not leak database credentials in target diagnostics",
                "normalizes Supabase runtime URLs to session pooler TLS",
                "tracks the backpressure circuit window",
                "classifies Supabase pooler timeout variants as backpressure",
                "Connection terminated due to connection timeout",
                "timeout expired",
            ],
        },
    ];

    for check in checks {
        let path = root.join(check.path);
        let Some(src) = read_text(&path, &mut warnings) else {
            continue;
        };
        let missing: Vec<&str> = check
            .needles
            .iter()
            .copied()
            .filter(|needle| !src.contains(needle))
            .collect();
        if missing.is_empty() {
            evidence.push(EvidenceItem {
                kind: "jaipay_session_pooler_contract".to_string(),
                path: check.path.to_string(),
                line: find_line(&src, check.needles[0]),
                detail: format!("{} contains required Session Pooler markers", check.label),
            });
        } else {
            warnings.push(format!(
                "[jaipay-supabase-session-pooler] {} missing marker(s): {}",
                check.label,
                missing.join(", ")
            ));
        }
        entities.push(json!({
            "path": check.path,
            "label": check.label,
            "missing": missing,
        }));
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_jaipay_supabase_session_pooler"),
        kind: "doctor".to_string(),
        summary: format!(
            "Jai Pay Supabase Session Pooler contract checked: {} warning(s)",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.64 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "doctor": "jaipay-supabase-session-pooler",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::doctor_jaipay_supabase_session_pooler;
    use std::fs;
    use std::path::{Path, PathBuf};

    const RUNTIME: &str = r#"
        import { attachDatabasePool } from "@vercel/functions";
        function isDatabaseBackpressureError(error: unknown): boolean {
          const lower = String(error).toLowerCase();
          return lower.includes("connection terminated due to connection timeout") ||
            lower.includes("timeout expired") ||
            lower.includes("unable to check out connection from the pool");
        }
        function isSupabaseHost(hostname: string): boolean {
          return hostname.toLowerCase().endsWith(".supabase.com");
        }
        export function normalizeDatabaseUrlForRuntime(raw: string | undefined): string | undefined {
          const url = new URL(raw || "");
          if (isSupabaseHost(url.hostname) && (url.port === "6543" || url.searchParams.get("pgbouncer") === "true")) {
            url.port = "5432";
            url.searchParams.delete("pgbouncer");
          }
          url.searchParams.set("sslmode", "no-verify");
          return url.toString();
        }
        function shouldUseDatabaseTls(raw: string): boolean {
          const url = new URL(raw);
          return isSupabaseHost(url.hostname) && url.searchParams.get("sslmode") !== "disable";
        }
        const databaseUrl = normalizeDatabaseUrlForRuntime(process.env.DATABASE_URL);
        const defaultPoolMax = process.env.VERCEL ? 1 : 10;
        const idleTimeoutMillis = process.env.VERCEL ? 1_000 : 30_000;
        const pool = new Pool({
          connectionString: databaseUrl,
          ssl: databaseUrl && shouldUseDatabaseTls(databaseUrl) ? { rejectUnauthorized: false } : undefined,
        });
        attachDatabasePool(pool);
    "#;

    const PRISMA_CONFIG: &str = r#"
        import { defineConfig, env } from "prisma/config";
        function isSupabaseHost(hostname: string): boolean {
          return hostname.toLowerCase().endsWith(".supabase.com");
        }
        function normalizeDatabaseUrlForPrisma(raw: string | undefined): string | undefined {
          const url = new URL(raw || "");
          if (isSupabaseHost(url.hostname) && (url.port === "6543" || url.searchParams.get("pgbouncer") === "true")) {
            url.port = "5432";
            url.searchParams.delete("pgbouncer");
          }
          url.searchParams.set("sslmode", "require");
          return url.toString();
        }
        const directStr = process.env.DIRECT_URL?.trim();
        const cliDatabaseUrl = directStr
          ? normalizeDatabaseUrlForPrisma(directStr)
          : normalizeDatabaseUrlForPrisma(env("DATABASE_URL"));
        export default defineConfig({ datasource: { url: cliDatabaseUrl as string } });
    "#;

    const TESTS: &str = r#"
        it("attaches the pg pool to the Vercel function lifecycle", () => {});
        it("does not leak database credentials in target diagnostics", () => {});
        it("normalizes Supabase runtime URLs to session pooler TLS", () => {});
        it("tracks the backpressure circuit window", () => {});
        it("classifies Supabase pooler timeout variants as backpressure", () => {
          expect("Connection terminated due to connection timeout").toBeTruthy();
          expect("timeout expired").toBeTruthy();
        });
    "#;

    fn temp_repo(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio-jaipay-session-pooler-{name}-{}",
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

    #[test]
    fn healthy_contract_has_no_warnings() {
        let root = temp_repo("healthy");
        write(&root, "jai-pay/src/lib/prisma.ts", RUNTIME);
        write(&root, "jai-pay/prisma.config.ts", PRISMA_CONFIG);
        write(&root, "jai-pay/src/lib/prisma.test.ts", TESTS);

        let envelope = doctor_jaipay_supabase_session_pooler(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_timeout_classification_warns() {
        let root = temp_repo("missing-timeout");
        write(
            &root,
            "jai-pay/src/lib/prisma.ts",
            &RUNTIME.replace("timeout expired", "some other timeout"),
        );
        write(&root, "jai-pay/prisma.config.ts", PRISMA_CONFIG);
        write(&root, "jai-pay/src/lib/prisma.test.ts", TESTS);

        let envelope = doctor_jaipay_supabase_session_pooler(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("Jai Pay runtime Prisma pool")),
            "{:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(root);
    }
}
