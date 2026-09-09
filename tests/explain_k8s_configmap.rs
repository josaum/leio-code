//! Integration tests for Kubernetes ConfigMap provenance in `explain env-var`.
//!
//! ConfigMap entries are indexed from YAML files and surface as
//! `K8sConfigMap` bindings at precedence 30 (after all dotenv and profile
//! sources). Only files containing `kind: ConfigMap` and a `data:` section are
//! parsed; other YAML files are left untouched.

use std::fs;
use std::path::Path;

use leio_code::indexer::{build_or_update_index, detect_k8s_configmap};
use leio_code::query::explain_env_var;
use leio_code::value_resolution::ValueResolutionOpts;
use tempfile::TempDir;

fn write_repo(files: &[(&str, &str)]) -> TempDir {
    let tmp = tempfile::tempdir().expect("create tempdir");
    for (rel, content) in files {
        let full = tmp.path().join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(&full, content).expect("write file");
    }
    tmp
}

fn build(tmp: &Path) -> leio_code::model::RepoIndex {
    let index_path = tmp.join(".leio-code").join("index.json");
    fs::create_dir_all(index_path.parent().unwrap()).unwrap();
    build_or_update_index(tmp, &index_path, true).expect("build index")
}

static SAMPLE_CONFIGMAP: &str = r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: app-config
  namespace: production
data:
  DB_HOST: postgres.internal
  DB_PORT: "5432"
  LOG_LEVEL: info
"#;

/// `explain env-var` surfaces a K8sConfigMap binding for keys found in the
/// `data:` section of a Kubernetes ConfigMap YAML file.
#[test]
fn k8s_configmap_binding_surfaces_for_data_key() {
    let repo = write_repo(&[("deploy/k8s/app-config.yaml", SAMPLE_CONFIGMAP)]);
    let index = build(repo.path());

    // The ConfigMap should be indexed.
    assert_eq!(
        index.k8s_configmaps.len(),
        1,
        "expected one ConfigMap, got {:?}",
        index.k8s_configmaps
    );
    assert_eq!(index.k8s_configmaps[0].map_name, "app-config");
    assert_eq!(
        index.k8s_configmaps[0].namespace.as_deref(),
        Some("production")
    );

    let envelope = explain_env_var(&index, "DB_HOST", ValueResolutionOpts::default());
    let entity = &envelope.entities[0];
    let bindings = entity["value_bindings"].as_array().expect("value_bindings");

    assert_eq!(bindings.len(), 1, "expected one binding, got {bindings:?}");

    let b = &bindings[0];
    assert_eq!(b["source"]["kind"], "k8s_config_map");
    assert_eq!(b["source"]["map_name"], "app-config");
    assert_eq!(b["state"], "set");
    assert_eq!(b["display"], "postgres.internal");
    assert_eq!(b["redacted"], false);
}

/// When a ConfigMap and a `.env` file both declare a key, both bindings appear
/// and the `.env` file wins (precedence 3 < 30).
///
/// Uses a collision-proof var name on purpose: `explain env-var` also reports
/// a `shell_env` binding when the variable is set in the ambient process
/// environment (e.g. `LOG_LEVEL=INFO` in a developer shell), which would add
/// a third binding and make the count assertion environment-dependent.
#[test]
fn k8s_configmap_binding_sorts_after_env_file() {
    let configmap = r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: app-config
  namespace: production
data:
  LEIO_TEST_K8S_LOG_LEVEL: info
"#;
    let repo = write_repo(&[
        ("k8s/app-config.yaml", configmap),
        (".env", "LEIO_TEST_K8S_LOG_LEVEL=debug\n"),
    ]);
    let index = build(repo.path());

    let envelope = explain_env_var(
        &index,
        "LEIO_TEST_K8S_LOG_LEVEL",
        ValueResolutionOpts::default(),
    );
    let entity = &envelope.entities[0];
    let bindings = entity["value_bindings"].as_array().expect("value_bindings");

    assert_eq!(bindings.len(), 2, "expected env_file + k8s_config_map");

    // EnvFile (precedence 3) sorts before K8sConfigMap (precedence 30).
    assert_eq!(bindings[0]["source"]["kind"], "env_file");
    assert_eq!(bindings[0]["display"], "debug");
    assert_eq!(bindings[1]["source"]["kind"], "k8s_config_map");
    assert_eq!(bindings[1]["display"], "info");

    // Effective binding is the env file value.
    assert_eq!(entity["effective"]["display"], "debug");
}

/// A key absent from the ConfigMap's `data:` section produces no binding.
#[test]
fn k8s_configmap_missing_key_produces_no_binding() {
    let repo = write_repo(&[("k8s/app-config.yaml", SAMPLE_CONFIGMAP)]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, "MISSING_KEY", ValueResolutionOpts::default());
    let entity = &envelope.entities[0];
    let bindings = entity["value_bindings"].as_array().expect("value_bindings");
    assert_eq!(bindings.len(), 0);
}

/// A YAML file that is NOT a ConfigMap (different `kind:`) must not be indexed
/// as a ConfigMap.
#[test]
fn non_configmap_yaml_is_not_indexed() {
    let repo = write_repo(&[(
        "k8s/deployment.yaml",
        r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-app
spec:
  template:
    spec:
      containers:
      - name: app
        env:
        - name: DB_HOST
          value: postgres.internal
"#,
    )]);
    let index = build(repo.path());

    assert_eq!(
        index.k8s_configmaps.len(),
        0,
        "Deployment YAML must not produce a ConfigMap record"
    );
}

// ── Unit tests for detect_k8s_configmap ─────────────────────────────────────

/// `detect_k8s_configmap` correctly parses the canonical ConfigMap format.
#[test]
fn unit_detect_configmap_parses_entries() {
    let record = detect_k8s_configmap("k8s/cfg.yaml", SAMPLE_CONFIGMAP)
        .expect("should parse sample configmap");

    assert_eq!(record.map_name, "app-config");
    assert_eq!(record.namespace.as_deref(), Some("production"));
    assert_eq!(
        record.entries.get("DB_HOST").map(|s| s.as_str()),
        Some("postgres.internal")
    );
    assert_eq!(
        record.entries.get("LOG_LEVEL").map(|s| s.as_str()),
        Some("info")
    );
}

/// `detect_k8s_configmap` returns `None` for a non-ConfigMap YAML.
#[test]
fn unit_detect_configmap_returns_none_for_deployment() {
    let content = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: x\n";
    assert!(detect_k8s_configmap("k8s/deploy.yaml", content).is_none());
}

/// `detect_k8s_configmap` returns `None` when `metadata.name` is absent.
#[test]
fn unit_detect_configmap_returns_none_without_name() {
    let content = "kind: ConfigMap\ndata:\n  FOO: bar\n";
    assert!(detect_k8s_configmap("k8s/anon.yaml", content).is_none());
}
