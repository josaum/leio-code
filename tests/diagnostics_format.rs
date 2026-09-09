//! Tests for the `diagnostics` module — text / json / sarif rendering of
//! `QueryEnvelope`s for the `doctor --format=…` CLI flag.

use leio_code::diagnostics::{
    DiagFormat, Diagnostic, RunMeta, Severity, render, render_diagnostics,
};
use leio_code::model::{EvidenceItem, QueryEnvelope};

fn sample_envelope_with_warning_and_evidence() -> QueryEnvelope {
    QueryEnvelope {
        schema_version: leio_code::model::SCHEMA_VERSION.to_string(),
        query_id: "doctor-test".to_string(),
        kind: "doctor".to_string(),
        summary: "test summary".to_string(),
        confidence: 0.95,
        entities: Vec::new(),
        evidence: vec![EvidenceItem {
            kind: "drift".to_string(),
            path: "src/foo.rs".to_string(),
            line: Some(42),
            detail: "evidence detail".to_string(),
        }],
        warnings: vec!["warning A: something is off".to_string()],
        meta: None,
        timing_ms: 0,
    }
}

fn sample_meta() -> RunMeta {
    RunMeta {
        index_version: 3,
        commit_sha: "abc123".to_string(),
        tool_version: "1.1.0",
    }
}

#[test]
fn text_format_preserves_existing_behavior() {
    let env = sample_envelope_with_warning_and_evidence();
    let out = render(&env, DiagFormat::Text, &sample_meta());
    assert!(out.contains("test summary"), "missing summary: {out}");
    assert!(out.contains("warning A"), "missing warning text: {out}");
    assert!(out.contains("src/foo.rs"), "missing evidence path: {out}");
}

#[test]
fn json_format_has_required_keys() {
    let env = sample_envelope_with_warning_and_evidence();
    let out = render(&env, DiagFormat::Json, &sample_meta());
    let v: serde_json::Value = serde_json::from_str(&out).expect("json parse");
    assert_eq!(v["meta"]["index_version"], 3);
    assert_eq!(v["meta"]["commit_sha"], "abc123");
    let diags = v["diagnostics"].as_array().expect("diagnostics array");
    assert!(!diags.is_empty());
    for d in diags {
        assert!(d["rule_id"].is_string(), "missing rule_id");
        assert!(d["severity"].is_string(), "missing severity");
        assert!(d["message"].is_string(), "missing message");
    }
}

#[test]
fn sarif_format_has_required_keys() {
    let env = sample_envelope_with_warning_and_evidence();
    let out = render(&env, DiagFormat::Sarif, &sample_meta());
    let v: serde_json::Value = serde_json::from_str(&out).expect("sarif parse");
    assert_eq!(v["version"], "2.1.0");
    assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "leio-code");
    let results = v["runs"][0]["results"].as_array().expect("results array");
    assert!(!results.is_empty());
    let invocations = v["runs"][0]["invocations"]
        .as_array()
        .expect("invocations array");
    assert_eq!(invocations[0]["properties"]["index_version"], 3);
    assert_eq!(invocations[0]["properties"]["commit_sha"], "abc123");
}

#[test]
fn sarif_result_has_physical_location_when_evidence_has_path() {
    let diags = vec![Diagnostic {
        rule_id: "evidence".to_string(),
        severity: Severity::Note,
        path: Some("foo.rs".to_string()),
        line: Some(42),
        message: "hello".to_string(),
    }];
    let out = render_diagnostics(&diags, DiagFormat::Sarif, &sample_meta());
    let v: serde_json::Value = serde_json::from_str(&out).expect("sarif parse");
    let result = &v["runs"][0]["results"][0];
    assert_eq!(
        result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "foo.rs"
    );
    assert_eq!(
        result["locations"][0]["physicalLocation"]["region"]["startLine"],
        42
    );
}

#[test]
fn sarif_omits_location_when_no_path() {
    let diags = vec![Diagnostic {
        rule_id: "warning".to_string(),
        severity: Severity::Warning,
        path: None,
        line: None,
        message: "no path here".to_string(),
    }];
    let out = render_diagnostics(&diags, DiagFormat::Sarif, &sample_meta());
    let v: serde_json::Value = serde_json::from_str(&out).expect("sarif parse");
    let result = &v["runs"][0]["results"][0];
    // Either the key is absent or the array is empty.
    let absent_or_empty = result.get("locations").is_none()
        || result["locations"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false);
    assert!(absent_or_empty, "expected no locations, got {result}");
}

#[test]
fn severity_maps_to_sarif_level() {
    let diags = vec![
        Diagnostic {
            rule_id: "e".to_string(),
            severity: Severity::Error,
            path: None,
            line: None,
            message: "e".to_string(),
        },
        Diagnostic {
            rule_id: "w".to_string(),
            severity: Severity::Warning,
            path: None,
            line: None,
            message: "w".to_string(),
        },
        Diagnostic {
            rule_id: "n".to_string(),
            severity: Severity::Note,
            path: None,
            line: None,
            message: "n".to_string(),
        },
    ];
    let out = render_diagnostics(&diags, DiagFormat::Sarif, &sample_meta());
    let v: serde_json::Value = serde_json::from_str(&out).expect("sarif parse");
    let results = v["runs"][0]["results"].as_array().expect("results");
    assert_eq!(results[0]["level"], "error");
    assert_eq!(results[1]["level"], "warning");
    assert_eq!(results[2]["level"], "note");
}
