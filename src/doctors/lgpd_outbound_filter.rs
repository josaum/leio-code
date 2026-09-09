//! LGPD Art. 18 outbound-filter and inbound re-consent doctor.
//!
//! Locks the four-layer LGPD defense established by PRs #309/#319/#324/#325:
//!
//!   1. **Warehouse materialization** marks erased rows with
//!      `lgpd_erased BOOLEAN` and blocks the open_* identity fallback for
//!      them, so the `DADOS REMOVIDOS` sentinel + leaked `open_*` fields
//!      don't reanimate identity on the bronze→serving boundary.
//!
//!   2. **Outbound query filter** routes `query_debtors` / `query_members`
//!      through `temp_member_collectable` which is `WHERE NOT lgpd_erased`,
//!      so anything that reads the warehouse sees only collectable members.
//!
//!   3. **Inbound detector + re-consent flow** in Liz: when the Pacto API
//!      returns an erased record, Liz returns `lgpd_re_consent_required=True`
//!      and asks the data subject to re-cadastrate instead of using the
//!      blanked record. This covers the case where the data subject contacts
//!      us — Art. 18 binds outbound proactive contact but not the data
//!      subject's own inbound contact, but the engagement still needs fresh
//!      consent rather than a leaked email.
//!
//!   4. **Regression tests** lock the contract so a future refactor that
//!      drops the detector call or rewires the warehouse view fails the
//!      build.
//!
//! Warnings emitted by this doctor carry the `[lgpd-outbound-filter]`
//! prefix so the ledger can account for any future regression.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct LgpdOutboundFilterDoctor;

impl Doctor for LgpdOutboundFilterDoctor {
    fn name(&self) -> &'static str {
        "lgpd-outbound-filter"
    }

    fn description(&self) -> &'static str {
        "Locks the LGPD Art. 18 four-layer defense: detector, warehouse carve-out, outbound-filter view, inbound re-consent flow."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_lgpd_outbound_filter(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
}

pub fn doctor_lgpd_outbound_filter(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let checks = [
        Check {
            path: "cartridges/pacto/utils.py",
            label: "Pacto LGPD erasure detector",
            needles: &[
                "_PACTO_ERASED_NAME_MARKER = \"DADOS REMOVIDOS\"",
                "_PACTO_ERASED_PLACEHOLDER_DIGITS = \"00000000000\"",
                "def is_pacto_lgpd_erased(",
            ],
        },
        Check {
            path: "cartridges/liz_cobranca/agent.py",
            label: "Liz inbound re-consent branch",
            needles: &[
                "from cartridges.pacto.utils import is_pacto_lgpd_erased",
                "is_pacto_lgpd_erased(member)",
                "\"lgpd_re_consent_required\": True",
            ],
        },
        Check {
            path: "cartridges/liz_cobranca/tests/test_agent.py",
            label: "Liz re-consent regression test",
            needles: &["lgpd_re_consent_required"],
        },
        Check {
            path: "cartridges/pacto/tests/test_lgpd_detection.py",
            label: "Pacto detector unit tests",
            needles: &["is_pacto_lgpd_erased", "DADOS REMOVIDOS"],
        },
        Check {
            path: "cartridges/fitness_exclusive/scripts/materialize_pacto_warehouse.py",
            label: "Warehouse LGPD carve-out + lgpd_erased column",
            needles: &[
                "DADOS REMOVIDOS",
                "AS lgpd_erased",
                "bool_or(lgpd_erased) AS lgpd_erased",
            ],
        },
        Check {
            path: "cartridges/fitness_exclusive/scripts/query_pacto_warehouse.py",
            label: "Outbound-filter view + collectable routing",
            needles: &[
                "CREATE OR REPLACE TEMP VIEW temp_member_collectable AS",
                "WHERE NOT lgpd_erased",
                "FROM temp_member_collectable",
            ],
        },
    ];

    let mut passed = 0usize;
    let total = checks.len();
    for check in &checks {
        let full_path = root.join(check.path);
        let mut io_warnings = Vec::new();
        let Some(body) = read_text(&full_path, &mut io_warnings) else {
            warnings.push(format!(
                "[lgpd-outbound-filter] {} missing or unreadable: {}",
                check.label, check.path
            ));
            for w in io_warnings {
                warnings.push(format!("[lgpd-outbound-filter] {}", w));
            }
            evidence.push(EvidenceItem {
                kind: "lgpd_outbound_filter_missing_file".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: check.label.to_string(),
            });
            continue;
        };

        let missing: Vec<&str> = check
            .needles
            .iter()
            .filter(|needle| !body.contains(**needle))
            .copied()
            .collect();
        if missing.is_empty() {
            passed += 1;
        } else {
            warnings.push(format!(
                "[lgpd-outbound-filter] {} drift in {}: missing {}",
                check.label,
                check.path,
                missing.join(", ")
            ));
            evidence.push(EvidenceItem {
                kind: "lgpd_outbound_filter_missing_wiring".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: format!("missing: {}", missing.join(", ")),
            });
        }
    }

    entities.push(json!({
        "doctor": "lgpd-outbound-filter",
        "checks_passed": passed,
        "checks_total": total,
        "art": "LGPD Art. 18 — right of erasure",
        "defense_layers": [
            "detector (cartridges/pacto/utils.is_pacto_lgpd_erased)",
            "warehouse materialization (lgpd_erased + open_* carve-out)",
            "outbound filter (temp_member_collectable + WHERE NOT lgpd_erased)",
            "inbound re-consent (Liz returns lgpd_re_consent_required=True)",
        ],
        "forbidden_pattern": "querying serving.revops_member_canonical without going through temp_member_collectable; trusting any field of a Pacto member dict without is_pacto_lgpd_erased gate before outbound dispatch",
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_lgpd_outbound_filter"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "LGPD Art. 18 four-layer defense intact (detector, warehouse carve-out, outbound filter, inbound re-consent)".to_string()
        } else {
            format!("LGPD outbound-filter drift: {} warning(s)", warnings.len())
        },
        confidence: if warnings.is_empty() { 0.93 } else { 0.60 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "tenant": "fitness_exclusive",
            "prs_locked": ["#309", "#319", "#324", "#325"],
            "out_of_scope": "Outbound dunning code paths do not exist today in liz_cobranca; when scheduled dunning lands, the dispatcher must call is_pacto_lgpd_erased before egress. This doctor cannot enforce a path that does not exist yet.",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_lgpd_outbound_filter_{}_{}",
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

    fn write_all_needles(root: &Path) {
        write(
            root,
            "cartridges/pacto/utils.py",
            "_PACTO_ERASED_NAME_MARKER = \"DADOS REMOVIDOS\"\n\
             _PACTO_ERASED_PLACEHOLDER_DIGITS = \"00000000000\"\n\
             def is_pacto_lgpd_erased(member):\n    pass\n",
        );
        write(
            root,
            "cartridges/liz_cobranca/agent.py",
            "from cartridges.pacto.utils import is_pacto_lgpd_erased\n\
             if is_pacto_lgpd_erased(member):\n    return {\"lgpd_re_consent_required\": True}\n",
        );
        write(
            root,
            "cartridges/liz_cobranca/tests/test_agent.py",
            "def test_lgpd():\n    assert result[\"lgpd_re_consent_required\"] is True\n",
        );
        write(
            root,
            "cartridges/pacto/tests/test_lgpd_detection.py",
            "def test_dados():\n    assert is_pacto_lgpd_erased({\"name\": \"DADOS REMOVIDOS\"})\n",
        );
        write(
            root,
            "cartridges/fitness_exclusive/scripts/materialize_pacto_warehouse.py",
            "-- DADOS REMOVIDOS carve-out\n\
             ... AS lgpd_erased\n\
             bool_or(lgpd_erased) AS lgpd_erased\n",
        );
        write(
            root,
            "cartridges/fitness_exclusive/scripts/query_pacto_warehouse.py",
            "CREATE OR REPLACE TEMP VIEW temp_member_collectable AS\n\
             SELECT * FROM temp_member_with_lgpd WHERE NOT lgpd_erased;\n\
             SELECT ... FROM temp_member_collectable\n",
        );
    }

    #[test]
    fn clean_repo_with_all_needles_passes() {
        let root = temp_root("clean");
        write_all_needles(&root);
        let env = doctor_lgpd_outbound_filter(&root);
        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
        assert!(env.summary.contains("four-layer defense intact"));
    }

    #[test]
    fn missing_detector_emits_prefixed_warning() {
        let root = temp_root("no_detector");
        write_all_needles(&root);
        // Drop is_pacto_lgpd_erased from utils.py
        write(
            &root,
            "cartridges/pacto/utils.py",
            "_PACTO_ERASED_NAME_MARKER = \"DADOS REMOVIDOS\"\n\
             _PACTO_ERASED_PLACEHOLDER_DIGITS = \"00000000000\"\n",
        );
        let env = doctor_lgpd_outbound_filter(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.starts_with("[lgpd-outbound-filter]")
                    && w.contains("is_pacto_lgpd_erased")),
            "expected prefixed warning about missing detector, got: {:?}",
            env.warnings
        );
    }

    #[test]
    fn missing_outbound_filter_view_emits_warning() {
        let root = temp_root("no_view");
        write_all_needles(&root);
        // query_pacto_warehouse without the collectable view
        write(
            &root,
            "cartridges/fitness_exclusive/scripts/query_pacto_warehouse.py",
            "SELECT * FROM serving.revops_member_canonical;\n",
        );
        let env = doctor_lgpd_outbound_filter(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("temp_member_collectable")),
            "expected warning about missing collectable view, got: {:?}",
            env.warnings
        );
    }
}
