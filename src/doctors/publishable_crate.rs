//! Doctor: `publishable-crate`
//!
//! Wires down the **Example Sovereign Crate contract** — the rules that make a
//! crate a reusable, standalone, potentially-publishable (even public) crate.
//!
//! The footgun this closes: every one of the workspace's ~84 `Cargo.toml`
//! files defaults to `publish = true`, yet flagship sovereign primitives
//! (`fca-fast-core`, `example-ocr-models`, …) ship with bare metadata that
//! could never actually `cargo publish`. Intent is implicit and quality is
//! unenforced.
//!
//! ## The contract (opt-in, not default)
//!
//! A crate declares intent explicitly:
//!
//! ```toml
//! [package.metadata.example]
//! tier = "sovereign"        # this crate is a publishable standalone primitive
//! visibility = "internal"   # or "public" for crates.io-public-ready
//! ```
//!
//! For every crate that opts in (`tier = "sovereign"`) the doctor validates:
//!
//! - **Tier 1 — cargo-publish mechanics:** `version` (semver, not `0.0.0`),
//!   `edition`, `rust-version`; every internal `path = "..."` dependency also
//!   carries a `version` (path-only deps block `cargo publish`).
//! - **Tier 2 — discoverability metadata:** `description`, `license` (or
//!   `license-file`), `repository`, `readme`, `keywords`, `categories`,
//!   `authors`.
//! - **Tier 3 — standalone quality:** a `README.md` on disk and a `src/lib.rs`
//!   carrying `//!` crate-level docs.
//! - **Tier 4 — public-ready** (only when `visibility = "public"`): an
//!   OSI-approved license (`MIT`/`Apache-2.0`/dual) and a `CHANGELOG.md`.
//!
//! Workspace-inherited fields (`field.workspace = true`) count as present —
//! cargo resolves them at publish time.
//!
//! Crates that do not opt in are not validated, but the doctor counts those
//! that declare *neither* `publish = false` (internal) *nor* a sovereign tier
//! and surfaces that "undeclared publish intent" tally in `meta` (info, so it
//! never floods `audit --strict`).
//!
//! Vendored third-party crates (`**/vendor/**`) are out of scope.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// OSI-approved licenses accepted for a `visibility = "public"` crate.
const OSI_LICENSES: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "MIT OR Apache-2.0",
    "Apache-2.0 OR MIT",
    "BSD-3-Clause",
    "MPL-2.0",
    "ISC",
];

/// `[package]` fields required by Tier 2 (discoverability).  Each is "present"
/// if declared directly or inherited via `field.workspace = true`.
const TIER2_FIELDS: &[&str] = &[
    "description",
    "repository",
    "readme",
    "keywords",
    "categories",
    "authors",
];

pub struct PublishableCrateDoctor;

impl Doctor for PublishableCrateDoctor {
    fn name(&self) -> &'static str {
        "publishable-crate"
    }

    fn description(&self) -> &'static str {
        "Wires down the Example Sovereign Crate contract: for every crate that opts in via `[package.metadata.example] tier = \"sovereign\"`, validates cargo-publish mechanics (versioned path-deps, MSRV), discoverability metadata (license/description/repository/readme/keywords/categories/authors), standalone quality (README.md + lib.rs //! docs), and — when public — an OSI license + CHANGELOG. Surfaces the count of crates with undeclared publish intent."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_publishable_crate(root)
    }
}

/// True when a `[package]` field is present directly or workspace-inherited.
fn field_present(pkg: &toml::Value, name: &str) -> bool {
    pkg.get(name).is_some()
}

/// `"1.97.1"` / `"1.97"` → `"1.97"`. Patch level is not part of an MSRV contract.
fn minor_version(value: &str) -> Option<String> {
    let mut parts = value.trim().split('.');
    let major = parts.next()?.trim();
    let minor = parts.next()?.trim();
    if major.is_empty() || minor.is_empty() {
        return None;
    }
    Some(format!("{major}.{minor}"))
}

/// The project's canonical stable toolchain, as `X.Y`, from `rust-toolchain.toml`.
///
/// MSRV tracks stable by contract, and `rust-toolchain.toml` is the single place
/// that says which stable. Returns `None` when the file is absent or pins a
/// named channel (`stable`, `nightly`) rather than a version — there is nothing
/// to compare against, so the check stays quiet instead of guessing.
fn canonical_toolchain_minor(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("rust-toolchain.toml")).ok()?;
    let channel = text
        .parse::<toml::Value>()
        .ok()?
        .get("toolchain")?
        .get("channel")?
        .as_str()?
        .to_string();
    minor_version(&channel)
}

pub fn doctor_publishable_crate(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut entities: Vec<serde_json::Value> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let mut sovereign = 0usize;
    let mut public = 0usize;
    let mut undeclared: Vec<String> = Vec::new();
    let canonical_toolchain_minor = canonical_toolchain_minor(root);

    let walker = ignore::WalkBuilder::new(root).hidden(false).build();
    for entry in walker.flatten() {
        if entry.file_name() != "Cargo.toml" {
            continue;
        }
        let abs = entry.path();
        let rel = abs
            .strip_prefix(root)
            .unwrap_or(abs)
            .to_string_lossy()
            .replace('\\', "/");
        // Third-party vendored crates are not ours to publish or govern.
        if rel.contains("/vendor/") || rel.starts_with("vendor/") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(abs) else {
            continue;
        };
        let Ok(manifest) = text.parse::<toml::Value>() else {
            continue;
        };
        // Virtual workspace roots have no `[package]`; nothing to validate.
        let Some(pkg) = manifest.get("package") else {
            continue;
        };
        let name = pkg
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("<unnamed>")
            .to_string();

        let example_meta = pkg.get("metadata").and_then(|m| m.get("example"));
        let tier = example_meta
            .and_then(|m| m.get("tier"))
            .and_then(|t| t.as_str());
        let is_sovereign = tier == Some("sovereign");

        if !is_sovereign {
            // Footgun tally: crate neither declared internal nor sovereign.
            let declared_internal = matches!(pkg.get("publish"), Some(toml::Value::Boolean(false)));
            if !declared_internal {
                undeclared.push(rel.clone());
            }
            continue;
        }
        sovereign += 1;

        let visibility = example_meta
            .and_then(|m| m.get("visibility"))
            .and_then(|v| v.as_str())
            .unwrap_or("internal");
        let is_public = visibility == "public";
        if is_public {
            public += 1;
        }

        let mut missing: BTreeSet<String> = BTreeSet::new();

        // Tier 1 — cargo-publish mechanics.
        if !field_present(pkg, "version") {
            missing.insert("version".into());
        } else if pkg.get("version").and_then(|v| v.as_str()) == Some("0.0.0") {
            missing.insert("version (must be > 0.0.0)".into());
        }
        if !field_present(pkg, "edition") {
            missing.insert("edition".into());
        }
        if !field_present(pkg, "rust-version") {
            missing.insert("rust-version (MSRV)".into());
        } else if let Some(channel) = canonical_toolchain_minor.as_deref() {
            // MSRV tracks stable by contract. A workspace-inherited value is
            // resolved by cargo against the same workspace pin, so only an
            // explicit literal can drift.
            if let Some(declared) = pkg.get("rust-version").and_then(|v| v.as_str())
                && minor_version(declared).as_deref() != Some(channel)
            {
                missing.insert(format!(
                    "rust-version must track stable `{channel}` from rust-toolchain.toml, found `{declared}`"
                ));
            }
        }
        // Path-only deps (no `version`) block `cargo publish`.
        for table in ["dependencies", "build-dependencies"] {
            let Some(deps) = manifest.get(table).and_then(|d| d.as_table()) else {
                continue;
            };
            for (dep_name, dep_val) in deps {
                let Some(dep_tbl) = dep_val.as_table() else {
                    continue;
                };
                if dep_tbl.contains_key("path") && !dep_tbl.contains_key("version") {
                    missing.insert(format!(
                        "version on path-dep `{dep_name}` (path-only deps block cargo publish)"
                    ));
                }
            }
        }

        // Tier 2 — discoverability metadata.
        for field in TIER2_FIELDS {
            if !field_present(pkg, field) {
                missing.insert((*field).to_string());
            }
        }
        if !field_present(pkg, "license") && !field_present(pkg, "license-file") {
            missing.insert("license (or license-file)".into());
        }

        // Tier 3 — standalone quality (files on disk).
        let crate_dir = abs.parent().unwrap_or(root);
        if !crate_dir.join("README.md").exists() {
            missing.insert("README.md (file on disk)".into());
        }
        let lib_rs = crate_dir.join("src/lib.rs");
        if lib_rs.exists() {
            let lib_text = std::fs::read_to_string(&lib_rs).unwrap_or_default();
            if !lib_text.contains("//!") {
                missing.insert("src/lib.rs //! crate-level docs".into());
            }
        }

        // Tier 4 — public-ready extras.
        if is_public {
            let license = pkg.get("license").and_then(|v| v.as_str());
            let osi_ok = license.map(|l| OSI_LICENSES.contains(&l)).unwrap_or(false);
            if !osi_ok {
                missing.insert(format!(
                    "OSI-approved license for public crate (got {license:?}; need MIT/Apache-2.0/dual)"
                ));
            }
            if !crate_dir.join("CHANGELOG.md").exists() {
                missing.insert("CHANGELOG.md (required for public crates)".into());
            }
        }

        for field in &missing {
            warnings.push(format!(
                "[{name}] {rel}: sovereign{} crate missing `{field}`",
                if is_public { "/public" } else { "" }
            ));
            evidence.push(EvidenceItem {
                kind: "sovereign_contract_violation".to_string(),
                path: rel.clone(),
                line: None,
                detail: format!(
                    "crate `{name}` is sovereign but does not satisfy contract field `{field}`"
                ),
            });
        }
        entities.push(json!({
            "doctor": "publishable-crate",
            "crate": name,
            "manifest": rel,
            "visibility": visibility,
            "violations": missing.iter().collect::<Vec<_>>(),
        }));
    }

    let summary = format!(
        "publishable-crate: {sovereign} sovereign crate(s) ({public} public), {} contract warning(s); {} crate(s) with undeclared publish intent",
        warnings.len(),
        undeclared.len()
    );

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_publishable_crate"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.85 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "sovereign_crates": sovereign,
            "public_crates": public,
            "undeclared_publish_intent_count": undeclared.len(),
            "undeclared_publish_intent": undeclared,
            "contract": "docs/architecture/publishable-crate-contract.md",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn non_sovereign_crates_are_not_validated_but_tallied() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "internal/Cargo.toml",
            "[package]\nname = \"internal\"\nversion = \"0.1.0\"\n",
        );
        let env = doctor_publishable_crate(tmp.path());
        assert!(env.warnings.is_empty(), "non-sovereign crate must not warn");
        let meta = env.meta.unwrap();
        assert_eq!(meta["undeclared_publish_intent_count"], 1);
    }

    #[test]
    fn sovereign_crate_missing_metadata_warns_with_teeth() {
        let tmp = tempfile::tempdir().unwrap();
        // Opts in but is missing description/license/repository/readme/etc.
        write(
            tmp.path(),
            "primitive/Cargo.toml",
            "[package]\nname = \"primitive\"\nversion = \"0.1.0\"\nedition = \"2021\"\nrust-version = \"1.80\"\n\n[package.metadata.example]\ntier = \"sovereign\"\n",
        );
        let env = doctor_publishable_crate(tmp.path());
        assert!(
            !env.warnings.is_empty(),
            "sovereign crate missing metadata MUST warn"
        );
        let joined = env.warnings.join("\n");
        assert!(
            joined.contains("description"),
            "should flag missing description: {joined}"
        );
        assert!(
            joined.contains("license"),
            "should flag missing license: {joined}"
        );
        assert_eq!(env.meta.unwrap()["sovereign_crates"], 1);
    }

    #[test]
    fn fully_compliant_sovereign_crate_holds() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "good/README.md", "# good\nUsage example.\n");
        write(
            tmp.path(),
            "good/src/lib.rs",
            "//! good crate\npub fn x() {}\n",
        );
        write(
            tmp.path(),
            "good/Cargo.toml",
            "[package]\nname = \"good\"\nversion = \"0.1.0\"\nedition = \"2021\"\nrust-version = \"1.80\"\ndescription = \"d\"\nlicense = \"MIT\"\nrepository = \"https://example/good\"\nreadme = \"README.md\"\nkeywords = [\"a\"]\ncategories = [\"parsing\"]\nauthors = [\"x\"]\n\n[package.metadata.example]\ntier = \"sovereign\"\n",
        );
        let env = doctor_publishable_crate(tmp.path());
        assert!(
            env.warnings.is_empty(),
            "compliant sovereign crate must hold: {:?}",
            env.warnings
        );
    }

    /// MSRV tracks stable by contract. Without this, every sovereign crate sat
    /// at 1.94 while `rust-toolchain.toml` had moved to 1.97 and nothing said so.
    fn sovereign_with_msrv(root: &Path, msrv: &str) {
        write(root, "good/README.md", "# good\nUsage example.\n");
        write(root, "good/src/lib.rs", "//! good crate\npub fn x() {}\n");
        write(
            root,
            "good/Cargo.toml",
            &format!(
                "[package]\nname = \"good\"\nversion = \"0.1.0\"\nedition = \"2021\"\nrust-version = \"{msrv}\"\ndescription = \"d\"\nlicense = \"MIT\"\nrepository = \"https://example/good\"\nreadme = \"README.md\"\nkeywords = [\"a\"]\ncategories = [\"parsing\"]\nauthors = [\"x\"]\n\n[package.metadata.example]\ntier = \"sovereign\"\n"
            ),
        );
    }

    #[test]
    fn sovereign_msrv_behind_the_canonical_toolchain_warns() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"1.97.1\"\n",
        );
        sovereign_with_msrv(tmp.path(), "1.94");
        let env = doctor_publishable_crate(tmp.path());
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("must track stable `1.97`") && w.contains("found `1.94`")),
            "{:?}",
            env.warnings
        );
    }

    #[test]
    fn sovereign_msrv_matching_the_canonical_toolchain_holds() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"1.97.1\"\n",
        );
        // Patch level is not part of an MSRV contract: `1.97` matches `1.97.1`.
        sovereign_with_msrv(tmp.path(), "1.97");
        let env = doctor_publishable_crate(tmp.path());
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
    }

    /// A named channel gives nothing to compare against, so the check stays
    /// quiet rather than guessing a version.
    #[test]
    fn a_named_toolchain_channel_does_not_trigger_the_msrv_check() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"stable\"\n",
        );
        sovereign_with_msrv(tmp.path(), "1.80");
        let env = doctor_publishable_crate(tmp.path());
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
    }
}
