//! `package.json` `"scripts"` → invoked-binary edge detector.
//!
//! Parses the file with `serde_json`, walks the `scripts` object, and for each
//! `(name, value)` pair tokenizes the value on common shell separators
//! (`&&`, `||`, `;`, `|`) and takes the first whitespace-delimited token of
//! each tokenized command. When that token matches a relaxed binary-name
//! pattern, it's recorded.
//!
//! Phase 3 (P0 #2): script values whose first token is a `$VAR` / `${VAR}`
//! expansion are now emitted as [`UnresolvedEdge`]s with reason `NpmVariable`
//! rather than silently dropped.
//!
//! `npx some-tool` records `npx`, not `some-tool`; resolving `npx` to its
//! actual subcommand is out of scope and would need a real shell parser.
//!
//! Line attribution: `serde_json` doesn't preserve line numbers, so we do a
//! second pass over the raw source with a small regex to find the line where
//! each script name is declared. We restrict that search to the region between
//! the `"scripts"` key and the matching close brace so a `"build:"` substring
//! inside another script's value doesn't poison the lookup.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::model::{SourceLanguage, SubprocessCallOccurrence, UnresolvedEdge, UnresolvedReason};

use super::{DetectorOutput, truncate_snippet};

static BINARY_NAME_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Allow leading `@` (scoped packages like `@scope/cmd`), `/` (absolute
    // paths), `_`, letters; body characters cover what `node`-style CLIs use.
    Regex::new(r"^[A-Za-z_@/][A-Za-z0-9_./-]*$").expect("npm binary regex")
});

/// Detect literal binary invocations across all values of `.scripts` in a
/// `package.json`. Returns an empty result when the source isn't valid JSON or
/// when there's no `scripts` object.
pub fn detect_npm_script_invocations(path: &str, source: &str) -> DetectorOutput {
    let mut out = DetectorOutput::default();
    let Ok(parsed) = serde_json::from_str::<Value>(source) else {
        return out;
    };
    let Some(scripts) = parsed.get("scripts").and_then(Value::as_object) else {
        return out;
    };

    let scripts_region = scripts_region(source);

    for (name, value) in scripts {
        let Some(raw) = value.as_str() else { continue };
        let line = script_name_line(source, scripts_region, name).unwrap_or(1);
        for cmd in split_shell_pipeline(raw) {
            let Some(first) = cmd.split_whitespace().next() else {
                continue;
            };
            // Phase 3: `$VAR` / `${VAR}` is unresolved, not skipped.
            if first.starts_with('$') {
                out.unresolved.push(UnresolvedEdge {
                    source_path: path.to_string(),
                    source_line: line,
                    source_language: SourceLanguage::NpmScript,
                    edge_kind: "script_invocation".to_string(),
                    reason: UnresolvedReason::NpmVariable,
                    raw_snippet: truncate_snippet(cmd),
                });
                continue;
            }
            if first.contains('{') {
                // Other placeholder shape we don't recognise — skip.
                continue;
            }
            if !BINARY_NAME_PATTERN.is_match(first) {
                continue;
            }
            out.spawns.push(SubprocessCallOccurrence {
                binary: first.to_string(),
                path: path.to_string(),
                line,
                language: SourceLanguage::NpmScript,
                resolved_via_dataflow: false,
            });
        }
    }
    out
}

/// Best-effort split on `&&`, `||`, `;`, `|`. Not a real shell parser — quoted
/// regions aren't honoured. Good enough for npm script values, which are
/// overwhelmingly simple chained commands.
fn split_shell_pipeline(value: &str) -> Vec<&str> {
    let mut parts = vec![value];
    for sep in ["&&", "||", ";", "|"] {
        parts = parts
            .into_iter()
            .flat_map(|chunk| chunk.split(sep))
            .collect();
    }
    parts
}

/// Locate the byte range covering the `"scripts"` value object (open brace to
/// matching close brace). Used to bound the line-number lookup so a script
/// value can't masquerade as another script's name.
fn scripts_region(source: &str) -> Option<(usize, usize)> {
    let key_idx = source.find("\"scripts\"")?;
    let brace_start = source[key_idx..].find('{')? + key_idx;
    let mut depth = 0;
    for (i, byte) in source[brace_start..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((brace_start, brace_start + i));
                }
            }
            _ => {}
        }
    }
    None
}

fn script_name_line(source: &str, region: Option<(usize, usize)>, name: &str) -> Option<usize> {
    let (start, end) = region?;
    let needle = format!("\"{name}\"");
    let local = source[start..end].find(&needle)?;
    let absolute = start + local;
    Some(super::line_number_for_offset(source, absolute))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_single_script_invocation() {
        let src = r#"{ "scripts": { "build": "tsc" } }"#;
        let out = detect_npm_script_invocations("package.json", src);
        assert_eq!(out.spawns.len(), 1);
        assert_eq!(out.spawns[0].binary, "tsc");
        assert_eq!(out.spawns[0].language, SourceLanguage::NpmScript);
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn detects_pipeline_commands() {
        let src = r#"{ "scripts": { "ci": "lint && test && build" } }"#;
        let out = detect_npm_script_invocations("package.json", src);
        let bins: Vec<&str> = out.spawns.iter().map(|h| h.binary.as_str()).collect();
        assert_eq!(bins, vec!["lint", "test", "build"]);
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn skips_variable_substitution() {
        let src = r#"{ "scripts": { "run": "${RUNNER} foo" } }"#;
        let out = detect_npm_script_invocations("package.json", src);
        assert!(out.spawns.is_empty());
        assert_eq!(out.unresolved.len(), 1);
        assert_eq!(out.unresolved[0].reason, UnresolvedReason::NpmVariable);
    }

    #[test]
    fn invalid_json_returns_empty() {
        let out = detect_npm_script_invocations("package.json", "{ not json");
        assert!(out.spawns.is_empty());
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn no_scripts_returns_empty() {
        let out = detect_npm_script_invocations("package.json", r#"{"name":"x"}"#);
        assert!(out.spawns.is_empty());
        assert!(out.unresolved.is_empty());
    }
}
