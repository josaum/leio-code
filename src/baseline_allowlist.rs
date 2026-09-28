//! Substring allowlist for `--strict` doctor/status passes.
//!
//! When the CLI is asked to fail-on-warning, an operator can pre-declare a
//! list of known-noise patterns in `.leio-code/baseline-allowlist.txt`
//! (one substring per line; `#` for comments). [`filter_blocking_warnings`]
//! drops every warning whose full text contains any allowlisted substring,
//! and the CLI exits non-zero only on the remaining warnings.
//!
//! Intentionally minimal: no regex, no per-doctor scoping. Operators who
//! need precision write longer, more specific substrings.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// Load allowlist lines: non-empty, non-`#` comment; substring match against full warning text.
pub fn load_allowlist(path: &Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// Warnings that are not allowlisted (substring match: any pattern contained in warning).
pub fn filter_blocking_warnings(warnings: &[String], allowed_substrings: &[String]) -> Vec<String> {
    if allowed_substrings.is_empty() {
        return warnings.to_vec();
    }
    warnings
        .iter()
        .filter(|warning| {
            !allowed_substrings
                .iter()
                .any(|pat| !pat.is_empty() && warning.contains(pat.as_str()))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn blocking_filters_substring() {
        let w = vec!["[deploy] bad".to_string(), "[x] ok known".to_string()];
        let allow = vec!["known".to_string()];
        let b = filter_blocking_warnings(&w, &allow);
        assert_eq!(b, vec!["[deploy] bad".to_string()]);
    }

    #[test]
    fn empty_allowlist_keeps_all() {
        let w = vec!["a".to_string()];
        let b = filter_blocking_warnings(&w, &[]);
        assert_eq!(b, w);
    }

    #[test]
    fn missing_allowlist_is_an_empty_baseline() {
        let tmp = TempDir::new().expect("tempdir");

        let allow = load_allowlist(&tmp.path().join("missing.txt")).expect("missing is valid");

        assert!(allow.is_empty());
    }

    #[test]
    fn load_allowlist_trims_entries_and_ignores_blank_and_comment_lines() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("baseline-allowlist.txt");
        fs::write(
            &path,
            "\n  # known noise\n deploy timeout  \n\tredis:no-ttl\t\ninline # text\n",
        )
        .expect("write allowlist");

        let allow = load_allowlist(&path).expect("load allowlist");

        assert_eq!(
            allow,
            vec!["deploy timeout", "redis:no-ttl", "inline # text"]
        );
    }

    #[test]
    fn load_allowlist_reports_the_source_path_on_read_failure() {
        let tmp = TempDir::new().expect("tempdir");

        let error = load_allowlist(tmp.path()).expect_err("a directory is not an allowlist file");

        assert!(
            error
                .to_string()
                .contains(&format!("read {}", tmp.path().display())),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn empty_patterns_never_suppress_warnings_and_matching_is_case_sensitive() {
        let warnings = vec![
            "Known deploy timeout".to_string(),
            "known deploy timeout".to_string(),
            "different warning".to_string(),
        ];
        let allow = vec![String::new(), "known deploy".to_string()];

        let blocking = filter_blocking_warnings(&warnings, &allow);

        assert_eq!(
            blocking,
            vec![
                "Known deploy timeout".to_string(),
                "different warning".to_string()
            ]
        );
    }
}
