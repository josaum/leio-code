//! Makefile recipe → invoked-binary edge detector.
//!
//! Recipe lines in a Makefile begin with a literal tab. We strip the tab, then
//! strip any combination of GNU Make's per-line prefixes (`@` quiet, `-` ignore
//! errors, `+` recursive-make), tokenize on whitespace, and look at the first
//! non-empty token. When that token is a bare identifier (`^[A-Za-z_][A-Za-z0-9_-]*$`),
//! we record a [`SubprocessCallOccurrence`] with `language = Make`.
//!
//! Phase 3 (P0 #2): recipe lines whose first token is a `$(VAR)` / `${VAR}`
//! expansion are now emitted as [`UnresolvedEdge`]s with reason
//! `MakefileVariable` rather than silently dropped.
//!
//! This is not a Makefile parser. We don't follow variable bindings, conditional
//! blocks, or line continuations. The intent is to surface the obvious
//! `cargo build` / `pytest` / `tsc` invocations that show up in most repos.

use std::sync::LazyLock;

use regex::Regex;

use crate::model::{SourceLanguage, SubprocessCallOccurrence, UnresolvedEdge, UnresolvedReason};

use super::{DetectorOutput, truncate_snippet};

static BINARY_NAME_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_-]*$").expect("makefile binary regex"));

/// Detect first-token binary invocations in Makefile recipes. `path` should be
/// the repo-relative path of the Makefile; `source` is its full contents.
pub fn detect_makefile_invocations(path: &str, source: &str) -> DetectorOutput {
    let mut out = DetectorOutput::default();
    for (idx, line) in source.lines().enumerate() {
        let line_no = idx + 1;
        // Recipe lines start with a literal tab. Anything else (target headers,
        // variable assignments, comments, blank lines) is ignored.
        let Some(body) = line.strip_prefix('\t') else {
            continue;
        };
        let stripped = strip_recipe_prefixes(body);
        let Some(first) = stripped.split_whitespace().next() else {
            continue;
        };
        // Phase 3: `$(VAR)` / `${VAR}` expansion is unresolved, not skipped.
        if first.starts_with("$(") || first.starts_with("${") {
            out.unresolved.push(UnresolvedEdge {
                source_path: path.to_string(),
                source_line: line_no,
                source_language: SourceLanguage::Make,
                edge_kind: "script_invocation".to_string(),
                reason: UnresolvedReason::MakefileVariable,
                raw_snippet: truncate_snippet(line),
            });
            continue;
        }
        if !BINARY_NAME_PATTERN.is_match(first) {
            continue;
        }
        out.spawns.push(SubprocessCallOccurrence {
            binary: first.to_string(),
            path: path.to_string(),
            line: line_no,
            language: SourceLanguage::Make,
            resolved_via_dataflow: false,
        });
    }
    out
}

/// GNU Make recipe lines may carry any combination of `@` (suppress echo),
/// `-` (ignore exit status), and `+` (always exec even under `-n`) before the
/// real command. Strip them and any whitespace between/after.
fn strip_recipe_prefixes(body: &str) -> &str {
    let mut s = body.trim_start();
    loop {
        let next = s.strip_prefix(|c: char| matches!(c, '@' | '-' | '+'));
        match next {
            Some(rest) => s = rest.trim_start(),
            None => break,
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_plain_recipe_command() {
        let src = "build:\n\tcargo build\n";
        let out = detect_makefile_invocations("Makefile", src);
        assert_eq!(out.spawns.len(), 1);
        assert_eq!(out.spawns[0].binary, "cargo");
        assert_eq!(out.spawns[0].line, 2);
        assert_eq!(out.spawns[0].language, SourceLanguage::Make);
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn strips_at_dash_plus_prefixes() {
        let src = "x:\n\t@-+cargo test\n";
        let out = detect_makefile_invocations("Makefile", src);
        assert_eq!(out.spawns.len(), 1);
        assert_eq!(out.spawns[0].binary, "cargo");
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn skips_variable_expansion() {
        let src = "x:\n\t$(CARGO) build\n";
        let out = detect_makefile_invocations("Makefile", src);
        assert!(out.spawns.is_empty());
        assert_eq!(out.unresolved.len(), 1);
        assert_eq!(out.unresolved[0].reason, UnresolvedReason::MakefileVariable);
    }

    #[test]
    fn skips_non_recipe_lines() {
        let src = "VAR = cargo\nbuild:\n# comment line\n\tcargo build\n";
        let out = detect_makefile_invocations("Makefile", src);
        assert_eq!(out.spawns.len(), 1);
        assert_eq!(out.spawns[0].line, 4);
        assert!(out.unresolved.is_empty());
    }
}
