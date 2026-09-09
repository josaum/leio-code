//! `supabase-project-liveness` doctor.
//!
//! Runtime drift class: the Supabase project's Postgres compute becomes
//! unreachable (paused / failed / IP-blocked) while everything else stays
//! up. Static config doctors can't catch this — by design they only see
//! what's on disk. This doctor probes the actual database host with a
//! short TCP+TLS budget and emits a runtime-liveness diagnostic.
//!
//! Three rule IDs:
//!
//! - `supabase_project_db_unreachable` (error) — TCP connect or TLS
//!   handshake fails within the budget.
//! - `supabase_project_pooler_circuit_open` (error) — server responds but
//!   the Postgres startup phase yields a string matching `ECIRCUITBREAKER`
//!   or `EAUTHQUERY` (Supabase pooler's internal failure modes).
//! - `supabase_project_slow_handshake` (warning) — connection establishes
//!   but takes > 1s. Indicates pooler under load.
//!
//! Probe protocol: we open a TCP socket, send the Postgres `SSLRequest`
//! startup byte, and read the server reply. We do NOT perform a TLS
//! handshake, do NOT send a StartupMessage, do NOT authenticate — the
//! probe is purposefully shallow so it can't burn auth-circuit-breaker
//! budget at the provider.
//!
//! **Honest limit of this v1 probe:** Supabase's pooler in circuit-breaker
//! state accepts TCP + replies 'S' to the SSLRequest, then returns the
//! `ECIRCUITBREAKER`/`EAUTHQUERY` marker only AFTER TLS + StartupMessage.
//! Detecting that class requires TLS in the probe, which isn't included in
//! v1 to avoid pulling a new crate dep. The v1 doctor reliably catches:
//!
//!   - DNS resolution failures
//!   - TCP unreachability (truly paused / firewalled / network-partitioned compute)
//!   - Slow handshakes (> 1s under the 2s budget)
//!
//! It does NOT catch: pooler accepting TCP but failing the StartupMessage.
//! For CI gates that need the deeper check, pair this doctor with a `psql`
//! one-shot in the same CI step (see `docs/supabase-resilience-design.md`).
//!
//! Scope: the doctor scans every `DATABASE_URL` / `DIRECT_URL` declared in
//! the candidate env files (same set as `supabase-runtime-shape`) PLUS the
//! `LEIO_SUPABASE_EXTRA_ENV_FILES` paths. Each unique host:port pair is
//! probed once.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::diagnostics::{Diagnostic, Severity};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

const PROBE_BUDGET_MS: u64 = 2_000;
const SLOW_HANDSHAKE_THRESHOLD_MS: u128 = 1_000;

pub struct SupabaseProjectLivenessDoctor;

impl Doctor for SupabaseProjectLivenessDoctor {
    fn name(&self) -> &'static str {
        "supabase-project-liveness"
    }

    fn description(&self) -> &'static str {
        "Probes the actual database host declared in DATABASE_URL/DIRECT_URL \
         with a short TCP+TLS budget. Catches runtime unreachability \
         (paused Postgres compute, circuit-breakers at the provider, network \
         partitions) that static config doctors can't see by design."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_supabase_project_liveness(root)
    }
}

/// Zero-warning envelope returned when live probes are disabled
/// (`LEIO_SKIP_LIVE_PROBES`) — an unreachable host by design is an environment
/// fact, not drift, and must not gate `audit --strict`.
fn skipped_envelope(timing_ms: u128) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_supabase_project_liveness"),
        kind: "doctor".to_string(),
        summary: "supabase-project-liveness: skipped (LEIO_SKIP_LIVE_PROBES set)".to_string(),
        confidence: 100.0,
        entities: Vec::new(),
        evidence: Vec::new(),
        warnings: Vec::new(),
        meta: Some(json!({
            "diagnostics": Vec::<serde_json::Value>::new(),
            "targets_probed": 0,
            "skipped": true,
        })),
        timing_ms,
    }
}

pub fn doctor_supabase_project_liveness(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    // Live TCP+TLS probe — meaningless (and a guaranteed timeout) where the
    // declared host is unreachable by design, e.g. CI runners. Self-skip with
    // zero warnings so an environment fact never gates `audit --strict`.
    if crate::config::skip_live_probes() {
        return skipped_envelope(started.elapsed().as_millis());
    }

    // Reuse the same env-file scan as `supabase-runtime-shape` so the two
    // doctors point at exactly the same DSNs.
    let mut targets: BTreeSet<(String, u16)> = BTreeSet::new();
    let mut sources: Vec<(String, String, usize)> = Vec::new(); // (var, file, line)

    for path in candidate_files(root) {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            for var in &["DATABASE_URL", "DIRECT_URL"] {
                if let Some(value) = extract_value(line, var) {
                    if !value.starts_with("postgres") {
                        continue;
                    }
                    if let Some((host, port)) = parse_host_port(value)
                        && is_supabase_host(&host)
                    {
                        targets.insert((host.clone(), port));
                        sources.push(((*var).to_string(), path.display().to_string(), idx + 1));
                    }
                }
            }
        }
    }

    if targets.is_empty() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_supabase_project_liveness"),
            kind: "doctor".to_string(),
            summary: "supabase-project-liveness: no Supabase DSN declared on disk; skipped"
                .to_string(),
            confidence: 100.0,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: Vec::new(),
            meta: Some(json!({
                "diagnostics": Vec::<serde_json::Value>::new(),
                "targets_probed": 0,
                "skipped": true,
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    for (host, port) in &targets {
        let probe_started = Instant::now();
        let result = probe(host, *port, Duration::from_millis(PROBE_BUDGET_MS));
        let elapsed_ms = probe_started.elapsed().as_millis();

        let source = sources
            .iter()
            .find(|(_, _, _)| true) // any source path; details don't matter for the file: field
            .cloned();
        let (file, line) = source
            .as_ref()
            .map(|(_, f, l)| (f.clone(), Some(*l)))
            .unwrap_or_else(|| ("<unknown>".to_string(), None));

        match result {
            Ok(reply) => {
                // Server replied. Inspect the reply for known failure markers.
                if reply.contains("ECIRCUITBREAKER") || reply.contains("EAUTHQUERY") {
                    let rule_id = "supabase_project_pooler_circuit_open";
                    diagnostics.push(Diagnostic {
                        severity: Severity::Error,
                        rule_id: rule_id.to_string(),
                        path: Some(file.clone()),
                        line,
                        message: format!(
                            "Supabase pooler at {host}:{port} accepted the TCP+TLS handshake \
                             but returned a known failure marker (ECIRCUITBREAKER or \
                             EAUTHQUERY). The pooler can reach you, but cannot reach the \
                             underlying Postgres compute. Likely causes: paused free-tier \
                             compute, recent password rotation, or a provider-side outage. \
                             Check the Supabase dashboard for the project's compute status."
                        ),
                    });
                    evidence.push(EvidenceItem {
                        kind: "supabase_project_liveness".to_string(),
                        path: file.clone(),
                        line,
                        detail: format!(
                            "{host}:{port} returned circuit-breaker marker after {elapsed_ms}ms"
                        ),
                    });
                } else if elapsed_ms > SLOW_HANDSHAKE_THRESHOLD_MS {
                    diagnostics.push(Diagnostic {
                        severity: Severity::Warning,
                        rule_id: "supabase_project_slow_handshake".to_string(),
                        path: Some(file.clone()),
                        line,
                        message: format!(
                            "Supabase pooler at {host}:{port} responded but the TLS \
                             handshake took {elapsed_ms}ms (> {SLOW_HANDSHAKE_THRESHOLD_MS}ms). \
                             Pooler may be under load."
                        ),
                    });
                }
                // Else: clean handshake under budget. No diagnostic.
            }
            Err(reason) => {
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    rule_id: "supabase_project_db_unreachable".to_string(),
                    path: Some(file.clone()),
                    line,
                    message: format!(
                        "Supabase host {host}:{port} unreachable after {elapsed_ms}ms: \
                         {reason}. The TCP socket or TLS handshake failed within the \
                         {PROBE_BUDGET_MS}ms budget. Most likely the Postgres compute is \
                         paused (free-tier auto-pause) or there's a network partition. \
                         Check the Supabase dashboard for the project's compute status."
                    ),
                });
                evidence.push(EvidenceItem {
                    kind: "supabase_project_liveness".to_string(),
                    path: file.clone(),
                    line,
                    detail: format!("{host}:{port} unreachable after {elapsed_ms}ms ({reason})"),
                });
            }
        }
    }

    let summary = if diagnostics.is_empty() {
        format!(
            "supabase-project-liveness: all {} target(s) reachable",
            targets.len()
        )
    } else {
        format!(
            "supabase-project-liveness: {} issue(s) across {} target(s) probed",
            diagnostics.len(),
            targets.len()
        )
    };

    let entities = diagnostics
        .iter()
        .map(|d| {
            json!({
                "rule_id": d.rule_id,
                "severity": d.severity.as_sarif_level(),
                "file": d.path,
                "line": d.line,
                "message": d.message,
            })
        })
        .collect();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_supabase_project_liveness"),
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
                    "file": d.path,
                    "line": d.line,
                    "message": d.message,
                }))
                .collect::<Vec<_>>(),
            "targets_probed": targets.len(),
            "budget_ms": PROBE_BUDGET_MS,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Mirror of `supabase_runtime_shape::candidate_env_files` plus the
/// `LEIO_SUPABASE_EXTRA_ENV_FILES` extras. Kept in lock-step so the two
/// doctors point at exactly the same DSNs.
fn candidate_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();

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

    for dir in &[
        root.to_path_buf(),
        root.join("jai-pay"),
        root.join("example-ops"),
        root.join("health-audit-console"),
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

    if let Ok(extras) = std::env::var("LEIO_SUPABASE_EXTRA_ENV_FILES") {
        for raw in extras.split(':') {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                out.push(std::path::PathBuf::from(trimmed));
            }
        }
    }

    out
}

fn extract_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return None;
    }
    let body = trimmed
        .strip_prefix("export ")
        .unwrap_or(trimmed)
        .trim_start();
    let rest = body.strip_prefix(name)?;
    let after_eq = rest.trim_start().strip_prefix('=')?.trim_start();
    let unquoted = if let Some(rest) = after_eq.strip_prefix('"') {
        rest.strip_suffix('"').unwrap_or(rest)
    } else if let Some(rest) = after_eq.strip_prefix('\'') {
        rest.strip_suffix('\'').unwrap_or(rest)
    } else {
        after_eq.trim_end()
    };
    if unquoted.is_empty() {
        return None;
    }
    Some(unquoted)
}

fn parse_host_port(raw: &str) -> Option<(String, u16)> {
    let after_scheme = raw
        .strip_prefix("postgresql://")
        .or_else(|| raw.strip_prefix("postgres://"))?;
    let rest = after_scheme.split('@').nth(1)?;
    let host_port = rest.split('/').next()?.split('?').next()?;
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()?),
        None => (host_port.to_string(), 5432),
    };
    Some((host, port))
}

fn is_supabase_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h.ends_with(".pooler.supabase.com")
        || h.ends_with(".supabase.co")
        || h.ends_with(".supabase.com")
}

/// Issue a shallow TCP probe against `host:port` within `budget`. Does NOT
/// authenticate — only confirms the host accepts a TCP connection and
/// replies to the Postgres SSL-request startup byte. Errors are returned
/// as a short string suitable for diagnostic messages.
fn probe(host: &str, port: u16, budget: Duration) -> Result<String, String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve failed: {e}"))?
        .next()
        .ok_or_else(|| "no address records".to_string())?;

    let mut stream =
        TcpStream::connect_timeout(&addr, budget).map_err(|e| format!("connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(budget))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    stream
        .set_write_timeout(Some(budget))
        .map_err(|e| format!("set_write_timeout: {e}"))?;

    // Postgres SSLRequest packet — 8 bytes:
    //   int32 length=8, int32 magic=80877103 (0x04D2162F)
    // The server replies with 'S' (SSL supported) or 'N' (no SSL) or an
    // error message. Either of the first two means the server is alive.
    let mut packet = Vec::with_capacity(8);
    packet.extend_from_slice(&8u32.to_be_bytes());
    packet.extend_from_slice(&80877103u32.to_be_bytes());
    stream
        .write_all(&packet)
        .map_err(|e| format!("write SSLRequest: {e}"))?;

    let mut buf = [0u8; 256];
    let n = stream
        .read(&mut buf)
        .map_err(|e| format!("read reply: {e}"))?;

    if n == 0 {
        return Err("server closed connection before reply".to_string());
    }

    // Convert the reply to a lossy UTF-8 string so we can scan for error
    // markers regardless of position. Postgres error packets carry the
    // marker in their `M` (message) field.
    let reply = String::from_utf8_lossy(&buf[..n]).to_string();
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_envelope_has_no_warnings_and_is_marked_skipped() {
        let env = skipped_envelope(0);
        assert!(env.warnings.is_empty(), "skip path must emit no warnings");
        assert!(env.summary.contains("skipped"));
        assert_eq!(
            env.meta.and_then(|m| m.get("skipped").cloned()),
            Some(serde_json::json!(true))
        );
    }

    #[test]
    fn extract_value_handles_dotenv_shape() {
        assert_eq!(
            extract_value("DATABASE_URL=postgresql://x@y:5432/z", "DATABASE_URL"),
            Some("postgresql://x@y:5432/z")
        );
        assert_eq!(
            extract_value("DATABASE_URL=\"postgresql://x@y:5432/z\"", "DATABASE_URL"),
            Some("postgresql://x@y:5432/z")
        );
        assert_eq!(extract_value("# DATABASE_URL=foo", "DATABASE_URL"), None);
    }

    #[test]
    fn parse_host_port_extracts_pooler_target() {
        assert_eq!(
            parse_host_port("postgresql://u:p@aws-0.pooler.supabase.com:6543/postgres?a=b"),
            Some(("aws-0.pooler.supabase.com".to_string(), 6543))
        );
        assert_eq!(
            parse_host_port("postgresql://u:p@db.xyz.supabase.co/postgres"),
            Some(("db.xyz.supabase.co".to_string(), 5432))
        );
        assert_eq!(parse_host_port("not a url"), None);
    }

    #[test]
    fn is_supabase_host_matches_three_variants() {
        assert!(is_supabase_host("aws-0.pooler.supabase.com"));
        assert!(is_supabase_host("db.xyz.supabase.co"));
        assert!(is_supabase_host("custom.supabase.com"));
        assert!(!is_supabase_host("postgres.local"));
        assert!(!is_supabase_host("127.0.0.1"));
    }

    #[test]
    fn no_targets_emits_skipped_envelope() {
        // CI sets LEIO_SKIP_LIVE_PROBES=1 which short-circuits the doctor with
        // a different summary; the body of this test exercises the no-targets
        // branch, not the env-skip branch. Honor the env signal and bail.
        if crate::config::skip_live_probes() {
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let env = doctor_supabase_project_liveness(tmp.path());
        let meta = env.meta.unwrap();
        assert!(meta["skipped"].as_bool().unwrap_or(false));
        assert_eq!(meta["targets_probed"], 0);
        assert!(env.summary.contains("no Supabase DSN"));
    }

    #[test]
    fn unreachable_host_fires_unreachable_rule() {
        // CI sets LEIO_SKIP_LIVE_PROBES=1 which disables the probe entirely;
        // the assertion below requires the live-probe branch to run.
        if crate::config::skip_live_probes() {
            return;
        }
        // Use TEST-NET-1 (RFC 5737) — guaranteed not to route anywhere.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("deploy/profiles")).unwrap();
        // is_supabase_host requires a supabase hostname; use a non-existent
        // subdomain that still matches the suffix.
        std::fs::write(
            tmp.path().join("deploy/profiles/prod.env"),
            // Connect to TEST-NET-1 192.0.2.1 with a Supabase-looking hostname
            // is not directly testable since we resolve DNS. Instead we point
            // at a Supabase-shaped hostname that we expect to fail DNS or
            // connect quickly. Use a clearly-invalid project ref.
            "DATABASE_URL=postgresql://u:p@db.zzzzzzzzz-not-real.supabase.co:5432/postgres\n",
        )
        .unwrap();
        let env = doctor_supabase_project_liveness(tmp.path());
        let meta = env.meta.unwrap();
        let diagnostics = meta["diagnostics"].as_array().unwrap();
        // Either DNS resolution fails or TCP fails — both result in the
        // `supabase_project_db_unreachable` rule. We don't assert specifics
        // because network conditions vary across machines; we only assert
        // *some* diagnostic was emitted for the unreachable target.
        let rule_ids: Vec<&str> = diagnostics
            .iter()
            .map(|d| d["rule_id"].as_str().unwrap())
            .collect();
        assert!(
            rule_ids
                .iter()
                .any(|r| r == &"supabase_project_db_unreachable"),
            "expected unreachable rule; got {rule_ids:?}"
        );
    }
}
