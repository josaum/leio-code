use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;
use tempfile::TempDir;

fn cli(root: &Path, arguments: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_leio-code"))
        .args(["--json", "--repo", root.to_str().expect("repository path")])
        .args(arguments)
        .env("LEIO_REUSE_INDEX", "1")
        .env("LEIO_INDEX_TTL_SECS", "300")
        .env("LEIO_DISABLE_SEARCH_SIDECAR", "1")
        .output()
        .expect("run CLI");
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON envelope")
}

fn index(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(root.join(".leio-code/index.json")).expect("read index"))
        .expect("index JSON")
}

fn fixture() -> TempDir {
    let root = tempfile::tempdir().expect("temporary repository");
    for number in 0..300 {
        fs::write(
            root.path().join(format!("source_{number:04}.py")),
            format!("def original_{number:04}():\n    return 1\n"),
        )
        .expect("write indexed source");
    }
    cli(root.path(), &["index"]);
    assert_eq!(index(root.path())["files"].as_array().unwrap().len(), 300);
    root
}

fn symbols(index: &Value) -> Vec<&str> {
    index["files"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|file| file["symbols"].as_array().unwrap())
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect()
}

#[test]
fn reuse_flag_refreshes_edited_source_beyond_256_files() {
    let root = fixture();
    let before = index(root.path());
    let changed = &before["files"][299];
    let original = changed["symbols"][0]["name"].as_str().unwrap();
    let path = root.path().join(changed["path"].as_str().unwrap());
    fs::write(
        &path,
        "def replacement_after_sample_limit():\n    return 42\n",
    )
    .expect("edit source");

    let result = cli(root.path(), &["find", "replacement_after_sample_limit"]);
    assert!(!result["entities"].as_array().unwrap().is_empty());
    let after = index(root.path());
    assert!(symbols(&after).contains(&"replacement_after_sample_limit"));
    assert!(!symbols(&after).contains(&original));
}

#[test]
fn reuse_flag_refreshes_new_source_during_explicit_index() {
    let root = fixture();
    fs::write(
        root.path().join("added.py"),
        "def newly_added_source():\n    return 7\n",
    )
    .expect("add source");

    cli(root.path(), &["index"]);
    let after = index(root.path());
    assert_eq!(after["files"].as_array().unwrap().len(), 301);
    assert!(symbols(&after).contains(&"newly_added_source"));
}

#[test]
fn reuse_flag_refreshes_deleted_source_beyond_256_files() {
    let root = fixture();
    let before = index(root.path());
    let removed = &before["files"][299];
    let name = removed["symbols"][0]["name"].as_str().unwrap();
    fs::remove_file(root.path().join(removed["path"].as_str().unwrap())).expect("remove source");

    cli(root.path(), &["find", name]);
    let after = index(root.path());
    assert_eq!(after["files"].as_array().unwrap().len(), 299);
    assert!(!symbols(&after).contains(&name));
}

#[test]
fn reuse_flag_keeps_intact_index_even_when_ttl_expired() {
    let root = fixture();
    let path = root.path().join(".leio-code/index.json");
    fs::File::options()
        .write(true)
        .open(&path)
        .expect("open index")
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
        .expect("expire index timestamp");
    let before = fs::read(&path).expect("read original index");
    let modified = fs::metadata(&path).unwrap().modified().unwrap();

    cli(root.path(), &["find", "original_0000"]);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    cli(root.path(), &["index"]);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
}

#[test]
fn explicit_index_path_does_not_reuse_another_repository_file_records() {
    let original = fixture();
    let foreign = tempfile::tempdir().expect("another repository");
    let index_path = original.path().join(".leio-code/index.json");
    let before = index(original.path());
    for source in before["files"].as_array().unwrap() {
        let relative = source["path"].as_str().unwrap();
        let original_path = original.path().join(relative);
        let foreign_path = foreign.path().join(relative);
        let modified = fs::metadata(&original_path).unwrap().modified().unwrap();
        let content = fs::read_to_string(original_path).unwrap();
        fs::write(&foreign_path, content.replace("original", "modified"))
            .expect("write colliding source");
        fs::File::options()
            .write(true)
            .open(&foreign_path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .expect("match original timestamp");
        assert_eq!(
            fs::metadata(&foreign_path).unwrap().len(),
            source["bytes"].as_u64().unwrap()
        );
    }

    cli(
        foreign.path(),
        &["--index-path", index_path.to_str().unwrap(), "index"],
    );
    let after = index(original.path());
    let expected_root = foreign
        .path()
        .canonicalize()
        .expect("canonical foreign root");
    assert_eq!(
        after["root"].as_str().unwrap(),
        expected_root.to_str().unwrap()
    );
    assert_eq!(after["files"].as_array().unwrap().len(), 300);
    let recovered = symbols(&after);
    assert!(recovered.iter().all(|name| name.starts_with("modified_")));
    assert!(!recovered.iter().any(|name| name.starts_with("original_")));
}
