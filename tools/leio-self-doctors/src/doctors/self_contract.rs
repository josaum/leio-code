use std::path::Path;
use std::time::Instant;

use serde_json::{Value, json};

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use super::version_manifest::{VersionKind, VersionManifest, version_from_source, version_line};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

const EXPECTED_VERSION_MANIFESTS: &[VersionManifest<'static>] = &[
    VersionManifest {
        path: ".codex-plugin/plugin.json",
        kind: VersionKind::JsonPackage,
        label: "Codex plugin manifest",
    },
    VersionManifest {
        path: ".claude-plugin/plugin.json",
        kind: VersionKind::JsonPackage,
        label: "Claude plugin manifest",
    },
    VersionManifest {
        path: "Cargo.toml",
        kind: VersionKind::CargoPackage,
        label: "Rust crate manifest",
    },
    VersionManifest {
        path: "Cargo.lock",
        kind: VersionKind::CargoLockPackage("leio-code"),
        label: "Rust lockfile package",
    },
    VersionManifest {
        path: "mcp/package.json",
        kind: VersionKind::JsonPackage,
        label: "MCP package manifest",
    },
    VersionManifest {
        path: "mcp/package-lock.json",
        kind: VersionKind::NpmPackageLockRoot,
        label: "MCP package lock",
    },
    VersionManifest {
        path: "apps-sdk/package.json",
        kind: VersionKind::JsonPackage,
        label: "Apps SDK package manifest",
    },
    VersionManifest {
        path: "apps-sdk/package-lock.json",
        kind: VersionKind::NpmPackageLockRoot,
        label: "Apps SDK package lock",
    },
];

const REQUIRED_PACKAGE_PATHS: &[&str] = &[
    ".leio-code/config.toml",
    ".codex-plugin",
    ".claude-plugin",
    ".mcp.json",
    "Cargo.toml",
    "Cargo.lock",
    "README.md",
    "GEMINI.md",
    "docs",
    "benchmarks",
    "skills",
    "mcp",
    "apps-sdk",
    "src",
    "scripts",
    "assets",
    "prompts",
    "agents",
    "artifacts",
    "crates/leio-harness",
];

/// No path dependencies are required anymore: FCA induction ships as a
/// pre-built wheel under `artifacts/wheels/` instead of a vendored crate.
const REQUIRED_CARGO_PATH_DEPENDENCIES: &[(&str, &str)] = &[];

const REQUIRED_DOCKER_BUILD_COPIES: &[(&str, &str)] = &[
    ("Cargo manifests", "COPY Cargo.toml Cargo.lock ./"),
    ("src", "COPY src ./src"),
    ("crates", "COPY crates ./crates"),
];

const REQUIRED_DOCKER_RUNTIME_COPIES: &[(&str, &str)] = &[
    ("mcp", "COPY mcp ./mcp"),
    ("apps-sdk/auth.js", "COPY apps-sdk/auth.js ./auth.js"),
    (
        "apps-sdk/tenant-scope.mjs",
        "COPY apps-sdk/tenant-scope.mjs ./tenant-scope.mjs",
    ),
    (
        "apps-sdk/repo-url.js",
        "COPY apps-sdk/repo-url.js ./repo-url.js",
    ),
    (
        "apps-sdk/validate-repo-url.mjs",
        "COPY apps-sdk/validate-repo-url.mjs ./validate-repo-url.mjs",
    ),
    ("apps-sdk/server.js", "COPY apps-sdk/server.js ./server.js"),
    (
        "apps-sdk/entrypoint.sh",
        "COPY apps-sdk/entrypoint.sh ./entrypoint.sh",
    ),
    ("apps-sdk/keycloak", "COPY apps-sdk/keycloak ./keycloak"),
    ("apps-sdk/public", "COPY apps-sdk/public ./public"),
];

const REQUIRED_APPS_SDK_SCRIPTS: &[(&str, &str)] = &[
    (
        "docker:build",
        "docker build -t leio-code-apps-sdk -f Dockerfile ..",
    ),
    ("docker:smoke", "bash scripts/smoke-docker.sh"),
];

const REQUIRED_DOCKER_SMOKE_INVARIANTS: &[(&str, &str)] = &[
    ("optional image build", "npm run docker:build"),
    ("container launch", "docker run"),
    ("health probe", "/health"),
    ("baked repository smoke root", "/workspace/baked-repo"),
    ("Apps SDK smoke runner", "smoke.mjs"),
];

pub struct SelfContractDoctor;

impl Doctor for SelfContractDoctor {
    fn name(&self) -> &'static str {
        "self-contract"
    }

    fn description(&self) -> &'static str {
        "Checks LEIO Code's own profile, version manifests, packaging payload, Docker build/runtime payload, and MCP/App tool-surface parity."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_self_contract(index, root)
    }
}

pub fn doctor_self_contract(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    check_profile(root, &mut warnings, &mut evidence);
    check_binary_freshness(&mut warnings, &mut evidence);
    check_versions(root, &mut warnings, &mut evidence, &mut entities);
    check_cargo_path_dependencies(root, &mut warnings, &mut evidence);
    check_packaging_payload(root, &mut warnings, &mut evidence);
    check_docker_runtime_payload(root, &mut warnings, &mut evidence);
    check_apps_sdk_smoke_surface(root, &mut warnings, &mut evidence);
    check_cli_surface(root, &mut warnings, &mut evidence);
    check_tool_surfaces(root, &mut warnings, &mut evidence, &mut entities);
    check_binary_resolution(root, &mut warnings, &mut evidence);
    check_plugin_mcp_launch(root, &mut warnings, &mut evidence);

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_self_contract"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "LEIO Code self-contract is aligned across profile, versions, package payload, Docker build/runtime/smoke payload, tool surfaces, and plugin MCP launch".to_string()
        } else {
            format!(
                "LEIO Code self-contract found {} warning(s) across profile, versions, package payload, Docker build/runtime/smoke payload, tool surfaces, or plugin MCP launch",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.98 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "workspace_profile": "leio-code",
            "contract": "self-contract",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn check_profile(root: &Path, warnings: &mut Vec<String>, evidence: &mut Vec<EvidenceItem>) {
    let rel = ".leio-code/config.toml";
    let path = root.join(rel);
    let Some(src) = read_text(&path, warnings) else {
        return;
    };
    let needle = "workspace_profile = \"leio-code\"";
    if let Some(line) = find_line(&src, needle) {
        evidence.push(EvidenceItem {
            kind: "self_profile".to_string(),
            path: rel.to_string(),
            line: Some(line),
            detail: "repository opts into the LEIO Code self profile".to_string(),
        });
    } else {
        warnings.push(format!("{rel} must declare {needle}"));
    }
}

fn check_versions(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    entities: &mut Vec<Value>,
) {
    let source_version = read_version(root, &EXPECTED_VERSION_MANIFESTS[0], warnings, evidence);
    let mut versions = Vec::new();

    for manifest in EXPECTED_VERSION_MANIFESTS {
        let version = read_version(root, manifest, warnings, evidence);
        if let Some(version) = version.as_deref() {
            versions.push(json!({
                "path": manifest.path,
                "label": manifest.label,
                "version": version,
            }));
        }
        if let (Some(expected), Some(actual)) = (source_version.as_deref(), version.as_deref())
            && actual != expected
        {
            warnings.push(format!(
                "{} at {} has version {}, expected {} from .codex-plugin/plugin.json",
                manifest.label, manifest.path, actual, expected
            ));
        }
    }

    entities.push(json!({
        "contract": "versions",
        "source": ".codex-plugin/plugin.json",
        "expected_version": source_version,
        "manifests": versions,
    }));
}

fn read_version(
    root: &Path,
    manifest: &VersionManifest<'_>,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) -> Option<String> {
    let path = root.join(manifest.path);
    let src = read_text(&path, warnings)?;
    let version = version_from_source(&src, manifest.kind);

    match version {
        Some(version) => {
            evidence.push(EvidenceItem {
                kind: "version_contract".to_string(),
                path: manifest.path.to_string(),
                line: version_line(&src, manifest.kind),
                detail: format!("{} declares version {}", manifest.label, version),
            });
            Some(version)
        }
        None => {
            warnings.push(format!(
                "could not read package version from {} ({})",
                manifest.path, manifest.label
            ));
            None
        }
    }
}

fn check_packaging_payload(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    for script in [
        "scripts/package_codex_plugin.py",
        "scripts/install_codex_plugin.py",
    ] {
        let path = root.join(script);
        let Some(src) = read_text(&path, warnings) else {
            continue;
        };

        for rel in REQUIRED_PACKAGE_PATHS {
            let needle = format!("\"{rel}\"");
            if let Some(line) = find_line(&src, &needle) {
                evidence.push(EvidenceItem {
                    kind: "package_payload".to_string(),
                    path: script.to_string(),
                    line: Some(line),
                    detail: format!("portable package includes {rel}"),
                });
            } else {
                warnings.push(format!(
                    "{script} does not include required payload path `{rel}`"
                ));
            }
        }
    }
}

fn check_cargo_path_dependencies(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let rel = "Cargo.toml";
    let Some(src) = read_text(&root.join(rel), warnings) else {
        return;
    };

    for (package, needle) in REQUIRED_CARGO_PATH_DEPENDENCIES {
        if let Some(line) = find_line(&src, needle) {
            evidence.push(EvidenceItem {
                kind: "cargo_path_dependency".to_string(),
                path: rel.to_string(),
                line: Some(line),
                detail: format!("{package} is vendored inside the portable plugin payload"),
            });
        } else {
            warnings.push(format!(
                "{rel} path dependency `{package}` must use a portable in-repo vendor path"
            ));
        }
    }
}

fn check_docker_runtime_payload(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let rel = "apps-sdk/Dockerfile";
    let Some(src) = read_text(&root.join(rel), warnings) else {
        return;
    };

    for (payload, needle) in REQUIRED_DOCKER_BUILD_COPIES {
        if let Some(line) = find_line(&src, needle) {
            evidence.push(EvidenceItem {
                kind: "docker_payload".to_string(),
                path: rel.to_string(),
                line: Some(line),
                detail: format!("Docker builder includes {payload}"),
            });
        } else {
            warnings.push(format!(
                "{rel} does not copy required build payload `{payload}`"
            ));
        }
    }

    for (payload, needle) in REQUIRED_DOCKER_RUNTIME_COPIES {
        if let Some(line) = find_line(&src, needle) {
            evidence.push(EvidenceItem {
                kind: "docker_payload".to_string(),
                path: rel.to_string(),
                line: Some(line),
                detail: format!("Docker runtime includes {payload}"),
            });
        } else {
            warnings.push(format!(
                "{rel} does not copy required runtime payload `{payload}`"
            ));
        }
    }
}

fn check_apps_sdk_smoke_surface(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let package_rel = "apps-sdk/package.json";
    let Some(package_src) = read_text(&root.join(package_rel), warnings) else {
        return;
    };

    match serde_json::from_str::<Value>(&package_src) {
        Ok(package) => {
            let scripts = package.get("scripts").and_then(Value::as_object);
            for (script_name, expected_command) in REQUIRED_APPS_SDK_SCRIPTS {
                let actual = scripts
                    .and_then(|scripts| scripts.get(*script_name))
                    .and_then(Value::as_str);
                if actual == Some(*expected_command) {
                    evidence.push(EvidenceItem {
                        kind: "docker_smoke_surface".to_string(),
                        path: package_rel.to_string(),
                        line: find_line(&package_src, &format!("\"{script_name}\"")),
                        detail: format!("Apps SDK package exposes `{script_name}`"),
                    });
                } else {
                    warnings.push(format!(
                        "{package_rel} script `{script_name}` must be `{expected_command}`"
                    ));
                }
            }
        }
        Err(error) => warnings.push(format!("{package_rel} is not valid JSON: {error}")),
    }

    let smoke_rel = "apps-sdk/scripts/smoke-docker.sh";
    let Some(smoke_src) = read_text(&root.join(smoke_rel), warnings) else {
        return;
    };

    for (label, needle) in REQUIRED_DOCKER_SMOKE_INVARIANTS {
        if let Some(line) = find_line(&smoke_src, needle) {
            evidence.push(EvidenceItem {
                kind: "docker_smoke_surface".to_string(),
                path: smoke_rel.to_string(),
                line: Some(line),
                detail: format!("Docker smoke script includes {label}"),
            });
        } else {
            warnings.push(format!(
                "{smoke_rel} missing required Docker smoke invariant `{label}` ({needle})"
            ));
        }
    }
}

fn check_cli_surface(root: &Path, warnings: &mut Vec<String>, evidence: &mut Vec<EvidenceItem>) {
    let path = "src/main.rs";
    let Some(src) = read_text(&root.join(path), warnings) else {
        return;
    };
    for (needle, detail) in [
        ("Verify,", "CLI exposes the verification command"),
        ("Context {", "CLI exposes the context-bundle command"),
        ("Nav {", "CLI exposes stateful node navigation"),
        (
            "TrustDoctorPack",
            "CLI exposes explicit native doctor registration",
        ),
        (
            "kind: String",
            "CLI accepts repository-declared doctor names",
        ),
    ] {
        if let Some(line) = find_line(&src, needle) {
            evidence.push(EvidenceItem {
                kind: "cli_surface".to_string(),
                path: path.to_string(),
                line: Some(line),
                detail: detail.to_string(),
            });
        } else {
            warnings.push(format!(
                "{path} missing CLI self-contract invariant: {needle}"
            ));
        }
    }
}

fn check_tool_surfaces(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    entities: &mut Vec<Value>,
) {
    let mcp_path = "mcp/index.js";
    let apps_path = "apps-sdk/server.js";
    let mcp = read_text(&root.join(mcp_path), warnings);
    let apps = read_text(&root.join(apps_path), warnings);

    if let Some(mcp) = mcp.as_deref() {
        // Kind families are derived from the binary at startup
        // (`capabilities --catalog`), so the old mirror arrays must not
        // reappear: a literal list is exactly the drift surface the catalog
        // removed.
        check_source_contains(
            mcp_path,
            mcp,
            "capabilities\", \"--catalog\", \"--json\"",
            "MCP derives kind families from the binary catalog",
            warnings,
            evidence,
        );
        for array in [
            "const FIND_KINDS = [",
            "const DOCTOR_KINDS = [",
            "const EXPORT_KINDS = [",
        ] {
            if mcp.contains(array) {
                warnings.push(format!(
                    "{mcp_path}: literal `{}` re-introduces a kind mirror; derive from the catalog",
                    array.trim()
                ));
            }
        }
        check_source_contains(
            mcp_path,
            mcp,
            "leio_code_context",
            "MCP exposes the context-bundle tool",
            warnings,
            evidence,
        );
        check_source_contains(
            mcp_path,
            mcp,
            "leio_code_init",
            "MCP exposes one-shot init",
            warnings,
            evidence,
        );
        check_source_contains(
            mcp_path,
            mcp,
            "leio_code_verify",
            "MCP exposes profile verify",
            warnings,
            evidence,
        );
        check_source_contains(
            mcp_path,
            mcp,
            "leio_code_watch",
            "MCP exposes detached watch control",
            warnings,
            evidence,
        );
        check_source_contains(
            mcp_path,
            mcp,
            "leio_code_nav",
            "MCP exposes stateful node navigation",
            warnings,
            evidence,
        );
        check_source_contains(
            mcp_path,
            mcp,
            "resolve-binary.js",
            "MCP resolves leio-code via the shared cargo-bin helper",
            warnings,
            evidence,
        );
        check_js_array(
            mcp_path,
            mcp,
            "NAV_KINDS",
            &[
                "here",
                "goto",
                "select",
                "callers",
                "callees",
                "neighbors",
                "related",
                "parent",
                "child",
                "peer",
                "align",
                "back",
                "forward",
                "reset",
                "explain",
            ],
            warnings,
            evidence,
        );
    }

    if let Some(apps) = apps.as_deref() {
        check_source_contains(
            apps_path,
            apps,
            "capabilities\", \"--catalog\", \"--json\"",
            "Apps SDK derives kind families from the binary catalog",
            warnings,
            evidence,
        );
        for array in ["const findKinds = [", "const doctorKinds = ["] {
            if apps.contains(array) {
                warnings.push(format!(
                    "{apps_path}: literal `{}` re-introduces a kind mirror; derive from the catalog",
                    array.trim()
                ));
            }
        }
        check_source_contains(
            apps_path,
            apps,
            "prepare_repository_context",
            "Apps SDK exposes the context-bundle tool",
            warnings,
            evidence,
        );
        let map_path = "apps-sdk/tool-name-map.js";
        if let Some(map) = read_text(&root.join(map_path), warnings) {
            check_source_contains(
                map_path,
                &map,
                "leio_code_context: \"prepare_repository_context\"",
                "Apps SDK action palette maps the MCP context tool",
                warnings,
                evidence,
            );
        }
        check_source_contains(
            apps_path,
            apps,
            "tool-name-map.js",
            "Apps SDK imports the canonical tool name mapping",
            warnings,
            evidence,
        );
        check_source_contains(
            apps_path,
            apps,
            "resolve-binary.js",
            "Apps SDK resolves leio-code via the shared cargo-bin helper",
            warnings,
            evidence,
        );
    }

    if let (Some(mcp), Some(apps)) = (mcp.as_deref(), apps.as_deref()) {
        compare_js_arrays(
            mcp_path,
            mcp,
            "FIND_KINDS",
            apps_path,
            apps,
            "findKinds",
            warnings,
            evidence,
        );
        compare_js_arrays(
            mcp_path,
            mcp,
            "EXPLAIN_KINDS",
            apps_path,
            apps,
            "explainKinds",
            warnings,
            evidence,
        );
        compare_js_arrays(
            mcp_path,
            mcp,
            "DOCTOR_KINDS",
            apps_path,
            apps,
            "doctorKinds",
            warnings,
            evidence,
        );
    }

    entities.push(json!({
        "contract": "tool_surfaces",
        "mcp": mcp_path,
        "apps_sdk": apps_path,
        "checked_arrays": [
            "FIND_KINDS/findKinds",
            "EXPLAIN_KINDS/explainKinds",
            "DOCTOR_KINDS/doctorKinds",
            "GRAPH_KINDS",
            "EXPORT_KINDS",
            "KNOWLEDGE_KINDS",
            "leio_code_context/prepare_repository_context"
        ],
    }));
}

fn check_plugin_mcp_launch(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let launch_rel = "scripts/launch-stdio-mcp.sh";
    if let Some(src) = read_text(&root.join(launch_rel), warnings) {
        check_source_contains(
            launch_rel,
            &src,
            "CLAUDE_PLUGIN_ROOT",
            "stdio launch resolves the plugin root when hosts spawn off-cwd",
            warnings,
            evidence,
        );
    }

    let project_rel = ".mcp.json";
    if let Some(src) = read_text(&root.join(project_rel), warnings) {
        match serde_json::from_str::<Value>(&src) {
            Ok(value) => {
                let launch_warnings = project_mcp_launch_warnings(&value);
                if launch_warnings.is_empty() {
                    evidence.push(EvidenceItem {
                        kind: "plugin_mcp".to_string(),
                        path: project_rel.to_string(),
                        line: find_line(&src, "leio-code"),
                        detail: "project .mcp.json declares the leio-code stdio server".to_string(),
                    });
                } else {
                    for warning in launch_warnings {
                        warnings.push(format!("{project_rel} {warning}"));
                    }
                }
            }
            Err(err) => warnings.push(format!("{project_rel} is not valid JSON: {err}")),
        }
    }

    let plugin_rel = ".claude-plugin/plugin.json";
    if let Some(src) = read_text(&root.join(plugin_rel), warnings) {
        match serde_json::from_str::<Value>(&src) {
            Ok(value) => {
                let launch_warnings = claude_plugin_mcp_launch_warnings(&value);
                if launch_warnings.is_empty() {
                    evidence.push(EvidenceItem {
                        kind: "plugin_mcp".to_string(),
                        path: plugin_rel.to_string(),
                        line: find_line(&src, "CLAUDE_PLUGIN_ROOT"),
                        detail: "Claude plugin MCP launches via ${CLAUDE_PLUGIN_ROOT}".to_string(),
                    });
                } else {
                    for warning in launch_warnings {
                        warnings.push(format!("{plugin_rel} {warning}"));
                    }
                }
            }
            Err(err) => warnings.push(format!("{plugin_rel} is not valid JSON: {err}")),
        }
    }
}

/// Project `.mcp.json` may stay cwd-relative. The Claude plugin map must not.
fn project_mcp_launch_warnings(doc: &Value) -> Vec<String> {
    let Some(server) = doc
        .pointer("/mcpServers/leio-code")
        .or_else(|| doc.pointer("/leio-code"))
    else {
        return vec!["must declare mcpServers.leio-code".to_string()];
    };
    stdio_server_launch_warnings(server, false)
}

/// Hosts that load this checkout as a plugin often spawn with cwd != plugin root.
fn claude_plugin_mcp_launch_warnings(doc: &Value) -> Vec<String> {
    match doc.get("mcpServers") {
        None => vec!["must declare mcpServers".to_string()],
        Some(Value::String(_)) => vec![
            "mcpServers must be inline (not a path to .mcp.json) so ${CLAUDE_PLUGIN_ROOT} is expanded when cwd is not the plugin root".to_string(),
        ],
        Some(Value::Object(map)) => match map.get("leio-code") {
            Some(server) => stdio_server_launch_warnings(server, true),
            None => vec!["mcpServers.leio-code is required".to_string()],
        },
        Some(_) => vec!["mcpServers must be an object".to_string()],
    }
}

fn stdio_server_launch_warnings(server: &Value, require_plugin_root: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    let command = server.get("command").and_then(Value::as_str).unwrap_or("");
    if command != "bash" && command != "node" {
        warnings.push(format!(
            "stdio command must be bash or node, got {command:?}"
        ));
    }
    let joined = server
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    if !joined.contains("launch-stdio-mcp.sh") && !joined.contains("mcp/index.js") {
        warnings.push("args must launch scripts/launch-stdio-mcp.sh or mcp/index.js".to_string());
    }
    if require_plugin_root && !joined.contains("${CLAUDE_PLUGIN_ROOT}") {
        warnings.push(
            "args must include ${CLAUDE_PLUGIN_ROOT} so off-cwd plugin hosts can handshake"
                .to_string(),
        );
    }
    warnings
}

fn check_binary_resolution(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let resolve_path = "mcp/resolve-binary.js";
    let Some(src) = read_text(&root.join(resolve_path), warnings) else {
        return;
    };
    for (needle, detail) in [
        (
            "findInstalledBinary",
            "resolver prefers a cargo-installed leio-code",
        ),
        (
            "isCargoTargetBinary",
            "resolver must not treat target/{release,debug} as an install",
        ),
        (
            "findInstalledBinary(binaryName",
            "installed lookup runs before walking target/",
        ),
    ] {
        check_source_contains(resolve_path, &src, needle, detail, warnings, evidence);
    }
    if !installed_before_target_walk(&src) {
        warnings.push(
            "mcp/resolve-binary.js walks target/ before the cargo-installed binary".to_string(),
        );
    }
}

fn installed_before_target_walk(src: &str) -> bool {
    let Some(body) = src.split("export function resolveBinaryPath").nth(1) else {
        return false;
    };
    match (
        body.find("findInstalledBinary("),
        body.find("findBinaryUpwards("),
    ) {
        (Some(installed), Some(upwards)) => installed < upwards,
        (Some(_), None) => true,
        _ => false,
    }
}

fn check_source_contains(
    path: &str,
    src: &str,
    needle: &str,
    detail: &str,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    if let Some(line) = find_line(src, needle) {
        evidence.push(EvidenceItem {
            kind: "tool_surface".to_string(),
            path: path.to_string(),
            line: Some(line),
            detail: detail.to_string(),
        });
    } else {
        warnings.push(format!("{path} missing required surface token `{needle}`"));
    }
}

fn check_js_array(
    path: &str,
    src: &str,
    name: &str,
    expected: &[&str],
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let Some(actual) = parse_js_const_array(src, name) else {
        warnings.push(format!("{path} is missing JS const array `{name}`"));
        return;
    };
    let expected = expected
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    if actual == expected {
        evidence.push(EvidenceItem {
            kind: "tool_surface".to_string(),
            path: path.to_string(),
            line: find_line(src, &format!("const {name} = [")),
            detail: format!("{name} matches the self-contract"),
        });
    } else {
        warnings.push(format!(
            "{path} `{name}` drifted: actual={actual:?} expected={expected:?}"
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn compare_js_arrays(
    left_path: &str,
    left_src: &str,
    left_name: &str,
    right_path: &str,
    right_src: &str,
    right_name: &str,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let Some(left) = parse_js_const_array(left_src, left_name) else {
        return;
    };
    let Some(right) = parse_js_const_array(right_src, right_name) else {
        return;
    };
    if left == right {
        evidence.push(EvidenceItem {
            kind: "tool_surface".to_string(),
            path: right_path.to_string(),
            line: find_line(right_src, &format!("const {right_name} = [")),
            detail: format!("{right_name} mirrors {left_path}::{left_name}"),
        });
    } else {
        warnings.push(format!(
            "{right_path} `{right_name}` drifted from {left_path} `{left_name}`: left={left:?} right={right:?}"
        ));
    }
}

fn parse_js_const_array(src: &str, name: &str) -> Option<Vec<String>> {
    let marker = format!("const {name} = [");
    let (_, tail) = src.split_once(&marker)?;
    let (body, _) = tail.split_once("];")?;

    Some(
        body.split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| entry.trim_matches('"').trim_matches('\'').to_string())
            .collect(),
    )
}

/// Verdict for the installed-binary freshness check.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FreshnessVerdict {
    /// Binary and checkout are on the same commit.
    Current,
    /// The running binary lives inside the checkout's build dir; drifting
    /// from HEAD mid-edit is the normal developer state, never a warning.
    DevBuild,
    /// An installed binary built from a different commit than the checkout.
    Stale { baked: String, head: String },
    /// The check could not run (no git metadata, or no locatable checkout).
    Unknown(&'static str),
}

/// Compare the commit baked into the running binary against the checkout HEAD.
pub(crate) fn freshness_verdict(
    exe: &Path,
    baked: &str,
    checkout: Option<&Path>,
    checkout_head: Option<&str>,
) -> FreshnessVerdict {
    if baked == "unknown" {
        return FreshnessVerdict::Unknown("binary was built without git metadata");
    }
    let Some(checkout) = checkout else {
        return FreshnessVerdict::Unknown(
            "leio-code checkout not locatable; set LEIO_CODE_ROOT to silence this blind spot",
        );
    };
    let Some(head) = checkout_head else {
        return FreshnessVerdict::Unknown("checkout HEAD could not be read");
    };
    if baked == head {
        return FreshnessVerdict::Current;
    }
    if exe.starts_with(checkout) {
        return FreshnessVerdict::DevBuild;
    }
    FreshnessVerdict::Stale {
        baked: baked.to_string(),
        head: head.to_string(),
    }
}

/// Self-check #0: the tool that doctors everything else must not itself be a
/// stale binary. The classic failure is an Arrow-only checkout serving
/// through a binary still built from an older surface.
fn check_binary_freshness(warnings: &mut Vec<String>, evidence: &mut Vec<EvidenceItem>) {
    let host_commit = std::env::var("LEIO_DOCTOR_HOST_COMMIT")
        .unwrap_or_else(|_| leio_code::BUILD_COMMIT.to_owned());
    let baked = host_commit.as_str();
    let exe = std::env::var_os("LEIO_DOCTOR_HOST_EXE")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("leio-code"));
    let checkout = crate::update::locate_checkout().ok();
    let head = checkout.as_ref().and_then(|root| {
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                let full = String::from_utf8_lossy(&output.stdout).trim().to_string();
                full[..12.min(full.len())].to_string()
            })
    });

    match freshness_verdict(&exe, baked, checkout.as_deref(), head.as_deref()) {
        FreshnessVerdict::Current => evidence.push(EvidenceItem {
            kind: "binary_freshness".to_string(),
            path: exe.display().to_string(),
            line: None,
            detail: format!("binary built from {baked} matches the checkout HEAD"),
        }),
        FreshnessVerdict::DevBuild => evidence.push(EvidenceItem {
            kind: "binary_freshness".to_string(),
            path: exe.display().to_string(),
            line: None,
            detail: format!(
                "dev build ({baked}) running from inside the checkout; drift from HEAD is expected mid-edit"
            ),
        }),
        FreshnessVerdict::Stale { baked, head } => warnings.push(format!(
            "installed binary {} was built from {baked} but the checkout is at {head}; run `leio-code update`",
            exe.display()
        )),
        FreshnessVerdict::Unknown(reason) => warnings.push(format!(
            "binary freshness unverifiable: {reason}"
        )),
    }
}

#[cfg(test)]
mod tests {
    mod freshness {
        use super::*;

        #[test]
        fn installed_binary_from_other_commit_is_stale() {
            let verdict = freshness_verdict(
                Path::new("/usr/local/bin/leio-code"),
                "aaaabbbbcccc",
                Some(Path::new("/Users/x/leio-code")),
                Some("dddddddddddd"),
            );
            assert_eq!(
                verdict,
                FreshnessVerdict::Stale {
                    baked: "aaaabbbbcccc".to_string(),
                    head: "dddddddddddd".to_string()
                }
            );
        }

        #[test]
        fn matching_commit_is_current() {
            let verdict = freshness_verdict(
                Path::new("/Users/x/.cargo/bin/leio-code"),
                "aaaabbbbcccc",
                Some(Path::new("/Users/x/leio-code")),
                Some("aaaabbbbcccc"),
            );
            assert_eq!(verdict, FreshnessVerdict::Current);
        }

        #[test]
        fn dev_binary_inside_checkout_never_warns() {
            let checkout = Path::new("/Users/x/leio-code");
            let verdict = freshness_verdict(
                &checkout.join("target/debug/leio-code"),
                "aaaabbbbcccc",
                Some(checkout),
                Some("dddddddddddd"),
            );
            assert_eq!(verdict, FreshnessVerdict::DevBuild);
        }

        #[test]
        fn missing_metadata_is_unknown_not_stale() {
            assert_eq!(
                freshness_verdict(Path::new("/bin/leio-code"), "unknown", None, None),
                FreshnessVerdict::Unknown("binary was built without git metadata")
            );
            assert_eq!(
                freshness_verdict(Path::new("/bin/leio-code"), "aaaabbbbcccc", None, None),
                FreshnessVerdict::Unknown(
                    "leio-code checkout not locatable; set LEIO_CODE_ROOT to silence this blind spot"
                )
            );
        }
    }

    use super::*;

    #[test]
    fn claude_plugin_mcp_must_be_inline_and_plugin_root_relative() {
        let pointer = json!({ "mcpServers": "./.mcp.json" });
        let warnings = claude_plugin_mcp_launch_warnings(&pointer);
        assert!(
            warnings.iter().any(|w| w.contains("inline")),
            "{warnings:?}"
        );

        let relative = json!({
            "mcpServers": {
                "leio-code": {
                    "command": "bash",
                    "args": ["./scripts/launch-stdio-mcp.sh"]
                }
            }
        });
        let warnings = claude_plugin_mcp_launch_warnings(&relative);
        assert!(
            warnings.iter().any(|w| w.contains("CLAUDE_PLUGIN_ROOT")),
            "{warnings:?}"
        );

        let portable = json!({
            "mcpServers": {
                "leio-code": {
                    "command": "bash",
                    "args": ["${CLAUDE_PLUGIN_ROOT}/scripts/launch-stdio-mcp.sh"]
                }
            }
        });
        assert_eq!(
            claude_plugin_mcp_launch_warnings(&portable),
            Vec::<String>::new()
        );
    }

    #[test]
    fn project_mcp_json_may_stay_cwd_relative() {
        let project = json!({
            "mcpServers": {
                "leio-code": {
                    "command": "bash",
                    "args": ["./scripts/launch-stdio-mcp.sh"],
                    "cwd": "."
                }
            }
        });
        assert_eq!(project_mcp_launch_warnings(&project), Vec::<String>::new());
    }

    #[test]
    fn parse_js_const_array_reads_multiline_arrays() {
        let src = r#"
const doctorKinds = [
  "all",
  "deploy",
  "self-contract",
];
"#;
        assert_eq!(
            parse_js_const_array(src, "doctorKinds"),
            Some(vec![
                "all".to_string(),
                "deploy".to_string(),
                "self-contract".to_string(),
            ])
        );
    }

    #[test]
    fn binary_resolution_requires_installed_before_target() {
        let good = r#"
export function resolveBinaryPath({ binaryName, startDir } = {}) {
  return (
    findInstalledBinary(binaryName, options) ??
    findBinaryUpwards(startDir, binaryName, exists)
  );
}
export function isCargoTargetBinary(candidate) { return /\/target\/(release|debug)\//.test(candidate); }
"#;
        let with_helpers_first =
            format!("export function findBinaryUpwards(startDir, binaryName) {{}}\n{good}");
        assert!(installed_before_target_walk(good));
        assert!(installed_before_target_walk(&with_helpers_first));

        let reversed = r#"
export function resolveBinaryPath({ binaryName, startDir } = {}) {
  return (
    findBinaryUpwards(startDir, binaryName, exists) ??
    findInstalledBinary(binaryName, options)
  );
}
"#;
        assert!(!installed_before_target_walk(reversed));

        let stale = r#"
export function findBinaryUpwards(startDir, binaryName) { return startDir; }
export function resolveBinaryPath(startDir, binaryName) {
  return findBinaryUpwards(startDir, binaryName);
}
"#;
        assert!(!installed_before_target_walk(stale));
        assert!(installed_before_target_walk(include_str!(
            "../../../../mcp/resolve-binary.js"
        )));
    }
}
