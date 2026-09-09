//! Discovery-only contracts for repository-owned declarative doctor packs.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use leio_code::doctors::doctor_names;
use leio_code::doctors::local_packs::{
    LocalContentMode, LocalPackBudgets, LocalPackRequest, LocalPackRequestContext,
    discover_local_doctor_packs, run_local_doctor_pack,
};
use tempfile::TempDir;

struct TestRepo {
    temp: TempDir,
}

impl TestRepo {
    fn new() -> Self {
        let repo = Self {
            temp: TempDir::new().expect("tempdir"),
        };
        for args in [
            ["init"].as_slice(),
            ["config", "user.email", "local-doctor-packs@example.test"].as_slice(),
            ["config", "user.name", "Local Doctor Packs"].as_slice(),
        ] {
            Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("run git");
        }
        repo
    }

    fn path(&self) -> &Path {
        self.temp.path()
    }

    fn write(&self, relative: &str, body: &str) {
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write file");
    }

    #[allow(dead_code)] // Task 2 uses this shared fixture hook for non-UTF-8 inputs.
    fn write_bytes(&self, relative: &str, body: &[u8]) {
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write bytes");
    }

    fn git_add(&self, relative: &str) {
        Command::new("git")
            .args(["add", "--", relative])
            .current_dir(self.path())
            .status()
            .expect("git add");
    }

    #[allow(dead_code)] // Later revision-backed discovery tests use this fixture hook.
    fn git_commit(&self, message: &str) {
        Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(self.path())
            .status()
            .expect("git commit");
    }

    fn write_pack(&self, name: &str, body: String) {
        let relative = format!(".leio-code/doctors/{name}.toml");
        self.write(&relative, &body);
        self.git_add(&relative);
    }

    fn request(&self, mode: LocalContentMode) -> LocalPackRequest {
        LocalPackRequest {
            root: self.path().to_path_buf(),
            content_mode: mode,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(10),
            budgets: LocalPackBudgets::default(),
        }
    }
}

fn minimal_pack(name: &str) -> String {
    format!(
        r#"schema_version = 1
name = "{name}"
description = "Validate {name}."

[[checks]]
id = "required-anchor"
kind = "file-contains"
path = "src/example.rs"
contains = "required_symbol"
severity = "warning"
"#
    )
}

fn path_exists_pack(name: &str, path: &str) -> String {
    format!(
        "schema_version = 1\nname = \"{name}\"\ndescription = \"x\"\n\n[[checks]]\nid = \"x\"\nkind = \"path-exists\"\npath = \"{path}\"\nseverity = \"warning\"\n"
    )
}

fn cargo_pack(name: &str, workspace: &str, makefile: &str) -> String {
    format!(
        "schema_version = 1\nname = \"{name}\"\ndescription = \"x\"\n\n[[checks]]\nid = \"x\"\nkind = \"cargo-feature-packages-covered-by-make-target\"\nworkspace = \"{workspace}\"\nfeature = \"x\"\nmakefile = \"{makefile}\"\ntarget = \"x\"\nintegrity_severity = \"warning\"\nseverity = \"warning\"\n"
    )
}

fn make_target_fixture(makefile: &str) -> (TestRepo, LocalPackRequest) {
    let repo = TestRepo::new();
    repo.write("Makefile", makefile);
    repo.git_add("Makefile");
    repo.write_pack(
        "make-target",
        "schema_version = 1\nname = \"make-target\"\ndescription = \"x\"\n\n[[checks]]\nid = \"workspace-clippy-target\"\nkind = \"make-target-contains\"\npath = \"Makefile\"\ntarget = \"office-parsers-clippy\"\ncontains = [\"cargo clippy --workspace\", \"--all-targets\", \"-D warnings\"]\nseverity = \"warning\"\n".to_owned(),
    );
    let request = repo.request(LocalContentMode::WorkingTreeTracked);
    (repo, request)
}

fn cargo_feature_fixture(member_manifest: &str) -> (TestRepo, LocalPackRequest) {
    let repo = TestRepo::new();
    repo.write("Cargo.toml", "[workspace]\nmembers = [\"member\"]\n");
    repo.git_add("Cargo.toml");
    repo.write("member/Cargo.toml", member_manifest);
    repo.git_add("member/Cargo.toml");
    repo.write("Makefile", "office-parsers-clippy-features:\n\tcargo clippy -p member --features arrow -- -D warnings\n");
    repo.git_add("Makefile");
    repo.write_pack(
        "cargo-feature",
        "schema_version = 1\nname = \"cargo-feature\"\ndescription = \"x\"\n\n[[checks]]\nid = \"arrow-feature-packages-covered\"\nkind = \"cargo-feature-packages-covered-by-make-target\"\nworkspace = \"Cargo.toml\"\nfeature = \"arrow\"\nmakefile = \"Makefile\"\ntarget = \"office-parsers-clippy-features\"\nintegrity_severity = \"warning\"\nseverity = \"info\"\n".to_owned(),
    );
    let request = repo.request(LocalContentMode::WorkingTreeTracked);
    (repo, request)
}

/// `reason` is a static string, so the derived package metadata is the only
/// thing that can tell an operator which crate the Make target forgot.
#[test]
fn cargo_feature_findings_name_the_discovered_and_uncovered_packages() {
    let (repo, request) = cargo_feature_fixture(
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"member\", \"forgotten\"]\n",
    );
    repo.git_add("Cargo.toml");
    repo.write(
        "forgotten/Cargo.toml",
        "[package]\nname = \"forgotten\"\nversion = \"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.git_add("forgotten/Cargo.toml");
    let envelope = run_pack_with_request(request.clone(), "cargo-feature");
    assert_eq!(
        envelope.entities[0]["reason_code"],
        "cargo_package_uncovered"
    );
    assert_eq!(
        envelope.entities[0]["uncovered_packages"],
        serde_json::json!(["forgotten"])
    );
    // Discovery sorts member paths, so the reported order is stable.
    assert_eq!(
        envelope.entities[0]["discovered_packages"],
        serde_json::json!(["forgotten", "member"])
    );

    repo.write(
        "Makefile",
        "office-parsers-clippy-features:\n\tcargo clippy -p member --features arrow\n\tcargo clippy -p forgotten --features arrow\n",
    );
    repo.git_add("Makefile");
    let envelope = run_pack_with_request(request, "cargo-feature");
    assert_eq!(envelope.entities[0]["passed"], true, "{envelope:?}");
    assert_eq!(
        envelope.entities[0]["uncovered_packages"],
        serde_json::json!([])
    );
}

#[test]
fn make_fragments_in_another_target_do_not_satisfy_the_check() {
    let (repo, request) = make_target_fixture(
        "wrong:\n\tcargo clippy --workspace --all-targets -- -D warnings\n\noffice-parsers-clippy:\n\t@true\n",
    );
    let envelope = run_pack_with_request(request, "make-target");
    assert_eq!(envelope.entities[0]["passed"], false);
    assert_eq!(
        envelope.entities[0]["reason_code"],
        "make_target_literal_missing"
    );
    drop(repo);
}

#[test]
fn cargo_parse_failures_use_integrity_severity() {
    let (repo, request) = cargo_feature_fixture("not = [valid toml");
    let envelope = run_pack_with_request(request, "cargo-feature");
    assert_eq!(envelope.entities[0]["severity"], "warning");
    assert_eq!(envelope.entities[0]["reason_code"], "cargo_manifest_parse");
    assert_eq!(envelope.warnings.len(), 1);
    drop(repo);
}

#[test]
fn cargo_feature_packages_require_package_and_feature_anchors_in_the_target() {
    let (repo, request) = cargo_feature_fixture(
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\n\n[features]\narrow = []\n",
    );
    let envelope = run_pack_with_request(request, "cargo-feature");
    assert_eq!(envelope.entities[0]["passed"], true);
    drop(repo);
}

#[test]
fn cargo_feature_wildcard_members_are_sorted_and_selected_before_feature_filtering() {
    let (repo, request) =
        cargo_feature_fixture("[package]\nname = \"unused\"\nversion = \"0.1.0\"\n");
    repo.write("Cargo.toml", "[workspace]\nmembers = [\"members/*\"]\n");
    repo.git_add("Cargo.toml");
    repo.write(
        "members/z/Cargo.toml",
        "[package]\nname = \"z\"\nversion = \"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.write(
        "members/a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.git_add("members/z/Cargo.toml");
    repo.git_add("members/a/Cargo.toml");
    repo.write("Makefile", "office-parsers-clippy-features:\n\tcargo clippy -p a --features=arrow -p z --features arrow -- -D warnings\n");
    repo.git_add("Makefile");
    let envelope = run_pack_with_request(request, "cargo-feature");
    assert_eq!(envelope.entities[0]["passed"], true);
}

#[test]
fn revision_wildcard_members_ignore_mutable_working_tree_additions() {
    let (repo, mut request) =
        cargo_feature_fixture("[package]\nname = \"unused\"\nversion = \"0.1.0\"\n");
    repo.write("Cargo.toml", "[workspace]\nmembers = [\"members/*\"]\n");
    repo.git_add("Cargo.toml");
    repo.write(
        "members/a/Cargo.toml",
        "[package]\nname=\"a\"\nversion=\"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.git_add("members/a/Cargo.toml");
    repo.write(
        "Makefile",
        "office-parsers-clippy-features:\n\tcargo clippy -p a --features arrow\n",
    );
    repo.git_add("Makefile");
    repo.git_commit("pinned wildcard workspace");
    repo.write(
        "members/untracked/Cargo.toml",
        "[package]\nname=\"untracked\"\nversion=\"0.1.0\"\n[features]\narrow=[]\n",
    );
    request.content_mode = LocalContentMode::RevisionTracked;
    let envelope = run_pack_with_request(request, "cargo-feature");
    assert_eq!(envelope.entities[0]["passed"], true);
}

#[test]
fn revision_wildcard_members_match_path_components_not_string_prefixes() {
    let (repo, mut request) =
        cargo_feature_fixture("[package]\nname = \"unused\"\nversion = \"0.1.0\"\n");
    repo.write("Cargo.toml", "[workspace]\nmembers = [\"members/foo/*\"]\n");
    repo.git_add("Cargo.toml");
    repo.write(
        "members/foo/in/Cargo.toml",
        "[package]\nname=\"in\"\nversion=\"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.write(
        "members/foobar/out/Cargo.toml",
        "[package]\nname=\"out\"\nversion=\"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.git_add("members/foo/in/Cargo.toml");
    repo.git_add("members/foobar/out/Cargo.toml");
    repo.write(
        "Makefile",
        "office-parsers-clippy-features:\n\tcargo clippy -p in --features arrow\n",
    );
    repo.git_add("Makefile");
    repo.git_commit("component exact wildcard workspace");
    request.content_mode = LocalContentMode::RevisionTracked;
    let envelope = run_pack_with_request(request.clone(), "cargo-feature");
    assert_eq!(envelope.entities[0]["passed"], true, "{envelope:?}");
}

#[test]
fn revision_literal_members_do_not_match_component_prefixes() {
    let (repo, mut request) =
        cargo_feature_fixture("[package]\nname = \"unused\"\nversion = \"0.1.0\"\n");
    repo.write("Cargo.toml", "[workspace]\nmembers = [\"member\"]\n");
    repo.git_add("Cargo.toml");
    repo.write(
        "member/Cargo.toml",
        "[package]\nname=\"member\"\nversion=\"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.write(
        "member-extra/Cargo.toml",
        "[package]\nname=\"member-extra\"\nversion=\"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.git_add("member/Cargo.toml");
    repo.git_add("member-extra/Cargo.toml");
    repo.write(
        "Makefile",
        "office-parsers-clippy-features:\n\tcargo clippy -p member --features arrow\n",
    );
    repo.git_add("Makefile");
    repo.git_commit("component exact literal workspace");
    request.content_mode = LocalContentMode::RevisionTracked;
    let envelope = run_pack_with_request(request, "cargo-feature");
    assert_eq!(envelope.entities[0]["passed"], true, "{envelope:?}");
}

#[test]
fn revision_wildcard_git_query_budget_is_reserved_before_listing() {
    let (repo, mut request) = cargo_feature_fixture(
        "[package]\nname=\"member\"\nversion=\"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.write("Cargo.toml", "[workspace]\nmembers = [\"member\"]\n");
    repo.git_add("Cargo.toml");
    repo.write(
        "Makefile",
        "office-parsers-clippy-features:\n\tcargo clippy -p member --features arrow\n",
    );
    repo.git_add("Makefile");
    repo.git_commit("pinned query budget");
    request.content_mode = LocalContentMode::RevisionTracked;
    request.budgets.max_git_queries = 0;
    let envelope = run_pack_with_request(request, "cargo-feature");
    assert_eq!(
        envelope.entities[0]["reason_code"],
        "git_query_limit_exceeded"
    );
}

#[test]
fn cargo_recipe_tokens_reject_prefixes_and_cross_command_anchors() {
    let (repo, request) = cargo_feature_fixture(
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.write("Makefile", "office-parsers-clippy-features:\n\tcargo clippy -p member-extra --features=arrow\n\tcargo clippy -p member --features=notarrow\n");
    repo.git_add("Makefile");
    let envelope = run_pack_with_request(request, "cargo-feature");
    assert_eq!(
        envelope.entities[0]["reason_code"],
        "cargo_package_uncovered"
    );
}

#[test]
fn cargo_recipe_segments_do_not_join_anchors_across_shell_boundaries_or_comments() {
    let (repo, request) = cargo_feature_fixture(
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.write("Makefile", "office-parsers-clippy-features:\n\tcargo clippy -p member && cargo clippy --features arrow # -p member --features arrow\n");
    repo.git_add("Makefile");
    let envelope = run_pack_with_request(request, "cargo-feature");
    assert_eq!(
        envelope.entities[0]["reason_code"],
        "cargo_package_uncovered"
    );
}

#[test]
fn cargo_recipe_requires_a_cargo_command_and_honors_shell_quotes() {
    let (repo, request) = cargo_feature_fixture(
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\n[features]\narrow=[]\n",
    );
    repo.write(
        "Makefile",
        "office-parsers-clippy-features:\n\techo '-p member --features arrow'; cargo clippy -p member --features 'arrow' # cargo clippy -p member --features nope\n",
    );
    repo.git_add("Makefile");
    let envelope = run_pack_with_request(request.clone(), "cargo-feature");
    assert_eq!(envelope.entities[0]["passed"], true, "{envelope:?}");

    repo.write(
        "Makefile",
        "office-parsers-clippy-features:\n\techo -p member --features arrow\n",
    );
    repo.git_add("Makefile");
    let envelope = run_pack_with_request(request, "cargo-feature");
    assert_eq!(
        envelope.entities[0]["reason_code"],
        "cargo_package_uncovered"
    );
}

#[test]
fn aggregate_budget_stops_after_the_first_rejected_check() {
    let repo = TestRepo::new();
    repo.write("src/check.rs", "required_symbol\n");
    repo.git_add("src/check.rs");
    repo.write_pack(
        "check-budget-one",
        minimal_pack("check-budget-one").replace("src/example.rs", "src/check.rs"),
    );
    repo.write_pack(
        "check-budget-two",
        minimal_pack("check-budget-two").replace("src/example.rs", "src/check.rs"),
    );
    let mut request = repo.request(LocalContentMode::WorkingTreeTracked);
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    request.budgets.max_checks_per_request = 1;
    let mut context = LocalPackRequestContext::new(request);
    let first = run_local_doctor_pack(catalog.packs.get("check-budget-one").unwrap(), &mut context);
    let second =
        run_local_doctor_pack(catalog.packs.get("check-budget-two").unwrap(), &mut context);
    assert_eq!(first.entities[0]["passed"], true);
    assert_eq!(second.entities[0]["reason_code"], "check_limit_exceeded");
}

#[test]
fn unique_input_bytes_are_charged_once_for_cached_input() {
    let repo = TestRepo::new();
    repo.write("src/check.rs", "same\n");
    repo.git_add("src/check.rs");
    let mut pack = minimal_pack("unique-input")
        .replace("src/example.rs", "src/check.rs")
        .replace("required_symbol", "same");
    pack.push_str("\n[[checks]]\nid = \"same-input\"\nkind = \"file-contains\"\npath = \"src/check.rs\"\ncontains = \"same\"\nseverity = \"warning\"\n");
    repo.write_pack("unique-input", pack);
    let mut request = repo.request(LocalContentMode::WorkingTreeTracked);
    request.budgets.max_unique_input_bytes = 5;
    let envelope = run_pack_with_request(request, "unique-input");
    assert!(
        envelope
            .entities
            .iter()
            .all(|entity| entity["passed"] == true)
    );
}

#[test]
fn cancelled_shared_context_returns_a_deterministic_failure() {
    let repo = TestRepo::new();
    repo.write("src/check.rs", "required_symbol\n");
    repo.git_add("src/check.rs");
    repo.write_pack(
        "cancelled-pack",
        minimal_pack("cancelled-pack").replace("src/example.rs", "src/check.rs"),
    );
    let request = repo.request(LocalContentMode::WorkingTreeTracked);
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    let mut context = LocalPackRequestContext::new(request);
    context.cancel();
    let envelope =
        run_local_doctor_pack(catalog.packs.get("cancelled-pack").unwrap(), &mut context);
    assert_eq!(envelope.entities[0]["reason_code"], "cancelled");
}

#[test]
fn terminal_failure_is_emitted_once_across_three_shared_pack_calls() {
    let repo = TestRepo::new();
    repo.write("src/check.rs", "required_symbol\n");
    repo.git_add("src/check.rs");
    for name in ["terminal-one", "terminal-two", "terminal-three"] {
        repo.write_pack(
            name,
            minimal_pack(name).replace("src/example.rs", "src/check.rs"),
        );
    }
    let request = repo.request(LocalContentMode::WorkingTreeTracked);
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    let mut context = LocalPackRequestContext::new(request);
    context.cancel();
    let results = ["terminal-one", "terminal-two", "terminal-three"]
        .map(|name| run_local_doctor_pack(catalog.packs.get(name).unwrap(), &mut context));
    assert_eq!(
        results
            .iter()
            .map(|result| result.entities.len())
            .sum::<usize>(),
        1
    );
    assert_eq!(results[0].entities[0]["reason_code"], "cancelled");
}

fn contains_all_pack(name: &str, contains: &[&str]) -> String {
    let items = contains
        .iter()
        .map(|item| format!("\"{item}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "schema_version = 1\nname = \"{name}\"\ndescription = \"x\"\n\n[[checks]]\nid = \"x\"\nkind = \"file-contains-all\"\npath = \"a\"\ncontains = [{items}]\nseverity = \"warning\"\n"
    )
}

fn assert_diagnostic(
    catalog: &leio_code::doctors::local_packs::LocalDoctorCatalog,
    path: &str,
    code: &'static str,
    field: Option<&str>,
) {
    let diagnostic = catalog
        .diagnostics
        .iter()
        .find(|item| item.relative_path == path)
        .unwrap_or_else(|| panic!("missing diagnostic for {path}: {:?}", catalog.diagnostics));
    assert_eq!(diagnostic.code, code, "{diagnostic:?}");
    assert_eq!(diagnostic.field.as_deref(), field, "{diagnostic:?}");
}

fn assert_mode_budget_boundaries(mode: LocalContentMode) {
    let repo = TestRepo::new();
    for name in ["d-fourth", "c-third", "b-second", "a-first"] {
        repo.write_pack(name, minimal_pack(name));
    }
    if mode == LocalContentMode::RevisionTracked {
        repo.git_commit("commit reverse-created manifests");
    }
    let manifest_bytes = ["a-first", "b-second", "c-third", "d-fourth"]
        .iter()
        .map(|name| {
            fs::metadata(repo.path().join(format!(".leio-code/doctors/{name}.toml")))
                .expect("manifest metadata")
                .len()
        })
        .sum::<u64>();
    let first_two_manifest_bytes = ["a-first", "b-second"]
        .iter()
        .map(|name| {
            fs::metadata(repo.path().join(format!(".leio-code/doctors/{name}.toml")))
                .expect("manifest metadata")
                .len()
        })
        .sum::<u64>();

    let mut exact_packs = repo.request(mode);
    exact_packs.budgets.max_pack_files = 4;
    let catalog = discover_local_doctor_packs(&exact_packs, &compiled_names());
    assert_eq!(
        catalog.packs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["a-first", "b-second", "c-third", "d-fourth"]
    );
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);

    let mut over_packs = repo.request(mode);
    over_packs.budgets.max_pack_files = 2;
    let catalog = discover_local_doctor_packs(&over_packs, &compiled_names());
    assert_eq!(
        catalog.packs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["a-first", "b-second"]
    );
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_diagnostic(
        &catalog,
        ".leio-code/doctors/c-third.toml",
        "pack_limit_exceeded",
        None,
    );
    assert_eq!(
        catalog
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "pack_limit_exceeded")
            .count(),
        1
    );

    let mut exact_entries = repo.request(mode);
    exact_entries.budgets.max_directory_entries = 4;
    let catalog = discover_local_doctor_packs(&exact_entries, &compiled_names());
    assert_eq!(catalog.packs.len(), 4);
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);

    let mut over_entries = repo.request(mode);
    over_entries.budgets.max_directory_entries = 3;
    let catalog = discover_local_doctor_packs(&over_entries, &compiled_names());
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_diagnostic(
        &catalog,
        ".leio-code/doctors",
        "directory_entry_limit_exceeded",
        None,
    );

    let mut exact_bytes = repo.request(mode);
    exact_bytes.budgets.max_manifest_input_bytes = manifest_bytes;
    let catalog = discover_local_doctor_packs(&exact_bytes, &compiled_names());
    assert_eq!(catalog.packs.len(), 4);
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);

    let mut over_bytes = repo.request(mode);
    over_bytes.budgets.max_manifest_input_bytes = first_two_manifest_bytes;
    let catalog = discover_local_doctor_packs(&over_bytes, &compiled_names());
    assert_eq!(
        catalog.packs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["a-first", "b-second"]
    );
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_diagnostic(
        &catalog,
        ".leio-code/doctors/c-third.toml",
        "manifest_input_limit_exceeded",
        None,
    );
    assert_eq!(
        catalog
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "manifest_input_limit_exceeded")
            .count(),
        1
    );
}

fn compiled_names() -> BTreeSet<String> {
    doctor_names().into_iter().map(str::to_owned).collect()
}

fn run_pack(
    repo: &TestRepo,
    mode: LocalContentMode,
    name: &str,
) -> leio_code::model::QueryEnvelope {
    let request = repo.request(mode);
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);
    let pack = catalog.packs.get(name).expect("admitted pack");
    run_local_doctor_pack(pack, &mut LocalPackRequestContext::new(request))
}

fn run_pack_with_request(request: LocalPackRequest, name: &str) -> leio_code::model::QueryEnvelope {
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);
    let pack = catalog.packs.get(name).expect("admitted pack");
    run_local_doctor_pack(pack, &mut LocalPackRequestContext::new(request))
}

#[test]
fn absent_directory_is_an_empty_catalog() {
    let repo = TestRepo::new();
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert!(catalog.packs.is_empty());
    assert!(catalog.diagnostics.is_empty());
}

#[test]
fn revision_mode_absent_directory_is_an_empty_catalog() {
    let repo = TestRepo::new();
    repo.write("README.md", "fixture\n");
    repo.git_add("README.md");
    repo.git_commit("repository without local doctors");
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::RevisionTracked),
        &compiled_names(),
    );
    assert!(catalog.packs.is_empty());
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);
}

#[test]
fn path_exists_reports_safe_missing_path_as_a_finding() {
    let repo = TestRepo::new();
    repo.write_pack(
        "required-path",
        path_exists_pack("required-path", "src/required.rs"),
    );
    let envelope = run_pack(&repo, LocalContentMode::WorkingTreeTracked, "required-path");
    assert_eq!(envelope.warnings.len(), 1);
    assert_eq!(envelope.entities[0]["path"], "src/required.rs");
    assert_eq!(envelope.entities[0]["reason_code"], "missing_path");
}

#[test]
fn path_exists_accepts_a_tracked_directory() {
    let repo = TestRepo::new();
    repo.write("src/owned.rs", "pub fn owned() {}\n");
    repo.git_add("src/owned.rs");
    repo.write_pack(
        "tracked-directory",
        path_exists_pack("tracked-directory", "src"),
    );
    let envelope = run_pack(
        &repo,
        LocalContentMode::WorkingTreeTracked,
        "tracked-directory",
    );
    assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    assert_eq!(envelope.entities[0]["passed"], true);
}

#[test]
fn working_tree_rejects_untracked_content_reads_without_leaking_literals() {
    let repo = TestRepo::new();
    repo.write(".env", "TOKEN=secret-value\n");
    repo.write_pack(
        "tracked-only",
        minimal_pack("tracked-only")
            .replace("src/example.rs", ".env")
            .replace("required_symbol", "TOKEN="),
    );
    let envelope = run_pack(&repo, LocalContentMode::WorkingTreeTracked, "tracked-only");
    assert_eq!(envelope.entities[0]["reason_code"], "ineligible_input");
    let rendered = serde_json::to_string(&envelope).expect("serialize envelope");
    assert!(!rendered.contains("TOKEN="));
    assert!(!rendered.contains("secret-value"));
}

#[test]
fn revision_mode_reads_committed_content_not_the_mutable_working_tree() {
    let repo = TestRepo::new();
    repo.write("src/check.rs", "const REQUIRED: bool = true;\n");
    repo.git_add("src/check.rs");
    repo.write_pack(
        "revision-content",
        minimal_pack("revision-content")
            .replace("src/example.rs", "src/check.rs")
            .replace("required_symbol", "REQUIRED"),
    );
    repo.git_commit("commit assertion input");
    repo.write("src/check.rs", "const MUTATED: bool = true;\n");
    let envelope = run_pack(&repo, LocalContentMode::RevisionTracked, "revision-content");
    assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    assert_eq!(envelope.entities[0]["passed"], true);
}

#[test]
fn revision_catalog_keeps_evaluation_on_the_discovered_commit_after_head_moves() {
    let repo = TestRepo::new();
    repo.write("src/check.rs", "const PINNED: bool = true;\n");
    repo.git_add("src/check.rs");
    repo.write_pack(
        "pinned-head",
        minimal_pack("pinned-head")
            .replace("src/example.rs", "src/check.rs")
            .replace("required_symbol", "PINNED"),
    );
    repo.git_commit("first pinned revision");

    let request = repo.request(LocalContentMode::RevisionTracked);
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    let pack = catalog.packs.get("pinned-head").expect("admitted pack");

    repo.write("src/check.rs", "const MOVED: bool = true;\n");
    repo.git_add("src/check.rs");
    repo.git_commit("move HEAD after catalog discovery");

    let envelope = run_local_doctor_pack(pack, &mut LocalPackRequestContext::new(request));
    assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    assert_eq!(envelope.entities[0]["passed"], true);
}

fn per_file_input_limit_is_exact_in(mode: LocalContentMode) {
    let exact = TestRepo::new();
    exact.write("src/check.rs", "good");
    exact.git_add("src/check.rs");
    exact.write_pack(
        "exact-input-limit",
        minimal_pack("exact-input-limit")
            .replace("src/example.rs", "src/check.rs")
            .replace("required_symbol", "good"),
    );
    if mode == LocalContentMode::RevisionTracked {
        exact.git_commit("exact input limit");
    }
    let mut request = exact.request(mode);
    request.budgets.max_input_bytes_per_file = 4;
    let envelope = run_pack_with_request(request, "exact-input-limit");
    assert_eq!(envelope.entities[0]["passed"], true);

    let over = TestRepo::new();
    over.write("src/check.rs", "good!");
    over.git_add("src/check.rs");
    over.write_pack(
        "over-input-limit",
        minimal_pack("over-input-limit")
            .replace("src/example.rs", "src/check.rs")
            .replace("required_symbol", "good"),
    );
    if mode == LocalContentMode::RevisionTracked {
        over.git_commit("over input limit");
    }
    let mut request = over.request(mode);
    request.budgets.max_input_bytes_per_file = 4;
    let envelope = run_pack_with_request(request, "over-input-limit");
    assert_eq!(envelope.entities[0]["reason_code"], "input_too_large");
}

#[test]
fn per_file_input_limit_is_exact_and_first_over_in_both_content_modes() {
    per_file_input_limit_is_exact_in(LocalContentMode::WorkingTreeTracked);
    per_file_input_limit_is_exact_in(LocalContentMode::RevisionTracked);
}

#[test]
fn invalid_utf8_input_is_a_safe_failure_without_source_leakage() {
    let repo = TestRepo::new();
    repo.write_bytes("src/check.rs", b"safe\n\xffprivate");
    repo.git_add("src/check.rs");
    repo.write_pack(
        "invalid-utf8-input",
        minimal_pack("invalid-utf8-input").replace("src/example.rs", "src/check.rs"),
    );
    let envelope = run_pack(
        &repo,
        LocalContentMode::WorkingTreeTracked,
        "invalid-utf8-input",
    );
    assert_eq!(envelope.entities[0]["reason_code"], "invalid_utf8");
    assert!(
        !serde_json::to_string(&envelope)
            .unwrap()
            .contains("private")
    );
}

#[test]
fn denied_prefixes_are_exact_and_do_not_block_lookalike_paths() {
    let repo = TestRepo::new();
    repo.write(".gitconfig", "safe\n");
    repo.git_add(".gitconfig");
    repo.write_pack(
        "denied-prefix",
        minimal_pack("denied-prefix")
            .replace("src/example.rs", ".git/config")
            .replace("required_symbol", "config"),
    );
    repo.write_pack(
        "allowed-lookalike",
        minimal_pack("allowed-lookalike")
            .replace("src/example.rs", ".gitconfig")
            .replace("required_symbol", "safe"),
    );
    let denied = run_pack(&repo, LocalContentMode::WorkingTreeTracked, "denied-prefix");
    assert_eq!(denied.entities[0]["reason_code"], "denied_path");
    let allowed = run_pack(
        &repo,
        LocalContentMode::WorkingTreeTracked,
        "allowed-lookalike",
    );
    assert_eq!(allowed.entities[0]["passed"], true);
}

#[test]
fn failed_toml_assertion_and_severity_provenance_are_bounded() {
    let repo = TestRepo::new();
    repo.write("Cargo.toml", "[workspace]\nmembers = [\"member-a\"]\n");
    repo.git_add("Cargo.toml");
    repo.write_pack(
        "toml-provenance",
        "schema_version = 1\nname = \"toml-provenance\"\ndescription = \"x\"\n\n[[checks]]\nid = \"missing-warning\"\nkind = \"toml-array-contains\"\npath = \"Cargo.toml\"\nkey = \"workspace.members\"\nvalue = \"member-b\"\nseverity = \"warning\"\n\n[[checks]]\nid = \"missing-info\"\nkind = \"file-contains\"\npath = \"Cargo.toml\"\ncontains = \"NOT_PRESENT\"\nseverity = \"info\"\n".to_owned(),
    );
    let envelope = run_pack(
        &repo,
        LocalContentMode::WorkingTreeTracked,
        "toml-provenance",
    );
    assert_eq!(envelope.warnings.len(), 1);
    assert_eq!(envelope.entities.len(), 2);
    assert!(
        envelope
            .entities
            .iter()
            .all(|entity| entity["line"].is_null())
    );
    let rendered = serde_json::to_string(&envelope).unwrap();
    assert!(!rendered.contains("member-b"));
    assert!(!rendered.contains("NOT_PRESENT"));
}

#[test]
fn simple_assertions_report_line_provenance_and_toml_dotted_arrays() {
    let repo = TestRepo::new();
    repo.write("src/check.rs", "first\nneedle\nlast\n");
    repo.git_add("src/check.rs");
    repo.write("Cargo.toml", "[workspace]\nmembers = [\"member-a\"]\n");
    repo.git_add("Cargo.toml");
    repo.write_pack("simple-assertions", "schema_version = 1\nname = \"simple-assertions\"\ndescription = \"x\"\n\n[[checks]]\nid = \"contains\"\nkind = \"file-contains\"\npath = \"src/check.rs\"\ncontains = \"needle\"\nseverity = \"warning\"\n\n[[checks]]\nid = \"contains-all\"\nkind = \"file-contains-all\"\npath = \"src/check.rs\"\ncontains = [\"first\", \"last\"]\nseverity = \"warning\"\n\n[[checks]]\nid = \"not-contains\"\nkind = \"file-not-contains\"\npath = \"src/check.rs\"\ncontains = \"forbidden\"\nseverity = \"warning\"\n\n[[checks]]\nid = \"toml-array\"\nkind = \"toml-array-contains\"\npath = \"Cargo.toml\"\nkey = \"workspace.members\"\nvalue = \"member-a\"\nseverity = \"warning\"\n".to_owned());
    let envelope = run_pack(
        &repo,
        LocalContentMode::WorkingTreeTracked,
        "simple-assertions",
    );
    assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    assert_eq!(envelope.entities[0]["line"], 2);
    assert!(
        envelope
            .entities
            .iter()
            .all(|entity| entity["passed"] == true)
    );
}

#[test]
fn file_not_contains_failure_reports_line_without_source_content() {
    let repo = TestRepo::new();
    repo.write("src/check.rs", "first\nFORBIDDEN_SECRET\nlast\n");
    repo.git_add("src/check.rs");
    repo.write_pack(
        "not-contains-line",
        "schema_version = 1\nname = \"not-contains-line\"\ndescription = \"x\"\n\n[[checks]]\nid = \"forbidden\"\nkind = \"file-not-contains\"\npath = \"src/check.rs\"\ncontains = \"FORBIDDEN_SECRET\"\nseverity = \"warning\"\n".to_owned(),
    );
    let envelope = run_pack(
        &repo,
        LocalContentMode::WorkingTreeTracked,
        "not-contains-line",
    );
    assert_eq!(envelope.entities[0]["line"], 2);
    assert!(
        !serde_json::to_string(&envelope)
            .unwrap()
            .contains("FORBIDDEN_SECRET")
    );
}

#[test]
fn working_tree_rejects_tracked_but_ignored_and_literal_metacharacter_inputs() {
    let repo = TestRepo::new();
    repo.write("tracked-secret.txt", "private\n");
    repo.git_add("tracked-secret.txt");
    repo.write("literal[secret].txt", "private\n");
    repo.git_add("literal[secret].txt");
    repo.write(".gitignore", "tracked-secret.txt\n");
    repo.git_add(".gitignore");
    repo.write_pack(
        "ignored-input",
        minimal_pack("ignored-input").replace("src/example.rs", "tracked-secret.txt"),
    );
    repo.write_pack(
        "literal-input",
        minimal_pack("literal-input").replace("src/example.rs", "literal[secret].txt"),
    );
    let ignored = run_pack(&repo, LocalContentMode::WorkingTreeTracked, "ignored-input");
    assert_eq!(ignored.entities[0]["reason_code"], "ineligible_input");
    let literal = run_pack(&repo, LocalContentMode::WorkingTreeTracked, "literal-input");
    assert_eq!(literal.entities[0]["passed"], false);
    assert_eq!(literal.entities[0]["reason_code"], "literal_missing");
}

#[test]
fn revision_path_exists_rejects_symlink_and_uses_committed_tree_shape() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let repo = TestRepo::new();
        repo.write("real.txt", "safe\n");
        symlink("real.txt", repo.path().join("linked.txt")).expect("symlink");
        repo.git_add("real.txt");
        repo.git_add("linked.txt");
        repo.write_pack(
            "revision-symlink",
            path_exists_pack("revision-symlink", "linked.txt"),
        );
        repo.git_commit("commit symlink");
        let envelope = run_pack(&repo, LocalContentMode::RevisionTracked, "revision-symlink");
        assert_eq!(envelope.entities[0]["reason_code"], "ineligible_input");
    }
}

#[test]
fn working_tree_rejects_untracked_and_ignored_manifests() {
    let repo = TestRepo::new();
    repo.write(
        ".leio-code/doctors/untracked.toml",
        &minimal_pack("untracked"),
    );
    repo.write(".gitignore", ".leio-code/doctors/ignored.toml\n");
    repo.git_add(".gitignore");
    repo.write(".leio-code/doctors/ignored.toml", &minimal_pack("ignored"));
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert!(catalog.packs.is_empty());
    assert_eq!(
        catalog
            .diagnostics
            .iter()
            .filter(|item| item.code == "ineligible_manifest")
            .count(),
        2
    );
}

#[test]
fn revision_mode_uses_committed_manifest_bytes_not_working_tree() {
    let repo = TestRepo::new();
    repo.write_pack("stable", minimal_pack("stable"));
    repo.git_commit("commit manifest");
    repo.write(".leio-code/doctors/stable.toml", &minimal_pack("mutated"));
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::RevisionTracked),
        &compiled_names(),
    );
    assert!(catalog.diagnostics.is_empty());
    assert!(catalog.packs.contains_key("stable"));
    assert!(!catalog.packs.contains_key("mutated"));
}

#[test]
fn revision_mode_proves_exact_and_first_over_budget_boundaries() {
    assert_mode_budget_boundaries(LocalContentMode::RevisionTracked);
}

#[test]
fn revision_manifest_byte_limit_accepts_exact_and_reports_first_over_precisely() {
    let exact = TestRepo::new();
    let exact_manifest = minimal_pack("manifest-exact");
    let exact_len = exact_manifest.len() as u64;
    exact.write_pack("manifest-exact", exact_manifest);
    exact.git_commit("exact revision manifest limit");
    let mut request = exact.request(LocalContentMode::RevisionTracked);
    request.budgets.max_manifest_bytes = exact_len;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);
    assert!(catalog.packs.contains_key("manifest-exact"));

    let over = TestRepo::new();
    let over_manifest = minimal_pack("manifest-over");
    let over_len = over_manifest.len() as u64;
    over.write_pack("manifest-over", over_manifest);
    over.git_commit("over revision manifest limit");
    let mut request = over.request(LocalContentMode::RevisionTracked);
    request.budgets.max_manifest_bytes = over_len - 1;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert!(catalog.packs.is_empty());
    assert_diagnostic(
        &catalog,
        ".leio-code/doctors/manifest-over.toml",
        "manifest_too_large",
        None,
    );
}

#[test]
fn revision_directory_budget_counts_all_immediate_entries() {
    let repo = TestRepo::new();
    repo.write_pack("visible", minimal_pack("visible"));
    repo.write(
        ".leio-code/doctors/.temporary.toml",
        &minimal_pack("temporary"),
    );
    repo.write(
        ".leio-code/doctors/nested/ignored.toml",
        &minimal_pack("ignored"),
    );
    repo.write(".leio-code/doctors/readme.txt", "not a manifest");
    repo.git_add(".leio-code/doctors/.temporary.toml");
    repo.git_add(".leio-code/doctors/nested/ignored.toml");
    repo.git_add(".leio-code/doctors/readme.txt");
    repo.git_commit("commit immediate entry fixtures");

    let mut exact = repo.request(LocalContentMode::RevisionTracked);
    exact.budgets.max_directory_entries = 4;
    let catalog = discover_local_doctor_packs(&exact, &compiled_names());
    assert_eq!(
        catalog.packs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["visible"]
    );
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);

    let mut over = repo.request(LocalContentMode::RevisionTracked);
    over.budgets.max_directory_entries = 3;
    let catalog = discover_local_doctor_packs(&over, &compiled_names());
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_diagnostic(
        &catalog,
        ".leio-code/doctors",
        "directory_entry_limit_exceeded",
        None,
    );
}

#[cfg(unix)]
#[test]
fn revision_doctor_directory_symlink_is_rejected_by_tree_shape() {
    use std::os::unix::fs::symlink;

    let repo = TestRepo::new();
    repo.write("outside/pack.toml", &minimal_pack("outside"));
    fs::create_dir_all(repo.path().join(".leio-code")).expect("leio directory");
    symlink("../outside", repo.path().join(".leio-code/doctors")).expect("doctor symlink");
    repo.git_add("outside/pack.toml");
    repo.git_add(".leio-code/doctors");
    repo.git_commit("symlinked revision doctor directory");

    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::RevisionTracked),
        &compiled_names(),
    );
    assert!(catalog.packs.is_empty());
    assert_diagnostic(
        &catalog,
        ".leio-code/doctors",
        "ineligible_doctor_directory",
        None,
    );
}

#[test]
fn revision_doctor_directory_gitlink_is_rejected_by_tree_shape() {
    let repo = TestRepo::new();
    repo.write("README.md", "fixture\n");
    repo.git_add("README.md");
    repo.git_commit("base commit for gitlink");
    let object = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf8 object")
    .trim()
    .to_owned();
    let cacheinfo = format!("160000,{object},.leio-code/doctors");
    assert!(
        Command::new("git")
            .args(["update-index", "--add", "--cacheinfo", &cacheinfo])
            .current_dir(repo.path())
            .status()
            .expect("stage gitlink")
            .success()
    );
    repo.git_commit("gitlink doctor directory");

    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::RevisionTracked),
        &compiled_names(),
    );
    assert!(catalog.packs.is_empty());
    assert_diagnostic(
        &catalog,
        ".leio-code/doctors",
        "ineligible_doctor_directory",
        None,
    );
}

#[test]
fn every_assertion_variant_parses_and_missing_required_fields_are_schema_errors() {
    let repo = TestRepo::new();
    let variants = [
        ("exists", "path-exists", "path = \"a\""),
        (
            "all",
            "file-contains-all",
            "path = \"a\"\ncontains = [\"x\"]",
        ),
        ("not", "file-not-contains", "path = \"a\"\ncontains = \"x\""),
        (
            "make",
            "make-target-contains",
            "path = \"Makefile\"\ntarget = \"x\"\ncontains = [\"x\"]",
        ),
        (
            "toml",
            "toml-array-contains",
            "path = \"Cargo.toml\"\nkey = \"x\"\nvalue = \"y\"",
        ),
        (
            "cargo",
            "cargo-feature-packages-covered-by-make-target",
            "workspace = \"Cargo.toml\"\nfeature = \"x\"\nmakefile = \"Makefile\"\ntarget = \"x\"\nintegrity_severity = \"warning\"",
        ),
    ];
    for (id, kind, fields) in variants {
        let name = format!("variant-{id}");
        repo.write_pack(&name, format!("schema_version = 1\nname = \"{name}\"\ndescription = \"x\"\n[[checks]]\nid = \"x\"\nkind = \"{kind}\"\n{fields}\nseverity = \"warning\"\n"));
    }
    repo.write_pack("missing-field", "schema_version = 1\nname = \"missing-field\"\ndescription = \"x\"\n[[checks]]\nid = \"x\"\nkind = \"file-contains\"\npath = \"a\"\nseverity = \"warning\"\n".to_string());
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert_eq!(catalog.packs.len(), 6);
    assert!(
        catalog
            .diagnostics
            .iter()
            .any(|item| item.code == "schema_parse_error")
    );
}

#[test]
fn every_variant_rejects_unknown_and_missing_required_fields() {
    let repo = TestRepo::new();
    let variants = [
        ("path-exists", "path = \"a\""),
        ("file-contains", "path = \"a\"\ncontains = \"x\""),
        ("file-contains-all", "path = \"a\"\ncontains = [\"x\"]"),
        ("file-not-contains", "path = \"a\"\ncontains = \"x\""),
        (
            "make-target-contains",
            "path = \"a\"\ntarget = \"x\"\ncontains = [\"x\"]",
        ),
        (
            "toml-array-contains",
            "path = \"a\"\nkey = \"x\"\nvalue = \"y\"",
        ),
        (
            "cargo-feature-packages-covered-by-make-target",
            "workspace = \"a\"\nfeature = \"x\"\nmakefile = \"b\"\ntarget = \"x\"\nintegrity_severity = \"warning\"",
        ),
    ];
    for (index, (kind, fields)) in variants.iter().enumerate() {
        let unknown = format!("unknown-{index}");
        repo.write_pack(&unknown, format!("schema_version=1\nname=\"{unknown}\"\ndescription=\"x\"\n[[checks]]\nid=\"x\"\nkind=\"{kind}\"\n{fields}\nseverity=\"warning\"\nextra=1\n"));
        let missing = format!("missing-{index}");
        repo.write_pack(&missing, format!("schema_version=1\nname=\"{missing}\"\ndescription=\"x\"\n[[checks]]\nid=\"x\"\nkind=\"{kind}\"\nseverity=\"warning\"\n"));
    }
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics.len(), 14);
    assert!(
        catalog
            .diagnostics
            .iter()
            .all(|d| d.code == "schema_parse_error")
    );
}

#[test]
fn working_tree_proves_reverse_created_exact_and_first_over_budget_boundaries() {
    assert_mode_budget_boundaries(LocalContentMode::WorkingTreeTracked);
}

#[test]
fn path_depth_boundaries_report_each_path_field_precisely() {
    let exact = TestRepo::new();
    exact.write_pack(
        "depth-path-exact",
        path_exists_pack("depth-path-exact", "a/b/c"),
    );
    exact.write_pack(
        "depth-cargo-exact",
        cargo_pack("depth-cargo-exact", "a/b/c", "d/e/f"),
    );
    let mut request = exact.request(LocalContentMode::WorkingTreeTracked);
    request.budgets.max_path_depth = 3;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert_eq!(catalog.packs.len(), 2);
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);

    let over = TestRepo::new();
    over.write_pack(
        "depth-path-over",
        path_exists_pack("depth-path-over", "a/b/c/d"),
    );
    over.write_pack(
        "depth-workspace-over",
        cargo_pack("depth-workspace-over", "a/b/c/d", "e/f/g"),
    );
    over.write_pack(
        "depth-makefile-over",
        cargo_pack("depth-makefile-over", "a/b/c", "d/e/f/g"),
    );
    let mut request = over.request(LocalContentMode::WorkingTreeTracked);
    request.budgets.max_path_depth = 3;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics.len(), 3);
    for (name, field) in [
        ("depth-path-over", "checks.path"),
        ("depth-workspace-over", "checks.workspace"),
        ("depth-makefile-over", "checks.makefile"),
    ] {
        assert_diagnostic(
            &catalog,
            &format!(".leio-code/doctors/{name}.toml"),
            "invalid_path",
            Some(field),
        );
    }
}

#[test]
fn scalar_byte_boundaries_report_each_path_field_precisely() {
    const LIMIT: usize = 32;
    let exact_path = format!("a/{}", "b".repeat(LIMIT - 2));
    let over_path = format!("a/{}", "b".repeat(LIMIT - 1));
    assert_eq!(exact_path.len(), LIMIT);
    assert_eq!(over_path.len(), LIMIT + 1);

    let exact = TestRepo::new();
    exact.write_pack(
        "scalar-path-exact",
        path_exists_pack("scalar-path-exact", &exact_path),
    );
    exact.write_pack(
        "scalar-cargo-exact",
        cargo_pack("scalar-cargo-exact", &exact_path, &exact_path),
    );
    let mut request = exact.request(LocalContentMode::WorkingTreeTracked);
    request.budgets.max_scalar_bytes = LIMIT;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert_eq!(catalog.packs.len(), 2);
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);

    let over = TestRepo::new();
    over.write_pack(
        "scalar-path-over",
        path_exists_pack("scalar-path-over", &over_path),
    );
    over.write_pack(
        "scalar-workspace-over",
        cargo_pack("scalar-workspace-over", &over_path, &exact_path),
    );
    over.write_pack(
        "scalar-makefile-over",
        cargo_pack("scalar-makefile-over", &exact_path, &over_path),
    );
    let mut request = over.request(LocalContentMode::WorkingTreeTracked);
    request.budgets.max_scalar_bytes = LIMIT;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics.len(), 3);
    for (name, field) in [
        ("scalar-path-over", "checks.path"),
        ("scalar-workspace-over", "checks.workspace"),
        ("scalar-makefile-over", "checks.makefile"),
    ] {
        assert_diagnostic(
            &catalog,
            &format!(".leio-code/doctors/{name}.toml"),
            "scalar_limit_exceeded",
            Some(field),
        );
    }
}

#[test]
fn list_item_boundary_reports_the_exact_field_and_manifest() {
    let repo = TestRepo::new();
    repo.write_pack("list-exact", contains_all_pack("list-exact", &["x", "y"]));
    repo.write_pack(
        "list-over",
        contains_all_pack("list-over", &["x", "y", "z"]),
    );
    let mut request = repo.request(LocalContentMode::WorkingTreeTracked);
    request.budgets.max_list_items = 2;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert_eq!(
        catalog.packs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["list-exact"]
    );
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_diagnostic(
        &catalog,
        ".leio-code/doctors/list-over.toml",
        "list_limit_exceeded",
        Some("checks.contains"),
    );
}

#[test]
fn revision_deadline_rejects_before_acceptance() {
    let repo = TestRepo::new();
    repo.write_pack("one", minimal_pack("one"));
    repo.git_commit("one");
    let mut request = repo.request(LocalContentMode::RevisionTracked);
    request.deadline = std::time::Instant::now() - std::time::Duration::from_millis(1);
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics[0].code, "deadline_exceeded");
}

#[test]
fn discovers_valid_packs_in_filename_order() {
    let repo = TestRepo::new();
    repo.write_pack("z-last", minimal_pack("z-last"));
    repo.write_pack("a-first", minimal_pack("a-first"));
    let request = repo.request(LocalContentMode::WorkingTreeTracked);
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert_eq!(
        catalog.packs.keys().cloned().collect::<Vec<_>>(),
        ["a-first", "z-last"]
    );
    assert!(catalog.diagnostics.is_empty());
}

#[test]
fn ignores_nested_hidden_and_non_toml_entries() {
    let repo = TestRepo::new();
    repo.write_pack("visible", minimal_pack("visible"));
    repo.write(
        ".leio-code/doctors/.temporary.toml",
        &minimal_pack("temporary"),
    );
    repo.write(
        ".leio-code/doctors/nested/ignored.toml",
        &minimal_pack("ignored"),
    );
    repo.write(".leio-code/doctors/readme.txt", "not a manifest");
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert_eq!(
        catalog.packs.keys().cloned().collect::<Vec<_>>(),
        ["visible"]
    );
    assert!(catalog.diagnostics.is_empty());
}

#[test]
fn rejects_reserved_and_compiled_names() {
    let repo = TestRepo::new();
    repo.write_pack("all", minimal_pack("all"));
    repo.write_pack("self-contract", minimal_pack("self-contract"));
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics.len(), 2);
    assert!(
        catalog
            .diagnostics
            .iter()
            .all(|item| item.code == "reserved_or_compiled_name")
    );
}

#[test]
fn schema_failures_are_diagnostic_and_bounded() {
    let repo = TestRepo::new();
    repo.write_pack(
        "unknown-field",
        format!(
            "{}extra = \"{}\"\n",
            minimal_pack("unknown-field"),
            "x".repeat(70_000)
        ),
    );
    repo.write_pack("bad-slug", minimal_pack("bad_slug"));
    repo.write_pack("mismatch", minimal_pack("other"));
    repo.write_pack(
        "duplicate-checks",
        minimal_pack("duplicate-checks").replace(
            "id = \"required-anchor\"",
            "id = \"same\"\nkind = \"file-contains\"\npath = \"src/second.rs\"\ncontains = \"x\"\nseverity = \"warning\"\n\n[[checks]]\nid = \"same\"",
        ),
    );
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics.len(), 4);
    for diagnostic in &catalog.diagnostics {
        assert!(diagnostic.relative_path.starts_with(".leio-code/doctors/"));
        assert!(diagnostic.message.len() <= 1024, "{diagnostic:?}");
        assert!(!diagnostic.message.contains(&"x".repeat(70_000)));
    }
}

#[test]
fn rejects_schema_boundaries() {
    let repo = TestRepo::new();
    let no_checks = minimal_pack("no-checks").replace(
        "\n[[checks]]\nid = \"required-anchor\"\nkind = \"file-contains\"\npath = \"src/example.rs\"\ncontains = \"required_symbol\"\nseverity = \"warning\"\n",
        "\nchecks = []\n",
    );
    repo.write_pack("no-checks", no_checks);
    repo.write_pack(
        "bad-version",
        minimal_pack("bad-version").replace("schema_version = 1", "schema_version = 2"),
    );
    repo.write_pack(
        "bad-kind",
        minimal_pack("bad-kind").replace("kind = \"file-contains\"", "kind = \"unknown\""),
    );
    repo.write_pack(
        "bad-suite",
        minimal_pack("bad-suite").replace(
            "description = \"Validate bad-suite.\"",
            "description = \"Validate bad-suite.\"\nsuites = [\"ci\"]",
        ),
    );
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics.len(), 4);
}

#[test]
fn manifest_limits_produce_bounded_diagnostics() {
    let repo = TestRepo::new();
    repo.write_pack(
        "oversized",
        format!("# {}\n{}", "x".repeat(1_048_576), minimal_pack("oversized")),
    );
    let mut request = repo.request(LocalContentMode::WorkingTreeTracked);
    request.budgets.max_manifest_bytes = 32;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_eq!(catalog.diagnostics[0].code, "manifest_too_large");
    assert!(catalog.diagnostics[0].message.len() <= 1024);
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_manifest_and_doctor_directory() {
    use std::os::unix::fs::symlink;

    let manifest_repo = TestRepo::new();
    manifest_repo.write("outside.toml", &minimal_pack("linked"));
    fs::create_dir_all(manifest_repo.path().join(".leio-code/doctors")).expect("doctor directory");
    symlink(
        manifest_repo.path().join("outside.toml"),
        manifest_repo.path().join(".leio-code/doctors/linked.toml"),
    )
    .expect("symlink manifest");
    let catalog = discover_local_doctor_packs(
        &manifest_repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_eq!(catalog.diagnostics[0].code, "symlink_rejected");

    let directory_repo = TestRepo::new();
    directory_repo.write("real/pack.toml", &minimal_pack("pack"));
    fs::create_dir_all(directory_repo.path().join(".leio-code")).expect("leio directory");
    symlink(
        directory_repo.path().join("real"),
        directory_repo.path().join(".leio-code/doctors"),
    )
    .expect("symlink directory");
    let catalog = discover_local_doctor_packs(
        &directory_repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_eq!(catalog.diagnostics[0].code, "symlink_rejected");
}

#[test]
fn non_regular_manifest_is_rejected() {
    let repo = TestRepo::new();
    fs::create_dir_all(repo.path().join(".leio-code/doctors/not-a-file.toml"))
        .expect("directory manifest");
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_eq!(catalog.diagnostics[0].code, "non_regular_file");
}

#[cfg(unix)]
#[test]
fn special_manifest_is_rejected() {
    let repo = TestRepo::new();
    let fifo = repo.path().join(".leio-code/doctors/stream.toml");
    fs::create_dir_all(fifo.parent().expect("fifo parent")).expect("doctor directory");
    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo must create the special fixture");
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_eq!(catalog.diagnostics[0].code, "non_regular_file");
}

#[cfg(unix)]
#[test]
fn working_tree_special_assertion_inputs_are_rejected_without_reading_or_hanging() {
    use std::os::unix::net::UnixDatagram;

    let repo = TestRepo::new();
    let fifo = repo.path().join("input.fifo");
    let socket = repo.path().join("input.sock");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo")
            .success()
    );
    let _socket = UnixDatagram::bind(&socket).expect("bind unix socket");
    repo.write_pack("fifo-input", path_exists_pack("fifo-input", "input.fifo"));
    repo.write_pack(
        "socket-input",
        path_exists_pack("socket-input", "input.sock"),
    );

    for name in ["fifo-input", "socket-input"] {
        let envelope = run_pack(&repo, LocalContentMode::WorkingTreeTracked, name);
        assert_eq!(envelope.entities[0]["reason_code"], "ineligible_input");
    }
}

#[test]
fn budgets_limit_pack_count_and_checks_per_pack() {
    let repo = TestRepo::new();
    repo.write_pack("first", minimal_pack("first"));
    repo.write_pack("second", minimal_pack("second"));
    let mut request = repo.request(LocalContentMode::WorkingTreeTracked);
    request.budgets.max_pack_files = 1;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert_eq!(catalog.packs.len(), 1);
    assert!(
        catalog
            .diagnostics
            .iter()
            .any(|item| item.code == "pack_limit_exceeded")
    );

    let repo = TestRepo::new();
    repo.write_pack("many-checks", minimal_pack("many-checks"));
    let mut request = repo.request(LocalContentMode::WorkingTreeTracked);
    request.budgets.max_checks_per_pack = 0;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert!(catalog.packs.is_empty());
    assert_eq!(
        catalog.diagnostics[0].code,
        "checks_per_pack_limit_exceeded"
    );

    let repo = TestRepo::new();
    repo.write_pack("first", minimal_pack("first"));
    repo.write_pack("second", minimal_pack("second"));
    let mut request = repo.request(LocalContentMode::WorkingTreeTracked);
    request.budgets.max_checks_per_request = 1;
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert_eq!(catalog.packs.len(), 1);
    assert!(
        catalog
            .diagnostics
            .iter()
            .any(|item| item.code == "checks_request_limit_exceeded")
    );
}
