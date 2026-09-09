//! Strict, discovery-only boundary for repository-owned declarative doctors.
//!
//! Evaluation deliberately lives in a later unit.  This module only admits
//! regular, immediate TOML manifests below `.leio-code/doctors`, parses the
//! versioned schema, and returns a deterministic catalog plus safe diagnostics.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;
use serde_json::json;

use crate::doctors::utils;
use crate::model::{QueryEnvelope, SCHEMA_VERSION};

pub const LOCAL_DOCTOR_DIR: &str = ".leio-code/doctors";
pub const LOCAL_CHECK_ENTITY_KIND: &str = "local_doctor_check";
const MAX_GIT_STDOUT_BYTES: u64 = 16 * 1024 * 1024;

const RESERVED_NAMES: [&str; 3] = ["all", "baseline", "ci"];
const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 1024;
const MAX_REVISION_DIRECTORY_ENTRY_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalContentMode {
    RevisionTracked,
    WorkingTreeTracked,
}

#[derive(Debug, Clone)]
pub struct LocalPackBudgets {
    pub max_pack_files: usize,
    pub max_checks_per_pack: usize,
    pub max_manifest_bytes: u64,
    pub max_input_bytes_per_file: u64,
    pub max_manifest_input_bytes: u64,
    pub max_checks_per_request: usize,
    pub max_unique_input_bytes: u64,
    pub max_directory_entries: usize,
    pub max_workspace_members: usize,
    pub max_path_depth: usize,
    pub max_list_items: usize,
    pub max_scalar_bytes: usize,
    pub max_findings: usize,
    pub max_encoded_output_bytes: usize,
    /// Git object queries are separately metered because revision reads are
    /// trusted but still externally-backed work.
    pub max_git_queries: usize,
}

impl Default for LocalPackBudgets {
    fn default() -> Self {
        Self {
            max_pack_files: 128,
            max_checks_per_pack: 256,
            max_manifest_bytes: 1024 * 1024,
            max_input_bytes_per_file: 16 * 1024 * 1024,
            max_manifest_input_bytes: 8 * 1024 * 1024,
            max_checks_per_request: 1024,
            max_unique_input_bytes: 256 * 1024 * 1024,
            max_directory_entries: 20_000,
            max_workspace_members: 4096,
            max_path_depth: 32,
            max_list_items: 4096,
            max_scalar_bytes: 64 * 1024,
            max_findings: 4096,
            max_encoded_output_bytes: 8 * 1024 * 1024,
            max_git_queries: 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LocalPackRequest {
    pub root: PathBuf,
    pub content_mode: LocalContentMode,
    pub deadline: Instant,
    pub budgets: LocalPackBudgets,
}

/// Per-request trusted content boundary for local doctor evaluation.
pub struct LocalPackRequestContext {
    request: LocalPackRequest,
    revision: Option<String>,
    // The revision component makes cache identity explicit so aggregate charging
    // can be layered on later without changing what constitutes one input.
    content_cache: BTreeMap<ContentIdentity, Arc<[u8]>>,
    counters: RequestCounters,
    cancelled: bool,
    terminal: Option<&'static str>,
    terminal_reported: bool,
}

#[derive(Debug, Default)]
struct RequestCounters {
    checks: u64,
    unique_input_bytes: u64,
    findings: u64,
    encoded_output_bytes: u64,
    directory_entries: u64,
    git_queries: u64,
}

enum BudgetOperation {
    Check,
    UniqueInput(u64),
    Finding,
    Output(usize),
    DirectoryEntry,
    GitQuery,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ContentIdentity {
    relative_path: PathBuf,
    revision: Option<String>,
}

impl LocalPackRequestContext {
    pub fn new(request: LocalPackRequest) -> Self {
        let revision = (request.content_mode == LocalContentMode::RevisionTracked)
            .then(|| git_revision_commit(&request.root).ok())
            .flatten();
        Self {
            request,
            revision,
            content_cache: BTreeMap::new(),
            counters: RequestCounters::default(),
            cancelled: false,
            terminal: None,
            terminal_reported: false,
        }
    }

    fn ensure_active(&mut self) -> Result<(), CheckFailure> {
        if let Some(code) = self.terminal {
            return Err(CheckFailure::new(code, "local doctor request is terminal"));
        }
        if self.cancelled {
            self.terminal = Some("cancelled");
            return Err(CheckFailure::new(
                "cancelled",
                "local doctor evaluation was cancelled",
            ));
        }
        if Instant::now() >= self.request.deadline {
            self.terminal = Some("deadline_exceeded");
            return Err(CheckFailure::new(
                "deadline_exceeded",
                "local doctor evaluation deadline exceeded",
            ));
        }
        Ok(())
    }

    fn run_git_output(&mut self, args: &[&str]) -> Result<Vec<u8>, CheckFailure> {
        self.run_git_output_with_limit(args, MAX_GIT_STDOUT_BYTES)
    }

    fn run_git_output_with_limit(
        &mut self,
        args: &[&str],
        max_bytes: u64,
    ) -> Result<Vec<u8>, CheckFailure> {
        self.charge(BudgetOperation::GitQuery)?;
        self.ensure_active()?;
        let mut child = Command::new("git")
            .args(args)
            .current_dir(&self.request.root)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
            })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
        })?;
        let mut raw = Vec::new();
        let mut bounded = stdout.take(max_bytes.saturating_add(1));
        bounded.read_to_end(&mut raw).map_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
            CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
        })?;
        if raw.len() as u64 > max_bytes {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CheckFailure::new(
                "git_output_limit_exceeded",
                "git output exceeds the configured byte limit",
            ));
        }
        if !child.wait().map(|status| status.success()).unwrap_or(false) {
            return Err(CheckFailure::new(
                "revision_unavailable",
                "authorized revision cannot be read",
            ));
        }
        self.ensure_active()?;
        Ok(raw)
    }

    /// Cooperative cancellation seam for transports sharing this request context.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    fn charge(&mut self, operation: BudgetOperation) -> Result<(), CheckFailure> {
        if let Some(code) = self.terminal {
            return Err(CheckFailure::new(code, "local doctor request is terminal"));
        }
        if self.cancelled {
            self.terminal = Some("cancelled");
            return Err(CheckFailure::new(
                "cancelled",
                "local doctor evaluation was cancelled",
            ));
        }
        if Instant::now() >= self.request.deadline {
            self.terminal = Some("deadline_exceeded");
            return Err(CheckFailure::new(
                "deadline_exceeded",
                "local doctor evaluation deadline exceeded",
            ));
        }
        let (counter, addition, maximum, code) = match operation {
            BudgetOperation::Check => (
                &mut self.counters.checks,
                1,
                self.request.budgets.max_checks_per_request as u64,
                "check_limit_exceeded",
            ),
            BudgetOperation::UniqueInput(bytes) => (
                &mut self.counters.unique_input_bytes,
                bytes,
                self.request.budgets.max_unique_input_bytes,
                "unique_input_limit_exceeded",
            ),
            BudgetOperation::Finding => (
                &mut self.counters.findings,
                1,
                self.request.budgets.max_findings as u64,
                "finding_limit_exceeded",
            ),
            BudgetOperation::Output(bytes) => (
                &mut self.counters.encoded_output_bytes,
                bytes as u64,
                self.request.budgets.max_encoded_output_bytes as u64,
                "encoded_output_limit_exceeded",
            ),
            BudgetOperation::DirectoryEntry => (
                &mut self.counters.directory_entries,
                1,
                self.request.budgets.max_directory_entries as u64,
                "directory_entry_limit_exceeded",
            ),
            BudgetOperation::GitQuery => (
                &mut self.counters.git_queries,
                1,
                self.request.budgets.max_git_queries as u64,
                "git_query_limit_exceeded",
            ),
        };
        if counter.saturating_add(addition) > maximum {
            self.terminal = Some(code);
            return Err(CheckFailure::new(
                code,
                "local doctor request budget exceeded",
            ));
        }
        *counter = counter.saturating_add(addition);
        Ok(())
    }

    fn check_path_exists(&mut self, relative: &Path) -> Result<bool, CheckFailure> {
        validate_input_path(relative)?;
        deny_input_path(relative)?;
        match self.request.content_mode {
            LocalContentMode::RevisionTracked => self.revision_path_exists(relative),
            LocalContentMode::WorkingTreeTracked => self.working_tree_path_exists(relative),
        }
    }

    fn read_tracked_regular_file(
        &mut self,
        relative: &Path,
    ) -> Result<ValidatedInput, CheckFailure> {
        validate_input_path(relative)?;
        deny_input_path(relative)?;
        let identity = ContentIdentity {
            relative_path: relative.to_path_buf(),
            revision: self.revision.clone(),
        };
        if let Some(bytes) = self.content_cache.get(&identity) {
            return Ok(ValidatedInput {
                relative_path: relative.to_path_buf(),
                bytes: Arc::clone(bytes),
            });
        }
        let bytes = match self.request.content_mode {
            LocalContentMode::RevisionTracked => self.read_revision_file(relative)?,
            LocalContentMode::WorkingTreeTracked => self.read_working_tree_file(relative)?,
        };
        if bytes.len() as u64 > self.request.budgets.max_input_bytes_per_file {
            return Err(CheckFailure::new(
                "input_too_large",
                "input exceeds the configured byte limit",
            ));
        }
        let bytes: Arc<[u8]> = Arc::from(bytes);
        self.charge(BudgetOperation::UniqueInput(bytes.len() as u64))?;
        self.content_cache.insert(identity, Arc::clone(&bytes));
        Ok(ValidatedInput {
            relative_path: relative.to_path_buf(),
            bytes,
        })
    }

    fn working_tree_path_exists(&self, relative: &Path) -> Result<bool, CheckFailure> {
        let absolute = self.request.root.join(relative);
        let metadata = match open_existing_no_follow(&self.request.root, relative) {
            Ok(None) => return Ok(false),
            Ok(Some(metadata)) => metadata,
            Err(_) => {
                return Err(CheckFailure::new(
                    "ineligible_input",
                    "input cannot be inspected safely",
                ));
            }
        };
        if metadata.is_file() {
            return Ok(working_tree_path_is_tracked(&self.request.root, relative));
        }
        if metadata.is_dir() {
            return Ok(working_tree_directory_is_tracked(
                &self.request.root,
                relative,
            ));
        }
        let _ = absolute;
        Err(CheckFailure::new(
            "ineligible_input",
            "input must be a tracked regular file or directory",
        ))
    }

    fn revision_path_exists(&mut self, relative: &Path) -> Result<bool, CheckFailure> {
        let Some((mode, _)) = self.revision_entry(relative)? else {
            return Ok(false);
        };
        if mode == "100644" || mode == "100755" || mode == "040000" {
            Ok(true)
        } else {
            Err(CheckFailure::new(
                "ineligible_input",
                "revision input must be a regular file or directory",
            ))
        }
    }

    fn read_working_tree_file(&self, relative: &Path) -> Result<Vec<u8>, CheckFailure> {
        if !working_tree_path_is_tracked(&self.request.root, relative) {
            return Err(CheckFailure::new(
                "ineligible_input",
                "working-tree input must be tracked and not ignored",
            ));
        }
        let metadata = open_existing_no_follow(&self.request.root, relative)
            .map_err(|_| CheckFailure::new("ineligible_input", "input cannot be inspected safely"))?
            .ok_or_else(|| CheckFailure::new("missing_path", "input path is missing"))?;
        if !metadata.is_file() {
            return Err(CheckFailure::new(
                "ineligible_input",
                "input must be a regular file",
            ));
        }
        if metadata.len() > self.request.budgets.max_input_bytes_per_file {
            return Err(CheckFailure::new(
                "input_too_large",
                "input exceeds the configured byte limit",
            ));
        }
        read_regular_file_bytes_no_follow(
            &self.request.root,
            relative,
            self.request.budgets.max_input_bytes_per_file,
        )
        .map_err(|_| CheckFailure::new("ineligible_input", "input cannot be read safely"))
    }

    fn read_revision_file(&mut self, relative: &Path) -> Result<Vec<u8>, CheckFailure> {
        let Some((mode, object)) = self.revision_entry(relative)? else {
            return Err(CheckFailure::new(
                "missing_path",
                "input path is missing from the authorized revision",
            ));
        };
        if mode != "100644" && mode != "100755" {
            return Err(CheckFailure::new(
                "ineligible_input",
                "revision input must be a regular file",
            ));
        }
        let size = self.revision_blob_size(&object)?;
        if size > self.request.budgets.max_input_bytes_per_file {
            return Err(CheckFailure::new(
                "input_too_large",
                "input exceeds the configured byte limit",
            ));
        }
        // The blob size is already bounded above, so reuse it as the read limit:
        // an over-long blob is an unreadable revision, matching the prior code.
        self.run_git_output_with_limit(&["cat-file", "blob", &object], size)
            .map_err(|error| {
                if error.reason_code == "git_output_limit_exceeded" {
                    CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
                } else {
                    error
                }
            })
    }

    fn revision_blob_size(&mut self, object: &str) -> Result<u64, CheckFailure> {
        let raw = self.run_git_output(&["cat-file", "-s", object])?;
        String::from_utf8(raw)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .ok_or_else(|| {
                CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
            })
    }

    fn revision_entry(
        &mut self,
        relative: &Path,
    ) -> Result<Option<(String, String)>, CheckFailure> {
        let commit = self.revision.clone().ok_or_else(|| {
            CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
        })?;
        let path = relative.to_string_lossy().into_owned();
        let raw = self.run_git_output(&[
            "ls-tree",
            "--format=%(objectmode) %(objectname)",
            &commit,
            "--",
            &path,
        ])?;
        let text = String::from_utf8(raw).map_err(|_| {
            CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
        })?;
        Ok(text
            .split_whitespace()
            .next()
            .zip(text.split_whitespace().nth(1))
            .map(|(m, o)| (m.to_owned(), o.to_owned())))
    }
}

struct ValidatedInput {
    relative_path: PathBuf,
    bytes: Arc<[u8]>,
}

#[derive(Debug)]
struct CheckFailure {
    reason_code: &'static str,
    reason: &'static str,
}

impl CheckFailure {
    fn new(reason_code: &'static str, reason: &'static str) -> Self {
        Self {
            reason_code,
            reason,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LocalDoctorCatalog {
    pub packs: BTreeMap<String, LocalDoctorPack>,
    pub diagnostics: Vec<LocalPackDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct LocalPackDiagnostic {
    pub code: &'static str,
    pub relative_path: String,
    pub field: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct LocalDoctorPack {
    pub schema_version: u32,
    pub name: String,
    pub description: String,
    pub suites: Vec<String>,
    pub checks: Vec<LocalDoctorCheck>,
    pub manifest_path: String,
    revision_commit: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Info,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LocalDoctorCheck {
    PathExists {
        id: String,
        path: String,
        severity: Severity,
    },
    FileContains {
        id: String,
        path: String,
        contains: String,
        severity: Severity,
    },
    FileContainsAll {
        id: String,
        path: String,
        contains: Vec<String>,
        severity: Severity,
    },
    FileNotContains {
        id: String,
        path: String,
        contains: String,
        severity: Severity,
    },
    MakeTargetContains {
        id: String,
        path: String,
        target: String,
        contains: Vec<String>,
        severity: Severity,
    },
    TomlArrayContains {
        id: String,
        path: String,
        key: String,
        value: String,
        severity: Severity,
    },
    CargoFeaturePackagesCoveredByMakeTarget {
        id: String,
        workspace: String,
        feature: String,
        makefile: String,
        target: String,
        integrity_severity: Severity,
        severity: Severity,
    },
}

impl LocalDoctorCheck {
    fn id(&self) -> &str {
        match self {
            Self::PathExists { id, .. }
            | Self::FileContains { id, .. }
            | Self::FileContainsAll { id, .. }
            | Self::FileNotContains { id, .. }
            | Self::MakeTargetContains { id, .. }
            | Self::TomlArrayContains { id, .. }
            | Self::CargoFeaturePackagesCoveredByMakeTarget { id, .. } => id,
        }
    }

    fn severity(&self) -> Severity {
        match self {
            Self::PathExists { severity, .. }
            | Self::FileContains { severity, .. }
            | Self::FileContainsAll { severity, .. }
            | Self::FileNotContains { severity, .. }
            | Self::MakeTargetContains { severity, .. }
            | Self::TomlArrayContains { severity, .. }
            | Self::CargoFeaturePackagesCoveredByMakeTarget { severity, .. } => *severity,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::PathExists { .. } => "path-exists",
            Self::FileContains { .. } => "file-contains",
            Self::FileContainsAll { .. } => "file-contains-all",
            Self::FileNotContains { .. } => "file-not-contains",
            Self::MakeTargetContains { .. } => "make-target-contains",
            Self::TomlArrayContains { .. } => "toml-array-contains",
            Self::CargoFeaturePackagesCoveredByMakeTarget { .. } => {
                "cargo-feature-packages-covered-by-make-target"
            }
        }
    }
}

/// A single static assertion outcome.  Only safe, repository-relative facts
/// cross this boundary; source bytes and assertion literals never do.
#[derive(Debug, Clone)]
pub struct LocalCheckResult {
    pub passed: bool,
    pub reason_code: &'static str,
    pub reason: &'static str,
    pub line: Option<usize>,
    pub discovered_packages: Option<Vec<String>>,
    pub uncovered_packages: Option<Vec<String>>,
}

/// Evaluate the Task 2 static assertion subset for one admitted local pack.
pub fn run_local_doctor_pack(
    pack: &LocalDoctorPack,
    context: &mut LocalPackRequestContext,
) -> QueryEnvelope {
    if context.terminal.is_some() && context.terminal_reported {
        return QueryEnvelope {
            schema_version: SCHEMA_VERSION.to_owned(),
            query_id: utils::query_id("local_doctor_terminal"),
            kind: "doctor".to_owned(),
            summary: "local doctor request terminated".to_owned(),
            confidence: 0.0,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: Vec::new(),
            meta: None,
            timing_ms: 0,
        };
    }
    if let Some(commit) = &pack.revision_commit {
        context.revision = Some(commit.clone());
    }
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut informational_findings = Vec::new();
    for check in &pack.checks {
        let result = match context.charge(BudgetOperation::Check) {
            Ok(()) => evaluate_simple_check(check, context),
            Err(error) => LocalCheckResult {
                passed: false,
                reason_code: error.reason_code,
                reason: error.reason,
                line: None,
                discovered_packages: None,
                uncovered_packages: None,
            },
        };
        let path = check_path(check).unwrap_or_default();
        let effective_severity = if matches!(
            check,
            LocalDoctorCheck::CargoFeaturePackagesCoveredByMakeTarget { .. }
        ) && !result.passed
            && result.reason_code != "cargo_package_uncovered"
        {
            match check {
                LocalDoctorCheck::CargoFeaturePackagesCoveredByMakeTarget {
                    integrity_severity,
                    ..
                } => *integrity_severity,
                _ => unreachable!(),
            }
        } else {
            check.severity()
        };
        let severity = match effective_severity {
            Severity::Warning => "warning",
            Severity::Info => "info",
        };
        let key = format!("local-doctor/{}/{}", pack.name, check.id());
        let terminal = matches!(
            result.reason_code,
            "cancelled"
                | "deadline_exceeded"
                | "check_limit_exceeded"
                | "finding_limit_exceeded"
                | "encoded_output_limit_exceeded"
                | "unique_input_limit_exceeded"
                | "directory_entry_limit_exceeded"
                | "git_query_limit_exceeded"
        );
        if !result.passed {
            match effective_severity {
                Severity::Warning => warnings.push(format!("[{key}] {}", result.reason)),
                Severity::Info => informational_findings.push(key.clone()),
            }
            if !terminal && context.charge(BudgetOperation::Finding).is_err() {
                break;
            }
        }
        let entity = json!({
            "entity_kind": LOCAL_CHECK_ENTITY_KIND,
            "doctor": pack.name,
            "check": check.id(),
            "kind": check.kind(),
            "severity": severity,
            "passed": result.passed,
            "path": path,
            "line": result.line,
            "reason_code": result.reason_code,
            "reason": result.reason,
            "discovered_packages": result.discovered_packages,
            "uncovered_packages": result.uncovered_packages,
        });
        let encoded = serde_json::to_vec(&entity)
            .map(|value| value.len())
            .unwrap_or(usize::MAX);
        if !terminal && context.charge(BudgetOperation::Output(encoded)).is_err() {
            break;
        }
        entities.push(entity);
        if terminal {
            context.terminal_reported = true;
            break;
        }
    }
    let envelope = QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_owned(),
        query_id: utils::query_id(&format!("local_doctor_{}", pack.name)),
        kind: "doctor".to_owned(),
        summary: format!("evaluated local doctor `{}`", pack.name),
        confidence: if warnings.is_empty() { 0.98 } else { 0.68 },
        entities,
        evidence: Vec::new(),
        warnings,
        meta: (!informational_findings.is_empty()).then(|| {
            json!({
                "informational_findings": informational_findings,
            })
        }),
        timing_ms: 0,
    };
    // Entity charging is deliberately conservative, but JSON envelope fields
    // also consume the caller's output budget. Never return an oversized
    // partial success: replace it with a single deterministic terminal result.
    if serde_json::to_vec(&envelope)
        .map(|bytes| bytes.len() > context.request.budgets.max_encoded_output_bytes)
        .unwrap_or(true)
    {
        context.terminal = Some("encoded_output_limit_exceeded");
        context.terminal_reported = true;
        return terminal_output_envelope(pack.name.as_str());
    }
    envelope
}

fn terminal_output_envelope(pack: &str) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_owned(),
        query_id: utils::query_id("local_doctor_output_limit"),
        kind: "doctor".to_owned(),
        summary: "local doctor request terminated".to_owned(),
        confidence: 0.0,
        entities: vec![json!({
            "entity_kind": LOCAL_CHECK_ENTITY_KIND,
            "doctor": pack,
            "passed": false,
            "reason_code": "encoded_output_limit_exceeded",
        })],
        evidence: Vec::new(),
        warnings: vec!["local doctor output limit exceeded".to_owned()],
        meta: None,
        timing_ms: 0,
    }
}

fn check_path(check: &LocalDoctorCheck) -> Option<&str> {
    match check {
        LocalDoctorCheck::PathExists { path, .. }
        | LocalDoctorCheck::FileContains { path, .. }
        | LocalDoctorCheck::FileContainsAll { path, .. }
        | LocalDoctorCheck::FileNotContains { path, .. }
        | LocalDoctorCheck::MakeTargetContains { path, .. }
        | LocalDoctorCheck::TomlArrayContains { path, .. } => Some(path),
        LocalDoctorCheck::CargoFeaturePackagesCoveredByMakeTarget { .. } => None,
    }
}

fn evaluate_simple_check(
    check: &LocalDoctorCheck,
    context: &mut LocalPackRequestContext,
) -> LocalCheckResult {
    match check {
        LocalDoctorCheck::PathExists { path, .. } => {
            match context.check_path_exists(Path::new(path)) {
                Ok(true) => LocalCheckResult {
                    passed: true,
                    reason_code: "ok",
                    reason: "required path exists",
                    line: None,
                    discovered_packages: None,
                    uncovered_packages: None,
                },
                Ok(false) => LocalCheckResult {
                    passed: false,
                    reason_code: "missing_path",
                    reason: "required path is missing",
                    line: None,
                    discovered_packages: None,
                    uncovered_packages: None,
                },
                Err(error) => LocalCheckResult {
                    passed: false,
                    reason_code: error.reason_code,
                    reason: error.reason,
                    line: None,
                    discovered_packages: None,
                    uncovered_packages: None,
                },
            }
        }
        LocalDoctorCheck::FileContains { path, contains, .. } => {
            literal_result(context, path, &[contains.as_str()], false)
        }
        LocalDoctorCheck::FileContainsAll { path, contains, .. } => literal_result(
            context,
            path,
            &contains.iter().map(String::as_str).collect::<Vec<_>>(),
            false,
        ),
        LocalDoctorCheck::FileNotContains { path, contains, .. } => {
            literal_result(context, path, &[contains.as_str()], true)
        }
        LocalDoctorCheck::TomlArrayContains {
            path, key, value, ..
        } => toml_array_result(context, path, key, value),
        LocalDoctorCheck::MakeTargetContains {
            path,
            target,
            contains,
            ..
        } => make_target_result(context, path, target, contains),
        LocalDoctorCheck::CargoFeaturePackagesCoveredByMakeTarget {
            workspace,
            feature,
            makefile,
            target,
            ..
        } => cargo_feature_result(context, workspace, feature, makefile, target),
    }
}

fn make_target_result(
    context: &mut LocalPackRequestContext,
    path: &str,
    target: &str,
    contains: &[String],
) -> LocalCheckResult {
    let input = match context.read_tracked_regular_file(Path::new(path)) {
        Ok(input) => input,
        Err(e) => return failure(e),
    };
    let source = match std::str::from_utf8(&input.bytes) {
        Ok(s) => s,
        Err(_) => {
            return LocalCheckResult {
                passed: false,
                reason_code: "invalid_utf8",
                reason: "input must be valid UTF-8 text",
                line: None,
                discovered_packages: None,
                uncovered_packages: None,
            };
        }
    };
    let recipe = match make_target_recipe(source, target) {
        Ok(recipe) => recipe,
        Err(e) => return failure(e),
    };
    if contains
        .iter()
        .all(|needle| recipe.iter().any(|(_, line)| line.contains(needle)))
    {
        LocalCheckResult {
            passed: true,
            reason_code: "ok",
            reason: "required target recipe fragments are present",
            line: None,
            discovered_packages: None,
            uncovered_packages: None,
        }
    } else {
        LocalCheckResult {
            passed: false,
            reason_code: "make_target_literal_missing",
            reason: "required target recipe fragment is missing",
            line: None,
            discovered_packages: None,
            uncovered_packages: None,
        }
    }
}

fn make_target_recipe<'a>(
    source: &'a str,
    target: &str,
) -> Result<Vec<(usize, &'a str)>, CheckFailure> {
    let mut active = false;
    let mut recipe = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if line.starts_with('\t') {
            if active {
                recipe.push((index + 1, line));
            }
            continue;
        }
        let ordinary_target = !line.starts_with(char::is_whitespace)
            && line
                .split_once(':')
                .is_some_and(|(left, _)| left.split_whitespace().any(|name| name == target));
        if ordinary_target {
            active = true;
            continue;
        }
        if active && !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            break;
        }
    }
    if recipe.is_empty() {
        return Err(CheckFailure::new(
            "make_target_missing",
            "named Make target has no static recipe",
        ));
    }
    Ok(recipe)
}

fn cargo_feature_result(
    context: &mut LocalPackRequestContext,
    workspace: &str,
    feature: &str,
    makefile: &str,
    target: &str,
) -> LocalCheckResult {
    let packages = match discover_feature_packages(context, Path::new(workspace), feature) {
        Ok(p) => p,
        Err(e) => return failure(e),
    };
    let input = match context.read_tracked_regular_file(Path::new(makefile)) {
        Ok(input) => input,
        Err(e) => return failure(e),
    };
    let source = match std::str::from_utf8(&input.bytes) {
        Ok(s) => s,
        Err(_) => {
            return LocalCheckResult {
                passed: false,
                reason_code: "invalid_utf8",
                reason: "input must be valid UTF-8 text",
                line: None,
                discovered_packages: None,
                uncovered_packages: None,
            };
        }
    };
    let recipe = match make_target_recipe(source, target) {
        Ok(r) => r,
        Err(e) => return failure(e),
    };
    let text = recipe
        .iter()
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n");
    // `reason` is a static string, so the derived package names are the only
    // way a finding can say *which* crate the Make target forgot.
    let discovered = packages
        .iter()
        .map(|package| package.name.clone())
        .collect::<Vec<_>>();
    let uncovered = packages
        .iter()
        .filter(|package| !recipe_covers(&text, &package.name, feature))
        .map(|package| package.name.clone())
        .collect::<Vec<_>>();
    if uncovered.is_empty() {
        LocalCheckResult {
            passed: true,
            reason_code: "ok",
            reason: "feature packages are covered by the Make target",
            line: None,
            discovered_packages: Some(discovered),
            uncovered_packages: Some(uncovered),
        }
    } else {
        LocalCheckResult {
            passed: false,
            reason_code: "cargo_package_uncovered",
            reason: "feature package is not covered by the Make target",
            line: None,
            discovered_packages: Some(discovered),
            uncovered_packages: Some(uncovered),
        }
    }
}

#[derive(Debug)]
struct FeaturePackage {
    name: String,
}

fn discover_feature_packages(
    context: &mut LocalPackRequestContext,
    workspace_manifest: &Path,
    feature: &str,
) -> Result<Vec<FeaturePackage>, CheckFailure> {
    let workspace_input = context.read_tracked_regular_file(workspace_manifest)?;
    let workspace_source = std::str::from_utf8(&workspace_input.bytes)
        .map_err(|_| CheckFailure::new("invalid_utf8", "input must be valid UTF-8 text"))?;
    let workspace: toml::Value = toml::from_str(workspace_source).map_err(|_| {
        CheckFailure::new(
            "cargo_manifest_parse",
            "Cargo workspace manifest is not valid TOML",
        )
    })?;
    let members = workspace
        .get("workspace")
        .and_then(|v| v.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| {
            CheckFailure::new(
                "cargo_workspace_members_missing",
                "Cargo workspace members are missing",
            )
        })?;
    let base = workspace_manifest.parent().unwrap_or(Path::new(""));
    let mut member_paths = Vec::new();
    for member in members {
        let member = member.as_str().ok_or_else(|| {
            CheckFailure::new(
                "cargo_workspace_members_invalid",
                "Cargo workspace members must be strings",
            )
        })?;
        expand_workspace_member(context, base, member, &mut member_paths)?;
    }
    member_paths.sort();
    member_paths.dedup();
    let mut packages = Vec::new();
    for path in member_paths {
        let input = context.read_tracked_regular_file(&path)?;
        let source = std::str::from_utf8(&input.bytes)
            .map_err(|_| CheckFailure::new("invalid_utf8", "input must be valid UTF-8 text"))?;
        let manifest: toml::Value = toml::from_str(source).map_err(|_| {
            CheckFailure::new(
                "cargo_manifest_parse",
                "Cargo member manifest is not valid TOML",
            )
        })?;
        let name = manifest
            .get("package")
            .and_then(|v| v.get("name"))
            .and_then(toml::Value::as_str)
            .ok_or_else(|| {
                CheckFailure::new(
                    "cargo_package_name_missing",
                    "Cargo member package name is missing",
                )
            })?;
        if manifest
            .get("features")
            .and_then(|v| v.get(feature))
            .is_some()
        {
            packages.push(FeaturePackage {
                name: name.to_owned(),
            });
        }
    }
    Ok(packages)
}

fn expand_workspace_member(
    context: &mut LocalPackRequestContext,
    base: &Path,
    member: &str,
    output: &mut Vec<PathBuf>,
) -> Result<(), CheckFailure> {
    let parts = Path::new(member).components().collect::<Vec<_>>();
    if parts
        .iter()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(CheckFailure::new(
            "cargo_workspace_member_escape",
            "Cargo workspace member escapes the repository",
        ));
    }
    if context.request.content_mode == LocalContentMode::RevisionTracked {
        return expand_revision_workspace_member(context, base, member, output);
    }
    fn walk(
        context: &mut LocalPackRequestContext,
        current: &Path,
        parts: &[Component<'_>],
        depth: usize,
        output: &mut Vec<PathBuf>,
    ) -> Result<(), CheckFailure> {
        if depth > context.request.budgets.max_path_depth {
            return Err(CheckFailure::new(
                "workspace_path_depth_exceeded",
                "Cargo workspace member path exceeds the depth limit",
            ));
        }
        if parts.is_empty() {
            let manifest = current.join("Cargo.toml");
            let relative = manifest.strip_prefix(&context.request.root).map_err(|_| {
                CheckFailure::new(
                    "cargo_workspace_member_escape",
                    "Cargo workspace member escapes the repository",
                )
            })?;
            validate_input_path(relative)?;
            if output.len() >= context.request.budgets.max_workspace_members {
                return Err(CheckFailure::new(
                    "workspace_member_limit_exceeded",
                    "workspace member limit exceeded",
                ));
            }
            output.push(relative.to_path_buf());
            return Ok(());
        }
        let segment = parts[0].as_os_str().to_string_lossy();
        if segment == "*" {
            let mut entries = fs::read_dir(current)
                .map_err(|_| {
                    CheckFailure::new(
                        "workspace_traversal_failed",
                        "Cargo workspace member directory cannot be read",
                    )
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| {
                    CheckFailure::new(
                        "workspace_traversal_failed",
                        "Cargo workspace member directory cannot be read",
                    )
                })?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                context.charge(BudgetOperation::DirectoryEntry)?;
                let ty = entry.file_type().map_err(|_| {
                    CheckFailure::new(
                        "workspace_traversal_failed",
                        "Cargo workspace member entry cannot be inspected",
                    )
                })?;
                if ty.is_symlink() || !ty.is_dir() {
                    continue;
                }
                walk(context, &entry.path(), &parts[1..], depth + 1, output)?;
            }
        } else {
            // Literal components are an equally important no-follow boundary:
            // do not let a workspace member escape through a symlink before a
            // later Cargo.toml validation can reject it.
            let next = current.join(segment.as_ref());
            let metadata = fs::symlink_metadata(&next).map_err(|_| {
                CheckFailure::new(
                    "workspace_traversal_failed",
                    "Cargo workspace member directory cannot be inspected",
                )
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(CheckFailure::new(
                    "workspace_traversal_failed",
                    "Cargo workspace member component is not an eligible directory",
                ));
            }
            walk(context, &next, &parts[1..], depth + 1, output)?;
        }
        Ok(())
    }
    walk(context, &context.request.root.join(base), &parts, 0, output)
}

fn expand_revision_workspace_member(
    context: &mut LocalPackRequestContext,
    base: &Path,
    member: &str,
    output: &mut Vec<PathBuf>,
) -> Result<(), CheckFailure> {
    let commit = context.revision.clone().ok_or_else(|| {
        CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
    })?;
    let limit = context
        .request
        .budgets
        .max_workspace_members
        .saturating_add(1)
        .saturating_mul(512);
    // The helper reserves the Git budget before spawning and re-checks the
    // cooperative boundary once the child returns. Output past `limit` is a
    // member-expansion overflow here, not a generic Git read failure.
    let raw = context
        .run_git_output_with_limit(
            &["ls-tree", "-r", "-z", "--name-only", &commit, "--"],
            limit.saturating_sub(1) as u64,
        )
        .map_err(|error| {
            if error.reason_code == "git_output_limit_exceeded" {
                CheckFailure::new(
                    "workspace_member_limit_exceeded",
                    "workspace member expansion exceeded the configured limit",
                )
            } else {
                error
            }
        })?;
    for name in raw
        .split(|byte| *byte == 0)
        .filter_map(|item| std::str::from_utf8(item).ok())
    {
        if !revision_workspace_member_matches(name, base, member) {
            continue;
        }
        if output.len() >= context.request.budgets.max_workspace_members {
            return Err(CheckFailure::new(
                "workspace_member_limit_exceeded",
                "workspace member limit exceeded",
            ));
        }
        output.push(PathBuf::from(name));
    }
    Ok(())
}

/// Cargo member globs are component patterns, not byte prefixes. This keeps a
/// pinned `member` from admitting `member-extra`, and `members/foo/*` from
/// admitting sibling paths with a shared textual prefix.
fn revision_workspace_member_matches(name: &str, base: &Path, member: &str) -> bool {
    let candidate = Path::new(name).components().collect::<Vec<_>>();
    let mut pattern = base.components().collect::<Vec<_>>();
    pattern.extend(Path::new(member).components());
    pattern.push(Component::Normal(std::ffi::OsStr::new("Cargo.toml")));
    candidate.len() == pattern.len()
        && candidate
            .iter()
            .zip(pattern)
            .all(|(actual, expected)| match expected {
                Component::Normal(segment) if segment == "*" => {
                    matches!(actual, Component::Normal(_))
                }
                _ => actual == &expected,
            })
}

fn recipe_covers(recipe: &str, package: &str, feature: &str) -> bool {
    recipe.lines().flat_map(shell_argv_segments).any(|argv| {
        let cargo = argv.iter().position(|token| token == "cargo");
        let Some(cargo) = cargo else { return false };
        // Only accept a Cargo invocation at the command anchor. Environment
        // assignments are permitted before it, but `echo cargo ...` is not.
        if argv[..cargo]
            .iter()
            .any(|token| !is_shell_assignment(token))
        {
            return false;
        }
        let argv = &argv[cargo + 1..];
        let package_match = argv
            .windows(2)
            .any(|pair| pair[0] == "-p" && pair[1] == package)
            || argv.iter().any(|token| token == &format!("-p={package}"));
        let feature_match = argv.windows(2).any(|pair| {
            pair[0] == "--features" && pair[1].split(',').any(|value| value == feature)
        }) || argv
            .iter()
            .filter_map(|token| token.strip_prefix("--features="))
            .any(|values| values.split(',').any(|value| value == feature));
        package_match && feature_match
    })
}

fn is_shell_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
    })
}

/// Small bounded shell lexer for static Make recipes. It deliberately does
/// not execute expansion; it only preserves argv boundaries across quotes,
/// escapes, comments, continuations, and shell control operators.
fn shell_argv_segments(line: &str) -> Vec<Vec<String>> {
    const MAX_TOKENS: usize = 512;
    const MAX_TOKEN_BYTES: usize = 64 * 1024;
    let mut segments = Vec::new();
    let mut argv = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    let bytes = line.trim_start_matches('\t').as_bytes();
    let mut index = 0;
    let finish = |token: &mut String, argv: &mut Vec<String>| {
        if !token.is_empty() && argv.len() < MAX_TOKENS {
            argv.push(std::mem::take(token));
        }
    };
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            if byte != b'\n' && token.len() < MAX_TOKEN_BYTES {
                token.push(byte as char);
            }
            escaped = false;
            index += 1;
            continue;
        }
        if byte == b'\\' && quote != Some(b'\'') {
            escaped = true;
            index += 1;
            continue;
        }
        if let Some(active) = quote {
            if byte == active {
                quote = None;
            } else if token.len() < MAX_TOKEN_BYTES {
                token.push(byte as char);
            }
            index += 1;
            continue;
        }
        match byte {
            b'\'' | b'\"' => quote = Some(byte),
            b'#' => break,
            b' ' | b'\t' | b'\r' | b'\n' => finish(&mut token, &mut argv),
            b';' | b'|' | b'&' => {
                finish(&mut token, &mut argv);
                if !argv.is_empty() {
                    segments.push(std::mem::take(&mut argv));
                }
                if matches!(byte, b'|' | b'&') && bytes.get(index + 1) == Some(&byte) {
                    index += 1;
                }
            }
            _ if token.len() < MAX_TOKEN_BYTES => token.push(byte as char),
            _ => {}
        }
        index += 1;
    }
    finish(&mut token, &mut argv);
    if !argv.is_empty() {
        segments.push(argv);
    }
    segments
}

fn failure(error: CheckFailure) -> LocalCheckResult {
    LocalCheckResult {
        passed: false,
        reason_code: error.reason_code,
        reason: error.reason,
        line: None,
        discovered_packages: None,
        uncovered_packages: None,
    }
}

fn literal_result(
    context: &mut LocalPackRequestContext,
    path: &str,
    needles: &[&str],
    negate: bool,
) -> LocalCheckResult {
    let input = match context.read_tracked_regular_file(Path::new(path)) {
        Ok(input) => input,
        Err(error) => {
            return LocalCheckResult {
                passed: false,
                reason_code: error.reason_code,
                reason: error.reason,
                line: None,
                discovered_packages: None,
                uncovered_packages: None,
            };
        }
    };
    let _relative_path = &input.relative_path;
    let source = match std::str::from_utf8(&input.bytes) {
        Ok(source) => source,
        Err(_) => {
            return LocalCheckResult {
                passed: false,
                reason_code: "invalid_utf8",
                reason: "input must be valid UTF-8 text",
                line: None,
                discovered_packages: None,
                uncovered_packages: None,
            };
        }
    };
    let found = needles.iter().find_map(|needle| source.find(needle));
    if negate {
        return match found {
            Some(_) => LocalCheckResult {
                passed: false,
                reason_code: "literal_present",
                reason: "forbidden literal is present",
                line: found.map(|offset| line_column(source, offset).0.unwrap_or(1)),
                discovered_packages: None,
                uncovered_packages: None,
            },
            None => LocalCheckResult {
                passed: true,
                reason_code: "ok",
                reason: "forbidden literal is absent",
                line: None,
                discovered_packages: None,
                uncovered_packages: None,
            },
        };
    }
    if needles.iter().all(|needle| source.contains(needle)) {
        LocalCheckResult {
            passed: true,
            reason_code: "ok",
            reason: "required literal is present",
            line: found.map(|offset| line_column(source, offset).0.unwrap_or(1)),
            discovered_packages: None,
            uncovered_packages: None,
        }
    } else {
        LocalCheckResult {
            passed: false,
            reason_code: "literal_missing",
            reason: "required literal is missing",
            line: None,
            discovered_packages: None,
            uncovered_packages: None,
        }
    }
}

fn toml_array_result(
    context: &mut LocalPackRequestContext,
    path: &str,
    key: &str,
    value: &str,
) -> LocalCheckResult {
    let input = match context.read_tracked_regular_file(Path::new(path)) {
        Ok(input) => input,
        Err(error) => {
            return LocalCheckResult {
                passed: false,
                reason_code: error.reason_code,
                reason: error.reason,
                line: None,
                discovered_packages: None,
                uncovered_packages: None,
            };
        }
    };
    let _relative_path = &input.relative_path;
    let source = match std::str::from_utf8(&input.bytes) {
        Ok(source) => source,
        Err(_) => {
            return LocalCheckResult {
                passed: false,
                reason_code: "invalid_utf8",
                reason: "input must be valid UTF-8 text",
                line: None,
                discovered_packages: None,
                uncovered_packages: None,
            };
        }
    };
    let value_root = match toml::from_str::<toml::Value>(source) {
        Ok(value_root) => value_root,
        Err(_) => {
            return LocalCheckResult {
                passed: false,
                reason_code: "toml_parse_error",
                reason: "input is not valid TOML",
                line: None,
                discovered_packages: None,
                uncovered_packages: None,
            };
        }
    };
    let resolved = key
        .split('.')
        .try_fold(&value_root, |node, segment| node.get(segment));
    match resolved.and_then(toml::Value::as_array) {
        Some(items) if items.iter().any(|item| item.as_str() == Some(value)) => LocalCheckResult {
            passed: true,
            reason_code: "ok",
            reason: "required array member is present",
            line: None,
            discovered_packages: None,
            uncovered_packages: None,
        },
        Some(_) => LocalCheckResult {
            passed: false,
            reason_code: "array_member_missing",
            reason: "required array member is missing",
            line: None,
            discovered_packages: None,
            uncovered_packages: None,
        },
        None => LocalCheckResult {
            passed: false,
            reason_code: "toml_array_missing",
            reason: "required TOML array is missing",
            line: None,
            discovered_packages: None,
            uncovered_packages: None,
        },
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalDoctorManifest {
    schema_version: u32,
    name: String,
    description: String,
    #[serde(default)]
    suites: Vec<String>,
    checks: Vec<LocalDoctorCheck>,
}

/// Discover immediate regular `*.toml` manifests in deterministic filename
/// order. Invalid candidates are never admitted and always yield a bounded,
/// source-free diagnostic.
pub fn discover_local_doctor_packs(
    request: &LocalPackRequest,
    compiled_names: &BTreeSet<String>,
) -> LocalDoctorCatalog {
    let mut catalog = LocalDoctorCatalog::default();
    if request.content_mode == LocalContentMode::RevisionTracked {
        return discover_revision_local_doctor_packs(request, compiled_names);
    }
    let doctor_dir = Path::new(LOCAL_DOCTOR_DIR);
    let directory = match open_existing_no_follow(&request.root, doctor_dir) {
        Ok(Some(metadata)) if metadata.is_dir() => request.root.join(doctor_dir),
        Ok(Some(_)) => {
            catalog.diagnostics.push(diagnostic(
                "non_directory",
                LOCAL_DOCTOR_DIR,
                None,
                None,
                None,
                "doctor manifest location must be a directory",
            ));
            return catalog;
        }
        Ok(None) => return catalog,
        Err(error) => {
            catalog
                .diagnostics
                .push(error.into_diagnostic(LOCAL_DOCTOR_DIR));
            return catalog;
        }
    };

    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(_) => {
            catalog.diagnostics.push(diagnostic(
                "directory_unreadable",
                LOCAL_DOCTOR_DIR,
                None,
                None,
                None,
                "doctor manifest directory cannot be enumerated",
            ));
            return catalog;
        }
    };
    let mut candidates = Vec::new();
    let mut directory_over_limit = false;
    for (entry_count, entry) in entries.enumerate() {
        if entry_count == request.budgets.max_directory_entries {
            directory_over_limit = true;
            break;
        }
        let Ok(entry) = entry else {
            catalog.diagnostics.push(diagnostic(
                "directory_entry_unreadable",
                LOCAL_DOCTOR_DIR,
                None,
                None,
                None,
                "doctor manifest directory entry cannot be inspected",
            ));
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') || !name.ends_with(".toml") {
            continue;
        }
        candidates.push(name.to_owned());
    }
    candidates.sort();
    if directory_over_limit {
        // `read_dir` order is unspecified. Reject the bounded snapshot as a
        // whole instead of admitting a filesystem-order-dependent subset.
        catalog.diagnostics.clear();
        catalog.diagnostics.push(diagnostic(
            "directory_entry_limit_exceeded",
            LOCAL_DOCTOR_DIR,
            None,
            None,
            None,
            "doctor manifest directory entry limit exceeded",
        ));
        return catalog;
    }

    let mut manifest_bytes = 0u64;
    let mut request_checks = 0usize;
    for (position, filename) in candidates.into_iter().enumerate() {
        let relative_path = format!("{LOCAL_DOCTOR_DIR}/{filename}");
        if position >= request.budgets.max_pack_files {
            catalog.diagnostics.push(diagnostic(
                "pack_limit_exceeded",
                &relative_path,
                None,
                None,
                None,
                "local doctor manifest count limit exceeded",
            ));
            break;
        }
        if Instant::now() > request.deadline {
            catalog.diagnostics.push(diagnostic(
                "deadline_exceeded",
                &relative_path,
                None,
                None,
                None,
                "local doctor discovery deadline exceeded",
            ));
            break;
        }
        let stem = filename.trim_end_matches(".toml");
        if !is_slug(stem) {
            catalog.diagnostics.push(diagnostic(
                "invalid_slug",
                &relative_path,
                Some("filename"),
                None,
                None,
                "manifest filename must use the local doctor slug grammar",
            ));
            continue;
        }

        let metadata = match open_existing_no_follow(&request.root, Path::new(&relative_path)) {
            Ok(Some(metadata)) if metadata.is_file() => metadata,
            Ok(Some(_)) => {
                catalog.diagnostics.push(diagnostic(
                    "non_regular_file",
                    &relative_path,
                    None,
                    None,
                    None,
                    "doctor manifest must be a regular file",
                ));
                continue;
            }
            Ok(None) => continue,
            Err(error) => {
                catalog
                    .diagnostics
                    .push(error.into_diagnostic(&relative_path));
                continue;
            }
        };
        if !working_tree_manifest_is_tracked(&request.root, &relative_path) {
            catalog.diagnostics.push(diagnostic(
                "ineligible_manifest",
                &relative_path,
                None,
                None,
                None,
                "working-tree doctor manifest must be tracked and not ignored",
            ));
            continue;
        }
        if metadata.len() > request.budgets.max_manifest_bytes {
            catalog.diagnostics.push(diagnostic(
                "manifest_too_large",
                &relative_path,
                None,
                None,
                None,
                "doctor manifest exceeds the configured byte limit",
            ));
            continue;
        }
        manifest_bytes = manifest_bytes.saturating_add(metadata.len());
        if manifest_bytes > request.budgets.max_manifest_input_bytes {
            catalog.diagnostics.push(diagnostic(
                "manifest_input_limit_exceeded",
                &relative_path,
                None,
                None,
                None,
                "aggregate doctor manifest byte limit exceeded",
            ));
            break;
        }
        let raw = match read_regular_file_no_follow(
            &request.root,
            Path::new(&relative_path),
            request.budgets.max_manifest_bytes,
        ) {
            Ok(raw) => raw,
            Err(error) => {
                catalog
                    .diagnostics
                    .push(error.into_diagnostic(&relative_path));
                continue;
            }
        };
        let manifest = match toml::from_str::<LocalDoctorManifest>(&raw) {
            Ok(manifest) => manifest,
            Err(error) => {
                let (line, column) = error
                    .span()
                    .map(|span| line_column(&raw, span.start))
                    .unwrap_or((None, None));
                catalog.diagnostics.push(diagnostic(
                    "schema_parse_error",
                    &relative_path,
                    None,
                    line,
                    column,
                    "doctor manifest does not match the supported TOML schema",
                ));
                continue;
            }
        };
        if let Err(problem) = validate_manifest(&manifest, stem, compiled_names, &request.budgets) {
            catalog.diagnostics.push(diagnostic(
                problem.code,
                &relative_path,
                problem.field,
                None,
                None,
                problem.message,
            ));
            continue;
        }
        request_checks = request_checks.saturating_add(manifest.checks.len());
        if request_checks > request.budgets.max_checks_per_request {
            catalog.diagnostics.push(diagnostic(
                "checks_request_limit_exceeded",
                &relative_path,
                Some("checks"),
                None,
                None,
                "local doctor checks exceed the configured request limit",
            ));
            continue;
        }
        catalog.packs.insert(
            manifest.name.clone(),
            LocalDoctorPack {
                schema_version: manifest.schema_version,
                name: manifest.name,
                description: manifest.description,
                suites: manifest.suites,
                checks: manifest.checks,
                manifest_path: relative_path,
                revision_commit: None,
            },
        );
    }
    catalog
}

fn discover_revision_local_doctor_packs(
    request: &LocalPackRequest,
    compiled_names: &BTreeSet<String>,
) -> LocalDoctorCatalog {
    let commit = match git_revision_commit(&request.root) {
        Ok(commit) => commit,
        Err(_) => {
            return LocalDoctorCatalog {
                packs: BTreeMap::new(),
                diagnostics: vec![diagnostic(
                    "revision_unavailable",
                    LOCAL_DOCTOR_DIR,
                    None,
                    None,
                    None,
                    "authorized repository revision cannot be read",
                )],
            };
        }
    };
    let mut catalog = discover_revision_local_doctor_packs_with(
        request,
        compiled_names,
        || {
            git_revision_directory_entries_at(
                &request.root,
                &commit,
                request.budgets.max_directory_entries,
            )
        },
        |relative| {
            git_revision_text_file(
                &request.root,
                &commit,
                relative,
                request.budgets.max_manifest_bytes,
            )
        },
        Instant::now,
    );
    for pack in catalog.packs.values_mut() {
        pack.revision_commit = Some(commit.clone());
    }
    catalog
}

fn discover_revision_local_doctor_packs_with<ListRevision, ReadRevision, Now>(
    request: &LocalPackRequest,
    compiled_names: &BTreeSet<String>,
    list_revision: ListRevision,
    mut read_revision: ReadRevision,
    now: Now,
) -> LocalDoctorCatalog
where
    ListRevision: FnOnce() -> Result<Vec<String>, RevisionListError>,
    ReadRevision: FnMut(&str) -> Result<String, RevisionManifestReadError>,
    Now: Fn() -> Instant,
{
    let mut catalog = LocalDoctorCatalog::default();
    let output = list_revision();
    let output = match output {
        Ok(output) => output,
        Err(RevisionListError::DirectoryEntryLimitExceeded) => {
            catalog.diagnostics.push(diagnostic(
                "directory_entry_limit_exceeded",
                LOCAL_DOCTOR_DIR,
                None,
                None,
                None,
                "doctor manifest directory entry limit exceeded",
            ));
            return catalog;
        }
        Err(RevisionListError::Unavailable) => {
            catalog.diagnostics.push(diagnostic(
                "revision_unavailable",
                LOCAL_DOCTOR_DIR,
                None,
                None,
                None,
                "authorized repository revision cannot be read",
            ));
            return catalog;
        }
        Err(RevisionListError::IneligibleDirectory) => {
            catalog.diagnostics.push(diagnostic(
                "ineligible_doctor_directory",
                LOCAL_DOCTOR_DIR,
                None,
                None,
                None,
                "local doctor directory must be a tree in the authorized revision",
            ));
            return catalog;
        }
    };
    if output.len() > request.budgets.max_directory_entries {
        catalog.diagnostics.push(diagnostic(
            "directory_entry_limit_exceeded",
            LOCAL_DOCTOR_DIR,
            None,
            None,
            None,
            "doctor manifest directory entry limit exceeded",
        ));
        return catalog;
    }
    let mut entries: Vec<String> = output
        .into_iter()
        .filter(|name| !name.starts_with('.') && name.ends_with(".toml"))
        .collect();
    entries.sort();
    let mut aggregate_bytes = 0u64;
    for (position, filename) in entries.into_iter().enumerate() {
        let relative = format!("{LOCAL_DOCTOR_DIR}/{filename}");
        if position >= request.budgets.max_pack_files {
            catalog.diagnostics.push(diagnostic(
                "pack_limit_exceeded",
                &relative,
                None,
                None,
                None,
                "local doctor manifest count limit exceeded",
            ));
            break;
        }
        if now() > request.deadline {
            catalog.diagnostics.push(diagnostic(
                "deadline_exceeded",
                &relative,
                None,
                None,
                None,
                "local doctor discovery deadline exceeded",
            ));
            break;
        }
        let raw = match read_revision(&relative) {
            Ok(raw) => raw,
            Err(error) => {
                let (code, message) = match error {
                    RevisionManifestReadError::TooLarge => (
                        "manifest_too_large",
                        "doctor manifest exceeds the configured byte limit",
                    ),
                    RevisionManifestReadError::Ineligible => (
                        "ineligible_manifest",
                        "manifest is not an eligible regular file in the authorized revision",
                    ),
                    RevisionManifestReadError::Unavailable => (
                        "revision_read_failed",
                        "manifest cannot be read from authorized revision",
                    ),
                };
                catalog
                    .diagnostics
                    .push(diagnostic(code, &relative, None, None, None, message));
                continue;
            }
        };
        if now() > request.deadline {
            catalog.diagnostics.push(diagnostic(
                "deadline_exceeded",
                &relative,
                None,
                None,
                None,
                "local doctor discovery deadline exceeded",
            ));
            break;
        }
        if raw.len() as u64 > request.budgets.max_manifest_bytes {
            catalog.diagnostics.push(diagnostic(
                "manifest_too_large",
                &relative,
                None,
                None,
                None,
                "doctor manifest exceeds the configured byte limit",
            ));
            continue;
        }
        aggregate_bytes = aggregate_bytes.saturating_add(raw.len() as u64);
        if aggregate_bytes > request.budgets.max_manifest_input_bytes {
            catalog.diagnostics.push(diagnostic(
                "manifest_input_limit_exceeded",
                &relative,
                None,
                None,
                None,
                "aggregate doctor manifest byte limit exceeded",
            ));
            break;
        }
        let stem = filename.trim_end_matches(".toml");
        insert_parsed_manifest(
            &mut catalog,
            &raw,
            stem,
            &relative,
            request,
            compiled_names,
            None,
        );
    }
    catalog
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevisionListError {
    Unavailable,
    DirectoryEntryLimitExceeded,
    IneligibleDirectory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevisionManifestReadError {
    TooLarge,
    Ineligible,
    Unavailable,
}

fn git_revision_directory_entries_at(
    root: &Path,
    commit: &str,
    max: usize,
) -> Result<Vec<String>, RevisionListError> {
    let shape = Command::new("git")
        .args([
            "ls-tree",
            "--format=%(objectmode)",
            commit,
            "--",
            LOCAL_DOCTOR_DIR,
        ])
        .current_dir(root)
        .output()
        .map_err(|_| RevisionListError::Unavailable)?;
    if !shape.status.success() {
        return Err(RevisionListError::Unavailable);
    }
    let mode = String::from_utf8(shape.stdout).map_err(|_| RevisionListError::Unavailable)?;
    let mode = mode.trim();
    if mode.is_empty() {
        return Ok(Vec::new());
    }
    if mode != "040000" {
        return Err(RevisionListError::IneligibleDirectory);
    }
    let tree = Command::new("git")
        .args(["cat-file", "-e", &format!("{commit}:{LOCAL_DOCTOR_DIR}")])
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| RevisionListError::Unavailable)?;
    if !tree.success() {
        return Ok(Vec::new());
    }
    let max_bytes = max
        .saturating_add(1)
        .saturating_mul(MAX_REVISION_DIRECTORY_ENTRY_BYTES);
    let mut child = Command::new("git")
        .args([
            "ls-tree",
            "--name-only",
            "-z",
            &format!("{commit}:{LOCAL_DOCTOR_DIR}"),
        ])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| RevisionListError::Unavailable)?;
    let mut raw = Vec::new();
    let result = child
        .stdout
        .take()
        .ok_or(RevisionListError::Unavailable)?
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut raw);
    if result.is_err() || raw.len() > max_bytes {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RevisionListError::DirectoryEntryLimitExceeded);
    }
    if !child.wait().map(|s| s.success()).unwrap_or(false) {
        return Err(RevisionListError::Unavailable);
    }
    let entries = raw
        .split(|b| *b == b'\0')
        .filter(|e| !e.is_empty())
        .map(|e| String::from_utf8(e.to_vec()).map_err(|_| RevisionListError::Unavailable))
        .collect::<Result<Vec<_>, _>>()?;
    if entries.len() > max {
        Err(RevisionListError::DirectoryEntryLimitExceeded)
    } else {
        Ok(entries)
    }
}

fn git_revision_text_file(
    root: &Path,
    commit: &str,
    relative: &str,
    max: u64,
) -> Result<String, RevisionManifestReadError> {
    let output = Command::new("git")
        .args([
            "ls-tree",
            "--format=%(objectmode) %(objectname)",
            commit,
            "--",
            relative,
        ])
        .current_dir(root)
        .output()
        .map_err(|_| RevisionManifestReadError::Unavailable)?;
    if !output.status.success() {
        return Err(RevisionManifestReadError::Unavailable);
    }
    let text =
        String::from_utf8(output.stdout).map_err(|_| RevisionManifestReadError::Unavailable)?;
    let mut parts = text.split_whitespace();
    let mode = parts.next().ok_or(RevisionManifestReadError::Unavailable)?;
    let object = parts.next().ok_or(RevisionManifestReadError::Unavailable)?;
    if mode != "100644" && mode != "100755" {
        return Err(RevisionManifestReadError::Ineligible);
    }
    if git_blob_size(root, object).map_err(|_| RevisionManifestReadError::Unavailable)? > max {
        return Err(RevisionManifestReadError::TooLarge);
    }
    let mut child = Command::new("git")
        .args(["cat-file", "blob", object])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| RevisionManifestReadError::Unavailable)?;
    let bytes = read_bounded_bytes(
        child
            .stdout
            .take()
            .ok_or(RevisionManifestReadError::Unavailable)?,
        max,
    )
    .map_err(|_| RevisionManifestReadError::Unavailable)?;
    if !child.wait().map(|s| s.success()).unwrap_or(false) {
        return Err(RevisionManifestReadError::Unavailable);
    }
    String::from_utf8(bytes).map_err(|_| RevisionManifestReadError::Unavailable)
}

fn working_tree_manifest_is_tracked(root: &Path, relative: &str) -> bool {
    working_tree_path_is_tracked(root, Path::new(relative))
}

fn working_tree_path_is_tracked(root: &Path, relative: &Path) -> bool {
    let relative = relative.to_string_lossy();
    let tracked = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .map(|output| {
            output.status.success()
                && output
                    .stdout
                    .split(|byte| *byte == b'\0')
                    .any(|entry| entry == relative.as_bytes())
        })
        .unwrap_or(false);
    let ignored = Command::new("git")
        .args(["check-ignore", "--no-index", "--quiet", "--", &relative])
        .current_dir(root)
        .status()
        .map(|s| s.success())
        .unwrap_or(true);
    tracked && !ignored
}

fn git_revision_commit(root: &Path) -> Result<String, CheckFailure> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .current_dir(root)
        .output()
        .map_err(|_| {
            CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
        })?;
    if !output.status.success() {
        return Err(CheckFailure::new(
            "revision_unavailable",
            "authorized revision cannot be read",
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| {
            CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
        })
}

fn git_blob_size(root: &Path, object: &str) -> Result<u64, CheckFailure> {
    let output = Command::new("git")
        .args(["cat-file", "-s", object])
        .current_dir(root)
        .output()
        .map_err(|_| {
            CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
        })?;
    if !output.status.success() {
        return Err(CheckFailure::new(
            "revision_unavailable",
            "authorized revision cannot be read",
        ));
    }
    String::from_utf8(output.stdout)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| {
            CheckFailure::new("revision_unavailable", "authorized revision cannot be read")
        })
}

fn working_tree_directory_is_tracked(root: &Path, relative: &Path) -> bool {
    let prefix = format!("{}/", relative.to_string_lossy().trim_end_matches('/'));
    let output = Command::new("git")
        .args(["ls-files", "--", &prefix])
        .current_dir(root)
        .output();
    output
        .map(|value| value.status.success() && !value.stdout.is_empty())
        .unwrap_or(false)
}

fn validate_input_path(relative: &Path) -> Result<(), CheckFailure> {
    relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
        .then_some(())
        .ok_or_else(|| {
            CheckFailure::new(
                "invalid_path",
                "input path must be repository-relative without traversal",
            )
        })
}

fn deny_input_path(relative: &Path) -> Result<(), CheckFailure> {
    let components = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    let denied = matches!(
        components.first(),
        Some(&".git") | Some(&".leio-code") | Some(&"profiles") | Some(&"secrets")
    ) || components.first() == Some(&"deploy")
        && components.get(1) == Some(&"secrets");
    (!denied).then_some(()).ok_or_else(|| {
        CheckFailure::new(
            "denied_path",
            "input path is outside the local-doctor trust boundary",
        )
    })
}

fn insert_parsed_manifest(
    catalog: &mut LocalDoctorCatalog,
    raw: &str,
    stem: &str,
    relative_path: &str,
    request: &LocalPackRequest,
    compiled_names: &BTreeSet<String>,
    revision_commit: Option<String>,
) {
    let manifest = match toml::from_str::<LocalDoctorManifest>(raw) {
        Ok(value) => value,
        Err(_) => {
            catalog.diagnostics.push(diagnostic(
                "schema_parse_error",
                relative_path,
                None,
                None,
                None,
                "doctor manifest does not match the supported TOML schema",
            ));
            return;
        }
    };
    if let Err(problem) = validate_manifest(&manifest, stem, compiled_names, &request.budgets) {
        catalog.diagnostics.push(diagnostic(
            problem.code,
            relative_path,
            problem.field,
            None,
            None,
            problem.message,
        ));
        return;
    }
    catalog.packs.insert(
        manifest.name.clone(),
        LocalDoctorPack {
            schema_version: manifest.schema_version,
            name: manifest.name,
            description: manifest.description,
            suites: manifest.suites,
            checks: manifest.checks,
            manifest_path: relative_path.to_owned(),
            revision_commit,
        },
    );
}

struct ValidationProblem {
    code: &'static str,
    field: Option<&'static str>,
    message: &'static str,
}

fn validate_manifest(
    manifest: &LocalDoctorManifest,
    filename_stem: &str,
    compiled_names: &BTreeSet<String>,
    budgets: &LocalPackBudgets,
) -> Result<(), ValidationProblem> {
    if manifest.schema_version != 1 {
        return Err(problem(
            "unsupported_schema_version",
            Some("schema_version"),
            "doctor manifest schema version is unsupported",
        ));
    }
    if !is_slug(&manifest.name) {
        return Err(problem(
            "invalid_slug",
            Some("name"),
            "doctor name must use the local doctor slug grammar",
        ));
    }
    if manifest.name.len() > budgets.max_scalar_bytes {
        return Err(problem(
            "scalar_limit_exceeded",
            Some("name"),
            "doctor name exceeds the configured scalar byte limit",
        ));
    }
    if manifest.name != filename_stem {
        return Err(problem(
            "filename_name_mismatch",
            Some("name"),
            "doctor name must match the manifest filename",
        ));
    }
    if RESERVED_NAMES.contains(&manifest.name.as_str()) || compiled_names.contains(&manifest.name) {
        return Err(problem(
            "reserved_or_compiled_name",
            Some("name"),
            "doctor name is reserved or collides with a compiled doctor",
        ));
    }
    if manifest.description.is_empty() || manifest.description.len() > budgets.max_scalar_bytes {
        return Err(problem(
            "scalar_limit_exceeded",
            Some("description"),
            "description must be non-empty and within the scalar byte limit",
        ));
    }
    if manifest.suites.len() > budgets.max_list_items {
        return Err(problem(
            "list_limit_exceeded",
            Some("suites"),
            "suite list exceeds the configured item limit",
        ));
    }
    if manifest.suites.iter().any(|suite| suite != "all") {
        return Err(problem(
            "invalid_suite",
            Some("suites"),
            "local doctor suites may contain only `all`",
        ));
    }
    if manifest
        .suites
        .iter()
        .any(|suite| suite.len() > budgets.max_scalar_bytes)
    {
        return Err(problem(
            "scalar_limit_exceeded",
            Some("suites"),
            "suite item exceeds the configured scalar byte limit",
        ));
    }
    if manifest.checks.is_empty() {
        return Err(problem(
            "empty_checks",
            Some("checks"),
            "doctor manifest requires at least one check",
        ));
    }
    if manifest.checks.len() > budgets.max_checks_per_pack {
        return Err(problem(
            "checks_per_pack_limit_exceeded",
            Some("checks"),
            "doctor check count exceeds the configured pack limit",
        ));
    }
    let mut ids = HashSet::new();
    for check in &manifest.checks {
        if !is_slug(check.id()) {
            return Err(problem(
                "invalid_slug",
                Some("checks.id"),
                "check id must use the local doctor slug grammar",
            ));
        }
        if check.id().len() > budgets.max_scalar_bytes {
            return Err(problem(
                "scalar_limit_exceeded",
                Some("checks.id"),
                "check id exceeds the configured scalar byte limit",
            ));
        }
        if !ids.insert(check.id()) {
            return Err(problem(
                "duplicate_check_id",
                Some("checks.id"),
                "doctor check ids must be unique",
            ));
        }
        validate_check(check, budgets)?;
    }
    Ok(())
}

fn validate_check(
    check: &LocalDoctorCheck,
    budgets: &LocalPackBudgets,
) -> Result<(), ValidationProblem> {
    let scalar = |value: &str, field| {
        (value.is_empty() || value.len() > budgets.max_scalar_bytes).then_some(problem(
            "scalar_limit_exceeded",
            Some(field),
            "required scalar is empty or exceeds the configured byte limit",
        ))
    };
    let path = |value: &str, field| {
        if value.is_empty() || value.len() > budgets.max_scalar_bytes {
            Some(problem(
                "scalar_limit_exceeded",
                Some(field),
                "assertion path is empty or exceeds the configured scalar byte limit",
            ))
        } else if validate_relative_path(value, budgets.max_path_depth).is_err() {
            Some(problem(
                "invalid_path",
                Some(field),
                "assertion path must be a relative path without traversal within the depth limit",
            ))
        } else {
            None
        }
    };
    let list = |values: &[String], field| {
        (values.is_empty() || values.len() > budgets.max_list_items).then_some(problem(
            "list_limit_exceeded",
            Some(field),
            "required list is empty or exceeds the configured item limit",
        ))
    };
    match check {
        LocalDoctorCheck::PathExists { path: value, .. } => path(value, "checks.path"),
        LocalDoctorCheck::FileContains {
            path: value,
            contains,
            ..
        }
        | LocalDoctorCheck::FileNotContains {
            path: value,
            contains,
            ..
        } => path(value, "checks.path").or_else(|| scalar(contains, "checks.contains")),
        LocalDoctorCheck::FileContainsAll {
            path: value,
            contains,
            ..
        } => path(value, "checks.path")
            .or_else(|| list(contains, "checks.contains"))
            .or_else(|| {
                contains
                    .iter()
                    .find_map(|item| scalar(item, "checks.contains"))
            }),
        LocalDoctorCheck::MakeTargetContains {
            path: value,
            target,
            contains,
            ..
        } => path(value, "checks.path")
            .or_else(|| scalar(target, "checks.target"))
            .or_else(|| list(contains, "checks.contains"))
            .or_else(|| {
                contains
                    .iter()
                    .find_map(|item| scalar(item, "checks.contains"))
            }),
        LocalDoctorCheck::TomlArrayContains {
            path: value,
            key,
            value: member,
            ..
        } => path(value, "checks.path")
            .or_else(|| scalar(key, "checks.key"))
            .or_else(|| scalar(member, "checks.value")),
        LocalDoctorCheck::CargoFeaturePackagesCoveredByMakeTarget {
            workspace,
            feature,
            makefile,
            target,
            ..
        } => path(workspace, "checks.workspace")
            .or_else(|| path(makefile, "checks.makefile"))
            .or_else(|| scalar(feature, "checks.feature"))
            .or_else(|| scalar(target, "checks.target")),
    }
    .map_or(Ok(()), Err)
}

fn validate_relative_path(value: &str, max_depth: usize) -> Result<(), ()> {
    let path = Path::new(value);
    if value.is_empty() || path.is_absolute() {
        return Err(());
    }
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => {
                depth += 1;
                if depth > max_depth {
                    return Err(());
                }
            }
            _ => return Err(()),
        }
    }
    Ok(())
}

fn is_slug(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase_or_digit()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase_or_digit() || *byte == b'-')
}

trait AsciiSlugByte {
    fn is_ascii_lowercase_or_digit(&self) -> bool;
}

impl AsciiSlugByte for u8 {
    fn is_ascii_lowercase_or_digit(&self) -> bool {
        self.is_ascii_lowercase() || self.is_ascii_digit()
    }
}

#[derive(Debug)]
enum NoFollowError {
    Symlink,
    Missing,
    Unreadable,
}

impl NoFollowError {
    fn into_diagnostic(self, relative_path: &str) -> LocalPackDiagnostic {
        match self {
            Self::Symlink => diagnostic(
                "symlink_rejected",
                relative_path,
                None,
                None,
                None,
                "symbolic links are not permitted in local doctor discovery",
            ),
            Self::Missing => diagnostic(
                "missing_path",
                relative_path,
                None,
                None,
                None,
                "doctor manifest path disappeared during discovery",
            ),
            Self::Unreadable => diagnostic(
                "path_unreadable",
                relative_path,
                None,
                None,
                None,
                "doctor manifest path cannot be inspected safely",
            ),
        }
    }
}

/// Component-by-component `symlink_metadata` guard.  The caller keeps the
/// resulting regular-file handle for its only read, avoiding a pathname reopen.
fn open_existing_no_follow(
    root: &Path,
    relative: &Path,
) -> Result<Option<fs::Metadata>, NoFollowError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(NoFollowError::Unreadable);
        };
        current.push(segment);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(NoFollowError::Symlink);
            }
            Ok(metadata) => {
                if current != root.join(relative) && !metadata.is_dir() {
                    return Err(NoFollowError::Unreadable);
                }
                if current == root.join(relative) {
                    return Ok(Some(metadata));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(NoFollowError::Unreadable),
        }
    }
    Err(NoFollowError::Missing)
}

fn read_regular_file_no_follow(
    root: &Path,
    relative: &Path,
    max_bytes: u64,
) -> Result<String, NoFollowError> {
    #[cfg(unix)]
    {
        open_regular_file_no_follow(root, relative, max_bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, relative, max_bytes);
        Err(NoFollowError::Unreadable)
    }
}

fn read_regular_file_bytes_no_follow(
    root: &Path,
    relative: &Path,
    max_bytes: u64,
) -> Result<Vec<u8>, NoFollowError> {
    #[cfg(unix)]
    {
        open_regular_file_bytes_no_follow(root, relative, max_bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, relative, max_bytes);
        Err(NoFollowError::Unreadable)
    }
}

#[cfg(unix)]
fn open_regular_file_no_follow(
    root: &Path,
    relative: &Path,
    max_bytes: u64,
) -> Result<String, NoFollowError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    unsafe extern "C" {
        fn openat(dirfd: i32, pathname: *const std::ffi::c_char, flags: i32) -> i32;
    }
    #[cfg(target_os = "macos")]
    const O_NOFOLLOW: i32 = 0x100;
    #[cfg(not(target_os = "macos"))]
    const O_NOFOLLOW: i32 = 0x20000;
    #[cfg(target_os = "macos")]
    const O_DIRECTORY: i32 = 0x100000;
    #[cfg(not(target_os = "macos"))]
    const O_DIRECTORY: i32 = 0x10000;
    let mut parent = File::open(root).map_err(|_| NoFollowError::Unreadable)?;
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(NoFollowError::Unreadable);
        };
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(name.as_bytes()).map_err(|_| NoFollowError::Unreadable)?;
        let last = index + 1 == components.len();
        let flags = O_NOFOLLOW | if last { 0 } else { O_DIRECTORY };
        let fd = unsafe { openat(parent.as_raw_fd(), c.as_ptr(), flags) };
        if fd < 0 {
            return Err(NoFollowError::Unreadable);
        }
        let next = unsafe { File::from_raw_fd(fd) };
        if last {
            let metadata = next.metadata().map_err(|_| NoFollowError::Unreadable)?;
            if !metadata.is_file() {
                return Err(NoFollowError::Unreadable);
            }
            return read_bounded_utf8(next, max_bytes);
        }
        parent = next;
    }
    Err(NoFollowError::Missing)
}

#[cfg(unix)]
fn open_regular_file_bytes_no_follow(
    root: &Path,
    relative: &Path,
    max_bytes: u64,
) -> Result<Vec<u8>, NoFollowError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    unsafe extern "C" {
        fn openat(dirfd: i32, pathname: *const std::ffi::c_char, flags: i32) -> i32;
    }
    #[cfg(target_os = "macos")]
    const O_NOFOLLOW: i32 = 0x100;
    #[cfg(not(target_os = "macos"))]
    const O_NOFOLLOW: i32 = 0x20000;
    #[cfg(target_os = "macos")]
    const O_DIRECTORY: i32 = 0x100000;
    #[cfg(not(target_os = "macos"))]
    const O_DIRECTORY: i32 = 0x10000;
    let mut parent = File::open(root).map_err(|_| NoFollowError::Unreadable)?;
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(NoFollowError::Unreadable);
        };
        use std::os::unix::ffi::OsStrExt;
        let name =
            std::ffi::CString::new(name.as_bytes()).map_err(|_| NoFollowError::Unreadable)?;
        let last = index + 1 == components.len();
        let fd = unsafe {
            openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                O_NOFOLLOW | if last { 0 } else { O_DIRECTORY },
            )
        };
        if fd < 0 {
            return Err(NoFollowError::Unreadable);
        }
        let next = unsafe { File::from_raw_fd(fd) };
        if last {
            if !next
                .metadata()
                .map_err(|_| NoFollowError::Unreadable)?
                .is_file()
            {
                return Err(NoFollowError::Unreadable);
            }
            return read_bounded_bytes(next, max_bytes);
        }
        parent = next;
    }
    Err(NoFollowError::Missing)
}

fn read_bounded_utf8(reader: impl Read, max_bytes: u64) -> Result<String, NoFollowError> {
    let mut raw = String::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_string(&mut raw)
        .map_err(|_| NoFollowError::Unreadable)?;
    if raw.len() as u64 > max_bytes {
        return Err(NoFollowError::Unreadable);
    }
    Ok(raw)
}

fn read_bounded_bytes(mut reader: impl Read, max_bytes: u64) -> Result<Vec<u8>, NoFollowError> {
    let mut raw = Vec::new();
    reader
        .by_ref()
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut raw)
        .map_err(|_| NoFollowError::Unreadable)?;
    if raw.len() as u64 > max_bytes {
        return Err(NoFollowError::Unreadable);
    }
    Ok(raw)
}

fn line_column(source: &str, offset: usize) -> (Option<usize>, Option<usize>) {
    if offset > source.len() {
        return (None, None);
    }
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit('\n')
        .next()
        .map(|part| part.chars().count() + 1);
    (Some(line), column)
}

fn problem(
    code: &'static str,
    field: Option<&'static str>,
    message: &'static str,
) -> ValidationProblem {
    ValidationProblem {
        code,
        field,
        message,
    }
}

fn diagnostic(
    code: &'static str,
    relative_path: &str,
    field: Option<&str>,
    line: Option<usize>,
    column: Option<usize>,
    message: &str,
) -> LocalPackDiagnostic {
    LocalPackDiagnostic {
        code,
        relative_path: relative_path.to_owned(),
        field: field.map(str::to_owned),
        line,
        column,
        message: bounded_message(message),
    }
}

fn bounded_message(message: &str) -> String {
    let mut sanitized = message.replace(['\n', '\r', '\t'], " ");
    if sanitized.len() > MAX_DIAGNOSTIC_MESSAGE_BYTES {
        sanitized.truncate(MAX_DIAGNOSTIC_MESSAGE_BYTES);
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{
        ContentIdentity, LocalContentMode, LocalPackBudgets, LocalPackRequest,
        LocalPackRequestContext, discover_revision_local_doctor_packs_with, is_slug,
        read_bounded_utf8, read_regular_file_bytes_no_follow,
    };

    #[test]
    fn slug_uses_ascii_bytes_only() {
        assert!(is_slug("valid-123"));
        assert!(!is_slug("invalid_slug"));
        assert!(!is_slug("áccent"));
        assert!(!is_slug(&"a".repeat(65)));
    }

    #[test]
    fn bounded_reader_accepts_u64_max_without_overflow() {
        let raw = read_bounded_utf8(Cursor::new(b"correct bytes"), u64::MAX)
            .expect("u64::MAX must not overflow the bounded-reader probe");
        assert_eq!(raw.as_bytes(), b"correct bytes");
    }

    #[test]
    fn revision_read_crossing_deadline_is_rejected_before_insertion() {
        let before = Instant::now();
        let deadline = before + Duration::from_secs(1);
        let after = deadline + Duration::from_millis(1);
        let clock = Cell::new(before);
        let request = LocalPackRequest {
            root: PathBuf::new(),
            content_mode: LocalContentMode::RevisionTracked,
            deadline,
            budgets: LocalPackBudgets::default(),
        };
        let catalog =
            discover_revision_local_doctor_packs_with(
                &request,
                &BTreeSet::new(),
                || Ok(vec!["late.toml".to_owned()]),
                |_| {
                    clock.set(after);
                    Ok("schema_version = 1\nname = \"late\"\ndescription = \"late\"\nchecks = []\n"
                    .to_owned())
                },
                || clock.get(),
            );
        assert!(catalog.packs.is_empty());
        assert_eq!(catalog.diagnostics.len(), 1);
        assert_eq!(catalog.diagnostics[0].code, "deadline_exceeded");
        assert_eq!(
            catalog.diagnostics[0].relative_path,
            ".leio-code/doctors/late.toml"
        );
    }

    #[test]
    fn content_cache_hits_only_for_the_same_selected_revision_identity() {
        let request = LocalPackRequest {
            root: PathBuf::new(),
            content_mode: LocalContentMode::WorkingTreeTracked,
            deadline: Instant::now() + Duration::from_secs(1),
            budgets: LocalPackBudgets::default(),
        };
        let mut context = LocalPackRequestContext::new(request);
        let path = PathBuf::from("input.txt");
        let working_identity = ContentIdentity {
            relative_path: path.clone(),
            revision: None,
        };
        context
            .content_cache
            .insert(working_identity, Arc::from(&b"cached"[..]));
        assert_eq!(
            &*context
                .read_tracked_regular_file(&path)
                .expect("cache hit")
                .bytes,
            b"cached"
        );

        context.revision = Some("first".to_owned());
        let first_revision_identity = ContentIdentity {
            relative_path: path.clone(),
            revision: context.revision.clone(),
        };
        context.revision = Some("second".to_owned());
        let second_revision_identity = ContentIdentity {
            relative_path: path,
            revision: context.revision.clone(),
        };
        assert_ne!(first_revision_identity, second_revision_identity);
        assert!(
            !context
                .content_cache
                .contains_key(&second_revision_identity)
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_fails_closed_when_a_validated_file_is_swapped_for_a_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        let input = temp.path().join("input.txt");
        fs::write(&input, "safe").expect("write input");
        // Establish the pre-swap validation shape, then replace it before the
        // content read.  The subsequent descriptor-relative open must refuse it.
        assert!(
            super::open_existing_no_follow(temp.path(), PathBuf::from("input.txt").as_path())
                .expect("validate")
                .expect("input exists")
                .is_file()
        );
        fs::remove_file(&input).expect("remove original");
        symlink("outside.txt", &input).expect("replace with symlink");
        assert!(
            read_regular_file_bytes_no_follow(
                temp.path(),
                PathBuf::from("input.txt").as_path(),
                64
            )
            .is_err()
        );
    }
}
