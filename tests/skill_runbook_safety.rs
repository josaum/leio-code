//! Static privileged-runbook refusal contracts (Task 4).
//!
//! Asserts that deploy, ship, sigint-pentest, and platform-operator require
//! exact target, authorization, preview, and fresh confirmation before any
//! mutation, and that unsafe secret-printing / unconditional destructive
//! guidance is absent.

use std::fs;
use std::path::{Path, PathBuf};

use serde_yaml::Value;

/// The multi-repo workspace root (this repo's parent) when `required` exists
/// inside it, else `None`.
///
/// These contracts assert against runbooks that live in *other* repos of the
/// workspace (`.claude/skills/`, `example-gateway/skills/`). This repo also
/// ships standalone, where those runbooks are absent and there is nothing to
/// assert — the contract then skips instead of failing, so a standalone
/// checkout can still reach a green run.
fn workspace_root_with(required: &str) -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .ok()?;
    root.join(required).exists().then_some(root)
}

struct PrivilegedSkill {
    name: &'static str,
    skill_md: &'static str,
    description: &'static str,
    display_name: &'static str,
    short_description: &'static str,
    default_prompt: &'static str,
    extra_required: &'static [&'static str],
    forbidden: &'static [&'static str],
}

const SKILLS: &[PrivilegedSkill] = &[
    PrivilegedSkill {
        name: "deploy",
        skill_md: ".claude/skills/deploy/SKILL.md",
        description: "Use when a user explicitly requests a deployment or deployment preview for a named Example target and service.",
        display_name: "Deploy",
        short_description: "Authorized Example deployment workflow",
        default_prompt: "Use $deploy to prepare a deployment preview for the explicitly named target and service, stopping before mutation until separately confirmed.",
        extra_required: &[
            "exact target",
            "authorization",
            "preview",
            "fresh confirmation",
            "remote Docker",
            "inspect",
            "diagnose",
        ],
        forbidden: &[
            "env | grep",
            "docker system prune",
            "docker compose up -d --build",
            "docker compose build --no-cache",
            "without asking",
            "Make your best judgment",
        ],
    },
    PrivilegedSkill {
        name: "ship",
        skill_md: ".claude/skills/ship/SKILL.md",
        description: "Use when a user explicitly requests publication of a named branch through verification, pull request creation, and merge.",
        display_name: "Ship",
        short_description: "Authorized branch publication workflow",
        default_prompt: "Use $ship to verify the named branch and preview its publication steps, stopping before each external mutation until separately confirmed.",
        extra_required: &[
            "exact target",
            "authorization",
            "preview",
            "fresh confirmation",
            "inspect",
            "diagnose",
        ],
        forbidden: &[
            "gh pr merge",
            "git push -u origin",
            "--delete-branch",
            "without asking",
            "Make your best judgment",
            "high-velocity \"go / merge\"",
        ],
    },
    PrivilegedSkill {
        name: "sigint-pentest",
        skill_md: ".claude/skills/sigint-pentest/SKILL.md",
        description: "Use when a user requests a scoped defensive SIGINT or penetration-test exercise and provides current written rules of engagement for the exact target and date.",
        display_name: "SIGINT Pentest",
        short_description: "Authorized defensive SIGINT exercises",
        default_prompt: "Use $sigint-pentest to validate current rules of engagement and prepare a non-mutating exercise preview for the exact target.",
        extra_required: &[
            "exact target",
            "authorization",
            "preview",
            "fresh confirmation",
            "rules of engagement",
            "roe-requirements.md",
            "inspect",
            "diagnose",
        ],
        forbidden: &[
            "camp-2026-sigint-brother-gpu",
            "brother-gpu-workspace-0",
            "10.0.0.206",
            "tail21cbc4.ts.net",
            "2026-07-07",
            "be creative and use all the tools",
            "active SIGINT campaign",
            "references/campaign.md",
            "airmon-ng check kill",
        ],
    },
    PrivilegedSkill {
        name: "platform-operator",
        skill_md: "example-gateway/skills/platform-operator/SKILL.md",
        description: "Use when diagnosing Example platform runtime health, services, Redis, databases, routing, Vault, or recovery operations where target-specific authorization and dry-run gates are required.",
        display_name: "Example Platform Operator",
        short_description: "Diagnose guarded platform operations",
        default_prompt: "Use $platform-operator to diagnose the named platform target and prepare a non-mutating recovery preview.",
        extra_required: &[
            "exact target",
            "authorization",
            "preview",
            "fresh confirmation",
            "inspect",
            "diagnose",
            "dry-run",
        ],
        forbidden: &[
            "pkill -9",
            "kill -9",
            "echo $VAULT_MASTER_KEY",
            "cp data/backups/gateway-latest.duckdb data/gateway.duckdb",
            "curl -X POST http://127.0.0.1:9382/api/tenants",
            "curl -X PUT http://127.0.0.1:9382/api/agents",
            "without asking",
        ],
    },
];

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn split_frontmatter(content: &str) -> (Value, String) {
    let trimmed = content.trim_start_matches('\u{feff}');
    assert!(
        trimmed.starts_with("---\n") || trimmed.starts_with("---\r\n"),
        "skill must start with YAML frontmatter"
    );
    let after = trimmed
        .trim_start_matches("---\r\n")
        .trim_start_matches("---\n");
    let end = after
        .find("\n---\n")
        .or_else(|| after.find("\n---\r\n"))
        .expect("closing frontmatter delimiter");
    let yaml = &after[..end];
    let body = after[end..]
        .trim_start_matches("\n---\r\n")
        .trim_start_matches("\n---\n")
        .to_string();
    let value: Value = serde_yaml::from_str(yaml).expect("parse frontmatter YAML");
    (value, body)
}

fn assert_two_key_frontmatter(value: &Value, expected_name: &str, expected_description: &str) {
    let map = value.as_mapping().expect("frontmatter must be a mapping");
    assert_eq!(
        map.len(),
        2,
        "frontmatter must contain exactly name and description"
    );
    let name = map
        .get(Value::String("name".into()))
        .and_then(Value::as_str)
        .expect("name");
    let description = map
        .get(Value::String("description".into()))
        .and_then(Value::as_str)
        .expect("description");
    assert_eq!(name, expected_name);
    assert_eq!(description, expected_description);
    assert!(
        description.len() >= 40 && description.len() <= 1024,
        "description length out of range: {}",
        description.len()
    );
}

fn assert_restricted_ui(
    ui_path: &Path,
    display_name: &str,
    short_description: &str,
    default_prompt: &str,
) {
    assert!(
        ui_path.is_file(),
        "missing UI metadata {}",
        ui_path.display()
    );
    let raw = read_text(ui_path);
    let value: Value = serde_yaml::from_str(&raw).expect("parse agents/openai.yaml");
    let interface = value
        .get("interface")
        .and_then(Value::as_mapping)
        .expect("interface mapping");
    assert_eq!(
        interface
            .get(Value::String("display_name".into()))
            .and_then(Value::as_str),
        Some(display_name)
    );
    assert_eq!(
        interface
            .get(Value::String("short_description".into()))
            .and_then(Value::as_str),
        Some(short_description)
    );
    assert_eq!(
        interface
            .get(Value::String("default_prompt".into()))
            .and_then(Value::as_str),
        Some(default_prompt)
    );
    let allow = value
        .get("policy")
        .and_then(|p| p.get("allow_implicit_invocation"))
        .and_then(Value::as_bool);
    assert_eq!(
        allow,
        Some(false),
        "{} must set policy.allow_implicit_invocation: false",
        ui_path.display()
    );
}

fn assert_contains_all(haystack: &str, needles: &[&str], label: &str) {
    let lower = haystack.to_ascii_lowercase();
    for needle in needles {
        assert!(
            lower.contains(&needle.to_ascii_lowercase()),
            "{label} must contain required language: {needle}"
        );
    }
}

fn assert_contains_none(haystack: &str, needles: &[&str], label: &str) {
    let lower = haystack.to_ascii_lowercase();
    for needle in needles {
        assert!(
            !lower.contains(&needle.to_ascii_lowercase()),
            "{label} must not contain forbidden guidance: {needle}"
        );
    }
}

#[test]
fn privileged_runbooks_enforce_exact_gates() {
    let Some(root) = workspace_root_with(".claude/skills") else {
        eprintln!("skipped: no workspace .claude/skills beside this repo");
        return;
    };
    for skill in SKILLS {
        let skill_path = root.join(skill.skill_md);
        let content = read_text(&skill_path);
        let (frontmatter, body) = split_frontmatter(&content);
        assert_two_key_frontmatter(&frontmatter, skill.name, skill.description);

        let skill_dir = skill_path.parent().expect("skill directory");
        let ui_path = skill_dir.join("agents/openai.yaml");
        assert_restricted_ui(
            &ui_path,
            skill.display_name,
            skill.short_description,
            skill.default_prompt,
        );

        let combined = format!("{content}\n{}", read_text(&ui_path));
        assert_contains_all(&combined, skill.extra_required, skill.name);
        assert_contains_none(&combined, skill.forbidden, skill.name);
        assert_contains_all(
            &body,
            &[
                "exact target",
                "authorization",
                "preview",
                "fresh confirmation",
            ],
            &format!("{} body", skill.name),
        );
        assert!(
            content.lines().count() < 500,
            "{} exceeds 500 lines",
            skill.name
        );
    }
}

#[test]
fn sigint_references_drop_expired_campaign_and_keep_roe_contract() {
    let Some(root) = workspace_root_with(".claude/skills/sigint-pentest") else {
        eprintln!("skipped: no workspace sigint-pentest skill beside this repo");
        return;
    };
    let refs = root.join(".claude/skills/sigint-pentest/references");
    assert!(
        !refs.join("campaign.md").exists(),
        "expired campaign.md must be removed"
    );
    let roe = refs.join("roe-requirements.md");
    assert!(roe.is_file(), "roe-requirements.md must exist");
    let roe_text = read_text(&roe);
    assert_contains_all(
        &roe_text,
        &[
            "exact target",
            "valid_until",
            "written rules of engagement",
            "expired",
        ],
        "roe-requirements.md",
    );
    assert_contains_none(
        &roe_text,
        &[
            "camp-2026-sigint-brother-gpu",
            "brother-gpu-workspace-0",
            "10.0.0.206",
            "2026-07-07",
        ],
        "roe-requirements.md",
    );

    let evidence = read_text(&refs.join("evidence-schema.md"));
    assert_contains_none(
        &evidence,
        &["camp-2026-sigint-brother-gpu"],
        "evidence-schema.md",
    );
    assert_contains_all(
        &evidence,
        &["campaign_id", "phase", "dry-run", "mock"],
        "evidence-schema.md",
    );

    let sfr = read_text(&refs.join("sfr-mapping.md"));
    assert_contains_none(&sfr, &["brother-gpu", "cloudflared"], "sfr-mapping.md");
}

#[test]
fn platform_operator_removes_zip_and_unsafe_troubleshooting() {
    let Some(root) = workspace_root_with("example-gateway/skills/platform-operator") else {
        eprintln!("skipped: no workspace platform-operator skill beside this repo");
        return;
    };
    assert!(
        !root
            .join("example-gateway/skills/platform-operator.zip")
            .exists(),
        "platform-operator.zip must be deleted"
    );
    let troubleshooting = read_text(
        &root.join("example-gateway/skills/platform-operator/references/troubleshooting.md"),
    );
    assert_contains_none(
        &troubleshooting,
        &[
            "pkill -9",
            "kill -9",
            "echo $VAULT_MASTER_KEY",
            "cp data/backups/gateway-latest.duckdb data/gateway.duckdb",
        ],
        "troubleshooting.md",
    );
    assert_contains_all(
        &troubleshooting,
        &[
            "exact target",
            "authorization",
            "preview",
            "fresh confirmation",
            "dry-run",
        ],
        "troubleshooting.md",
    );
}

#[test]
fn sigint_installer_documents_plan_only_default_and_confirm_digest() {
    let Some(root) = workspace_root_with(".claude/skills/sigint-pentest") else {
        eprintln!("skipped: no workspace sigint-pentest skill beside this repo");
        return;
    };
    let script =
        read_text(&root.join(".claude/skills/sigint-pentest/scripts/sigint-linux-install.sh"));
    assert_contains_all(
        &script,
        &[
            "--roe",
            "--target",
            "--confirm-digest",
            "plan only",
            "preview",
        ],
        "sigint-linux-install.sh",
    );
    assert_contains_none(&script, &["|| true", "|| echo"], "sigint-linux-install.sh");
    assert!(
        script.contains("set -euo pipefail") || script.contains("set -eu"),
        "installer must fail closed on errors"
    );
}
