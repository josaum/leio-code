//! Resolves environment-variable values from indexed sources, with redaction
//! for secret-keyed names.
//!
//! This module powers the `value_bindings` field on `explain env-var` output.
//! Sources are ordered by precedence (highest first):
//!   1. `.env.local` (developer override)
//!   2. `.env`       (committed default)
//!   3. `.env.example` (documented default; usually empty values)
//!   4. `deploy/profiles/*` (build-time profiles)
//!   5. `deploy/secret-sets/*` (declared secrets — values usually absent)
//!
//! Redaction: keys matching `*_SECRET|*_KEY|*_TOKEN|*_PASSWORD|*_PWD` (case-
//! sensitive on uppercase ASCII) are hidden by default; `--show-secrets` opts
//! out. Redacted display: `[redacted, N chars]`.
//!
//! Scope:
//!   - Multi-line dotenv values are now supported via `DeclaredVar::raw_value`.
//!     Non-secret rendering collapses to `<first line> […N more lines]`; secret
//!     redaction counts the full multi-line char count and hashes the full body.
//!   - Shell env is checked at query time (not indexed). Precedence -1 puts it
//!     above all file-based sources.
//!   - Kubernetes ConfigMap entries are indexed from YAML files and surface at
//!     precedence 30 (after all dotenv and profile sources).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Options controlling value rendering. Default: redact secrets.
#[derive(Debug, Clone, Default)]
pub struct ValueResolutionOpts {
    /// When true, show raw values for secret-keyed names. Off by default.
    pub show_secrets: bool,
}

/// State of a resolved value at a single source.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValueState {
    /// Variable is declared with a non-empty value at this source.
    Set,
    /// Variable is declared but the value is empty (e.g. `FOO=`).
    Empty,
    /// Variable is declared at no source we know about.
    Unset,
}

impl ValueState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Empty => "empty",
            Self::Unset => "unset",
        }
    }
}

/// Where a `ValueBinding` came from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindingSource {
    /// A root-level dotenv-style file (`.env`, `.env.local`, `.env.example`, …).
    EnvFile {
        path: String,
        /// Precedence: 0 = highest. See module docs.
        precedence: u8,
    },
    /// A `deploy/profiles/<name>.env` build-time profile.
    DeployProfile { name: String, path: String },
    /// A `deploy/secret-sets/<name>` declared-secret set.
    SecretSet { name: String, path: String },
    /// The process's live shell environment (`std::env::var`). Checked at
    /// query time — not stored in the index. Precedence -1 puts this above all
    /// file-based sources because shell env overrides files in most runtimes.
    ShellEnv,
    /// A Kubernetes ConfigMap `data:` entry found in a YAML file.
    K8sConfigMap { path: String, map_name: String },
}

impl BindingSource {
    /// Relative sort order: lower value = higher priority (shown first).
    ///
    /// `ShellEnv` uses -1 so it sorts before all file-based sources.
    pub fn precedence(&self) -> i16 {
        match self {
            Self::ShellEnv => -1,
            Self::EnvFile { precedence, .. } => i16::from(*precedence),
            Self::DeployProfile { .. } => 10,
            Self::SecretSet { .. } => 20,
            Self::K8sConfigMap { .. } => 30,
        }
    }
}

/// A single resolved binding: where the value comes from, what state it's in,
/// and what we'll show the user (already redacted if applicable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueBinding {
    pub state: ValueState,
    /// Display string. Already redacted when `redacted == true`. Empty string
    /// when state is `Unset` or `Empty`.
    pub display: String,
    pub source: BindingSource,
    /// True when the raw value was hidden because the key matched a secret
    /// pattern and `show_secrets` was false.
    pub redacted: bool,
}

/// Heuristic: does the variable name look like a secret?
///
/// Matches case-sensitive uppercase suffixes commonly used for credentials.
/// Conservative on purpose — false negatives are worse than false positives
/// here.
pub fn is_secret_key(name: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "_SECRET",
        "_KEY",
        "_TOKEN",
        "_PASSWORD",
        "_PWD",
        "_PRIVATE_KEY",
        "_CREDENTIAL",
        "_CREDENTIALS",
    ];
    // Exact whole-name matches users sometimes write.
    if matches!(name, "SECRET" | "PASSWORD" | "TOKEN" | "API_KEY") {
        return true;
    }
    SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// Refuse `--show-secrets` when the operator is piping output into a
/// non-interactive sink (file, log collector, CI capture) AND hasn't
/// explicitly acknowledged the risk via `--i-know-what-i-am-doing`.
///
/// Returns `Ok(())` if the request is safe to proceed, `Err(reason)` if it
/// should be rejected. Pure function so callers can test the policy
/// independently of how `is_tty` is determined at runtime.
pub fn check_show_secrets_guard(
    show_secrets: bool,
    override_ack: bool,
    is_tty: bool,
) -> Result<(), &'static str> {
    if !show_secrets {
        return Ok(());
    }
    if is_tty || override_ack {
        return Ok(());
    }
    Err("refusing to print raw secret values to a non-TTY sink. \
         Re-run on a terminal, or pass `--i-know-what-i-am-doing` if you \
         really want secret values in this output stream.")
}

/// Render a value for display, applying redaction.
///
/// Format: `[redacted, N chars, sha256:<8 hex>]`. The 8-char sha256 prefix
/// lets callers detect whether two redacted slots hold the same underlying
/// value without leaking the value itself (useful for spotting
/// configuration drift between e.g. `.env.local` and `.env`).
pub fn redact_display(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    let fp: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("[redacted, {} chars, sha256:{fp}]", raw.chars().count())
}

/// Resolve all value bindings for `name`, drawing from every source on the
/// indexed `RepoIndex`. Returned vector is sorted by precedence (highest first,
/// i.e. lowest precedence number first).
pub fn resolve_value_bindings(
    name: &str,
    index: &crate::model::RepoIndex,
    opts: &ValueResolutionOpts,
) -> Vec<ValueBinding> {
    let mut bindings = Vec::new();
    let is_secret = is_secret_key(name);

    for env_file in &index.env_files {
        for var in &env_file.vars {
            if var.name != name {
                continue;
            }
            let raw = var
                .raw_value
                .as_deref()
                .or(var.value_preview.as_deref())
                .unwrap_or("");
            let (state, display, redacted) = render(raw, is_secret, opts);
            bindings.push(ValueBinding {
                state,
                display,
                source: BindingSource::EnvFile {
                    path: env_file.path.clone(),
                    precedence: env_file.precedence,
                },
                redacted,
            });
        }
    }

    for profile in &index.profiles {
        for var in &profile.vars {
            if var.name != name {
                continue;
            }
            let raw = var
                .raw_value
                .as_deref()
                .or(var.value_preview.as_deref())
                .unwrap_or("");
            let (state, display, redacted) = render(raw, is_secret, opts);
            bindings.push(ValueBinding {
                state,
                display,
                source: BindingSource::DeployProfile {
                    name: profile.name.clone(),
                    path: profile.path.clone(),
                },
                redacted,
            });
        }
    }

    for secret_set in &index.secret_sets {
        for var in &secret_set.vars {
            if var.name != name {
                continue;
            }
            let raw = var
                .raw_value
                .as_deref()
                .or(var.value_preview.as_deref())
                .unwrap_or("");
            let (state, display, redacted) = render(raw, is_secret, opts);
            bindings.push(ValueBinding {
                state,
                display,
                source: BindingSource::SecretSet {
                    name: secret_set.name.clone(),
                    path: secret_set.path.clone(),
                },
                redacted,
            });
        }
    }

    // Shell environment — checked at query time, not stored in the index.
    // Precedence -1 puts this above all file-based sources.
    if let Ok(val) = std::env::var(name) {
        let (state, display, redacted) = render(&val, is_secret, opts);
        bindings.push(ValueBinding {
            state,
            display,
            source: BindingSource::ShellEnv,
            redacted,
        });
    }

    // Kubernetes ConfigMap entries found during indexing.
    for cm in &index.k8s_configmaps {
        if let Some(val) = cm.entries.get(name) {
            let (state, display, redacted) = render(val, is_secret, opts);
            bindings.push(ValueBinding {
                state,
                display,
                source: BindingSource::K8sConfigMap {
                    path: cm.path.clone(),
                    map_name: cm.map_name.clone(),
                },
                redacted,
            });
        }
    }

    bindings.sort_by_key(|b| b.source.precedence());
    bindings
}

/// Translate a raw value into a (state, display, redacted) triple.
///
/// Rules:
///   - empty raw → (Empty, "", false)
///   - secret-keyed and not opted in → (Set, "[redacted, N chars, sha256:…]", true)
///   - non-secret multi-line → (Set, "<first line> […N more lines]", false)
///   - otherwise → (Set, raw, false)
fn render(raw: &str, is_secret: bool, opts: &ValueResolutionOpts) -> (ValueState, String, bool) {
    if raw.is_empty() {
        return (ValueState::Empty, String::new(), false);
    }
    if is_secret && !opts.show_secrets {
        return (ValueState::Set, redact_display(raw), true);
    }
    if !is_secret && let Some(idx) = raw.find('\n') {
        let first_line = &raw[..idx];
        let n_more = raw[idx..].matches('\n').count();
        return (
            ValueState::Set,
            format!("{first_line} […{n_more} more lines]"),
            false,
        );
    }
    (ValueState::Set, raw.to_string(), false)
}
