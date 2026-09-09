use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use serde_yaml::{Mapping, Value};
use sha2::{Digest, Sha256};

use super::Doctor;
use super::utils::{git_tracked_files, query_id};
use crate::model::{QueryEnvelope, RepoIndex};

const REGISTRY_PATH: &str = "skills/registry.toml";
const MIN_DESCRIPTION_CHARS: usize = 40;
const MAX_DESCRIPTION_CHARS: usize = 1024;
const MAX_SKILL_LINES_EXCLUSIVE: usize = 500;
const FORBIDDEN_RESOURCE_EXTENSIONS: &[&str] = &["pyc", "zip", "deprecated", "legacy"];
const PRIVATE_KEY_MARKERS: &[&str] = &[
    "-----BEGIN PRIVATE KEY-----",
    "-----BEGIN RSA PRIVATE KEY-----",
    "-----BEGIN EC PRIVATE KEY-----",
    "-----BEGIN OPENSSH PRIVATE KEY-----",
    "-----BEGIN PGP PRIVATE KEY BLOCK-----",
];

pub struct SkillContractDoctor;

impl Doctor for SkillContractDoctor {
    fn name(&self) -> &'static str {
        "skill-contract"
    }

    fn description(&self) -> &'static str {
        "Validates the tracked repository skill registry, metadata, resources, restricted invocation policy, links, and credential hygiene."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_skill_contract(root)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillRegistry {
    schema_version: u32,
    #[serde(default)]
    credential_allowlist: Vec<CredentialAllowlistEntry>,
    #[serde(default)]
    skills: Vec<RegistrySkill>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialAllowlistEntry {
    path: String,
    fingerprint: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrySkill {
    path: String,
    name: String,
    status: String,
    #[serde(default)]
    requires_explicit_invocation: bool,
    #[serde(default, alias = "target", alias = "alias_of")]
    alias_target: Option<String>,
}

#[derive(Debug)]
struct ParsedSkill {
    name: String,
    description: String,
    keys: BTreeSet<String>,
    body: String,
}

#[derive(Default)]
struct Findings {
    counts: BTreeMap<(String, String), usize>,
}

impl Findings {
    fn add(&mut self, path: impl Into<String>, rule: impl Into<String>) {
        self.add_count(path, rule, 1);
    }

    fn add_count(&mut self, path: impl Into<String>, rule: impl Into<String>, count: usize) {
        if count == 0 {
            return;
        }
        *self.counts.entry((path.into(), rule.into())).or_default() += count;
    }

    fn total(&self) -> usize {
        self.counts.values().sum()
    }

    fn warnings(&self) -> Vec<String> {
        self.counts
            .iter()
            .map(|((path, rule), count)| format!("{path}: {rule} (count={count})"))
            .collect()
    }

    fn entities(&self) -> Vec<serde_json::Value> {
        self.counts
            .iter()
            .map(|((path, rule), count)| {
                json!({
                    "path": path,
                    "rule": rule,
                    "count": count,
                })
            })
            .collect()
    }
}

pub fn doctor_skill_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut findings = Findings::default();

    let registry_source = match fs::read_to_string(root.join(REGISTRY_PATH)) {
        Ok(source) => Some(source),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => {
            findings.add(REGISTRY_PATH, "registry-missing-or-unreadable");
            return build_envelope(started, findings, 0, 0, 0);
        }
    };
    let Some(registry_source) = registry_source else {
        // A non-skills repo has neither a registry nor tracked skill
        // manifests; the doctor self-deactivates (info, never a warning) so
        // `audit --strict` cannot fail a repo this contract does not apply
        // to. Tracked manifests without a registry remain a real finding.
        let has_skill_manifests = git_tracked_files(root)
            .map(|files| files.iter().any(|path| is_skill_manifest(path)))
            .unwrap_or(false);
        if has_skill_manifests {
            findings.add(REGISTRY_PATH, "registry-missing-or-unreadable");
        }
        return build_envelope(started, findings, 0, 0, 0);
    };
    let registry = match toml::from_str::<SkillRegistry>(&registry_source) {
        Ok(registry) => registry,
        Err(_) => {
            findings.add(REGISTRY_PATH, "registry-invalid");
            return build_envelope(started, findings, 0, 0, 0);
        }
    };
    if registry.schema_version != 1 {
        findings.add(REGISTRY_PATH, "registry-schema-version");
    }

    let tracked = match git_tracked_files(root) {
        Some(files) => files,
        None => {
            findings.add(REGISTRY_PATH, "git-tracked-files-unavailable");
            return build_envelope(started, findings, registry.skills.len(), 0, 0);
        }
    };
    let tracked_set = tracked.iter().cloned().collect::<BTreeSet<_>>();
    let tracked_skills = tracked
        .iter()
        .filter(|path| is_skill_manifest(path))
        .cloned()
        .collect::<Vec<_>>();

    validate_registry(&registry, &tracked_set, &mut findings);

    let registered_paths = registry
        .skills
        .iter()
        .map(|skill| skill.path.as_str())
        .collect::<BTreeSet<_>>();
    for path in &tracked_skills {
        if !registered_paths.contains(path.as_str()) {
            findings.add(path, "tracked-skill-unregistered");
        }
    }

    let mut parsed_by_path = BTreeMap::new();
    let mut paths_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in &tracked_skills {
        let source = match fs::read_to_string(root.join(path)) {
            Ok(source) => source,
            Err(_) => {
                findings.add(path, "skill-unreadable");
                continue;
            }
        };
        if source.lines().count() >= MAX_SKILL_LINES_EXCLUSIVE {
            findings.add(path, "skill-line-count");
        }
        let parsed = match parse_frontmatter(&source) {
            Ok(parsed) => parsed,
            Err(_) => {
                findings.add(path, "frontmatter-invalid");
                continue;
            }
        };
        let expected_keys = BTreeSet::from(["description".to_string(), "name".to_string()]);
        if parsed.keys != expected_keys {
            let count = parsed.keys.symmetric_difference(&expected_keys).count();
            findings.add_count(path, "frontmatter-keys", count.max(1));
        }
        let description_chars = parsed.description.chars().count();
        if !(MIN_DESCRIPTION_CHARS..=MAX_DESCRIPTION_CHARS).contains(&description_chars) {
            findings.add(path, "description-length");
        }
        let folder_name = Path::new(path)
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        if folder_name != Some(parsed.name.as_str()) {
            findings.add(path, "skill-name-folder-mismatch");
        }
        paths_by_name
            .entry(parsed.name.clone())
            .or_default()
            .push(path.clone());
        validate_references(root, path, &parsed.body, &tracked_set, &mut findings);
        parsed_by_path.insert(path.clone(), parsed);
    }
    for paths in paths_by_name.values() {
        if paths.len() > 1 {
            for path in paths {
                findings.add(path, "skill-name-duplicate");
            }
        }
    }

    for skill in registry
        .skills
        .iter()
        .filter(|skill| skill.status == "retain")
    {
        if let Some(parsed) = parsed_by_path.get(&skill.path)
            && parsed.name != skill.name
        {
            findings.add(&skill.path, "registry-name-mismatch");
        }
        validate_ui_metadata(root, skill, &tracked_set, &mut findings);
    }

    let skill_roots = tracked_skills
        .iter()
        .filter_map(|path| Path::new(path).parent().map(Path::to_path_buf))
        .collect::<BTreeSet<_>>();
    let mut scanned_resources = 0usize;
    for relative in &tracked {
        if !is_under_skill_root(relative, &skill_roots) {
            continue;
        }
        scanned_resources += 1;
        validate_tracked_resource(
            root,
            relative,
            &tracked_set,
            &registry.credential_allowlist,
            &mut findings,
        );
    }

    build_envelope(
        started,
        findings,
        registry.skills.len(),
        tracked_skills.len(),
        scanned_resources,
    )
}

fn validate_registry(
    registry: &SkillRegistry,
    tracked: &BTreeSet<String>,
    findings: &mut Findings,
) {
    let mut path_counts = BTreeMap::new();
    let mut name_counts = BTreeMap::new();
    for skill in &registry.skills {
        *path_counts.entry(skill.path.as_str()).or_insert(0usize) += 1;
        *name_counts.entry(skill.name.as_str()).or_insert(0usize) += 1;

        if !is_safe_relative_path(&skill.path) || !is_skill_manifest(&skill.path) {
            findings.add(&skill.path, "registry-path-invalid");
        }
        match skill.status.as_str() {
            "retain" => {
                if !tracked.contains(&skill.path) {
                    findings.add(&skill.path, "retained-skill-missing");
                }
                if skill.alias_target.is_some() {
                    findings.add(&skill.path, "retain-alias-target-unexpected");
                }
            }
            "alias" => {
                let valid_target = skill.alias_target.as_deref().is_some_and(|target| {
                    target != skill.name
                        && registry.skills.iter().any(|candidate| {
                            candidate.name == target && candidate.status == "retain"
                        })
                });
                if !valid_target {
                    findings.add(&skill.path, "alias-target-invalid");
                }
            }
            "retire" => {
                if skill.alias_target.is_some() {
                    findings.add(&skill.path, "retire-alias-target-unexpected");
                }
            }
            _ => findings.add(&skill.path, "registry-status-invalid"),
        }
    }
    for (path, count) in path_counts {
        if count > 1 {
            findings.add_count(path, "registry-path-duplicate", count);
        }
    }
    for count in name_counts.into_values() {
        if count > 1 {
            findings.add_count(REGISTRY_PATH, "registry-name-duplicate", count);
        }
    }
}

fn parse_frontmatter(source: &str) -> Result<ParsedSkill, ()> {
    let lines = source.lines().collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some("---") {
        return Err(());
    }
    let closing = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (line.trim() == "---").then_some(index))
        .ok_or(())?;
    let frontmatter = lines[1..closing].join("\n");
    let value = serde_yaml::from_str::<Value>(&frontmatter).map_err(|_| ())?;
    let mapping = value.as_mapping().ok_or(())?;
    let keys = mapping
        .keys()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if keys.len() != mapping.len() {
        return Err(());
    }
    let name = mapping_string(mapping, "name").ok_or(())?.to_string();
    let description = mapping_string(mapping, "description")
        .ok_or(())?
        .to_string();
    if name.trim().is_empty() || description.trim().is_empty() {
        return Err(());
    }
    Ok(ParsedSkill {
        name,
        description,
        keys,
        body: lines[closing + 1..].join("\n"),
    })
}

fn validate_ui_metadata(
    root: &Path,
    skill: &RegistrySkill,
    tracked: &BTreeSet<String>,
    findings: &mut Findings,
) {
    let Some(skill_dir) = Path::new(&skill.path).parent() else {
        findings.add(&skill.path, "registry-path-invalid");
        return;
    };
    let ui_relative = normalize_relative(&skill_dir.join("agents/openai.yaml"));
    if !tracked.contains(&ui_relative) {
        findings.add(&ui_relative, "ui-metadata-missing");
        return;
    }
    let source = match fs::read_to_string(root.join(&ui_relative)) {
        Ok(source) => source,
        Err(_) => {
            findings.add(&ui_relative, "ui-metadata-unreadable");
            return;
        }
    };
    let value = match serde_yaml::from_str::<Value>(&source) {
        Ok(value) => value,
        Err(_) => {
            findings.add(&ui_relative, "ui-metadata-invalid");
            return;
        }
    };
    let Some(root_mapping) = value.as_mapping() else {
        findings.add(&ui_relative, "ui-metadata-invalid");
        return;
    };
    let allowed_root_keys = if skill.requires_explicit_invocation {
        BTreeSet::from(["interface", "policy"])
    } else {
        BTreeSet::from(["interface"])
    };
    let root_keys = yaml_mapping_keys(root_mapping);
    if root_keys != allowed_root_keys {
        findings.add_count(
            &ui_relative,
            "ui-metadata-keys",
            root_keys
                .symmetric_difference(&allowed_root_keys)
                .count()
                .max(1),
        );
    }

    let Some(interface) = mapping_value(root_mapping, "interface").and_then(Value::as_mapping)
    else {
        findings.add(&ui_relative, "ui-interface-invalid");
        return;
    };
    let expected_interface_keys =
        BTreeSet::from(["default_prompt", "display_name", "short_description"]);
    let interface_keys = yaml_mapping_keys(interface);
    if interface_keys != expected_interface_keys {
        findings.add_count(
            &ui_relative,
            "ui-interface-keys",
            interface_keys
                .symmetric_difference(&expected_interface_keys)
                .count()
                .max(1),
        );
    }
    for field in ["display_name", "short_description", "default_prompt"] {
        if mapping_string(interface, field).is_none_or(|value| value.trim().is_empty()) {
            findings.add(&ui_relative, "ui-interface-value");
        }
        if !interface_field_is_quoted(&source, field) {
            findings.add(&ui_relative, "ui-interface-unquoted");
        }
    }
    if let Some(prompt) = mapping_string(interface, "default_prompt") {
        if !contains_exact_skill_invocation(prompt, &skill.name) {
            findings.add(&ui_relative, "ui-default-prompt-skill");
        }
        if !is_one_sentence(prompt) {
            findings.add(&ui_relative, "ui-default-prompt-sentence");
        }
    }

    let policy = mapping_value(root_mapping, "policy").and_then(Value::as_mapping);
    if skill.requires_explicit_invocation {
        let Some(policy) = policy else {
            findings.add(&ui_relative, "restricted-invocation-policy");
            return;
        };
        let expected_policy_keys = BTreeSet::from(["allow_implicit_invocation"]);
        let policy_keys = yaml_mapping_keys(policy);
        if policy_keys != expected_policy_keys {
            findings.add_count(
                &ui_relative,
                "ui-policy-keys",
                policy_keys
                    .symmetric_difference(&expected_policy_keys)
                    .count()
                    .max(1),
            );
        }
        if mapping_bool(policy, "allow_implicit_invocation") != Some(false) {
            findings.add(&ui_relative, "restricted-invocation-policy");
        }
    } else if policy.is_some() {
        findings.add(&ui_relative, "unexpected-invocation-policy");
    }
}

fn validate_references(
    root: &Path,
    skill_path: &str,
    body: &str,
    tracked: &BTreeSet<String>,
    findings: &mut Findings,
) {
    let Some(skill_dir) = Path::new(skill_path).parent() else {
        return;
    };
    if let Ok(link_regex) = Regex::new(r#"\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)"#) {
        let mut missing = 0usize;
        let mut untracked = 0usize;
        let mut outside = 0usize;
        for captures in link_regex.captures_iter(body) {
            let Some(target) = captures.get(1).map(|value| value.as_str()) else {
                continue;
            };
            if let Some(relative) = local_reference_target(target) {
                match classify_repository_target(root, skill_dir, relative, tracked) {
                    RepositoryTarget::Tracked => {}
                    RepositoryTarget::Missing => missing += 1,
                    RepositoryTarget::Untracked => untracked += 1,
                    RepositoryTarget::Outside => outside += 1,
                }
            }
        }
        findings.add_count(skill_path, "relative-link-missing", missing);
        findings.add_count(skill_path, "relative-link-untracked", untracked);
        findings.add_count(skill_path, "relative-link-outside-repository", outside);
    }

    if let Ok(resource_regex) = Regex::new(r#"`((?:references|scripts|assets|examples)/[^`\s]+)`"#)
    {
        let mut missing = 0usize;
        let mut untracked = 0usize;
        let mut outside = 0usize;
        for captures in resource_regex.captures_iter(body) {
            let Some(target) = captures.get(1).map(|value| value.as_str()) else {
                continue;
            };
            let target = target.trim_end_matches(['.', ',', ':', ';', ')', ']', '}']);
            match classify_repository_target(root, skill_dir, target, tracked) {
                RepositoryTarget::Tracked => {}
                RepositoryTarget::Missing => missing += 1,
                RepositoryTarget::Untracked => untracked += 1,
                RepositoryTarget::Outside => outside += 1,
            }
        }
        findings.add_count(skill_path, "referenced-resource-missing", missing);
        findings.add_count(skill_path, "referenced-resource-untracked", untracked);
        findings.add_count(
            skill_path,
            "referenced-resource-outside-repository",
            outside,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepositoryTarget {
    Tracked,
    Missing,
    Untracked,
    Outside,
}

fn classify_repository_target(
    root: &Path,
    base: &Path,
    target: &str,
    tracked: &BTreeSet<String>,
) -> RepositoryTarget {
    let Some(relative) = normalize_repo_relative(base, target) else {
        return RepositoryTarget::Outside;
    };
    let metadata = match fs::symlink_metadata(root.join(&relative)) {
        Ok(metadata) => metadata,
        Err(_) => return RepositoryTarget::Missing,
    };
    if !tracked.contains(&relative) {
        return RepositoryTarget::Untracked;
    }
    if metadata.file_type().is_symlink() {
        return match resolve_tracked_symlink(root, &relative, tracked) {
            Ok(_) => RepositoryTarget::Tracked,
            Err("dangling-symlink") => RepositoryTarget::Missing,
            Err(_) => RepositoryTarget::Outside,
        };
    }
    RepositoryTarget::Tracked
}

fn validate_tracked_resource(
    root: &Path,
    relative: &str,
    tracked: &BTreeSet<String>,
    allowlist: &[CredentialAllowlistEntry],
    findings: &mut Findings,
) {
    if has_forbidden_resource_extension(relative) {
        findings.add(relative, "tracked-resource-extension");
    }

    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(_) => {
            findings.add(relative, "tracked-resource-missing");
            return;
        }
    };
    let resolved = if metadata.file_type().is_symlink() {
        match resolve_tracked_symlink(root, relative, tracked) {
            Ok(resolved) => resolved,
            Err(rule) => {
                findings.add(relative, rule);
                return;
            }
        }
    } else {
        ResolvedTrackedResource {
            path,
            relative: relative.to_string(),
        }
    };
    if !resolved.path.is_file() {
        return;
    }
    let bytes = match fs::read(&resolved.path) {
        Ok(bytes) => bytes,
        Err(_) => {
            findings.add(relative, "tracked-resource-unreadable");
            return;
        }
    };
    let source = String::from_utf8_lossy(&bytes);
    let credential_scan = scan_credentials(relative, &resolved.relative, &source, allowlist);
    findings.add_count(
        relative,
        "credential-like-content",
        credential_scan.match_count,
    );
    if credential_scan.structured_parse_failed {
        findings.add(relative, "structured-credential-scan-invalid");
    }
}

#[derive(Debug)]
struct ResolvedTrackedResource {
    path: PathBuf,
    relative: String,
}

fn resolve_tracked_symlink(
    root: &Path,
    relative: &str,
    tracked: &BTreeSet<String>,
) -> Result<ResolvedTrackedResource, &'static str> {
    let mut current = relative.to_string();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return Err("symlink-cycle");
        }
        let path = root.join(&current);
        let metadata = fs::symlink_metadata(&path).map_err(|_| "dangling-symlink")?;
        if !metadata.file_type().is_symlink() {
            return Ok(ResolvedTrackedResource {
                path,
                relative: current,
            });
        }
        let target = fs::read_link(&path).map_err(|_| "dangling-symlink")?;
        if target.is_absolute() {
            return Err("symlink-target-untracked-or-outside");
        }
        let target = target
            .to_str()
            .ok_or("symlink-target-untracked-or-outside")?;
        let base = Path::new(&current)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        let next =
            normalize_repo_relative(base, target).ok_or("symlink-target-untracked-or-outside")?;
        if fs::symlink_metadata(root.join(&next)).is_err() {
            return Err("dangling-symlink");
        }
        if !tracked.contains(&next) {
            return Err("symlink-target-untracked-or-outside");
        }
        current = next;
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CredentialScan {
    match_count: usize,
    structured_parse_failed: bool,
}

fn scan_credentials(
    report_path: &str,
    format_path: &str,
    source: &str,
    allowlist: &[CredentialAllowlistEntry],
) -> CredentialScan {
    let mut scan = CredentialScan::default();
    if PRIVATE_KEY_MARKERS
        .iter()
        .any(|marker| source.contains(marker))
        && !is_credential_allowlisted(report_path, source.as_bytes(), allowlist)
    {
        scan.match_count += 1;
    }

    let prefix_regex = Regex::new(
        r"(?:AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{20,}|xox[baprs]-[A-Za-z0-9-]{20,}|glpat-[A-Za-z0-9_-]{20,}|hf_[A-Za-z0-9]{20,})",
    )
    .ok();
    if let Some(prefix_regex) = prefix_regex.as_ref() {
        scan.match_count += prefix_regex
            .find_iter(source)
            .filter(|value| {
                !is_credential_allowlisted(report_path, value.as_str().as_bytes(), allowlist)
            })
            .count();
    }

    if is_yaml_or_json(format_path) {
        match serde_yaml::from_str::<Value>(source) {
            Ok(value) => {
                if let Ok(hex_regex) = Regex::new(r"(?i)[0-9a-f]{32,}") {
                    scan.match_count += count_scalar_hex_matches(
                        &value,
                        report_path,
                        &hex_regex,
                        prefix_regex.as_ref(),
                        allowlist,
                    );
                }
            }
            Err(_) => scan.structured_parse_failed = true,
        }
    }
    scan
}

fn count_scalar_hex_matches(
    value: &Value,
    path: &str,
    hex_regex: &Regex,
    prefix_regex: Option<&Regex>,
    allowlist: &[CredentialAllowlistEntry],
) -> usize {
    match value {
        Value::String(value) => {
            let prefix_ranges = prefix_regex
                .map(|regex| {
                    regex
                        .find_iter(value)
                        .map(|matched| matched.start()..matched.end())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            hex_regex
                .find_iter(value)
                .filter(|matched| has_hex_token_boundaries(value, matched.start(), matched.end()))
                .filter(|matched| {
                    !prefix_ranges
                        .iter()
                        .any(|prefix| matched.start() < prefix.end && prefix.start < matched.end())
                })
                .filter(|matched| {
                    !is_credential_allowlisted(path, matched.as_str().as_bytes(), allowlist)
                })
                .count()
        }
        Value::Sequence(values) => values
            .iter()
            .map(|value| count_scalar_hex_matches(value, path, hex_regex, prefix_regex, allowlist))
            .sum(),
        Value::Mapping(mapping) => mapping
            .values()
            .map(|value| count_scalar_hex_matches(value, path, hex_regex, prefix_regex, allowlist))
            .sum(),
        Value::Tagged(tagged) => {
            count_scalar_hex_matches(&tagged.value, path, hex_regex, prefix_regex, allowlist)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

fn is_credential_allowlisted(
    path: &str,
    value: &[u8],
    allowlist: &[CredentialAllowlistEntry],
) -> bool {
    if !allowlist.iter().any(|entry| entry.path == path) {
        return false;
    }
    let mut hasher = Sha256::new();
    hasher.update(value);
    let fingerprint = format!("{:x}", hasher.finalize());
    allowlist
        .iter()
        .any(|entry| entry.path == path && entry.fingerprint == fingerprint)
}

fn build_envelope(
    started: Instant,
    findings: Findings,
    registry_entries: usize,
    tracked_skills: usize,
    scanned_resources: usize,
) -> QueryEnvelope {
    let finding_count = findings.total();
    let finding_groups = findings.counts.len();
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_skill_contract"),
        kind: "doctor".to_string(),
        summary: if finding_count == 0 {
            format!(
                "skill contract passed for {tracked_skills} tracked skill(s) and {scanned_resources} tracked resource(s)"
            )
        } else {
            format!(
                "skill contract found {finding_count} finding(s) across {finding_groups} path/rule group(s)"
            )
        },
        confidence: if finding_count == 0 { 0.98 } else { 0.72 },
        entities: findings.entities(),
        evidence: Vec::new(),
        warnings: findings.warnings(),
        meta: Some(json!({
            "registry_entries": registry_entries,
            "tracked_skills": tracked_skills,
            "scanned_resources": scanned_resources,
            "finding_count": finding_count,
            "finding_groups": finding_groups,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn mapping_value<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(Value::String(key.to_string()))
}

fn mapping_string<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a str> {
    mapping_value(mapping, key).and_then(Value::as_str)
}

fn mapping_bool(mapping: &Mapping, key: &str) -> Option<bool> {
    mapping_value(mapping, key).and_then(Value::as_bool)
}

fn yaml_mapping_keys(mapping: &Mapping) -> BTreeSet<&str> {
    mapping.keys().filter_map(Value::as_str).collect()
}

fn interface_field_is_quoted(source: &str, field: &str) -> bool {
    let lines = source.lines().collect::<Vec<_>>();
    let Some(interface_index) = lines
        .iter()
        .position(|line| leading_spaces(line) == 0 && line.trim() == "interface:")
    else {
        return false;
    };
    let block_end = lines
        .iter()
        .enumerate()
        .skip(interface_index + 1)
        .find_map(|(index, line)| {
            let trimmed = line.trim();
            (!trimmed.is_empty() && !trimmed.starts_with('#') && leading_spaces(line) == 0)
                .then_some(index)
        })
        .unwrap_or(lines.len());
    let block = &lines[interface_index + 1..block_end];
    let Some(direct_indent) = block
        .iter()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .map(|line| leading_spaces(line))
        .filter(|indent| *indent > 0)
        .min()
    else {
        return false;
    };
    let prefix = format!("{field}:");
    let values = block
        .iter()
        .filter(|line| leading_spaces(line) == direct_indent)
        .filter_map(|line| line.trim_start().strip_prefix(&prefix))
        .map(str::trim)
        .collect::<Vec<_>>();
    values.len() == 1
        && ((values[0].starts_with('"') && values[0].ends_with('"'))
            || (values[0].starts_with('\'') && values[0].ends_with('\'')))
}

fn leading_spaces(value: &str) -> usize {
    value
        .chars()
        .take_while(|character| *character == ' ')
        .count()
}

fn is_one_sentence(value: &str) -> bool {
    let value = value.trim();
    let terminal_count = value
        .chars()
        .filter(|character| matches!(character, '.' | '!' | '?'))
        .count();
    terminal_count == 1 && value.ends_with(['.', '!', '?'])
}

fn contains_exact_skill_invocation(prompt: &str, skill_name: &str) -> bool {
    let needle = format!("${skill_name}");
    prompt.match_indices(&needle).any(|(start, matched)| {
        let before_is_token = prompt[..start]
            .chars()
            .next_back()
            .is_some_and(is_skill_token_character);
        let end = start + matched.len();
        let after_is_token = prompt[end..]
            .chars()
            .next()
            .is_some_and(is_skill_token_character);
        !before_is_token && !after_is_token
    })
}

fn is_skill_token_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
}

fn local_reference_target(target: &str) -> Option<&str> {
    let target = target.trim_matches(['<', '>']);
    if target.is_empty()
        || target.starts_with('#')
        || target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("mailto:")
    {
        return None;
    }
    let without_fragment = target.split_once('#').map_or(target, |(before, _)| before);
    let target = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(before, _)| before);
    (!target.is_empty()).then_some(target)
}

fn normalize_repo_relative(base: &Path, target: &str) -> Option<String> {
    let target = Path::new(target);
    if target.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in base.join(target).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!normalized.as_os_str().is_empty()).then(|| normalize_relative(&normalized))
}

fn is_skill_manifest(path: &str) -> bool {
    path == "SKILL.md" || path.ends_with("/SKILL.md")
}

fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn normalize_relative(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_under_skill_root(relative: &str, roots: &BTreeSet<PathBuf>) -> bool {
    let path = Path::new(relative);
    roots.iter().any(|root| path.starts_with(root))
}

fn has_forbidden_resource_extension(path: &str) -> bool {
    let path = Path::new(path);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    if extension
        .as_deref()
        .is_some_and(|extension| FORBIDDEN_RESOURCE_EXTENSIONS.contains(&extension))
    {
        return true;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|name| {
            FORBIDDEN_RESOURCE_EXTENSIONS
                .iter()
                .any(|extension| name == format!(".{extension}"))
        })
}

fn is_yaml_or_json(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|extension| matches!(extension.as_str(), "yaml" | "yml" | "json"))
}

fn has_hex_token_boundaries(source: &str, start: usize, end: usize) -> bool {
    let before_is_hex = source[..start]
        .chars()
        .next_back()
        .is_some_and(|character| character.is_ascii_hexdigit());
    let after_is_hex = source[end..]
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_hexdigit());
    !before_is_hex && !after_is_hex
}
