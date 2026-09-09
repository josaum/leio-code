use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::jsonc::parse_jsonc;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct TypescriptConfigHygieneDoctor;

impl Doctor for TypescriptConfigHygieneDoctor {
    fn name(&self) -> &'static str {
        "typescript-config-hygiene"
    }

    fn description(&self) -> &'static str {
        "Checks shared TypeScript base config anchors, repo-local editor settings, and obvious tsconfig contradictions."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_typescript_config_hygiene(index, root)
    }
}

pub fn doctor_typescript_config_hygiene(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    for base in expected_base_files() {
        let path = root.join(base);
        if !path.exists() {
            warnings.push(format!("missing shared TypeScript base config: {base}"));
            continue;
        }

        evidence.push(EvidenceItem {
            kind: "config".to_string(),
            path: base.to_string(),
            line: Some(1),
            detail: "shared TypeScript base config exists".to_string(),
        });
    }

    let gitignore_path = root.join(".gitignore");
    let gitignore_src = read_text(&gitignore_path, &mut warnings);
    if let Some(src) = gitignore_src.as_deref() {
        for (needle, detail) in [
            (
                ".vscode/*",
                "root gitignore keeps most workspace editor files untracked",
            ),
            (
                "!.vscode/settings.json",
                "shared repo-local VS Code settings remain committable",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "config".to_string(),
                    path: ".gitignore".to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "root gitignore missing shared editor settings invariant: {needle}"
                ));
            }
        }
    }

    let vscode_settings_path = root.join(".vscode/settings.json");
    let vscode_settings_src = read_text(&vscode_settings_path, &mut warnings);
    if let Some(src) = vscode_settings_src.as_deref() {
        for (needle, detail) in [
            (
                "\"npm.packageManager\": \"pnpm\"",
                "workspace editor defaults align package-manager operations with pnpm",
            ),
            (
                "\"javascript.updateImportsOnFileMove.enabled\": \"always\"",
                "JavaScript import rewrites stay enabled during file moves",
            ),
            (
                "\"typescript.updateImportsOnFileMove.enabled\": \"always\"",
                "TypeScript import rewrites stay enabled during file moves",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "config".to_string(),
                    path: ".vscode/settings.json".to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    ".vscode/settings.json missing shared TypeScript workspace setting: {needle}"
                ));
            }
        }
    }

    for contract in expected_extends() {
        let path = root.join(contract.config_path);
        let src = read_text(&path, &mut warnings);
        if let Some(src) = src.as_deref() {
            match base_anchor_evidence(root, contract, src) {
                Ok((line, detail)) => {
                    evidence.push(EvidenceItem {
                        kind: "config".to_string(),
                        path: contract.config_path.to_string(),
                        line: Some(line),
                        detail,
                    });
                }
                Err(warning) => warnings.push(warning),
            }
        }
    }

    for file in index
        .files
        .iter()
        .filter(|file| file.path.ends_with(".json") && file.path.contains("tsconfig"))
    {
        let path = root.join(&file.path);
        let src = read_text(&path, &mut warnings);
        let Some(src) = src.as_deref() else {
            continue;
        };
        match parse_jsonc(src) {
            Ok(json) => {
                let compiler_options = json
                    .get("compilerOptions")
                    .and_then(|value| value.as_object());
                let declaration = compiler_options
                    .and_then(|options| options.get("declaration"))
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let no_emit = compiler_options
                    .and_then(|options| options.get("noEmit"))
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);

                if declaration && no_emit {
                    warnings.push(format!(
                        "{} sets both `declaration: true` and `noEmit: true`; declaration emit is disabled by construction",
                        file.path
                    ));
                }
            }
            Err(err) => warnings.push(format!(
                "{} is no longer valid tsconfig-style JSONC: {}",
                file.path, err
            )),
        }
    }

    entities.push(json!({
        "doctor": "typescript-config-hygiene",
        "shared_base_count": expected_base_files().len(),
        "anchored_config_count": expected_extends().len(),
        "warning_count": warnings.len(),
        "evidence_count": evidence.len(),
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_typescript_config_hygiene"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "shared TypeScript config anchors and editor settings are intact, and no tsconfig contradictions were found".to_string()
        } else {
            format!(
                "TypeScript config hygiene found {} warning(s) across shared bases, editor settings, or tsconfig contradictions",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.96 } else { 0.72 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn expected_base_files() -> &'static [&'static str] {
    &[
        "tsconfig.base.strict.json",
        "tsconfig.base.bundler-app.json",
        "tsconfig.base.react-bundler.json",
        "tsconfig.base.react-es2020.json",
        "tsconfig.base.react-es2021.json",
        "tsconfig.base.react-es2022.json",
        "tsconfig.base.react-preserve.json",
        "tsconfig.base.next.json",
        "tsconfig.base.next-preserve.json",
        "tsconfig.base.next-es2022.json",
        "tsconfig.base.bundler-tooling.json",
        "tsconfig.base.bundler-tooling-composite.json",
        "tsconfig.base.node-lib.json",
        "tsconfig.base.node-cjs.json",
        "tsconfig.base.bundler-package.json",
        "tsconfig.base.bundler-package-decls.json",
    ]
}

#[derive(Debug, Clone, Copy)]
struct TsconfigBaseContract {
    config_path: &'static str,
    expected_extends: &'static str,
    allow_local_base: bool,
    allow_inline_base: bool,
}

const fn shared_base(
    config_path: &'static str,
    expected_extends: &'static str,
) -> TsconfigBaseContract {
    TsconfigBaseContract {
        config_path,
        expected_extends,
        allow_local_base: false,
        allow_inline_base: false,
    }
}

const fn self_contained_base(
    config_path: &'static str,
    expected_extends: &'static str,
) -> TsconfigBaseContract {
    TsconfigBaseContract {
        config_path,
        expected_extends,
        allow_local_base: true,
        allow_inline_base: false,
    }
}

const fn self_contained_inline_base(
    config_path: &'static str,
    expected_extends: &'static str,
) -> TsconfigBaseContract {
    TsconfigBaseContract {
        config_path,
        expected_extends,
        allow_local_base: true,
        allow_inline_base: true,
    }
}

static EXPECTED_EXTENDS: &[TsconfigBaseContract] = &[
    shared_base("analyst-hub/tsconfig.json", "../tsconfig.base.next.json"),
    self_contained_base("assurant-ops/tsconfig.json", "../tsconfig.base.next.json"),
    shared_base(
        "chatfacil/tsconfig.json",
        "../tsconfig.base.next-preserve.json",
    ),
    shared_base(
        "example-ops/tsconfig.json",
        "../tsconfig.base.next-es2022.json",
    ),
    shared_base(
        "health-audit-console/tsconfig.json",
        "../tsconfig.base.next.json",
    ),
    self_contained_inline_base("jai-pay/tsconfig.json", "../tsconfig.base.next.json"),
    shared_base(
        "leio-landing/tsconfig.json",
        "../tsconfig.base.next-preserve.json",
    ),
    shared_base(
        "example-align/web/tsconfig.json",
        "../../tsconfig.base.next-es2022.json",
    ),
    shared_base(
        "example-api/sdks/typescript/tsconfig.json",
        "../../../tsconfig.base.node-lib.json",
    ),
    shared_base(
        "example-extractor/tsconfig.json",
        "../tsconfig.base.react-es2021.json",
    ),
    shared_base(
        "example-gateway/gateway-operator/tsconfig.json",
        "../../tsconfig.base.node-lib.json",
    ),
    shared_base(
        "example-gateway/crm_mcp/tsconfig.json",
        "../../tsconfig.base.bundler-app.json",
    ),
    shared_base(
        "example-gateway/plugins/multi-llm/mcp/tsconfig.json",
        "../../../../tsconfig.base.bundler-package-decls.json",
    ),
    shared_base("example-hud/tsconfig.json", "../tsconfig.base.next.json"),
    shared_base(
        "example-mcp-bridge/tsconfig.json",
        "../tsconfig.base.bundler-package.json",
    ),
    self_contained_base("example-scc/tsconfig.json", "./tsconfig.base.next.json"),
    shared_base(
        "example-platform/example-chrome-extension/tsconfig.json",
        "../../tsconfig.base.react-es2020.json",
    ),
    shared_base(
        "example-platform/example-desktop/tsconfig.json",
        "../../tsconfig.base.react-es2020.json",
    ),
    shared_base(
        "example-platform/example-desktop/tsconfig.node.json",
        "../../tsconfig.base.bundler-tooling-composite.json",
    ),
    shared_base(
        "example-platform/example-office-addins/tsconfig.json",
        "../../tsconfig.base.react-es2020.json",
    ),
    shared_base(
        "example-platform/example-teams/tsconfig.json",
        "../../tsconfig.base.react-es2020.json",
    ),
    shared_base(
        "example-platform/example-teams/bot/tsconfig.json",
        "../../../tsconfig.base.node-cjs.json",
    ),
    shared_base(
        "packages/trpc/tsconfig.json",
        "../../tsconfig.base.react-es2022.json",
    ),
    shared_base(
        "vigoros-mcp/tsconfig.json",
        "../tsconfig.base.node-lib.json",
    ),
    shared_base(
        "vigoros/app/tsconfig.json",
        "../../tsconfig.base.strict.json",
    ),
    shared_base(
        "vigoros/app/tsconfig.app.json",
        "../../tsconfig.base.react-es2022.json",
    ),
    shared_base(
        "vigoros/app/tsconfig.node.json",
        "../../tsconfig.base.bundler-tooling.json",
    ),
];

fn expected_extends() -> &'static [TsconfigBaseContract] {
    EXPECTED_EXTENDS
}

fn base_anchor_evidence(
    root: &Path,
    contract: &TsconfigBaseContract,
    src: &str,
) -> Result<(usize, String), String> {
    let shared_needle = format!("\"extends\": \"{}\"", contract.expected_extends);
    if let Some(line) = find_line(src, &shared_needle) {
        return Ok((
            line,
            format!(
                "config stays anchored to shared base `{}`",
                contract.expected_extends
            ),
        ));
    }

    let local_base = Path::new(contract.expected_extends)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(contract.expected_extends);
    let local_extends = format!("./{local_base}");
    let local_needle = format!("\"extends\": \"{local_extends}\"");
    if contract.allow_local_base
        && find_line(src, &local_needle).is_some()
        && local_base_exists(root, contract.config_path, local_base)
    {
        return Ok((
            find_line(src, &local_needle).unwrap_or(1),
            format!(
                "config uses checked-in self-contained base `{local_extends}` for isolated Vercel/project builds"
            ),
        ));
    }

    if contract.allow_inline_base
        && !src.contains("\"extends\"")
        && inline_next_base_matches(src, contract.expected_extends)
    {
        return Ok((
            find_line(src, "\"compilerOptions\"").unwrap_or(1),
            format!(
                "config carries a self-contained inline base equivalent to `{}` for isolated Vercel/project builds",
                contract.expected_extends
            ),
        ));
    }

    Err(format!(
        "{} no longer extends the expected shared base `{}`",
        contract.config_path, contract.expected_extends
    ))
}

fn local_base_exists(root: &Path, config_path: &str, local_base: &str) -> bool {
    let config_dir = Path::new(config_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    root.join(config_dir).join(local_base).exists()
}

fn inline_next_base_matches(src: &str, expected_extends: &str) -> bool {
    let Ok(json) = parse_jsonc(src) else {
        return false;
    };
    let Some(options) = json
        .get("compilerOptions")
        .and_then(|value| value.as_object())
    else {
        return false;
    };

    let expected_target = if expected_extends.contains("es2022") {
        "ES2022"
    } else {
        "ES2017"
    };

    string_option(options, "target") == Some(expected_target)
        && string_option(options, "module") == Some("ESNext")
        && string_option(options, "moduleResolution") == Some("bundler")
        && bool_option(options, "allowJs") == Some(true)
        && bool_option(options, "incremental") == Some(true)
        && bool_option(options, "noEmit") == Some(true)
        && bool_option(options, "esModuleInterop") == Some(true)
        && bool_option(options, "resolveJsonModule") == Some(true)
        && bool_option(options, "isolatedModules") == Some(true)
        && bool_option(options, "strict") == Some(true)
        && bool_option(options, "skipLibCheck") == Some(true)
        && bool_option(options, "forceConsistentCasingInFileNames") == Some(true)
        && string_option(options, "jsx") == Some("react-jsx")
        && array_contains_all(options, "lib", &["dom", "dom.iterable", "esnext"])
        && next_plugin_present(options)
}

fn string_option<'a>(
    options: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a str> {
    options.get(key).and_then(|value| value.as_str())
}

fn bool_option(options: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<bool> {
    options.get(key).and_then(|value| value.as_bool())
}

fn array_contains_all(
    options: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    expected: &[&str],
) -> bool {
    let Some(items) = options.get(key).and_then(|value| value.as_array()) else {
        return false;
    };
    expected.iter().all(|needle| {
        items
            .iter()
            .any(|item| item.as_str().is_some_and(|value| value == *needle))
    })
}

fn next_plugin_present(options: &serde_json::Map<String, serde_json::Value>) -> bool {
    let Some(plugins) = options.get("plugins").and_then(|value| value.as_array()) else {
        return false;
    };
    plugins.iter().any(|plugin| {
        plugin
            .get("name")
            .and_then(|value| value.as_str())
            .is_some_and(|name| name == "next")
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{base_anchor_evidence, self_contained_base, self_contained_inline_base};

    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-tsconfig-hygiene-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn accepts_checked_in_local_base_for_self_contained_project() {
        let dir = unique_tempdir("local-base");
        write_file(
            &dir.join("assurant-ops/tsconfig.base.next.json"),
            r#"{"compilerOptions":{"strict":true}}"#,
        );
        let src = r#"{
  "extends": "./tsconfig.base.next.json",
  "compilerOptions": {
    "paths": {
      "@/*": ["./src/*"]
    }
  }
}
"#;
        let contract =
            self_contained_base("assurant-ops/tsconfig.json", "../tsconfig.base.next.json");

        let result = base_anchor_evidence(&dir, &contract, src).expect("local base should pass");

        assert_eq!(result.0, 2);
        assert!(result.1.contains("self-contained base"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_local_base_when_the_checked_in_copy_is_missing() {
        let dir = unique_tempdir("missing-local-base");
        let src = r#"{
  "extends": "./tsconfig.base.next.json"
}
"#;
        let contract =
            self_contained_base("assurant-ops/tsconfig.json", "../tsconfig.base.next.json");

        let warning =
            base_anchor_evidence(&dir, &contract, src).expect_err("missing local base should warn");

        assert!(warning.contains("assurant-ops/tsconfig.json no longer extends"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn accepts_inline_next_base_for_legacy_self_contained_project() {
        let dir = unique_tempdir("inline-base");
        let src = r#"{
  "compilerOptions": {
    "target": "ES2017",
    "lib": ["dom", "dom.iterable", "esnext"],
    "allowJs": true,
    "jsx": "react-jsx",
    "incremental": true,
    "module": "ESNext",
    "moduleResolution": "bundler",
    "noEmit": true,
    "esModuleInterop": true,
    "resolveJsonModule": true,
    "isolatedModules": true,
    "strict": true,
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true,
    "plugins": [
      {
        "name": "next"
      }
    ],
    "baseUrl": "."
  }
}
"#;
        let contract =
            self_contained_inline_base("jai-pay/tsconfig.json", "../tsconfig.base.next.json");

        let result = base_anchor_evidence(&dir, &contract, src).expect("inline base should pass");

        assert_eq!(result.0, 2);
        assert!(result.1.contains("self-contained inline base"));
        let _ = fs::remove_dir_all(dir);
    }
}
