//! Reports which `find` / `explain` / `graph` / `doctor` / `export` verbs are
//! meaningful for the current repository profile and indexed facets.
//!
//! Two consumers care about this:
//! - the `capabilities` CLI subcommand (so an agent can route up-front
//!   without trying verbs that will return empty);
//! - the MCP/Apps-SDK wrappers, which mirror the same surface as UI hints
//!   (`workspace_capability_hints`, `action_palette`). The inline test
//!   `mirrored_js_surfaces_keep_doctor_and_export_contracts_in_sync` enforces
//!   that the Rust list and the JS mirror agree.
//!
//! Profile-specific facets (deploy targets, cartridges, secret sets) are
//! gated on what the index actually contains, so a generic repo doesn't get
//! `find deploy-target` advertised.

use std::path::Path;

use crate::config::repo_profile;
use crate::doctors::doctor_names_for_profile;
use crate::model::{RepoIndex, WorkspaceCapabilitySummary, WorkspaceFacetSummary};

const BASE_FIND_KINDS: &[&str] = &[
    "symbol",
    "env-var",
    "redis-key",
    "api-route",
    "docker-service",
];
const BASE_EXPLAIN_KINDS: &[&str] = &["env-var", "redis-key"];
pub const GRAPH_KINDS: &[&str] = &[
    "callers-of",
    "callees-of",
    "callsites-of",
    "symbols-in",
    "imports-in",
    "importers-of",
    "resolved-imports-in",
    "resolved-importers-of",
    "dead-code",
];

const EXPORT_KINDS: &[&str] = &["formal-context", "code-graph", "arrow-nodes", "hypergraph"];

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

pub fn workspace_capabilities(index: &RepoIndex, root: &Path) -> WorkspaceCapabilitySummary {
    workspace_capabilities_from_facets(&index.workspace_facets(), root)
}

pub fn workspace_capabilities_from_facets(
    workspace_facets: &WorkspaceFacetSummary,
    root: &Path,
) -> WorkspaceCapabilitySummary {
    let workspace_profile = repo_profile(root);

    let mut find_kinds = strings(BASE_FIND_KINDS);
    if workspace_facets.has_deploy_topology {
        find_kinds.push("deploy-target".to_string());
    }
    if workspace_facets.has_cartridges {
        find_kinds.push("cartridge".to_string());
    }

    let mut explain_kinds = strings(BASE_EXPLAIN_KINDS);
    if workspace_facets.has_deploy_topology {
        explain_kinds.push("deploy-target".to_string());
    }
    if workspace_facets.has_cartridges {
        explain_kinds.push("cartridge".to_string());
    }

    let doctor_kinds = doctor_names_for_profile(&workspace_profile)
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();

    let mut notes = Vec::new();
    if !workspace_facets.has_deploy_topology {
        notes.push("repository does not model deploy targets".to_string());
    }
    if !workspace_facets.has_cartridges {
        notes.push("repository does not model cartridges".to_string());
    }
    if !workspace_facets.has_profile_envs {
        notes.push("repository does not publish profile env sets".to_string());
    }
    if !workspace_facets.has_secret_sets {
        notes.push("repository does not publish secret-set topology".to_string());
    }
    if doctor_kinds.is_empty() {
        notes.push(format!(
            "no workspace-specific doctors are configured for workspace profile `{workspace_profile}`"
        ));
    }

    WorkspaceCapabilitySummary {
        workspace_profile,
        workspace_facets: workspace_facets.clone(),
        find_kinds,
        explain_kinds,
        doctor_kinds,
        graph_kinds: strings(GRAPH_KINDS),
        export_kinds: strings(EXPORT_KINDS),
        notes,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::model::{
        DeclaredVar, DeployTargetRecord, FileRecord, ProfileRecord, SecretSetRecord, SourceLanguage,
    };

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio-code-capabilities-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create temp repo");
        root
    }

    fn empty_index(root: &Path) -> RepoIndex {
        RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: Vec::new(),
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    #[test]
    fn generic_profile_capabilities_hide_workspace_specific_facets() {
        let root = temp_root("generic");
        let capabilities = workspace_capabilities(&empty_index(&root), &root);

        assert_eq!(capabilities.workspace_profile, "generic");
        assert_eq!(
            capabilities.find_kinds,
            vec![
                "symbol",
                "env-var",
                "redis-key",
                "api-route",
                "docker-service",
            ]
        );
        assert_eq!(capabilities.explain_kinds, vec!["env-var", "redis-key"]);
        // The generic doctor pack: every entry either gates itself on its
        // inputs existing (slop → spine.json, import-boundary → config rules,
        // orphan-files → config surfaces, env-contract → declaration sources)
        // or no-ops cleanly when the repo lacks the relevant facets
        // (redis-key-hygiene, rust-toolchain-pin-coherence). Order follows
        // the doctor registry.
        assert_eq!(
            capabilities.doctor_kinds,
            vec![
                "skill-contract",
                "orphan-files",
                "redis-key-hygiene",
                "rust-toolchain-pin-coherence",
                "slop",
                "env-contract",
                "import-boundary",
                "repo-hygiene",
                "codex-orchestration",
                "leio-release-coherence",
            ]
        );
        assert!(
            capabilities
                .notes
                .iter()
                .any(|note| note.contains("does not model deploy targets"))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn example_profile_capabilities_surface_optional_facets_and_doctors() {
        let root = temp_root("example");
        fs::write(
            root.join(".leio-code").join("config.toml"),
            "workspace_profile = \"example\"\n",
        )
        .expect("write config");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "src/main.rs".to_string(),
                language: SourceLanguage::Rust,
                bytes: 42,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: vec![DeployTargetRecord {
                name: "backend".to_string(),
                path: "deploy/backend.toml".to_string(),
                profile: Some("default".to_string()),
                readiness_target: None,
                deploy_class: None,
                topology: Some("service".to_string()),
                ui_role: None,
                ui_path: None,
                frontend_project: None,
                backend_profile: Some("backend".to_string()),
                secret_set: Some("backend".to_string()),
                health_checks: Vec::new(),
                smoke_suite: None,
                rollback_command: None,
                cartridges: vec!["fitness_exclusive".to_string()],
                required_integrations: Vec::new(),
                promotion_policy: None,
            }],
            profiles: vec![ProfileRecord {
                name: "backend.env".to_string(),
                path: "profiles/backend.env".to_string(),
                vars: vec![DeclaredVar {
                    name: "JWT_SECRET".to_string(),
                    value_preview: None,
                    raw_value: None,
                }],
            }],
            secret_sets: vec![SecretSetRecord {
                name: "backend.env.example".to_string(),
                path: "profiles/backend.env.example".to_string(),
                vars: vec![DeclaredVar {
                    name: "JWT_SECRET".to_string(),
                    value_preview: None,
                    raw_value: None,
                }],
            }],
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let capabilities = workspace_capabilities(&index, &root);

        assert_eq!(capabilities.workspace_profile, "example");
        assert!(
            capabilities
                .find_kinds
                .contains(&"deploy-target".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"sisfron-ooda-runtime".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"sisfron-simulation-durability".to_string())
        );
        assert!(capabilities.find_kinds.contains(&"cartridge".to_string()));
        assert!(capabilities.doctor_kinds.contains(&"deploy".to_string()));
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"cartridge-boundary".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"egress-compliance".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"inference-contracts".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"redis-key-hygiene".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"typescript-config-hygiene".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"induced-invariants".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"py-rust-boundary".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"artifact-reuse".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"codex-orchestration".to_string())
        );
        assert!(
            capabilities
                .doctor_kinds
                .contains(&"leio-release-coherence".to_string())
        );
        assert!(capabilities.notes.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn platform_runtime_trust_boundary_is_a_derived_example_capability() {
        let root = temp_root("platform-runtime-trust-boundary");
        fs::write(
            root.join(".leio-code").join("config.toml"),
            "workspace_profile = \"example\"\n",
        )
        .expect("write config");

        let capabilities = workspace_capabilities(&empty_index(&root), &root);

        assert!(
            capabilities
                .doctor_kinds
                .contains(&"platform-runtime-trust-boundary".to_string()),
            "expected derived capabilities to include platform-runtime-trust-boundary; got {:?}",
            capabilities.doctor_kinds
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn leio_code_profile_capabilities_surface_self_contract_doctor() {
        let root = temp_root("leio-code");
        fs::write(
            root.join(".leio-code").join("config.toml"),
            "workspace_profile = \"leio-code\"\n",
        )
        .expect("write config");

        let capabilities = workspace_capabilities(&empty_index(&root), &root);

        assert_eq!(capabilities.workspace_profile, "leio-code");
        // slop and import-boundary are registered under every profile (each
        // gated on its inputs at run time), so they show up alongside the
        // leio-code self-contract doctor. vendored-crate-provenance is here
        // because leio-code is the tree that holds the first-party vendored
        // copies it governs.
        assert_eq!(
            capabilities.doctor_kinds,
            vec![
                "self-contract",
                // Runs on leio-code itself since 2026-09-21: this repository
                // shipped a floating `channel = "stable"` past its own pin
                // doctor because the doctor was never registered for it.
                "rust-toolchain-pin-coherence",
                "slop",
                "import-boundary",
                "repo-hygiene",
                "codex-orchestration",
                "leio-release-coherence",
                "vendored-crate-provenance",
            ]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn js_surfaces_derive_kinds_from_the_binary_catalog() {
        let root = temp_root("example-js-surfaces");
        fs::write(
            root.join(".leio-code").join("config.toml"),
            "workspace_profile = \"example\"\n",
        )
        .expect("write config");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "src/main.rs".to_string(),
                language: SourceLanguage::Rust,
                bytes: 42,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: vec![DeployTargetRecord {
                name: "backend".to_string(),
                path: "deploy/backend.toml".to_string(),
                profile: Some("default".to_string()),
                readiness_target: None,
                deploy_class: None,
                topology: Some("service".to_string()),
                ui_role: None,
                ui_path: None,
                frontend_project: None,
                backend_profile: Some("backend".to_string()),
                secret_set: Some("backend".to_string()),
                health_checks: Vec::new(),
                smoke_suite: None,
                rollback_command: None,
                cartridges: vec!["fitness_exclusive".to_string()],
                required_integrations: Vec::new(),
                promotion_policy: None,
            }],
            profiles: vec![ProfileRecord {
                name: "backend.env".to_string(),
                path: "profiles/backend.env".to_string(),
                vars: vec![DeclaredVar {
                    name: "JWT_SECRET".to_string(),
                    value_preview: None,
                    raw_value: None,
                }],
            }],
            secret_sets: vec![SecretSetRecord {
                name: "backend.env.example".to_string(),
                path: "profiles/backend.env.example".to_string(),
                vars: vec![DeclaredVar {
                    name: "JWT_SECRET".to_string(),
                    value_preview: None,
                    raw_value: None,
                }],
            }],
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let _capabilities = workspace_capabilities(&index, &root);
        let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mcp_script =
            fs::read_to_string(manifest_root.join("mcp/index.js")).expect("read mcp/index.js");
        let apps_script = fs::read_to_string(manifest_root.join("apps-sdk/server.js"))
            .expect("read apps-sdk/server.js");

        // The JS surfaces must derive every kind family from the binary's
        // `capabilities --catalog` — literal mirror arrays are the drift
        // surface this contract exists to prevent.
        for (label, script) in [
            ("mcp/index.js", &mcp_script),
            ("apps-sdk/server.js", &apps_script),
        ] {
            assert!(
                script.contains("capabilities\", \"--catalog\", \"--json\""),
                "{label} must load the binary kind catalog"
            );
        }
        for literal in [
            "const FIND_KINDS = [",
            "const DOCTOR_KINDS = [",
            "const EXPORT_KINDS = [",
            "const doctorKinds = [",
        ] {
            assert!(
                !mcp_script.contains(literal) && !apps_script.contains(literal),
                "literal mirror `{literal}` re-introduced a kind copy"
            );
        }

        let _ = fs::remove_dir_all(root);
    }
}
