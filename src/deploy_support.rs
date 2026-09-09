//! Shared helpers for parsing deploy commands and matching deploy targets.
//!
//! Extracted out of [`crate::query`] so the deploy/explain envelopes stay
//! focused on shape rather than command-string parsing. Used by both
//! `explain deploy-target` and the deploy/route doctors:
//!
//! - [`command_references_existing_path`] — does a CLI command actually point
//!   at a runnable file under the deploy root?
//! - [`extract_smoke_target`] / [`extract_rollback_target`] — extract the
//!   target identifier from a `compose_smoke.sh` invocation or a `--target`
//!   flag, after standard shell-quote trimming.
//! - [`readiness_lineage_matches`] — two deploy targets share a profile +
//!   secret-set lineage (used to pair primary and readiness targets).

use std::fs;
use std::path::{Path, PathBuf};

use crate::model::DeployTargetRecord;

pub fn command_references_existing_path(command: &str, deploy_root: &Path) -> bool {
    let parts = command.split_whitespace().collect::<Vec<_>>();
    parts
        .iter()
        .enumerate()
        .filter(|(_, part)| part.contains('/') || part.ends_with(".sh") || part.ends_with(".py"))
        .any(|(idx, part)| {
            let normalized = part.trim_matches(|ch| ch == '"' || ch == '\'');
            let direct = deploy_root.join(normalized.trim_start_matches("./"));
            let resolved = if direct.exists() {
                direct
            } else {
                PathBuf::from(normalized)
            };
            if !resolved.exists() {
                return false;
            }

            is_runnable_command_path(&resolved, &parts, idx)
        })
}

pub fn extract_smoke_target(command: &str) -> Option<String> {
    let parts = command.split_whitespace().collect::<Vec<_>>();
    let idx = parts.iter().position(|part| {
        trim_shell_token(part)
            .trim_start_matches("./")
            .ends_with("compose_smoke.sh")
    })?;
    parts
        .get(idx + 1)
        .map(|value| trim_shell_token(value))
        .filter(|value| !value.is_empty() && !value.starts_with('-'))
        .map(str::to_string)
}

pub fn extract_rollback_target(command: &str) -> Option<String> {
    let parts = command.split_whitespace().collect::<Vec<_>>();
    if parts.array_windows::<2>().any(|[script, action]| {
        trim_shell_token(script)
            .trim_start_matches("./")
            .ends_with("health-audit-aws.sh")
            && trim_shell_token(action) == "rollback"
    }) {
        return Some("health_audit".to_string());
    }
    parts
        .array_windows::<2>()
        .find_map(|[flag, value]| {
            (trim_shell_token(flag) == "--target").then(|| trim_shell_token(value))
        })
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn readiness_lineage_matches(
    target: &DeployTargetRecord,
    readiness_target: &DeployTargetRecord,
) -> bool {
    target.backend_profile == readiness_target.backend_profile
        && target.secret_set == readiness_target.secret_set
}

fn is_runnable_command_path(path: &Path, parts: &[&str], path_index: usize) -> bool {
    let via_interpreter = parts
        .get(path_index.saturating_sub(1))
        .map(|value| trim_shell_token(value))
        .is_some_and(|value| matches!(value, "bash" | "sh" | "python" | "python3" | "python3.12"))
        || parts
            .get(path_index.saturating_sub(2))
            .zip(parts.get(path_index.saturating_sub(1)))
            .map(|(left, right)| (trim_shell_token(left), trim_shell_token(right)))
            .is_some_and(|(left, right)| {
                matches!(left, "uv" | "uvx") && matches!(right, "run" | "tool")
            });

    via_interpreter || is_executable(path)
}

fn trim_shell_token(value: &str) -> &str {
    value.trim_matches(|ch| ch == '"' || ch == '\'')
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{command_references_existing_path, readiness_lineage_matches};
    use crate::model::DeployTargetRecord;

    fn temp_dir(label: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "leio-code-{label}-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        dir
    }

    #[test]
    fn resolves_relative_script_under_deploy_root() {
        let root = temp_dir("deploy-support-relative");
        fs::create_dir_all(root.join("scripts")).expect("create temp deploy scripts dir");
        let script_path = root.join("scripts/check.sh");
        fs::write(&script_path, "#!/usr/bin/env bash\n").expect("write script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script_path)
                .expect("stat script")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script_path, permissions).expect("chmod script");
        }

        assert!(command_references_existing_path(
            "./scripts/check.sh --target health_audit",
            &root,
        ));

        fs::remove_dir_all(root).expect("cleanup temp deploy root");
    }

    #[test]
    fn ignores_commands_without_resolvable_paths() {
        let root = temp_dir("deploy-support-missing");
        fs::create_dir_all(&root).expect("create temp deploy root");

        assert!(!command_references_existing_path(
            "bash ./scripts/missing.sh --target vigoros",
            &root,
        ));

        fs::remove_dir_all(root).expect("cleanup temp deploy root");
    }

    #[test]
    fn rejects_direct_non_executable_script_paths() {
        let root = temp_dir("deploy-support-non-exec");
        fs::create_dir_all(root.join("scripts")).expect("create temp deploy scripts dir");
        let script_path = root.join("scripts/check.sh");
        fs::write(&script_path, "#!/usr/bin/env bash\n").expect("write script");

        assert!(!command_references_existing_path(
            "./scripts/check.sh --target health_audit",
            &root,
        ));

        fs::remove_dir_all(root).expect("cleanup temp deploy root");
    }

    #[test]
    fn allows_non_executable_script_when_invoked_via_bash() {
        let root = temp_dir("deploy-support-bash");
        fs::create_dir_all(root.join("scripts")).expect("create temp deploy scripts dir");
        let script_path = root.join("scripts/check.sh");
        fs::write(&script_path, "#!/usr/bin/env bash\n").expect("write script");

        assert!(command_references_existing_path(
            "bash ./scripts/check.sh --target health_audit",
            &root,
        ));

        fs::remove_dir_all(root).expect("cleanup temp deploy root");
    }

    #[test]
    fn extracts_smoke_and_rollback_targets() {
        assert_eq!(
            super::extract_smoke_target("./scripts/compose_smoke.sh health_audit"),
            Some("health_audit".to_string())
        );
        assert_eq!(
            super::extract_rollback_target("./scripts/pull-and-restart.sh --target example-ops"),
            Some("example-ops".to_string())
        );
        assert_eq!(
            super::extract_rollback_target(
                "./scripts/health-audit-aws.sh rollback ${HEALTH_AUDIT_AWS_SSH_ALIAS}",
            ),
            Some("health_audit".to_string())
        );
    }

    #[test]
    fn readiness_lineage_requires_same_backend_profile_and_secret_set() {
        let target = DeployTargetRecord {
            name: "sentinel".to_string(),
            path: "deploy/targets/sentinel.toml".to_string(),
            profile: Some("health_audit".to_string()),
            readiness_target: Some("health_audit".to_string()),
            deploy_class: None,
            topology: None,
            ui_role: None,
            ui_path: None,
            frontend_project: None,
            backend_profile: Some("health_audit".to_string()),
            secret_set: Some("hospital_audit".to_string()),
            health_checks: Vec::new(),
            smoke_suite: None,
            rollback_command: None,
            cartridges: Vec::new(),
            required_integrations: Vec::new(),
            promotion_policy: None,
        };
        let readiness_target = DeployTargetRecord {
            name: "health_audit".to_string(),
            path: "deploy/targets/health_audit.toml".to_string(),
            profile: Some("health_audit".to_string()),
            readiness_target: None,
            deploy_class: None,
            topology: None,
            ui_role: None,
            ui_path: None,
            frontend_project: None,
            backend_profile: Some("health_audit".to_string()),
            secret_set: Some("hospital_audit".to_string()),
            health_checks: Vec::new(),
            smoke_suite: None,
            rollback_command: None,
            cartridges: Vec::new(),
            required_integrations: Vec::new(),
            promotion_policy: None,
        };
        let drifted_secret_set = DeployTargetRecord {
            secret_set: Some("other".to_string()),
            ..readiness_target.clone()
        };

        assert!(readiness_lineage_matches(&target, &readiness_target));
        assert!(!readiness_lineage_matches(&target, &drifted_secret_set));
    }
}
