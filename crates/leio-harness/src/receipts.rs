//! Native trio "receipts" check: does a lane's own output resolve the
//! evidence it cites? Ports the checks in
//! `skills/leio-trio/scripts/trio_receipts.py` into the harness runtime so a
//! day enforces them automatically, rather than depending on an agent
//! remembering to run a script.
//!
//! Pure and deterministic. Reads only the given text and the named repos'
//! `.leio-code/events/events.ndjson` PROV sidecars — no MCP calls, no
//! network. Reference Provider checks are id-shape only (`res:`/`snap:` 64 hex,
//! `cl:` 24 hex), never a live `reference_provider_verify`.

use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptsReport {
    pub cited_query_ids: Vec<String>,
    pub unresolved_query_ids: Vec<String>,
    pub reference_receipts: Vec<String>,
    pub reference_snapshots: Vec<String>,
    pub reference_claim_count: usize,
    pub malformed_reference_ids: Vec<String>,
    /// True when every cited query id resolved and every Reference Provider id is
    /// well-formed. A caller treats `false` as a fabrication signal.
    pub ok: bool,
}

/// Split `text` into identifier-shaped tokens (letters, digits, `_`, `-`,
/// `.`, `:`) on any other boundary (whitespace, backticks, punctuation). Real
/// query ids and Reference Provider ids always appear as whole tokens in practice
/// (inside backticks or comma/period-terminated prose), so matching a whole
/// token against each pattern is simpler and just as correct as an embedded
/// regex scan, with no external dependency.
fn tokenize(text: &str) -> Vec<&str> {
    text.split(|c: char| {
        !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == ':')
    })
    .filter(|token| !token.is_empty())
    .collect()
}

/// A LEIO query id: `<lowercase-kind>-<16 to 19 digit ns timestamp>`, e.g.
/// `context-1788477534952084000`.
fn is_query_id(token: &str) -> bool {
    let Some((kind, digits)) = token.rsplit_once('-') else {
        return false;
    };
    if kind.is_empty() || !kind.starts_with(|c: char| c.is_ascii_lowercase()) {
        return false;
    }
    if !kind
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' || c == '.')
    {
        return false;
    }
    (16..=19).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit())
}

fn extract_query_ids(text: &str) -> Vec<String> {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for token in tokenize(text) {
        if is_query_id(token) {
            ids.insert(token.to_owned());
        }
    }
    ids.into_iter().collect()
}

/// Tokens starting with `prefix` (e.g. `"res:"`) are split into well-formed
/// (hex part exactly `expected_hex_len` long) and malformed (any other
/// length) buckets.
fn extract_prefixed_ids(
    text: &str,
    prefix: &str,
    expected_hex_len: usize,
) -> (Vec<String>, Vec<String>) {
    let mut well_formed: BTreeSet<String> = BTreeSet::new();
    let mut malformed: BTreeSet<String> = BTreeSet::new();
    for token in tokenize(text) {
        let Some(hex_part) = token.strip_prefix(prefix) else {
            continue;
        };
        if hex_part.len() == expected_hex_len && hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
            well_formed.insert(token.to_owned());
        } else {
            malformed.insert(token.to_owned());
        }
    }
    (
        well_formed.into_iter().collect(),
        malformed.into_iter().collect(),
    )
}

/// Load every `query_id` field out of a repo's PROV sidecar
/// (`.leio-code/events/events.ndjson`, one JSON object per line). Missing
/// file or unreadable lines are silently skipped — a receipts check treats
/// "no sidecar" the same as "no ids resolve", never as an error.
fn load_query_ids(repo_root: &Path) -> BTreeSet<String> {
    let path = repo_root
        .join(".leio-code")
        .join("events")
        .join("events.ndjson");
    let mut ids = BTreeSet::new();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return ids;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(id) = value.get("query_id").and_then(|v| v.as_str()) {
            ids.insert(id.to_owned());
        }
    }
    ids
}

/// Scan `text` for LEIO query ids and Reference Provider ids, then resolve the
/// query ids against `query_id` fields in each of `leio_repo_roots`'
/// `.leio-code/events/events.ndjson`. Pass no repo roots to skip resolution
/// (every cited id is then reported as unresolved — a stricter default than
/// silently passing).
pub fn check_text<P: AsRef<Path>>(text: &str, leio_repo_roots: &[P]) -> ReceiptsReport {
    let cited_query_ids = extract_query_ids(text);
    let known: BTreeSet<String> = leio_repo_roots
        .iter()
        .flat_map(|root| load_query_ids(root.as_ref()))
        .collect();
    let unresolved_query_ids: Vec<String> = cited_query_ids
        .iter()
        .filter(|id| !known.contains(*id))
        .cloned()
        .collect();

    let (reference_receipts, malformed_res) = extract_prefixed_ids(text, "res:", 64);
    let (reference_snapshots, malformed_snap) = extract_prefixed_ids(text, "snap:", 64);
    let (reference_claims, malformed_cl) = extract_prefixed_ids(text, "cl:", 24);
    let mut malformed_reference_ids: BTreeSet<String> = malformed_res.into_iter().collect();
    malformed_reference_ids.extend(malformed_snap);
    malformed_reference_ids.extend(malformed_cl);
    let malformed_reference_ids: Vec<String> = malformed_reference_ids.into_iter().collect();

    let ok = unresolved_query_ids.is_empty() && malformed_reference_ids.is_empty();

    ReceiptsReport {
        cited_query_ids,
        unresolved_query_ids,
        reference_receipts,
        reference_snapshots,
        reference_claim_count: reference_claims.len(),
        malformed_reference_ids,
        ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_events(dir: &Path, query_ids: &[&str]) {
        let events_dir = dir.join(".leio-code").join("events");
        fs::create_dir_all(&events_dir).unwrap();
        let body: String = query_ids
            .iter()
            .map(|id| format!(r#"{{"query_id":"{id}","kind":"context"}}"#))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(events_dir.join("events.ndjson"), body).unwrap();
    }

    #[test]
    fn resolves_a_cited_query_id_present_in_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        write_events(dir.path(), &["context-1788477534952084000"]);
        let report = check_text(
            "Evidence: `context-1788477534952084000` shows the route.",
            &[dir.path()],
        );
        assert_eq!(report.cited_query_ids, vec!["context-1788477534952084000"]);
        assert!(report.unresolved_query_ids.is_empty());
        assert!(report.ok);
    }

    #[test]
    fn flags_a_cited_query_id_absent_from_every_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        write_events(dir.path(), &["context-1788477534952084000"]);
        let report = check_text(
            "Evidence: `find_symbol-1788478894304497000` (fabricated).",
            &[dir.path()],
        );
        assert_eq!(
            report.unresolved_query_ids,
            vec!["find_symbol-1788478894304497000"]
        );
        assert!(!report.ok);
    }

    #[test]
    fn no_repo_roots_means_every_cited_id_is_unresolved() {
        let report = check_text(
            "query id context-1788477534952084000 was used.",
            &[] as &[&Path],
        );
        assert_eq!(report.unresolved_query_ids.len(), 1);
        assert!(!report.ok);
    }

    #[test]
    fn text_with_no_ids_at_all_is_ok() {
        let report = check_text("Plain prose, no receipts anywhere.", &[] as &[&Path]);
        assert!(report.cited_query_ids.is_empty());
        assert!(report.ok);
    }

    #[test]
    fn well_formed_reference_ids_pass() {
        let receipt = format!("res:{}", "0".repeat(64));
        let snapshot = format!("snap:{}", "a".repeat(64));
        let claim = format!("cl:{}", "1".repeat(24));
        let report = check_text(&format!("{receipt} {snapshot} {claim}"), &[] as &[&Path]);
        assert_eq!(report.reference_receipts, vec![receipt]);
        assert_eq!(report.reference_snapshots, vec![snapshot]);
        assert_eq!(report.reference_claim_count, 1);
        assert!(report.malformed_reference_ids.is_empty());
        assert!(report.ok);
    }

    #[test]
    fn short_hex_after_reference_prefix_is_malformed() {
        // 6-char hex tail: too short to be a real receipt id.
        let report = check_text("res:abc123 snap:00 cl:deadbe", &[] as &[&Path]);
        assert_eq!(report.malformed_reference_ids.len(), 3);
        assert!(!report.ok);
    }

    #[test]
    fn non_hex_or_empty_reference_id_is_malformed() {
        let report = check_text("res:something snap:not-hex-either", &[] as &[&Path]);
        assert!(report.reference_receipts.is_empty());
        assert_eq!(report.malformed_reference_ids.len(), 2);
        assert!(!report.ok);

        let empty = check_text("receipt res: missing", &[] as &[&Path]);
        assert_eq!(empty.malformed_reference_ids, vec!["res:"]);
        assert!(!empty.ok);
    }

    #[test]
    fn trailing_punctuation_does_not_break_token_matching() {
        let dir = tempfile::tempdir().unwrap();
        write_events(dir.path(), &["context-1788477534952084000"]);
        let report = check_text(
            "See context-1788477534952084000, which resolves.",
            &[dir.path()],
        );
        assert!(report.unresolved_query_ids.is_empty());
    }

    #[test]
    fn dotted_and_hyphenated_query_kinds_resolve_as_whole_ids() {
        let dir = tempfile::tempdir().unwrap();
        let query_id = "doctor.flight-server-zero-copy-1788477534952084000";
        write_events(dir.path(), &[query_id]);
        let report = check_text(&format!("Evidence: `{query_id}`."), &[dir.path()]);
        assert_eq!(report.cited_query_ids, vec![query_id]);
        assert!(report.unresolved_query_ids.is_empty());
        assert!(report.ok);
    }
}
