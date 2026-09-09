//! Supabase runtime URL-shape doctor.
//!
//! Static drift class: a Supabase Postgres URL silently lands in production
//! with the wrong shape and the runtime fails *after* TLS — the failure mode
//! looks like a pool exhaustion timeout, but the real cause is the URL.
//!
//! Four rules:
//!
//! - `supabase_pooler_user_shape` — for hosts ending in `.pooler.supabase.com`,
//!   the URL's userinfo must be the project-ref form (`postgres.<project-ref>`),
//!   not plain `postgres`. The shared pooler routes by user; a bare `postgres`
//!   user against a pooler host accepts TLS and then silently times out at
//!   auth/route lookup.
//! - `supabase_transaction_pooler_pgbouncer` — when port is `6543` (transaction
//!   pooler), the URL must carry `pgbouncer=true`. Prisma's prepared-statement
//!   cache is incompatible with transaction-mode pooling; without the flag the
//!   pool silently misroutes prepared statements.
//! - `supabase_connection_limit_serverless` — when the URL points at a pooler
//!   host, `connection_limit` must be present and ≤ 10. Serverless deploys
//!   fan out beyond Supabase's per-project slot count without this cap.
//! - `supabase_direct_distinct_from_pooler` — `DATABASE_URL` (runtime) and
//!   `DIRECT_URL` (migrations) must point at distinct hosts. If both go through
//!   the pooler, migrations can't establish session-level state; if both go
//!   direct, runtime burns the project's connection budget.
//!
//! Scope: declared values in `deploy/profiles/*.env`,
//! `deploy/secret-sets/*.env.example`, root `.env*` files, and per-app
//! `jai-pay/.env*`. Runtime-only values (Vercel env vars, k8s secrets) are
//! out of scope — they don't live on disk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::diagnostics::{Diagnostic, Severity};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct SupabaseRuntimeShapeDoctor;

impl Doctor for SupabaseRuntimeShapeDoctor {
    fn name(&self) -> &'static str {
        "supabase-runtime-shape"
    }

    fn description(&self) -> &'static str {
        "Validates Supabase Postgres URL shapes across declared env files: \
         pooler hosts require project-ref user form, transaction pooler \
         requires pgbouncer=true, serverless deploys need a connection_limit \
         cap, and DATABASE_URL/DIRECT_URL must point at distinct hosts."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_supabase_runtime_shape(root)
    }
}

/// One observed (path, line, value) tuple for `DATABASE_URL` or `DIRECT_URL`.
#[derive(Debug, Clone)]
struct UrlSighting {
    path: PathBuf,
    line: usize,
    var_name: String,
    raw_value: String,
}

/// Parsed shape of a postgres URL, with only the bits this doctor needs.
#[derive(Debug, Clone)]
struct ParsedDsn {
    user: String,
    host: String,
    port: Option<String>,
    query_params: BTreeMap<String, String>,
}

pub fn doctor_supabase_runtime_shape(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();
    let mut sightings: Vec<UrlSighting> = Vec::new();

    let mut scan_paths = candidate_env_files(root);
    // Allow CI / operators to scan runtime-only env values pulled from
    // Vercel / k8s, by pointing at the dump path(s) via env var. Closes
    // the structural blind spot: on-disk declarations only see local-dev
    // DSNs; production DSNs live in Vercel env vars and never hit disk.
    //
    //   vercel env pull /tmp/prod.env --environment=production --yes
    //   LEIO_SUPABASE_EXTRA_ENV_FILES=/tmp/prod.env leio-code doctor supabase-runtime-shape --strict
    //
    // Multiple paths separated by `:` (POSIX PATH-style).
    if let Ok(extras) = std::env::var("LEIO_SUPABASE_EXTRA_ENV_FILES") {
        for raw in extras.split(':') {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            scan_paths.push(PathBuf::from(trimmed));
        }
    }

    for path in scan_paths {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (line_idx, line) in content.lines().enumerate() {
            let line_no = line_idx + 1;
            for prefix in &["DATABASE_URL", "DIRECT_URL"] {
                if let Some(value) = extract_assigned_value(line, prefix) {
                    if !value.starts_with("postgres") {
                        continue;
                    }
                    sightings.push(UrlSighting {
                        path: path.clone(),
                        line: line_no,
                        var_name: (*prefix).to_string(),
                        raw_value: value.to_string(),
                    });
                }
            }
        }
    }

    // Per-sighting URL-shape checks.
    for sighting in &sightings {
        let Some(dsn) = parse_dsn(&sighting.raw_value) else {
            continue;
        };

        let is_pooler = dsn.host.ends_with(".pooler.supabase.com");
        let is_supabase = is_supabase_host(&dsn.host);
        if !is_supabase {
            continue;
        }

        let path_str = relative(&sighting.path, root);

        // Rule 1: pooler hosts require project-ref user form.
        if is_pooler && !dsn.user.contains('.') {
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                rule_id: "supabase_pooler_user_shape".to_string(),
                path: Some(path_str.clone()),
                line: Some(sighting.line),
                message: format!(
                    "{} points at pooler host `{}` with user `{}`. \
                     Supabase shared pooler requires the project-ref form \
                     (`postgres.<project-ref>`); a plain `postgres` user \
                     accepts TLS and then silently times out at auth.",
                    sighting.var_name, dsn.host, dsn.user
                ),
            });
            evidence.push(EvidenceItem {
                kind: "supabase_runtime_shape".to_string(),
                path: path_str.clone(),
                line: Some(sighting.line),
                detail: format!(
                    "{} user `{}` against pooler host `{}` — expected `postgres.<project-ref>`",
                    sighting.var_name, dsn.user, dsn.host
                ),
            });
        }

        // Rule 2: transaction pooler (port 6543) requires pgbouncer=true.
        if is_pooler && dsn.port.as_deref() == Some("6543") {
            let pgbouncer = dsn.query_params.get("pgbouncer").map(String::as_str);
            if pgbouncer != Some("true") {
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    rule_id: "supabase_transaction_pooler_pgbouncer".to_string(),
                    path: Some(path_str.clone()),
                    line: Some(sighting.line),
                    message: format!(
                        "{} targets the transaction pooler (port 6543) but does \
                         not carry `pgbouncer=true`. Prisma's prepared-statement \
                         cache is incompatible with transaction-mode pooling — \
                         the flag is mandatory.",
                        sighting.var_name
                    ),
                });
                evidence.push(EvidenceItem {
                    kind: "supabase_runtime_shape".to_string(),
                    path: path_str.clone(),
                    line: Some(sighting.line),
                    detail: format!(
                        "{} on pooler:6543 missing pgbouncer=true (got `{}`)",
                        sighting.var_name,
                        pgbouncer.unwrap_or("absent")
                    ),
                });
            }
        }

        // Rule 3: pooler hosts need connection_limit caps for serverless.
        if is_pooler {
            let cap = dsn
                .query_params
                .get("connection_limit")
                .and_then(|v| v.parse::<u32>().ok());
            match cap {
                Some(n) if n <= 10 => {}
                Some(n) => {
                    diagnostics.push(Diagnostic {
                        severity: Severity::Warning,
                        rule_id: "supabase_connection_limit_serverless".to_string(),
                        path: Some(path_str.clone()),
                        line: Some(sighting.line),
                        message: format!(
                            "{} sets connection_limit={n} on pooler host. \
                             Serverless deploys (Vercel, Cloud Run) fan out per \
                             instance; values > 10 exhaust Supabase's per-project \
                             connection budget. Recommended: 1.",
                            sighting.var_name
                        ),
                    });
                }
                None => {
                    diagnostics.push(Diagnostic {
                        severity: Severity::Warning,
                        rule_id: "supabase_connection_limit_serverless".to_string(),
                        path: Some(path_str.clone()),
                        line: Some(sighting.line),
                        message: format!(
                            "{} on pooler host has no `connection_limit`. \
                             Add `connection_limit=1` for serverless deploys.",
                            sighting.var_name
                        ),
                    });
                }
            }
        }
    }

    // Rule 4: DATABASE_URL and DIRECT_URL must point at distinct hosts within
    // the same source file. (Multi-file deploys are aggregated by deploy
    // tooling — this check is only structural at the declaration site.)
    let mut by_file: BTreeMap<&Path, BTreeMap<&str, &UrlSighting>> = BTreeMap::new();
    for sighting in &sightings {
        by_file
            .entry(sighting.path.as_path())
            .or_default()
            .insert(sighting.var_name.as_str(), sighting);
    }
    for (file_path, vars) in &by_file {
        if let (Some(db), Some(direct)) = (vars.get("DATABASE_URL"), vars.get("DIRECT_URL")) {
            let db_host = parse_dsn(&db.raw_value).map(|d| d.host).unwrap_or_default();
            let direct_host = parse_dsn(&direct.raw_value)
                .map(|d| d.host)
                .unwrap_or_default();
            if !db_host.is_empty() && db_host == direct_host && is_supabase_host(&db_host) {
                let path_str = relative(file_path, root);
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    rule_id: "supabase_direct_distinct_from_pooler".to_string(),
                    path: Some(path_str.clone()),
                    line: Some(direct.line),
                    message: format!(
                        "DATABASE_URL and DIRECT_URL both point at `{}`. The \
                         runtime URL should target the pooler \
                         (`*.pooler.supabase.com`) and DIRECT_URL the direct \
                         host (`db.<project-ref>.supabase.co`) for migrations.",
                        db_host
                    ),
                });
                evidence.push(EvidenceItem {
                    kind: "supabase_runtime_shape".to_string(),
                    path: path_str,
                    line: Some(direct.line),
                    detail: format!(
                        "DATABASE_URL and DIRECT_URL share host `{}` (expected distinct)",
                        db_host
                    ),
                });
            }
        }
    }

    let summary = if diagnostics.is_empty() {
        "supabase-runtime-shape: no URL-shape issues detected".to_string()
    } else {
        format!(
            "supabase-runtime-shape: {} issue(s) across {} declared URL(s)",
            diagnostics.len(),
            sightings.len()
        )
    };

    let entities = diagnostics
        .iter()
        .map(|d| {
            json!({
                "rule_id": d.rule_id,
                "severity": d.severity.as_sarif_level(),
                "path": d.path.clone(),
                "line": d.line,
                "message": d.message,
            })
        })
        .collect();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_supabase_runtime_shape"),
        kind: "doctor".to_string(),
        summary,
        confidence: 90.0,
        entities,
        evidence,
        warnings: Vec::new(),
        meta: Some(json!({
            "diagnostics": diagnostics
                .iter()
                .map(|d| json!({
                    "rule_id": d.rule_id,
                    "severity": d.severity.as_sarif_level(),
                    "path": d.path.clone(),
                    "line": d.line,
                    "message": d.message,
                }))
                .collect::<Vec<_>>(),
            "urls_checked": sightings.len(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Enumerate the on-disk files this doctor should scan. Conservative — we
/// only read where we know declared env values live. Production-runtime
/// values (Vercel env vars, k8s secrets) are out of scope.
fn candidate_env_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();

    // Deploy-side declarations.
    for dir in &["deploy/profiles", "deploy/secret-sets"] {
        let path = root.join(dir);
        if let Ok(entries) = std::fs::read_dir(&path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file()
                    && let Some(name) = p.file_name().and_then(|n| n.to_str())
                    && (name.ends_with(".env") || name.ends_with(".env.example"))
                {
                    out.push(p);
                }
            }
        }
    }

    // Root + per-frontend-app `.env*` files. Don't recurse — only top-level.
    for dir in &[
        root.to_path_buf(),
        root.join("jai-pay"),
        root.join("health-audit-console"),
        root.join("example-ops"),
        root.join("example-hud"),
        root.join("assurant-ops"),
    ] {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if name == ".env"
                    || name == ".env.local"
                    || name == ".env.production"
                    || name == ".env.development"
                    || name == ".env.example"
                {
                    out.push(p);
                }
            }
        }
    }

    out
}

/// Extract the value assigned to `name` on a line of the form
/// `NAME=value` or `NAME = "value"`. Returns `None` if the line doesn't
/// match the expected key or the assignment is malformed.
fn extract_assigned_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return None;
    }
    // Skip `export ` prefix.
    let body = trimmed
        .strip_prefix("export ")
        .unwrap_or(trimmed)
        .trim_start();
    let rest = body.strip_prefix(name)?;
    let rest = rest.trim_start();
    let after_eq = rest.strip_prefix('=')?.trim_start();
    // Strip surrounding quotes if any. Inline comments are not stripped —
    // a postgres URL doesn't contain `#`, and `?` query params are part of
    // the value.
    let unquoted = if let Some(rest) = after_eq.strip_prefix('"') {
        rest.strip_suffix('"').unwrap_or(rest)
    } else if let Some(rest) = after_eq.strip_prefix('\'') {
        rest.strip_suffix('\'').unwrap_or(rest)
    } else {
        // Strip trailing whitespace.
        after_eq.trim_end()
    };
    if unquoted.is_empty() {
        return None;
    }
    Some(unquoted)
}

/// Minimal DSN parser for postgres URLs. Returns `None` for malformed input.
/// Hand-rolled to avoid adding a `url` crate dep just for four rules.
fn parse_dsn(raw: &str) -> Option<ParsedDsn> {
    // Strip scheme.
    let after_scheme = raw
        .strip_prefix("postgresql://")
        .or_else(|| raw.strip_prefix("postgres://"))?;

    // Split userinfo@hostinfo[/path][?query]
    let (userinfo, rest) = {
        let i = after_scheme.find('@')?;
        (&after_scheme[..i], &after_scheme[i + 1..])
    };

    let user = match userinfo.find(':') {
        Some(i) => userinfo[..i].to_string(),
        None => userinfo.to_string(),
    };

    // Strip path and query.
    let (host_port, query) = match rest.find('?') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let host_port = match host_port.find('/') {
        Some(i) => &host_port[..i],
        None => host_port,
    };

    let (host, port) = match host_port.rfind(':') {
        Some(i) => (
            host_port[..i].to_string(),
            Some(host_port[i + 1..].to_string()),
        ),
        None => (host_port.to_string(), None),
    };

    let mut query_params = BTreeMap::new();
    if !query.is_empty() {
        for kv in query.split('&') {
            if let Some(i) = kv.find('=') {
                query_params.insert(kv[..i].to_string(), kv[i + 1..].to_string());
            }
        }
    }

    Some(ParsedDsn {
        user,
        host,
        port,
        query_params,
    })
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn is_supabase_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host.ends_with(".pooler.supabase.com")
        || host.ends_with(".supabase.co")
        || host.ends_with(".supabase.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_assigned_value_handles_quoted_and_unquoted() {
        assert_eq!(
            extract_assigned_value("DATABASE_URL=foo", "DATABASE_URL"),
            Some("foo")
        );
        assert_eq!(
            extract_assigned_value("DATABASE_URL=\"foo\"", "DATABASE_URL"),
            Some("foo")
        );
        assert_eq!(
            extract_assigned_value("export DATABASE_URL=foo", "DATABASE_URL"),
            Some("foo")
        );
        assert_eq!(
            extract_assigned_value("# DATABASE_URL=foo", "DATABASE_URL"),
            None
        );
        assert_eq!(extract_assigned_value("OTHER=foo", "DATABASE_URL"), None);
    }

    #[test]
    fn parse_dsn_extracts_host_port_user_query() {
        let dsn = parse_dsn(
            "postgresql://postgres.abcd:hunter2@aws-1.pooler.supabase.com:6543/postgres?pgbouncer=true&connection_limit=1",
        )
        .unwrap();
        assert_eq!(dsn.user, "postgres.abcd");
        assert_eq!(dsn.host, "aws-1.pooler.supabase.com");
        assert_eq!(dsn.port.as_deref(), Some("6543"));
        assert_eq!(dsn.query_params.get("pgbouncer").unwrap(), "true");
        assert_eq!(dsn.query_params.get("connection_limit").unwrap(), "1");
    }

    #[test]
    fn parse_dsn_rejects_non_postgres_schemes() {
        assert!(parse_dsn("mysql://x@y/z").is_none());
        assert!(parse_dsn("not a url").is_none());
    }

    #[test]
    fn pooler_with_plain_postgres_user_fires() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("deploy/profiles")).unwrap();
        std::fs::write(
            root.join("deploy/profiles/prod.env"),
            "DATABASE_URL=postgresql://postgres:pw@aws-1.pooler.supabase.com:6543/postgres?pgbouncer=true&connection_limit=1\n",
        )
        .unwrap();
        let env = doctor_supabase_runtime_shape(root);
        let meta = env.meta.unwrap();
        let diagnostics = meta["diagnostics"].as_array().unwrap();
        let rule_ids: Vec<&str> = diagnostics
            .iter()
            .map(|d| d["rule_id"].as_str().unwrap())
            .collect();
        assert!(
            rule_ids.contains(&"supabase_pooler_user_shape"),
            "expected supabase_pooler_user_shape; got {rule_ids:?}"
        );
    }

    #[test]
    fn transaction_pooler_without_pgbouncer_fires() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("deploy/profiles")).unwrap();
        std::fs::write(
            root.join("deploy/profiles/prod.env"),
            "DATABASE_URL=postgresql://postgres.abc:pw@aws-1.pooler.supabase.com:6543/postgres?connection_limit=1\n",
        )
        .unwrap();
        let env = doctor_supabase_runtime_shape(root);
        let meta = env.meta.unwrap();
        let rule_ids: Vec<&str> = meta["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["rule_id"].as_str().unwrap())
            .collect();
        assert!(
            rule_ids.contains(&"supabase_transaction_pooler_pgbouncer"),
            "expected pgbouncer rule; got {rule_ids:?}"
        );
    }

    #[test]
    fn missing_connection_limit_warns() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("deploy/profiles")).unwrap();
        std::fs::write(
            root.join("deploy/profiles/prod.env"),
            "DATABASE_URL=postgresql://postgres.abc:pw@aws-1.pooler.supabase.com:6543/postgres?pgbouncer=true\n",
        )
        .unwrap();
        let env = doctor_supabase_runtime_shape(root);
        let meta = env.meta.unwrap();
        let rule_ids: Vec<&str> = meta["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["rule_id"].as_str().unwrap())
            .collect();
        assert!(
            rule_ids.contains(&"supabase_connection_limit_serverless"),
            "expected connection_limit rule; got {rule_ids:?}"
        );
    }

    #[test]
    fn well_formed_pooler_url_passes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("deploy/profiles")).unwrap();
        std::fs::write(
            root.join("deploy/profiles/prod.env"),
            "DATABASE_URL=postgresql://postgres.abc:pw@aws-1.pooler.supabase.com:6543/postgres?pgbouncer=true&connection_limit=1&sslmode=require\n",
        )
        .unwrap();
        let env = doctor_supabase_runtime_shape(root);
        let meta = env.meta.unwrap();
        let diagnostics = meta["diagnostics"].as_array().unwrap();
        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics; got {diagnostics:?}"
        );
    }

    #[test]
    fn direct_url_sharing_host_with_database_url_warns() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("deploy/profiles")).unwrap();
        std::fs::write(
            root.join("deploy/profiles/prod.env"),
            "DATABASE_URL=postgresql://postgres.abc:pw@aws-1.pooler.supabase.com:6543/postgres?pgbouncer=true&connection_limit=1\n\
             DIRECT_URL=postgresql://postgres.abc:pw@aws-1.pooler.supabase.com:6543/postgres?pgbouncer=true&connection_limit=1\n",
        )
        .unwrap();
        let env = doctor_supabase_runtime_shape(root);
        let meta = env.meta.unwrap();
        let rule_ids: Vec<&str> = meta["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["rule_id"].as_str().unwrap())
            .collect();
        assert!(
            rule_ids.contains(&"supabase_direct_distinct_from_pooler"),
            "expected distinct-host rule; got {rule_ids:?}"
        );
    }

    #[test]
    fn non_supabase_postgres_url_is_ignored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("deploy/profiles")).unwrap();
        std::fs::write(
            root.join("deploy/profiles/prod.env"),
            "DATABASE_URL=postgresql://postgres:pw@some-other-host.example.com:5432/postgres\n",
        )
        .unwrap();
        let env = doctor_supabase_runtime_shape(root);
        let meta = env.meta.unwrap();
        let diagnostics = meta["diagnostics"].as_array().unwrap();
        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics for non-supabase host; got {diagnostics:?}"
        );
    }

    #[test]
    fn local_database_and_direct_url_same_host_are_ignored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("jai-pay")).unwrap();
        std::fs::write(
            root.join("jai-pay/.env"),
            "DATABASE_URL=postgresql://jaipay:jaipay_dev@localhost:5433/jai_pay\n\
             DIRECT_URL=postgresql://jaipay:jaipay_dev@localhost:5433/jai_pay\n",
        )
        .unwrap();
        let env = doctor_supabase_runtime_shape(root);
        let meta = env.meta.unwrap();
        let diagnostics = meta["diagnostics"].as_array().unwrap();
        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics for local non-supabase DSNs; got {diagnostics:?}"
        );
    }
}
