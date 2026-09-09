use std::fs;
use std::path::Path;
use std::process::Command;

use leio_code::doctors::skill_contract::doctor_skill_contract;
use leio_code::model::QueryEnvelope;
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("create fixture repository");
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .expect("run git init");
        assert!(status.success(), "git init must succeed");
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn write(&self, relative: &str, content: impl AsRef<[u8]>) {
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(path, content).expect("write fixture file");
    }

    fn track(&self, relative: &str) {
        let status = Command::new("git")
            .args(["add", "-f", "--", relative])
            .current_dir(self.path())
            .status()
            .expect("run git add");
        assert!(status.success(), "git add must succeed for {relative}");
    }

    fn write_and_track(&self, relative: &str, content: impl AsRef<[u8]>) {
        self.write(relative, content);
        self.track(relative);
    }

    fn add_skill(
        &self,
        skill_path: &str,
        name: &str,
        description: &str,
        body: &str,
        ui: Option<&str>,
    ) {
        let skill = format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n");
        self.write_and_track(skill_path, skill);
        if let Some(ui) = ui {
            let ui_path = Path::new(skill_path)
                .parent()
                .expect("skill has a directory")
                .join("agents/openai.yaml");
            self.write_and_track(&ui_path.to_string_lossy(), ui);
        }
    }

    #[cfg(unix)]
    fn symlink_and_track(&self, relative: &str, target: &str) {
        use std::os::unix::fs::symlink;

        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create symlink parent");
        }
        symlink(target, &path).expect("create fixture symlink");
        self.track(relative);
    }
}

fn retained(path: &str, name: &str, restricted: bool) -> String {
    let restricted = if restricted {
        "\nrequires_explicit_invocation = true"
    } else {
        ""
    };
    format!("[[skills]]\npath = \"{path}\"\nname = \"{name}\"\nstatus = \"retain\"{restricted}\n")
}

fn registry(entries: &[String]) -> String {
    format!(
        "schema_version = 1\ncredential_allowlist = []\n\n{}",
        entries.join("\n")
    )
}

fn ui(name: &str) -> String {
    format!(
        "interface:\n  display_name: \"Fixture {name}\"\n  short_description: \"Validate the {name} fixture skill\"\n  default_prompt: \"Use ${name} to validate this isolated fixture.\"\n"
    )
}

fn restricted_ui(name: &str, allow_implicit_invocation: bool) -> String {
    format!(
        "{}policy:\n  allow_implicit_invocation: {allow_implicit_invocation}\n",
        ui(name)
    )
}

fn finding_count(envelope: &QueryEnvelope, path: &str, rule: &str) -> Option<u64> {
    envelope.entities.iter().find_map(|entity| {
        (entity.get("path").and_then(|value| value.as_str()) == Some(path)
            && entity.get("rule").and_then(|value| value.as_str()) == Some(rule))
        .then(|| entity.get("count").and_then(|value| value.as_u64()))
        .flatten()
    })
}

fn assert_finding(envelope: &QueryEnvelope, path: &str, rule: &str) {
    assert!(
        finding_count(envelope, path, rule).is_some(),
        "expected finding path={path:?} rule={rule:?}; entities={:?}",
        envelope.entities
    );
}

fn assert_no_finding(envelope: &QueryEnvelope, path: &str, rule: &str) {
    assert_eq!(
        finding_count(envelope, path, rule),
        None,
        "unexpected finding path={path:?} rule={rule:?}; entities={:?}",
        envelope.entities
    );
}

#[test]
fn clean_retained_skill_passes_the_contract() {
    let fixture = Fixture::new();
    let skill_path = "skills/clean/SKILL.md";
    let clean_ui = ui("clean");
    fixture.add_skill(
        skill_path,
        "clean",
        "Use when validating a clean retained skill fixture and its metadata.",
        "# Clean\n\nRead [the guide](references/guide.md#usage) or [this section](#usage).\n\n## Usage",
        Some(&clean_ui),
    );
    fixture.write_and_track(
        "skills/clean/references/guide.md",
        "# Guide\n\nThis tracked reference resolves.\n",
    );
    fixture.write_and_track(
        "skills/registry.toml",
        registry(&[retained(skill_path, "clean", false)]),
    );

    let envelope = doctor_skill_contract(fixture.path());

    assert!(
        envelope.warnings.is_empty(),
        "clean fixture should pass: {:?}",
        envelope.warnings
    );
    assert!(envelope.entities.is_empty());
}

#[test]
fn reports_frontmatter_identity_size_ui_and_restricted_policy_findings() {
    let fixture = Fixture::new();
    let mut entries = Vec::new();

    fixture.write_and_track(
        "skills/malformed/SKILL.md",
        "---\nname: [malformed\ndescription: broken\n---\n# Broken\n",
    );
    entries.push(retained("skills/malformed/SKILL.md", "malformed", false));

    fixture.write_and_track(
        "skills/extra/SKILL.md",
        "---\nname: extra\ndescription: Use when validating unsupported frontmatter keys in a fixture.\nversion: 1\n---\n# Extra\n",
    );
    entries.push(retained("skills/extra/SKILL.md", "extra", false));

    for (folder, name) in [
        ("duplicate-one", "duplicate"),
        ("duplicate-two", "duplicate"),
    ] {
        let skill_path = format!("skills/{folder}/SKILL.md");
        let duplicate_ui = ui(name);
        fixture.add_skill(
            &skill_path,
            name,
            "Use when validating duplicate skill identities across tracked folders.",
            "# Duplicate",
            Some(&duplicate_ui),
        );
        entries.push(retained(&skill_path, folder, false));
    }

    let mismatch_ui = ui("different-name");
    fixture.add_skill(
        "skills/folder-name/SKILL.md",
        "different-name",
        "Use when validating that the skill name matches its tracked folder.",
        "# Mismatch",
        Some(&mismatch_ui),
    );
    entries.push(retained(
        "skills/folder-name/SKILL.md",
        "different-name",
        false,
    ));

    let short_description = "x".repeat(39);
    let short_ui = ui("short");
    fixture.add_skill(
        "skills/short/SKILL.md",
        "short",
        &short_description,
        "# Short",
        Some(&short_ui),
    );
    entries.push(retained("skills/short/SKILL.md", "short", false));

    let long_description = "x".repeat(1025);
    let long_ui = ui("long");
    fixture.add_skill(
        "skills/long/SKILL.md",
        "long",
        &long_description,
        "# Long",
        Some(&long_ui),
    );
    entries.push(retained("skills/long/SKILL.md", "long", false));

    let oversized = format!(
        "---\nname: oversized\ndescription: Use when validating the strict skill file line-count boundary.\n---\n{}",
        "body line\n".repeat(496)
    );
    fixture.write_and_track("skills/oversized/SKILL.md", oversized);
    let oversized_ui = ui("oversized");
    fixture.write_and_track("skills/oversized/agents/openai.yaml", oversized_ui);
    entries.push(retained("skills/oversized/SKILL.md", "oversized", false));

    fixture.add_skill(
        "skills/missing-ui/SKILL.md",
        "missing-ui",
        "Use when validating that every retained skill has required UI metadata.",
        "# Missing UI",
        None,
    );
    entries.push(retained("skills/missing-ui/SKILL.md", "missing-ui", false));

    let misaligned_ui = ui("misaligned-ui-extra");
    fixture.add_skill(
        "skills/misaligned-ui/SKILL.md",
        "misaligned-ui",
        "Use when validating that UI prompts name the corresponding skill identity.",
        "# Misaligned UI",
        Some(&misaligned_ui),
    );
    entries.push(retained(
        "skills/misaligned-ui/SKILL.md",
        "misaligned-ui",
        false,
    ));

    let unsafe_ui = restricted_ui("restricted", true);
    fixture.add_skill(
        "skills/restricted/SKILL.md",
        "restricted",
        "Use when validating explicit-only invocation policy for privileged skills.",
        "# Restricted",
        Some(&unsafe_ui),
    );
    entries.push(retained("skills/restricted/SKILL.md", "restricted", true));

    fixture.write_and_track("skills/registry.toml", registry(&entries));

    let envelope = doctor_skill_contract(fixture.path());

    assert_finding(
        &envelope,
        "skills/malformed/SKILL.md",
        "frontmatter-invalid",
    );
    assert_finding(&envelope, "skills/extra/SKILL.md", "frontmatter-keys");
    assert_finding(
        &envelope,
        "skills/duplicate-one/SKILL.md",
        "skill-name-duplicate",
    );
    assert_finding(
        &envelope,
        "skills/duplicate-two/SKILL.md",
        "skill-name-duplicate",
    );
    assert_finding(
        &envelope,
        "skills/folder-name/SKILL.md",
        "skill-name-folder-mismatch",
    );
    assert_finding(&envelope, "skills/short/SKILL.md", "description-length");
    assert_finding(&envelope, "skills/long/SKILL.md", "description-length");
    assert_finding(&envelope, "skills/oversized/SKILL.md", "skill-line-count");
    assert_finding(
        &envelope,
        "skills/missing-ui/agents/openai.yaml",
        "ui-metadata-missing",
    );
    assert_finding(
        &envelope,
        "skills/misaligned-ui/agents/openai.yaml",
        "ui-default-prompt-skill",
    );
    assert_finding(
        &envelope,
        "skills/restricted/agents/openai.yaml",
        "restricted-invocation-policy",
    );
}

#[test]
fn reports_registry_discovery_and_alias_findings_but_ignores_untracked_copies() {
    let fixture = Fixture::new();
    let unregistered_ui = ui("unregistered");
    fixture.add_skill(
        "skills/unregistered/SKILL.md",
        "unregistered",
        "Use when validating discovery of tracked skills absent from the registry.",
        "# Unregistered",
        Some(&unregistered_ui),
    );

    fixture.write_and_track(".gitignore", "ignored/\n");
    fixture.write(
        "ignored/copy/SKILL.md",
        "---\nname: ignored-copy\ndescription: x\n---\n# Ignored\n",
    );
    fixture.write(
        "ignored/copy/payload.pyc",
        b"ignored generated resource with deadbeefdeadbeefdeadbeefdeadbeef",
    );

    let alias = "[[skills]]\npath = \"skills/old-name/SKILL.md\"\nname = \"old-name\"\nstatus = \"alias\"\nalias_target = \"missing-target\"\n";
    let registry = format!(
        "schema_version = 1\ncredential_allowlist = []\n\n{}\n{}",
        retained("skills/missing/SKILL.md", "missing", false),
        alias
    );
    fixture.write_and_track("skills/registry.toml", registry);

    let envelope = doctor_skill_contract(fixture.path());

    assert_finding(
        &envelope,
        "skills/unregistered/SKILL.md",
        "tracked-skill-unregistered",
    );
    assert_finding(
        &envelope,
        "skills/missing/SKILL.md",
        "retained-skill-missing",
    );
    assert_finding(
        &envelope,
        "skills/old-name/SKILL.md",
        "alias-target-invalid",
    );
    let serialized = serde_json::to_string(&envelope).expect("serialize envelope");
    assert!(!serialized.contains("ignored/copy"));
}

#[test]
fn duplicate_names_are_reported_only_against_sanitized_paths() {
    let fixture = Fixture::new();
    let sensitive_name = format!("ghp_{}", "A".repeat(40));
    for folder in ["duplicate-safe-one", "duplicate-safe-two"] {
        fixture.write_and_track(
            &format!("skills/{folder}/SKILL.md"),
            format!(
                "---\nname: {sensitive_name}\ndescription: Use when validating duplicate-name output sanitization without exposing metadata.\n---\n# Duplicate\n"
            ),
        );
    }
    fixture.write_and_track(
        "skills/registry.toml",
        registry(&[
            retained("skills/duplicate-safe-one/SKILL.md", &sensitive_name, false),
            retained("skills/duplicate-safe-two/SKILL.md", &sensitive_name, false),
        ]),
    );

    let envelope = doctor_skill_contract(fixture.path());

    assert_finding(
        &envelope,
        "skills/duplicate-safe-one/SKILL.md",
        "skill-name-duplicate",
    );
    assert_finding(
        &envelope,
        "skills/duplicate-safe-two/SKILL.md",
        "skill-name-duplicate",
    );
    assert_finding(&envelope, "skills/registry.toml", "registry-name-duplicate");
    let serialized = serde_json::to_string(&envelope).expect("serialize envelope");
    assert!(!serialized.contains(&sensitive_name));
}

#[test]
fn registry_rejects_unknown_policy_and_alias_fields() {
    for (label, entry) in [
        (
            "policy-typo",
            "[[skills]]\npath = \"skills/safe/SKILL.md\"\nname = \"safe\"\nstatus = \"retain\"\nrequires_explicit_invocaton = true\n",
        ),
        (
            "alias-typo",
            "[[skills]]\npath = \"skills/old/SKILL.md\"\nname = \"old\"\nstatus = \"alias\"\nalias_taret = \"safe\"\n",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.write_and_track(
            "skills/registry.toml",
            format!("schema_version = 1\ncredential_allowlist = []\n\n{entry}"),
        );

        let envelope = doctor_skill_contract(fixture.path());

        assert_finding(&envelope, "skills/registry.toml", "registry-invalid");
        assert_eq!(
            envelope.entities.len(),
            1,
            "{label} should stop at the strict registry parse boundary"
        );
    }
}

#[test]
fn links_resources_and_symlinks_must_resolve_to_tracked_repository_files() {
    let fixture = Fixture::new();
    let external = tempfile::NamedTempFile::new().expect("create external target");
    let external_path = external.path().to_string_lossy();
    let body = format!(
        "# Links\n\n[Tracked fragment](references/guide.md#section)\n[Local anchor](#section)\n[Ignored file](references/ignored.md)\n[External file]({external_path})\n\nRun `scripts/ignored.py`.\n\n## Section"
    );
    let links_ui = ui("links");
    fixture.add_skill(
        "skills/links/SKILL.md",
        "links",
        "Use when validating that skill links resolve inside the tracked repository boundary.",
        &body,
        Some(&links_ui),
    );
    fixture.write_and_track(
        "skills/links/references/guide.md",
        "# Guide\n\n## Section\n",
    );
    fixture.write_and_track(
        ".gitignore",
        "skills/links/references/ignored.md\nskills/links/scripts/ignored.py\n",
    );
    fixture.write("skills/links/references/ignored.md", "# ignored\n");
    fixture.write("skills/links/scripts/ignored.py", "# ignored\n");
    #[cfg(unix)]
    fixture.symlink_and_track(
        "skills/links/references/external.md",
        external.path().to_str().expect("UTF-8 external path"),
    );
    fixture.write_and_track(
        "skills/registry.toml",
        registry(&[retained("skills/links/SKILL.md", "links", false)]),
    );

    let envelope = doctor_skill_contract(fixture.path());

    assert_no_finding(&envelope, "skills/links/SKILL.md", "relative-link-missing");
    assert_finding(
        &envelope,
        "skills/links/SKILL.md",
        "relative-link-untracked",
    );
    assert_finding(
        &envelope,
        "skills/links/SKILL.md",
        "relative-link-outside-repository",
    );
    assert_finding(
        &envelope,
        "skills/links/SKILL.md",
        "referenced-resource-untracked",
    );
    #[cfg(unix)]
    assert_finding(
        &envelope,
        "skills/links/references/external.md",
        "symlink-target-untracked-or-outside",
    );
}

#[test]
fn scalar_hex_scan_ignores_yaml_comments_and_keys_and_deduplicates_prefix_overlap() {
    let fixture = Fixture::new();
    let skill_path = "skills/scalars/SKILL.md";
    let scalars_ui = ui("scalars");
    fixture.add_skill(
        skill_path,
        "scalars",
        "Use when validating structured scalar credential detection and overlap handling.",
        "# Scalars",
        Some(&scalars_ui),
    );
    fixture.write_and_track(
        "skills/registry.toml",
        registry(&[retained(skill_path, "scalars", false)]),
    );
    let ignored_hex = "abcdef0123456789".repeat(4);
    fixture.write_and_track(
        "skills/scalars/references/ignored.yaml",
        format!("# {ignored_hex}\n\"{ignored_hex}\": \"ordinary value\"\n"),
    );
    let prefixed = format!("ghp_{}", "A".repeat(40));
    fixture.write_and_track(
        "skills/scalars/references/overlap.yaml",
        format!("token: \"{prefixed}\"\n"),
    );
    let scalar_hex = "0123456789abcdef".repeat(4);
    fixture.write_and_track(
        "skills/scalars/references/scalar.yaml",
        format!("token: \"{scalar_hex}\"\n"),
    );

    let envelope = doctor_skill_contract(fixture.path());

    assert_no_finding(
        &envelope,
        "skills/scalars/references/ignored.yaml",
        "credential-like-content",
    );
    assert_eq!(
        finding_count(
            &envelope,
            "skills/scalars/references/overlap.yaml",
            "credential-like-content",
        ),
        Some(1)
    );
    assert_eq!(
        finding_count(
            &envelope,
            "skills/scalars/references/scalar.yaml",
            "credential-like-content",
        ),
        Some(1)
    );
    let serialized = serde_json::to_string(&envelope).expect("serialize envelope");
    assert!(!serialized.contains(&ignored_hex));
    assert!(!serialized.contains(&prefixed));
    assert!(!serialized.contains(&scalar_hex));
}

#[cfg(unix)]
#[test]
fn symlink_chains_stay_tracked_and_use_the_resolved_target_format_for_scanning() {
    let fixture = Fixture::new();
    let external = tempfile::NamedTempFile::new().expect("create external target");
    let scalar_hex = "fedcba9876543210".repeat(4);
    let symlink_ui = ui("symlink-chain");
    fixture.add_skill(
        "skills/symlink-chain/SKILL.md",
        "symlink-chain",
        "Use when validating tracked multi-hop symlinks and resolved resource formats.",
        "# Symlinks\n\n[Tracked config](references/config.txt)\n[External alias](../../shared/external-alias.md)",
        Some(&symlink_ui),
    );
    fixture.write_and_track("shared/secret.yaml", format!("token: \"{scalar_hex}\"\n"));
    fixture.symlink_and_track("shared/inner-alias", "secret.yaml");
    fixture.symlink_and_track(
        "skills/symlink-chain/references/config.txt",
        "../../../shared/inner-alias",
    );
    fixture.symlink_and_track(
        "shared/external-alias.md",
        external.path().to_str().expect("UTF-8 external path"),
    );
    fixture.write_and_track(
        "skills/registry.toml",
        registry(&[retained(
            "skills/symlink-chain/SKILL.md",
            "symlink-chain",
            false,
        )]),
    );

    let envelope = doctor_skill_contract(fixture.path());

    assert_eq!(
        finding_count(
            &envelope,
            "skills/symlink-chain/SKILL.md",
            "relative-link-outside-repository",
        ),
        Some(1),
        "only the external final target should fail link classification"
    );
    assert_no_finding(
        &envelope,
        "skills/symlink-chain/SKILL.md",
        "relative-link-missing",
    );
    assert_no_finding(
        &envelope,
        "skills/symlink-chain/references/config.txt",
        "symlink-target-untracked-or-outside",
    );
    assert_eq!(
        finding_count(
            &envelope,
            "skills/symlink-chain/references/config.txt",
            "credential-like-content",
        ),
        Some(1),
        "the .txt alias must scan the final tracked YAML scalar"
    );
    let serialized = serde_json::to_string(&envelope).expect("serialize envelope");
    assert!(!serialized.contains(&scalar_hex));
}

#[test]
fn prompt_identity_uses_exact_tokens_and_quoted_fields_are_direct_interface_scalars() {
    let fixture = Fixture::new();
    fixture.add_skill(
        "skills/foo/SKILL.md",
        "foo",
        "Use when validating exact prompt invocation tokens and quoted UI scalar fields.",
        "# Foo",
        Some(
            "interface:\n  display_name: Plain Foo\n  short_description: \"Quoted description\" # inline comments are not canonical\n  default_prompt: \"Use $foo-extra to validate the prefix collision.\"\n  nested:\n    display_name: \"Nested text must not satisfy the direct field.\"\n",
        ),
    );
    fixture.write_and_track(
        "skills/registry.toml",
        registry(&[retained("skills/foo/SKILL.md", "foo", false)]),
    );

    let envelope = doctor_skill_contract(fixture.path());

    assert_eq!(
        finding_count(
            &envelope,
            "skills/foo/agents/openai.yaml",
            "ui-default-prompt-skill",
        ),
        Some(1)
    );
    assert_eq!(
        finding_count(
            &envelope,
            "skills/foo/agents/openai.yaml",
            "ui-interface-unquoted",
        ),
        Some(2),
        "direct unquoted and inline-commented values must fail; nested quoted duplicates must not mask them"
    );

    let clean = Fixture::new();
    let exact_ui = ui("foo");
    clean.add_skill(
        "skills/foo/SKILL.md",
        "foo",
        "Use when validating that the exact dollar-prefixed skill invocation is accepted.",
        "# Foo",
        Some(&exact_ui),
    );
    clean.write_and_track(
        "skills/registry.toml",
        registry(&[retained("skills/foo/SKILL.md", "foo", false)]),
    );
    let clean_envelope = doctor_skill_contract(clean.path());
    assert_no_finding(
        &clean_envelope,
        "skills/foo/agents/openai.yaml",
        "ui-default-prompt-skill",
    );
    assert_no_finding(
        &clean_envelope,
        "skills/foo/agents/openai.yaml",
        "ui-interface-unquoted",
    );
}

#[test]
fn reports_broken_resources_forbidden_artifacts_and_sanitized_credentials() {
    let fixture = Fixture::new();
    let skill_path = "skills/resources/SKILL.md";
    let resources_ui = ui("resources");
    fixture.add_skill(
        skill_path,
        "resources",
        "Use when validating tracked links, resources, artifacts, and credential hygiene.",
        "# Resources\n\nRead [the absent guide](references/absent.md).\n\nRun `scripts/missing.py`.",
        Some(&resources_ui),
    );
    fixture.write_and_track(
        "skills/registry.toml",
        registry(&[retained(skill_path, "resources", false)]),
    );

    for extension in ["pyc", "zip", "deprecated", "legacy"] {
        fixture.write_and_track(
            &format!("skills/resources/references/generated.{extension}"),
            b"generated fixture",
        );
    }

    #[cfg(unix)]
    fixture.symlink_and_track(
        "skills/resources/references/dangling.md",
        "missing-target.md",
    );

    let raw_hex = "0123456789abcdef".repeat(4);
    fixture.write_and_track(
        "skills/resources/references/credential.yaml",
        format!("token: \"{raw_hex}\"\n"),
    );
    let raw_prefixed = format!("ghp_{}", "A".repeat(40));
    fixture.write_and_track(
        "skills/resources/references/prefix.txt",
        format!("token={raw_prefixed}\n"),
    );
    fixture.write_and_track(
        "skills/resources/references/private-key.txt",
        "-----BEGIN PRIVATE KEY-----\nfixture only\n-----END PRIVATE KEY-----\n",
    );

    let envelope = doctor_skill_contract(fixture.path());

    assert_finding(&envelope, skill_path, "relative-link-missing");
    assert_finding(&envelope, skill_path, "referenced-resource-missing");
    #[cfg(unix)]
    assert_finding(
        &envelope,
        "skills/resources/references/dangling.md",
        "dangling-symlink",
    );
    for extension in ["pyc", "zip", "deprecated", "legacy"] {
        assert_finding(
            &envelope,
            &format!("skills/resources/references/generated.{extension}"),
            "tracked-resource-extension",
        );
    }
    assert_finding(
        &envelope,
        "skills/resources/references/credential.yaml",
        "credential-like-content",
    );
    assert_finding(
        &envelope,
        "skills/resources/references/prefix.txt",
        "credential-like-content",
    );
    assert_finding(
        &envelope,
        "skills/resources/references/private-key.txt",
        "credential-like-content",
    );

    let serialized = serde_json::to_string(&envelope).expect("serialize envelope");
    assert!(!serialized.contains(&raw_hex));
    assert!(!serialized.contains(&raw_prefixed));
}
