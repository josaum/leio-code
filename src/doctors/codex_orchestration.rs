//! Repository-local Codex orchestration contract.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde_json::{Value as JsonValue, json};

use super::Doctor;
use super::utils::{find_line, git_tracked_files, query_id};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

const HOOK_SCRIPT: &str = ".codex/hooks/leio_codex_hook.py";
const ROLE_SPECS: &[(&str, &str, &str, &str)] = &[
    ("example_explorer", "gpt-5.6-terra", "low", "read-only"),
    (
        "example_worker",
        "gpt-5.6-terra",
        "medium",
        "workspace-write",
    ),
    (
        "example_verifier",
        "gpt-5.6-luna",
        "medium",
        "workspace-write",
    ),
    ("example_reviewer", "gpt-5.6-sol", "high", "read-only"),
    ("example_security", "gpt-5.6-sol", "xhigh", "read-only"),
];

const HOOK_MATCHERS: &[(&str, &str)] = &[
    ("SessionStart", "startup|resume|clear|compact"),
    (
        "SubagentStart",
        "example_explorer|example_worker|example_verifier|example_reviewer|example_security",
    ),
    ("PreToolUse", "apply_patch|Edit|Write"),
    ("PostToolUse", "Bash"),
    ("SubagentStop", ""),
    ("Stop", ""),
];

pub struct CodexOrchestrationDoctor;

impl Doctor for CodexOrchestrationDoctor {
    fn name(&self) -> &'static str {
        "codex-orchestration"
    }

    fn description(&self) -> &'static str {
        "Validates tracked, portable repository-local Codex configuration, routed agent roles, hooks, and multi-agent ownership boundaries."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_codex_orchestration(root)
    }
}

pub fn doctor_codex_orchestration(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    if !opts_into_orchestration(root) {
        return inactive_envelope(started);
    }

    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let tracked = match git_tracked_files(root) {
        Some(paths) => paths.into_iter().collect::<HashSet<_>>(),
        None => {
            warn(
                &mut warnings,
                "git ls-files failed; tracked Codex surfaces cannot be verified",
            );
            HashSet::new()
        }
    };

    validate_required_paths(root, &tracked, &mut warnings, &mut evidence);
    validate_ignore_rules(root, &mut warnings, &mut evidence);

    let config = parse_project_config(root, &mut warnings, &mut evidence);
    if let Some(config) = config.as_ref() {
        validate_project_config(config, &mut warnings);
        validate_leio_mcp_identity(config, &mut warnings);
    }

    validate_agents(root, &tracked, &mut warnings, &mut evidence, &mut entities);
    validate_hooks(root, &tracked, &mut warnings, &mut evidence);
    validate_agents_md(root, &mut warnings, &mut evidence);

    let warning_count = warnings.len();
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_codex_orchestration"),
        kind: "doctor".to_string(),
        summary: if warning_count == 0 {
            "codex-orchestration: tracked project config, five routed roles, hooks, and ownership boundaries are aligned".to_string()
        } else {
            format!("codex-orchestration: found {warning_count} contract warning(s)")
        },
        confidence: if warning_count == 0 { 0.98 } else { 0.72 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "activated": true,
            "roles": ROLE_SPECS.len(),
            "hook_events": HOOK_MATCHERS.len(),
            "required_paths": required_paths().len(),
            "warning_count": warning_count,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// True when the repository opts into Codex *multi-agent orchestration*, which
/// is what this contract governs.
///
/// A bare `.codex/` directory is not that opt-in. Registering an MCP server is
/// reason enough to create one, and demanding the full five-role fleet
/// (`example_*` agents, hooks, `AGENTS.md`, pinned models) from every repo that
/// does so buries the reader in warnings about infrastructure they never asked
/// for. The opt-in signal is an `.codex/agents/` directory or a config that
/// turns the multi-agent features on.
fn opts_into_orchestration(root: &Path) -> bool {
    let codex = root.join(".codex");
    if !codex.is_dir() {
        return false;
    }
    if codex.join("agents").is_dir() || codex.join("hooks").is_dir() {
        return true;
    }
    let Ok(raw) = std::fs::read_to_string(codex.join("config.toml")) else {
        return false;
    };
    raw.contains("multi_agent") || raw.contains("[agents]") || raw.contains("[features]")
}

fn inactive_envelope(started: Instant) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_codex_orchestration"),
        kind: "doctor".to_string(),
        summary: "codex-orchestration: no .codex directory; inactive".to_string(),
        confidence: 0.98,
        entities: Vec::new(),
        evidence: Vec::new(),
        warnings: Vec::new(),
        meta: Some(json!({ "activated": false })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn warn(warnings: &mut Vec<String>, message: impl AsRef<str>) {
    warnings.push(format!("[codex-orchestration] {}", message.as_ref()));
}

fn required_paths() -> Vec<String> {
    let mut paths = vec![
        ".codex/config.toml".to_string(),
        ".codex/hooks.json".to_string(),
        HOOK_SCRIPT.to_string(),
    ];
    paths.extend(
        ROLE_SPECS
            .iter()
            .map(|(name, _, _, _)| format!(".codex/agents/{name}.toml")),
    );
    paths
}

fn validate_required_paths(
    root: &Path,
    tracked: &HashSet<String>,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    for rel in required_paths() {
        if !root.join(&rel).is_file() {
            warn(warnings, format!("required file `{rel}` is missing"));
            continue;
        }
        if !tracked.contains(&rel) {
            warn(
                warnings,
                format!("required file `{rel}` is not git-tracked"),
            );
            continue;
        }
        evidence.push(EvidenceItem {
            kind: "codex_tracked_surface".to_string(),
            path: rel,
            line: Some(1),
            detail: "required Codex orchestration surface is git-tracked".to_string(),
        });
    }
}

fn validate_ignore_rules(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let path = root.join(".gitignore");
    let Ok(source) = std::fs::read_to_string(&path) else {
        warn(warnings, "root .gitignore is missing or unreadable");
        return;
    };
    let ignored = source.lines().any(|line| {
        let pattern = line.split('#').next().unwrap_or("").trim();
        !pattern.starts_with('!')
            && matches!(
                pattern,
                ".codex" | ".codex/" | "/.codex" | "/.codex/" | "**/.codex/"
            )
    });
    if ignored {
        warn(warnings, ".codex is ignored by the root .gitignore");
    } else {
        evidence.push(EvidenceItem {
            kind: "codex_ignore_contract".to_string(),
            path: ".gitignore".to_string(),
            line: None,
            detail: ".codex is not excluded by a root ignore rule".to_string(),
        });
    }
}

fn parse_project_config(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) -> Option<toml::Value> {
    let rel = ".codex/config.toml";
    let source = std::fs::read_to_string(root.join(rel)).ok()?;
    match toml::from_str::<toml::Value>(&source) {
        Ok(config) => {
            evidence.push(EvidenceItem {
                kind: "codex_config".to_string(),
                path: rel.to_string(),
                line: Some(1),
                detail: "project Codex config parses as TOML".to_string(),
            });
            Some(config)
        }
        Err(error) => {
            warn(warnings, format!("{rel} is invalid TOML: {error}"));
            None
        }
    }
}

fn toml_at<'a>(value: &'a toml::Value, path: &[&str]) -> Option<&'a toml::Value> {
    path.iter()
        .try_fold(value, |current, segment| current.as_table()?.get(*segment))
}

fn require_string(config: &toml::Value, path: &[&str], expected: &str, warnings: &mut Vec<String>) {
    let actual = toml_at(config, path).and_then(toml::Value::as_str);
    if actual != Some(expected) {
        warn(
            warnings,
            format!("{} must be `{expected}`, got {actual:?}", path.join(".")),
        );
    }
}

fn require_bool(config: &toml::Value, path: &[&str], expected: bool, warnings: &mut Vec<String>) {
    let actual = toml_at(config, path).and_then(toml::Value::as_bool);
    if actual != Some(expected) {
        warn(
            warnings,
            format!("{} must be `{expected}`, got {actual:?}", path.join(".")),
        );
    }
}

fn validate_project_config(config: &toml::Value, warnings: &mut Vec<String>) {
    require_string(config, &["model"], "gpt-5.6-sol", warnings);
    require_string(config, &["model_reasoning_effort"], "medium", warnings);
    require_string(config, &["plan_mode_reasoning_effort"], "high", warnings);
    require_string(config, &["sandbox_mode"], "workspace-write", warnings);
    require_bool(config, &["features", "multi_agent"], true, warnings);
    require_bool(config, &["features", "hooks"], true, warnings);
    require_bool(
        config,
        &["features", "multi_agent_v2", "enabled"],
        true,
        warnings,
    );

    let concurrency = toml_at(
        config,
        &[
            "features",
            "multi_agent_v2",
            "max_concurrent_threads_per_session",
        ],
    )
    .and_then(toml::Value::as_integer);
    if concurrency != Some(4) {
        warn(
            warnings,
            format!(
                "features.multi_agent_v2.max_concurrent_threads_per_session must be `4`, got {concurrency:?}"
            ),
        );
    }
    if toml_at(config, &["agents", "max_threads"]).is_some() {
        warn(
            warnings,
            "agents.max_threads is a legacy v1 bound and must be absent when multi_agent_v2 is enabled",
        );
    }
    let depth = toml_at(config, &["agents", "max_depth"]).and_then(toml::Value::as_integer);
    if depth != Some(1) {
        warn(
            warnings,
            format!("agents.max_depth must be `1`, got {depth:?}"),
        );
    }
    let runtime =
        toml_at(config, &["agents", "job_max_runtime_seconds"]).and_then(toml::Value::as_integer);
    if !matches!(runtime, Some(1..=3600)) {
        warn(
            warnings,
            format!(
                "agents.job_max_runtime_seconds must be bounded between 1 and 3600, got {runtime:?}"
            ),
        );
    }
    require_bool(config, &["agents", "interrupt_message"], true, warnings);
}

fn validate_leio_mcp_identity(config: &toml::Value, warnings: &mut Vec<String>) {
    let Some(servers) = toml_at(config, &["mcp_servers"]).and_then(toml::Value::as_table) else {
        return;
    };
    let aliases = servers
        .keys()
        .filter(|name| name.replace('_', "-").eq_ignore_ascii_case("leio-code"))
        .collect::<Vec<_>>();
    if aliases.len() > 1 {
        warn(
            warnings,
            format!("duplicate LEIO MCP identities in project config: {aliases:?}"),
        );
    }
}

fn nonempty_string<'a>(table: &'a toml::value::Table, key: &str) -> Option<&'a str> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn doctor_count_regex() -> Regex {
    Regex::new(r"(?i)\b\d+\s+(?:LEIO\s+)?doctors?\b").expect("valid doctor count regex")
}

fn validate_agents(
    root: &Path,
    tracked: &HashSet<String>,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    entities: &mut Vec<JsonValue>,
) {
    let count_regex = doctor_count_regex();
    for (name, model, effort, sandbox) in ROLE_SPECS {
        let rel = format!(".codex/agents/{name}.toml");
        let Ok(source) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        if count_regex.is_match(&source) {
            warn(
                warnings,
                format!("{rel} contains a hard-coded doctor count; use capabilities instead"),
            );
        }
        let parsed = match toml::from_str::<toml::Value>(&source) {
            Ok(parsed) => parsed,
            Err(error) => {
                warn(warnings, format!("{rel} is invalid TOML: {error}"));
                continue;
            }
        };
        let Some(table) = parsed.as_table() else {
            warn(warnings, format!("{rel} must contain a TOML table"));
            continue;
        };

        for required in ["name", "description", "developer_instructions"] {
            if nonempty_string(table, required).is_none() {
                warn(
                    warnings,
                    format!("{rel} must define non-empty `{required}`"),
                );
            }
        }
        for (field, expected) in [
            ("name", *name),
            ("model", *model),
            ("model_reasoning_effort", *effort),
            ("sandbox_mode", *sandbox),
        ] {
            let actual = nonempty_string(table, field);
            if actual != Some(expected) {
                warn(
                    warnings,
                    format!(
                        "role `{name}` field `{field}` must be `{expected}`, got {actual:?}; expected {sandbox} boundary"
                    ),
                );
            }
        }
        if tracked.contains(&rel) {
            evidence.push(EvidenceItem {
                kind: "codex_agent_role".to_string(),
                path: rel.clone(),
                line: find_line(&source, "name"),
                detail: format!("role {name} pins {model}/{effort} with {sandbox}"),
            });
        }
        entities.push(json!({
            "role": name,
            "model": model,
            "effort": effort,
            "sandbox": sandbox,
        }));
    }
}

fn validate_hooks(
    root: &Path,
    tracked: &HashSet<String>,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let rel = ".codex/hooks.json";
    let Ok(source) = std::fs::read_to_string(root.join(rel)) else {
        return;
    };
    if doctor_count_regex().is_match(&source) {
        warn(
            warnings,
            format!("{rel} contains a hard-coded doctor count; use capabilities instead"),
        );
    }
    let parsed = match serde_json::from_str::<JsonValue>(&source) {
        Ok(parsed) => parsed,
        Err(error) => {
            warn(warnings, format!("{rel} is invalid JSON: {error}"));
            return;
        }
    };
    let Some(events) = parsed.get("hooks").and_then(JsonValue::as_object) else {
        warn(warnings, format!("{rel} must contain a `hooks` object"));
        return;
    };
    let expected = HOOK_MATCHERS.iter().copied().collect::<HashMap<_, _>>();
    for event in events.keys() {
        if !expected.contains_key(event.as_str()) {
            warn(warnings, format!("unsupported hook event `{event}`"));
        }
    }
    for (event, matcher) in HOOK_MATCHERS {
        let Some(entries) = events.get(*event).and_then(JsonValue::as_array) else {
            warn(warnings, format!("missing hook event `{event}`"));
            continue;
        };
        if entries.is_empty() {
            warn(
                warnings,
                format!("hook event `{event}` has no registrations"),
            );
            continue;
        }
        for entry in entries {
            let actual_matcher = entry.get("matcher").and_then(JsonValue::as_str);
            if actual_matcher != Some(*matcher) {
                warn(
                    warnings,
                    format!(
                        "hook event `{event}` matcher must be `{matcher}`, got {actual_matcher:?}"
                    ),
                );
            }
            let Some(actions) = entry.get("hooks").and_then(JsonValue::as_array) else {
                warn(
                    warnings,
                    format!("hook event `{event}` has no command hooks"),
                );
                continue;
            };
            for action in actions {
                if action.get("type").and_then(JsonValue::as_str) != Some("command") {
                    warn(
                        warnings,
                        format!("hook event `{event}` must use command hooks"),
                    );
                }
                let command = action
                    .get("command")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                if command.contains("/Users/") {
                    warn(
                        warnings,
                        format!("hook event `{event}` command contains an absolute user path"),
                    );
                }
                if !command.contains("git rev-parse --show-toplevel")
                    || !command.contains(HOOK_SCRIPT)
                {
                    warn(
                        warnings,
                        format!(
                            "hook event `{event}` command must resolve `{HOOK_SCRIPT}` from the git root"
                        ),
                    );
                }
                let timeout = action.get("timeout").and_then(JsonValue::as_i64);
                if !matches!(timeout, Some(1..=10)) {
                    warn(
                        warnings,
                        format!("hook event `{event}` timeout must be between 1 and 10 seconds"),
                    );
                }
            }
        }
    }

    if tracked.contains(rel) {
        evidence.push(EvidenceItem {
            kind: "codex_hooks".to_string(),
            path: rel.to_string(),
            line: Some(1),
            detail: "supported Codex hook events use portable bounded commands".to_string(),
        });
    }

    if let Ok(script) = std::fs::read_to_string(root.join(HOOK_SCRIPT))
        && doctor_count_regex().is_match(&script)
    {
        warn(
            warnings,
            format!("{HOOK_SCRIPT} contains a hard-coded doctor count; use capabilities instead"),
        );
    }
}

fn validate_agents_md(root: &Path, warnings: &mut Vec<String>, evidence: &mut Vec<EvidenceItem>) {
    let Ok(source) = std::fs::read_to_string(root.join("AGENTS.md")) else {
        warn(warnings, "AGENTS.md is missing or unreadable");
        return;
    };
    for (name, _, _, _) in ROLE_SPECS {
        if !source.contains(name) {
            warn(
                warnings,
                format!("AGENTS.md does not reference role `{name}`"),
            );
        }
    }
    let boundary = "never assign overlapping file ownership to concurrent writers";
    if !source.to_ascii_lowercase().contains(boundary) {
        warn(
            warnings,
            "AGENTS.md must forbid overlapping file ownership for concurrent writers",
        );
    } else {
        evidence.push(EvidenceItem {
            kind: "codex_ownership_boundary".to_string(),
            path: "AGENTS.md".to_string(),
            line: find_line(&source.to_ascii_lowercase(), boundary),
            detail: "root instructions forbid concurrent overlapping writers".to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    const ROLES: &[(&str, &str, &str, &str)] = &[
        ("example_explorer", "gpt-5.6-terra", "low", "read-only"),
        (
            "example_worker",
            "gpt-5.6-terra",
            "medium",
            "workspace-write",
        ),
        (
            "example_verifier",
            "gpt-5.6-luna",
            "medium",
            "workspace-write",
        ),
        ("example_reviewer", "gpt-5.6-sol", "high", "read-only"),
        ("example_security", "gpt-5.6-sol", "xhigh", "read-only"),
    ];

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "leio-codex-orchestration-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create fixture root");
        let output = Command::new("git")
            .arg("-C")
            .arg(&root)
            .arg("init")
            .output()
            .expect("git init");
        assert!(output.status.success(), "git init failed");
        root
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(path, contents).expect("write fixture");
    }

    fn write_clean_contract(root: &Path) {
        write(&root.join(".gitignore"), "target/\n.leio-code/\n");
        write(
            &root.join(".codex/config.toml"),
            r#"model = "gpt-5.6-sol"
model_reasoning_effort = "medium"
plan_mode_reasoning_effort = "high"
sandbox_mode = "workspace-write"

[features]
multi_agent = true
hooks = true

[features.multi_agent_v2]
enabled = true
max_concurrent_threads_per_session = 4

[agents]
max_depth = 1
job_max_runtime_seconds = 1200
interrupt_message = true
"#,
        );

        for (name, model, effort, sandbox) in ROLES {
            write(
                &root.join(format!(".codex/agents/{name}.toml")),
                &format!(
                    "name = \"{name}\"\ndescription = \"route {name}\"\nmodel = \"{model}\"\nmodel_reasoning_effort = \"{effort}\"\nsandbox_mode = \"{sandbox}\"\ndeveloper_instructions = \"Use LEIO first and return evidence.\"\n"
                ),
            );
        }

        write(
            &root.join(".codex/hooks/leio_codex_hook.py"),
            "#!/usr/bin/env python3\n",
        );
        write(
            &root.join(".codex/hooks.json"),
            r#"{
  "hooks": {
    "SessionStart": [{"matcher":"startup|resume|clear|compact","hooks":[{"type":"command","command":"python3 \"$(git rev-parse --show-toplevel)/.codex/hooks/leio_codex_hook.py\" --event SessionStart","timeout":5}]}],
    "SubagentStart": [{"matcher":"example_explorer|example_worker|example_verifier|example_reviewer|example_security","hooks":[{"type":"command","command":"python3 \"$(git rev-parse --show-toplevel)/.codex/hooks/leio_codex_hook.py\" --event SubagentStart","timeout":3}]}],
    "PreToolUse": [{"matcher":"apply_patch|Edit|Write","hooks":[{"type":"command","command":"python3 \"$(git rev-parse --show-toplevel)/.codex/hooks/leio_codex_hook.py\" --event PreToolUse","timeout":5}]}],
    "PostToolUse": [{"matcher":"Bash","hooks":[{"type":"command","command":"python3 \"$(git rev-parse --show-toplevel)/.codex/hooks/leio_codex_hook.py\" --event PostToolUse","timeout":8}]}],
    "SubagentStop": [{"matcher":"","hooks":[{"type":"command","command":"python3 \"$(git rev-parse --show-toplevel)/.codex/hooks/leio_codex_hook.py\" --event SubagentStop","timeout":3}]}],
    "Stop": [{"matcher":"","hooks":[{"type":"command","command":"python3 \"$(git rev-parse --show-toplevel)/.codex/hooks/leio_codex_hook.py\" --event Stop","timeout":3}]}]
  }
}"#,
        );

        let mut agents = ROLES
            .iter()
            .map(|(name, _, _, _)| format!("- `{name}`"))
            .collect::<Vec<_>>();
        agents.push("never assign overlapping file ownership to concurrent writers".to_string());
        write(&root.join("AGENTS.md"), &agents.join("\n"));
    }

    fn stage_all(root: &Path) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["add", "-A", "-f"])
            .output()
            .expect("git add");
        assert!(output.status.success(), "git add failed");
    }

    #[test]
    fn absent_codex_directory_is_an_inactive_clean_skip() {
        let root = temp_repo("absent");
        let envelope = doctor_codex_orchestration(&root);
        assert!(envelope.warnings.is_empty());
        assert!(envelope.summary.contains("inactive"));
        fs::remove_dir_all(root).ok();
    }

    /// A `.codex/` holding nothing but an MCP server registration is not an
    /// opt-in to multi-agent orchestration, and must not be judged against the
    /// five-role contract. Before this, any repo that registered an MCP server
    /// collected 20 warnings demanding a `example_*` agent fleet it never had.
    #[test]
    fn mcp_only_codex_directory_stays_inactive() {
        let root = temp_repo("mcp-only");
        fs::create_dir_all(root.join(".codex")).expect("mkdir .codex");
        fs::write(
            root.join(".codex/config.toml"),
            "[mcp_servers.leio-code]\ncommand = \"bash\"\nargs = [\"./scripts/launch-stdio-mcp.sh\"]\n",
        )
        .expect("write config");

        let envelope = doctor_codex_orchestration(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
        assert!(envelope.summary.contains("inactive"));

        // Adding an agents/ directory is the opt-in, and the contract applies.
        fs::create_dir_all(root.join(".codex/agents")).expect("mkdir agents");
        let envelope = doctor_codex_orchestration(&root);
        assert!(
            !envelope.warnings.is_empty(),
            "an agents/ directory must activate the contract"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn clean_tracked_contract_has_no_warnings() {
        let root = temp_repo("clean");
        write_clean_contract(&root);
        stage_all(&root);
        let envelope = doctor_codex_orchestration(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unsafe_or_unportable_contract_reports_each_invariant() {
        let root = temp_repo("drift");
        write_clean_contract(&root);
        write(&root.join(".gitignore"), "/.codex/\n");
        write(
            &root.join(".codex/agents/example_security.toml"),
            "name='example_security'\nmodel='gpt-5.6-sol'\nmodel_reasoning_effort='xhigh'\nsandbox_mode='danger-full-access'\ndescription='security'\ndeveloper_instructions='review'\n",
        );
        write(
            &root.join(".codex/hooks.json"),
            r#"{"hooks":{"SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"python3 /Users/example/hook.py"}]}]}}"#,
        );
        stage_all(&root);

        let warnings = doctor_codex_orchestration(&root).warnings.join("\n");
        assert!(warnings.contains("[codex-orchestration] .codex is ignored"));
        assert!(warnings.contains("example_security"));
        assert!(warnings.contains("read-only"));
        assert!(warnings.contains("absolute user path"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn invalid_v2_bounds_and_duplicate_leio_mcp_identity_warn() {
        let root = temp_repo("config-drift");
        write_clean_contract(&root);
        write(
            &root.join(".codex/config.toml"),
            r#"model = "gpt-5.6-sol"
model_reasoning_effort = "medium"
plan_mode_reasoning_effort = "high"
sandbox_mode = "workspace-write"

[features]
multi_agent = true
hooks = true

[features.multi_agent_v2]
enabled = true
max_concurrent_threads_per_session = 8

[agents]
max_threads = 4
max_depth = 2
interrupt_message = true

[mcp_servers.leio-code]
url = "http://127.0.0.1:8181/mcp"

[mcp_servers.leio_code]
url = "http://127.0.0.1:8181/mcp"
"#,
        );
        stage_all(&root);

        let warnings = doctor_codex_orchestration(&root).warnings.join("\n");
        assert!(warnings.contains("max_concurrent_threads_per_session"));
        assert!(warnings.contains("agents.max_threads"));
        assert!(warnings.contains("agents.max_depth"));
        assert!(warnings.contains("job_max_runtime_seconds"));
        assert!(warnings.contains("duplicate LEIO MCP identities"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_untracked_and_unsupported_hook_surfaces_warn() {
        let root = temp_repo("surface-drift");
        write_clean_contract(&root);
        stage_all(&root);
        let rm = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rm", "--cached", ".codex/hooks/leio_codex_hook.py"])
            .output()
            .expect("git rm cached");
        assert!(rm.status.success());
        fs::remove_file(root.join(".codex/agents/example_reviewer.toml"))
            .expect("remove reviewer fixture");
        write(
            &root.join(".codex/hooks.json"),
            r#"{"hooks":{"BeforeEverything":[{"matcher":"*","hooks":[{"type":"command","command":"python3 \"$(git rev-parse --show-toplevel)/.codex/hooks/leio_codex_hook.py\" --event BeforeEverything","timeout":30}]}]}}"#,
        );

        let warnings = doctor_codex_orchestration(&root).warnings.join("\n");
        assert!(warnings.contains("example_reviewer.toml"));
        assert!(warnings.contains("not git-tracked"));
        assert!(warnings.contains("unsupported hook event `BeforeEverything`"));
        assert!(warnings.contains("missing hook event `SessionStart`"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn hard_coded_doctor_counts_in_agent_or_hook_text_warn() {
        let root = temp_repo("hard-coded-count");
        write_clean_contract(&root);
        write(
            &root.join(".codex/agents/example_explorer.toml"),
            "name='example_explorer'\ndescription='Use all 120 doctors'\nmodel='gpt-5.6-terra'\nmodel_reasoning_effort='low'\nsandbox_mode='read-only'\ndeveloper_instructions='Use LEIO first.'\n",
        );
        stage_all(&root);

        let warnings = doctor_codex_orchestration(&root).warnings.join("\n");
        assert!(warnings.contains("hard-coded doctor count"));
        fs::remove_dir_all(root).ok();
    }
}
