use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Active,
    Released,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LockScope {
    Worktree,
    Branch,
    DeploymentTarget,
    BuildConcurrencyGroup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLease {
    pub agent_id: String,
    pub run_id: String,
    pub worktree_path: String,
    pub branch: String,
    pub owner: String,
    pub heartbeat: String,
    pub state: LeaseState,
    pub lock_scopes: Vec<LockScope>,
    pub deployment_target: Option<String>,
    pub build_concurrency_group: Option<String>,
    /// Opaque exclusive-ownership keys (a dossier section, a hypothesis, a
    /// file set). Two active leases sharing any key conflict, so a lane's
    /// scope is enforced by the store instead of being merely declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRegistry {
    pub version: u32,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub leases: Vec<AgentLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSpec {
    pub run_id: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub output_dir: String,
    pub timeout_ms: u64,
    #[serde(default = "default_grace_ms")]
    pub kill_grace_ms: u64,
    #[serde(default = "default_output_bytes")]
    pub max_output_bytes: usize,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub required_approval_token: Option<String>,
    pub approval_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Passed,
    Failed,
    TimedOut,
    Canceled,
    InfraError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResult {
    pub version: u32,
    pub run_id: String,
    pub status: RunStatus,
    pub exit_code: Option<i32>,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub duration_ms: u64,
    pub stdout_path: String,
    pub stderr_path: String,
    pub events_arrow_path: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub error: Option<String>,
}

/// Supervised Agent Client Protocol (ACP) session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionSpec {
    pub session_id: String,
    pub cwd: String,
    pub output_dir: String,
    /// Agent server argv, e.g. `["grok", "agent", "stdio"]`.
    pub agent_argv: Vec<String>,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Terminate the session after this much ACP silence. Zero disables.
    #[serde(default = "default_idle_timeout_ms")]
    pub idle_timeout_ms: u64,
    /// Maximum ACP frame size in bytes; larger frames fail the session.
    #[serde(default = "default_max_frame_bytes")]
    pub max_frame_bytes: usize,
    /// Require `leio-code status --repo <cwd>` to pass before spawning.
    #[serde(default = "default_true")]
    pub preflight_leio: bool,
    /// Override the preflight command argv. Defaults to
    /// `["leio-code", "status", "--json", "--repo", <cwd>]`.
    #[serde(default)]
    pub preflight_cmd: Vec<String>,
    #[serde(default)]
    pub required_approval_token: Option<String>,
    #[serde(default)]
    pub approval_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionResult {
    pub version: u32,
    pub session_id: String,
    pub status: RunStatus,
    pub exit_code: Option<i32>,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub duration_ms: u64,
    pub events_arrow_path: String,
    pub error: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_idle_timeout_ms() -> u64 {
    600_000
}

fn default_max_frame_bytes() -> usize {
    16 * 1024 * 1024
}

fn default_grace_ms() -> u64 {
    2_000
}

fn default_output_bytes() -> usize {
    4 * 1024 * 1024
}
