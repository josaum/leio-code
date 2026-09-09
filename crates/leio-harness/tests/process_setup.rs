use leio_harness::{model::RunSpec, process};
#[test]
fn log_setup_failure_does_not_spawn_command() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("output");
    std::fs::create_dir_all(output.join("attempt/stdout.log")).unwrap();
    let spec: RunSpec = serde_json::from_value(serde_json::json!({
        "runId":"attempt", "argv":["/bin/sh","-c","printf spawned > marker"],
        "cwd":root.path(), "outputDir":output, "timeoutMs":1000
    }))
    .unwrap();
    assert!(process::run(spec).is_err());
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(!root.path().join("marker").exists());
}
