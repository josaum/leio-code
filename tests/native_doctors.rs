//! A repository can publish metadata, but execution requires separate host trust.
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};
const BIN: &str = env!("CARGO_BIN_EXE_leio-code");
fn command(root: &Path, state: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(["--json", "--repo"])
        .arg(root)
        .args(args)
        .env("LEIO_DOCTOR_TRUST_DIR", state)
        .output()
        .unwrap()
}
#[test]
fn trust_is_bound_to_repo_catalog_and_binary_and_does_not_hide_failures() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let state = dir.path().join("trust");
    fs::create_dir_all(root.join(".leio-code")).unwrap();
    let manifest = json!({"schema_version":1,"name":"fixture-pack","doctors":[{"name":"fixture-contract","description":"fixture","suites":["all","baseline","ci"]}]});
    fs::write(
        root.join(".leio-code/native-doctors.json"),
        manifest.to_string(),
    )
    .unwrap();
    let script = dir.path().join("reviewed.py");
    fs::write(&script,r#"#!/usr/bin/env python3
import json,sys,pathlib
r=json.load(open(sys.argv[2]))
pathlib.Path(r['root'],'EXECUTED').write_text('yes')
e={'schema_version':'1.0','query_id':'fixture','kind':'doctor','summary':'invariant failed','confidence':0.9,'entities':[],'evidence':[],'warnings':['intentional invariant violation'],'timing_ms':0}
print(json.dumps({'protocol':1,'request_id':r['request_id'],'results':{n:e for n in r['names']}}))
"#).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let capabilities = command(&root, &state, &["capabilities"]);
    assert!(capabilities.status.success());
    assert!(String::from_utf8_lossy(&capabilities.stdout).contains("fixture-contract"));
    assert!(!root.join("EXECUTED").exists());
    let denied = command(&root, &state, &["doctor", "fixture-contract"]);
    assert!(!denied.status.success());
    assert!(!root.join("EXECUTED").exists());
    assert!(String::from_utf8_lossy(&denied.stdout).contains("not trusted"));
    let trust = command(
        &root,
        &state,
        &["trust-doctor-pack", "--binary", script.to_str().unwrap()],
    );
    assert!(
        trust.status.success(),
        "{}",
        String::from_utf8_lossy(&trust.stderr)
    );
    let ran = command(&root, &state, &["doctor", "fixture-contract"]);
    assert!(!ran.status.success());
    let envelope: Value = serde_json::from_slice(&ran.stdout).unwrap();
    assert_eq!(envelope["warnings"][0], "intentional invariant violation");
    assert!(root.join("EXECUTED").exists());
    fs::remove_file(root.join("EXECUTED")).unwrap();
    let mut changed = manifest.clone();
    changed["doctors"][0]["description"] = json!("changed catalog");
    fs::write(
        root.join(".leio-code/native-doctors.json"),
        changed.to_string(),
    )
    .unwrap();
    let denied = command(&root, &state, &["doctor", "fixture-contract"]);
    assert!(!denied.status.success());
    assert!(!root.join("EXECUTED").exists());
    assert!(String::from_utf8_lossy(&denied.stdout).contains("catalog changed"));
    // The executable named by repository source was never the execution target.
    fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    fs::write(
        root.join(".leio-code/native-doctors.json"),
        manifest.to_string(),
    )
    .unwrap();
    assert!(
        !command(&root, &state, &["doctor", "fixture-contract"])
            .status
            .success()
    );
    assert!(root.join("EXECUTED").exists());
    // Hosted execution is denied even when a valid local trust record exists.
    fs::remove_file(root.join("EXECUTED")).unwrap();
    let hosted = Command::new(BIN)
        .args(["--json", "--repo"])
        .arg(&root)
        .args(["doctor", "fixture-contract"])
        .env("LEIO_DOCTOR_TRUST_DIR", &state)
        .env("LEIO_DISABLE_NATIVE_DOCTORS", "1")
        .output()
        .unwrap();
    assert!(!hosted.status.success());
    assert!(!root.join("EXECUTED").exists());
    assert!(String::from_utf8_lossy(&hosted.stdout).contains("disabled by this host"));
    // A modified installed binary cannot inherit the old trust decision.
    let binary = fs::read_dir(&state)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_none())
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let tampered = command(&root, &state, &["doctor", "fixture-contract"]);
    assert!(!tampered.status.success());
    assert!(!root.join("EXECUTED").exists());
    assert!(String::from_utf8_lossy(&tampered.stdout).contains("binary changed"));
    // A malformed installed response cannot become a green doctor.
    assert!(
        command(
            &root,
            &state,
            &["trust-doctor-pack", "--binary", script.to_str().unwrap()]
        )
        .status
        .success()
    );
    let invalid = command(&root, &state, &["doctor", "fixture-contract"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stdout).contains("invalid native doctor response"));
}

#[test]
fn declarative_repository_pack_is_discovered_and_runs_without_native_trust() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".leio-code/doctors")).unwrap();
    fs::write(root.join("policy.txt"), "required invariant").unwrap();
    fs::write(
        root.join(".leio-code/doctors/local-policy.toml"),
        r#"schema_version = 1
name = "local-policy"
description = "A fixture repository invariant"
suites = ["baseline", "ci"]
[[checks]]
kind = "file-contains"
id = "invariant"
path = "policy.txt"
contains = "required invariant"
severity = "warning"
"#,
    )
    .unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["add", "policy.txt", ".leio-code/doctors/local-policy.toml"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    let state = root.join("unused-trust");
    let first = command(root, &state, &["doctor", "local-policy"]);
    assert!(
        first.status.success(),
        "{} {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let capabilities = command(root, &state, &["capabilities"]);
    assert!(String::from_utf8_lossy(&capabilities.stdout).contains("local-policy"));
    assert!(!state.exists());
    fs::write(root.join("policy.txt"), "invariant removed").unwrap();
    let after = command(root, &state, &["doctor", "local-policy"]);
    assert!(!after.status.success());
    assert!(String::from_utf8_lossy(&after.stdout).contains("invariant"));
}

#[test]
fn catalog_rejects_cycles_and_builtin_shadowing_without_execution() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".leio-code")).unwrap();
    for definitions in [
        json!([{"name":"repo-hygiene","description":"shadow","suites":["all"]}]),
        json!([{"name":"first","description":"cycle","suites":["all"],"covered_by":"second"},
               {"name":"second","description":"cycle","suites":["all"],"covered_by":"first"}]),
        json!([{"name":"first","description":"missing parent","suites":["all"],"covered_by":"absent"}]),
    ] {
        fs::write(
            dir.path().join(leio_code::doctors::native::MANIFEST),
            json!({"schema_version":1,"name":"fixture","doctors":definitions}).to_string(),
        )
        .unwrap();
        assert!(leio_code::doctors::native::discover(dir.path()).is_err());
    }
}

#[test]
fn suite_bounds_batches_and_preserves_success_when_one_batch_fails() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let state = dir.path().join("trust");
    fs::create_dir_all(root.join(".leio-code")).unwrap();
    let definitions: Vec<_> = (0..9)
        .map(|i| json!({"name":format!("fixture-{i}"),"description":"fixture","suites":["all"]}))
        .collect();
    fs::write(
        root.join(leio_code::doctors::native::MANIFEST),
        json!({"schema_version":1,"name":"fixture-batches","doctors":definitions}).to_string(),
    )
    .unwrap();
    let script = dir.path().join("pack.py");
    fs::write(&script, r#"#!/usr/bin/env python3
import sys,json
r=json.load(open(sys.argv[2]))
assert len(r['names']) <= 8
if 'fixture-8' in r['names']: sys.exit(9)
e={'schema_version':'1.0','query_id':'fixture','kind':'doctor','summary':'completed','confidence':1.0,'entities':[],'evidence':[],'warnings':[],'timing_ms':0}
print(json.dumps({'protocol':1,'request_id':r['request_id'],'results':{n:e for n in r['names']}}))
"#).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert!(
        command(
            &root,
            &state,
            &["trust-doctor-pack", "--binary", script.to_str().unwrap()]
        )
        .status
        .success()
    );
    let result = command(&root, &state, &["doctor", "all"]);
    assert!(!result.status.success());
    let envelope: Value = serde_json::from_slice(&result.stdout).unwrap();
    let rows: Vec<_> = envelope["entities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["doctor"].as_str().unwrap_or("").starts_with("fixture-"))
        .collect();
    assert_eq!(rows.len(), 9);
    assert_eq!(rows.iter().filter(|e| e["warning_count"] == 0).count(), 8);
    assert_eq!(rows.iter().filter(|e| e["warning_count"] == 1).count(), 1);
}
