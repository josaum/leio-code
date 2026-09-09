//! Assurant Ops production doctor.
//!
//! Guards the dedicated GPU-box + Vercel production surface for
//! `deploy/targets/assurant-ops.toml`. This is the deploy-time complement to
//! [`super::assurant_seed_wiring`]: seed wiring keeps the backend cartridge fed;
//! this doctor keeps the **frontend proxy**, **production API URL contract**, and
//! **office-parsers-rs runtime surface** aligned so `/tools` and documentoscopia
//! do not silently talk to `api.getjai.com` or run without the fast parser wheels.
//!
//! Incident class caught here (2026-07-03):
//! - Vercel `EXAMPLE_API_URL` empty → Next.js proxy/login fell back to
//!   `https://api.getjai.com`; Assurant Ops UI rendered but every authenticated
//!   tool action returned 401/invalid credentials against the wrong API.
//! - Hardcoded `Ambiente: local` chrome while production hostname was live.
//! - Missing `runtime-env.ts` wiring in auth/proxy routes.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct AssurantOpsProductionDoctor;

impl Doctor for AssurantOpsProductionDoctor {
    fn name(&self) -> &'static str {
        "assurant-ops-production"
    }

    fn description(&self) -> &'static str {
        "Verifies Assurant Ops production wiring: GPU deploy target, Vercel API URL contract, auth/proxy runtime-env usage, office-parsers-rs imports, and local production env preflight."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_assurant_ops_production(root)
    }
}

const ASSURANT_OPS_REL: &str = "assurant-ops";
const RUNTIME_ENV_REL: &str = "assurant-ops/src/lib/runtime-env.ts";
const ENV_EXAMPLE_REL: &str = "assurant-ops/.env.example";
const TARGET_REL: &str = "deploy/targets/assurant-ops.toml";
const RUNBOOK_REL: &str = "docs/deployment/assurant-ops-production.md";
const STAGE_SCRIPT_REL: &str = "deploy/scripts/stage-assurant-gpu-artifacts.sh";
const CADDY_REL: &str = "deploy/caddy/assurant-api.Caddyfile";
const API_DOCKERFILE_REL: &str = "example-api/Dockerfile";
const PROD_ENV_LOCAL_REL: &str = "assurant-ops/.env.production.local";
const PRODUCTION_API_HOST: &str = "assurant-api.getjai.com";

const PROXY_ROUTE_FILES: &[&str] = &[
    "assurant-ops/src/app/api/assurant/[...path]/route.ts",
    "assurant-ops/src/app/api/auth/login/route.ts",
    "assurant-ops/src/app/api/auth/me/route.ts",
    "assurant-ops/src/app/api/auth/refresh/route.ts",
    "assurant-ops/src/app/api/auth/upload-target/route.ts",
];

const OFFICE_PARSER_ASSURANT_IMPORTS: &[(&str, &str)] = &[
    (
        "cartridges/assurant/documentoscopia/evidence.py",
        "from pdf_fast import",
    ),
    (
        "cartridges/assurant/prediction/etl.py",
        "from xlsx_fast import",
    ),
];

const DOCKER_REQUIRED_IMPORTS: &[&str] = &[
    "pdf_fast._core",
    "docx_fast",
    "email_fast",
    "pptx_fast",
    "xlsx_fast",
    "layout_fast._core",
    "fca_fast",
    "mcts_fast_py._core",
    "align_fast_py._core",
    "flight_contracts_py",
];

pub fn doctor_assurant_ops_production(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut io = Vec::new();

    if !root.join(ASSURANT_OPS_REL).is_dir() {
        entities.push(json!({
            "doctor": "assurant-ops-production",
            "skipped": true,
            "reason": "assurant-ops frontend not present",
        }));
        return envelope(
            "assurant-ops not present; skipped".to_string(),
            0.9,
            entities,
            evidence,
            warnings,
            started,
        );
    }

    // 1. Shared runtime-env contract for server routes.
    let runtime_env_body = read_text(&root.join(RUNTIME_ENV_REL), &mut io).unwrap_or_default();
    if runtime_env_body.is_empty() {
        warnings.push(format!(
            "{RUNTIME_ENV_REL}: missing shared runtime env helper; auth/proxy routes may fall back to the wrong API host"
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_ops_runtime_env_missing".to_string(),
            path: RUNTIME_ENV_REL.to_string(),
            line: None,
            detail: "expected getExampleApiUrl()/getEnvironmentLabel() helper".to_string(),
        });
    } else {
        if !runtime_env_body.contains(PRODUCTION_API_HOST) {
            warnings.push(format!(
                "{RUNTIME_ENV_REL}: does not pin production API host `{PRODUCTION_API_HOST}`"
            ));
            evidence.push(EvidenceItem {
                kind: "assurant_ops_production_api_host_missing".to_string(),
                path: RUNTIME_ENV_REL.to_string(),
                line: find_line(&runtime_env_body, "DEFAULT_PRODUCTION_API_URL"),
                detail: "production fallback must target the Assurant GPU API".to_string(),
            });
        }
        if !runtime_env_body.contains("getExampleApiUrl") {
            warnings.push(format!(
                "{RUNTIME_ENV_REL}: missing getExampleApiUrl(); server routes cannot resolve the Assurant API base URL"
            ));
            evidence.push(EvidenceItem {
                kind: "assurant_ops_api_url_helper_missing".to_string(),
                path: RUNTIME_ENV_REL.to_string(),
                line: None,
                detail: "expected getExampleApiUrl export".to_string(),
            });
        }
    }

    // 2. Auth/proxy routes must import runtime-env, not hardcode api.getjai.com.
    for rel in PROXY_ROUTE_FILES {
        let path = root.join(rel);
        let body = read_text(&path, &mut io).unwrap_or_default();
        if body.is_empty() {
            warnings.push(format!("{rel}: missing auth/proxy route file"));
            continue;
        }
        if !body.contains("getExampleApiUrl") {
            warnings.push(format!(
                "{rel}: does not use getExampleApiUrl(); production may fall back to api.getjai.com"
            ));
            evidence.push(EvidenceItem {
                kind: "assurant_ops_proxy_runtime_env_drift".to_string(),
                path: rel.to_string(),
                line: find_line(&body, "EXAMPLE_API_URL"),
                detail: "import getExampleApiUrl from assurant-ops/src/lib/runtime-env.ts"
                    .to_string(),
            });
        }
        if body.contains("|| \"https://api.getjai.com\"")
            || body.contains("|| 'https://api.getjai.com'")
        {
            warnings.push(format!(
                "{rel}: still hardcodes api.getjai.com fallback; Assurant Ops must default to `{PRODUCTION_API_HOST}` in production"
            ));
            evidence.push(EvidenceItem {
                kind: "assurant_ops_platform_api_fallback".to_string(),
                path: rel.to_string(),
                line: find_line(&body, "api.getjai.com"),
                detail: "remove platform API fallback from Assurant Ops server routes".to_string(),
            });
        }
    }

    // 3. Committed env example must document the GPU API host.
    let env_example = read_text(&root.join(ENV_EXAMPLE_REL), &mut io).unwrap_or_default();
    if !env_example.contains(PRODUCTION_API_HOST) {
        warnings.push(format!(
            "{ENV_EXAMPLE_REL}: must document EXAMPLE_API_URL=https://{PRODUCTION_API_HOST} for production"
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_ops_env_example_drift".to_string(),
            path: ENV_EXAMPLE_REL.to_string(),
            line: find_line(&env_example, "EXAMPLE_API_URL"),
            detail: "Vercel production env must mirror .env.example".to_string(),
        });
    }
    if env_example.contains("AUTH_ALLOW_MOCK_FALLBACK=true") {
        warnings.push(format!(
            "{ENV_EXAMPLE_REL}: must not enable AUTH_ALLOW_MOCK_FALLBACK in the committed contract"
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_ops_mock_auth_enabled".to_string(),
            path: ENV_EXAMPLE_REL.to_string(),
            line: find_line(&env_example, "AUTH_ALLOW_MOCK_FALLBACK"),
            detail: "production Assurant Ops must authenticate against the GPU API only"
                .to_string(),
        });
    }

    // 4. UI must not hardcode Ambiente=local on production surfaces.
    let home_page =
        read_text(&root.join("assurant-ops/src/app/page.tsx"), &mut io).unwrap_or_default();
    if home_page.contains("label=\"Ambiente\" value=\"local\"") {
        warnings.push(
            "assurant-ops/src/app/page.tsx: hardcodes Ambiente=local; use getEnvironmentLabel()"
                .to_string(),
        );
        evidence.push(EvidenceItem {
            kind: "assurant_ops_environment_label_hardcoded".to_string(),
            path: "assurant-ops/src/app/page.tsx".to_string(),
            line: find_line(&home_page, "Ambiente"),
            detail: "console chrome must reflect production vs local runtime".to_string(),
        });
    }
    let tools_page =
        read_text(&root.join("assurant-ops/src/app/tools/page.tsx"), &mut io).unwrap_or_default();
    if !tools_page.contains("getEnvironmentLabel") {
        warnings.push(
            "assurant-ops/src/app/tools/page.tsx: missing getEnvironmentLabel(); /tools may show the wrong environment"
                .to_string(),
        );
        evidence.push(EvidenceItem {
            kind: "assurant_ops_tools_env_label_missing".to_string(),
            path: "assurant-ops/src/app/tools/page.tsx".to_string(),
            line: None,
            detail: "tools dashboard should derive Ambiente from runtime-env helper".to_string(),
        });
    }

    // 5. Deploy target + GPU runbook surfaces.
    let target_body = read_text(&root.join(TARGET_REL), &mut io).unwrap_or_default();
    if !target_body.contains("ui_domain") || !target_body.contains("assurant-ops.getjai.com") {
        warnings.push(format!(
            "{TARGET_REL}: missing ui_domain for assurant-ops.getjai.com"
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_ops_target_ui_domain_missing".to_string(),
            path: TARGET_REL.to_string(),
            line: find_line(&target_body, "ui_domain"),
            detail: "deploy target must declare the production frontend hostname".to_string(),
        });
    }
    for (rel, needle) in [
        (RUNBOOK_REL, "stage-assurant-gpu-artifacts.sh"),
        (RUNBOOK_REL, PRODUCTION_API_HOST),
        (STAGE_SCRIPT_REL, "assurant-models/prediction"),
        (CADDY_REL, PRODUCTION_API_HOST),
    ] {
        let body = read_text(&root.join(rel), &mut io).unwrap_or_default();
        if body.is_empty() || !body.contains(needle) {
            warnings.push(format!(
                "{rel}: missing `{needle}` in Assurant Ops production runbook/staging surface"
            ));
            evidence.push(EvidenceItem {
                kind: "assurant_ops_deploy_surface_drift".to_string(),
                path: rel.to_string(),
                line: find_line(&body, needle),
                detail: format!("expected `{needle}` in {rel}"),
            });
        }
    }

    // 6. Assurant cartridge hot paths must stay on office-parsers-rs wheels.
    for (rel, needle) in OFFICE_PARSER_ASSURANT_IMPORTS {
        let body = read_text(&root.join(rel), &mut io).unwrap_or_default();
        if !body.contains(needle) {
            warnings.push(format!(
                "{rel}: missing `{needle}`; Assurant runtime may bypass office-parsers-rs fast path"
            ));
            evidence.push(EvidenceItem {
                kind: "assurant_ops_office_parser_import_missing".to_string(),
                path: rel.to_string(),
                line: find_line(&body, needle.trim_start_matches("from ")),
                detail:
                    "Assurant documentoscopia/prediction must import office-parsers-rs PyO3 wheels"
                        .to_string(),
            });
        }
    }
    let dockerfile = read_text(&root.join(API_DOCKERFILE_REL), &mut io).unwrap_or_default();
    for import_module in DOCKER_REQUIRED_IMPORTS {
        if !dockerfile.contains(import_module) {
            warnings.push(format!(
                "{API_DOCKERFILE_REL}: missing Docker verify import `{import_module}`; API image may ship without office-parsers-rs wheel"
            ));
            evidence.push(EvidenceItem {
                kind: "assurant_ops_api_wheel_verify_missing".to_string(),
                path: API_DOCKERFILE_REL.to_string(),
                line: find_line(&dockerfile, import_module),
                detail:
                    "example-api Dockerfile must verify all office-parsers-rs wheels on install"
                        .to_string(),
            });
        }
    }

    // 7. Local production env preflight (deploy machine only — skips when absent).
    let prod_env_local_path = root.join(PROD_ENV_LOCAL_REL);
    if prod_env_local_path.is_file()
        && let Ok(body) = std::fs::read_to_string(&prod_env_local_path)
    {
        let api_url = parse_env_value(&body, "EXAMPLE_API_URL");
        if api_url.is_empty() {
            warnings.push(format!(
                    "{PROD_ENV_LOCAL_REL}: EXAMPLE_API_URL is empty; the next Vercel deploy will proxy Assurant Ops to the wrong API"
                ));
            evidence.push(EvidenceItem {
                kind: "assurant_ops_vercel_api_url_empty".to_string(),
                path: PROD_ENV_LOCAL_REL.to_string(),
                line: find_line(&body, "EXAMPLE_API_URL"),
                detail:
                    "set EXAMPLE_API_URL=https://assurant-api.getjai.com before production deploy"
                        .to_string(),
            });
        } else if !api_url.contains("assurant-api") {
            warnings.push(format!(
                "{PROD_ENV_LOCAL_REL}: EXAMPLE_API_URL={api_url} is not the Assurant GPU API host"
            ));
            evidence.push(EvidenceItem {
                kind: "assurant_ops_vercel_api_url_wrong_host".to_string(),
                path: PROD_ENV_LOCAL_REL.to_string(),
                line: find_line(&body, "EXAMPLE_API_URL"),
                detail: format!("expected host `{PRODUCTION_API_HOST}`"),
            });
        }
    }

    entities.push(json!({
        "doctor": "assurant-ops-production",
        "runtime_env_present": !runtime_env_body.is_empty(),
        "production_api_host": PRODUCTION_API_HOST,
        "proxy_routes_checked": PROXY_ROUTE_FILES.len(),
        "office_parser_imports_checked": OFFICE_PARSER_ASSURANT_IMPORTS.len(),
        "docker_wheel_imports_checked": DOCKER_REQUIRED_IMPORTS.len(),
        "local_production_env_audited": root.join(PROD_ENV_LOCAL_REL).is_file(),
    }));

    warnings.extend(io);

    let summary = if warnings.is_empty() {
        "Assurant Ops production wiring intact: GPU API host, runtime-env proxy, and office-parsers-rs surface aligned"
            .to_string()
    } else {
        format!(
            "Assurant Ops production wiring has {} issue(s); /tools and authenticated proxy routes may fail in production",
            warnings.len()
        )
    };

    envelope(
        summary,
        if warnings.is_empty() { 0.95 } else { 0.55 },
        entities,
        evidence,
        warnings,
        started,
    )
}

fn parse_env_value(body: &str, key: &str) -> String {
    let prefix = format!("{key}=");
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix(prefix.as_str()) else {
            continue;
        };
        return rest.trim().trim_matches('"').trim_matches('\'').to_string();
    }
    String::new()
}

#[allow(clippy::too_many_arguments)]
fn envelope(
    summary: String,
    confidence: f32,
    entities: Vec<serde_json::Value>,
    evidence: Vec<EvidenceItem>,
    warnings: Vec<String>,
    started: Instant,
) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_assurant_ops_production"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-assurant-ops-prod-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn wire_intact(root: &Path) {
        fs::create_dir_all(root.join(ASSURANT_OPS_REL)).unwrap();
        write(
            root,
            RUNTIME_ENV_REL,
            "const DEFAULT_PRODUCTION_API_URL = \"https://assurant-api.getjai.com\";\nexport function getExampleApiUrl(){return DEFAULT_PRODUCTION_API_URL;}\nexport function getEnvironmentLabel(){return \"produção\";}\n",
        );
        write(
            root,
            ENV_EXAMPLE_REL,
            "EXAMPLE_API_URL=https://assurant-api.getjai.com\nAUTH_ALLOW_MOCK_FALLBACK=false\n",
        );
        for rel in PROXY_ROUTE_FILES {
            write(
                root,
                rel,
                "import { getExampleApiUrl } from \"@/lib/runtime-env\";\nconst EXAMPLE_API_URL = getExampleApiUrl();\n",
            );
        }
        write(
            root,
            "assurant-ops/src/app/tools/page.tsx",
            "import { getEnvironmentLabel } from \"@/lib/runtime-env\";\n",
        );
        write(
            root,
            "assurant-ops/src/app/page.tsx",
            "import { getEnvironmentLabel } from \"@/lib/runtime-env\";\n",
        );
        write(
            root,
            TARGET_REL,
            "ui_domain = \"assurant-ops.getjai.com\"\nfrontend_project = \"assurant-ops\"\n",
        );
        write(
            root,
            RUNBOOK_REL,
            "EXAMPLE_API_URL=https://assurant-api.getjai.com\nstage-assurant-gpu-artifacts.sh\n",
        );
        write(
            root,
            STAGE_SCRIPT_REL,
            "/opt/example/data/assurant-models/prediction\n",
        );
        write(root, CADDY_REL, "assurant-api.getjai.com {\n}\n");
        for (rel, needle) in OFFICE_PARSER_ASSURANT_IMPORTS {
            write(root, rel, &format!("{needle} parse_xlsx\n"));
        }
        write(
            root,
            API_DOCKERFILE_REL,
            &DOCKER_REQUIRED_IMPORTS
                .iter()
                .map(|item| format!("\"{item}\""))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    #[test]
    fn intact_wiring_is_silent() {
        let root = temp_repo("intact");
        wire_intact(&root);
        let env = doctor_assurant_ops_production(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_runtime_env_is_flagged() {
        let root = temp_repo("noruntime");
        wire_intact(&root);
        fs::remove_file(root.join(RUNTIME_ENV_REL)).unwrap();
        let env = doctor_assurant_ops_production(&root);
        assert!(
            env.warnings.iter().any(|w| w.contains("runtime-env.ts")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_local_production_env_is_flagged() {
        let root = temp_repo("emptyenv");
        wire_intact(&root);
        write(&root, PROD_ENV_LOCAL_REL, "EXAMPLE_API_URL=\n");
        let env = doctor_assurant_ops_production(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("EXAMPLE_API_URL is empty")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn hardcoded_home_environment_is_flagged() {
        let root = temp_repo("homelocal");
        wire_intact(&root);
        write(
            &root,
            "assurant-ops/src/app/page.tsx",
            "<MiniMetric label=\"Ambiente\" value=\"local\" />\n",
        );
        let env = doctor_assurant_ops_production(&root);
        assert!(
            env.warnings.iter().any(|w| w.contains("page.tsx")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }
}
