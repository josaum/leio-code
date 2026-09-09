//! JavaScript / TypeScript `child_process.<fn>("literal", …)` detector.
//!
//! Parallel to the Python detector in `python_subprocess.rs`: line-anchored
//! regex scanning, literal first-arg → `SubprocessCallOccurrence`. Phase 3
//! (P0 #2): anything dynamic that we used to drop silently is now emitted
//! as an [`UnresolvedEdge`] with a classified reason.
//!
//! What we detect:
//!   child_process.spawn("bin", [...])
//!   child_process.exec("bin --arg")          // string form — first token wins
//!   child_process.execFile("bin", [...])
//!   child_process.fork("script.js", [...])
//!
//! And the bare form (no `child_process.` prefix) when the file contains a
//! `child_process` import — either CommonJS `require("child_process")` or an
//! ES `from "child_process"` clause.
//!
//! Phase 3 classification:
//!   - First arg is a backtick template literal → `TemplateOrConcat`.
//!   - First arg contains `+` before the comma → `TemplateOrConcat` (concat).
//!   - First arg is a bare identifier / expression → `NonLiteralFirstArg`.
//!
//! Limitations:
//!   - We do not track imports across files. The bare form is only enabled
//!     when the *same file* contains the `child_process` import string.
//!   - For `exec`, the captured string is split on whitespace and we take the
//!     first token. Empty / pure-whitespace strings are dropped.

use std::sync::LazyLock;

use regex::Regex;

use crate::model::{SourceLanguage, SubprocessCallOccurrence, UnresolvedEdge, UnresolvedReason};

use super::dataflow::{self, ConcatPart, ResolveOutcome, SubstituteExpr};
use super::{DetectorOutput, line_at_offset, line_number_for_offset, truncate_snippet};

// Prefixed wide form: matches the call opening regardless of first-arg shape.
// Captures: 1 = fn name.
static CHILD_PROCESS_PREFIXED_WIDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"child_process\.(spawn|exec|execFile|fork)\s*\(")
        .expect("child_process prefixed wide regex compiles")
});

// Bare wide form (file must import child_process to use it).
static CHILD_PROCESS_BARE_WIDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^.\w])(spawn|exec|execFile|fork)\s*\(")
        .expect("child_process bare wide regex compiles")
});

// Literal forms: identical to pre-Phase-3 regexes. Used to test a candidate's
// first arg.
static CHILD_PROCESS_LITERAL_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*(?:"([^"$`]+?)"|'([^'$`]+?)')\s*[,)]"#)
        .expect("child_process literal head regex compiles")
});

/// Detect JavaScript / TypeScript `child_process.<fn>("literal", …)` calls
/// plus unresolved candidates (template literal / concat / variable).
pub fn detect_js_subprocess_calls(
    source: &str,
    path: &str,
    language: SourceLanguage,
) -> DetectorOutput {
    let mut out = DetectorOutput::default();

    // Prefixed form runs unconditionally.
    for caps in CHILD_PROCESS_PREFIXED_WIDE.captures_iter(source) {
        let whole = caps.get(0).expect("group 0 always present");
        let fn_name = caps.get(1).map(|g| g.as_str()).unwrap_or("");
        classify_js_call(
            source,
            whole.start(),
            whole.end(),
            fn_name,
            path,
            language,
            &mut out,
        );
    }

    // Bare form: only when the source mentions `"child_process"` /
    // `'child_process'` (require / from import).
    if has_child_process_import(source) {
        for caps in CHILD_PROCESS_BARE_WIDE.captures_iter(source) {
            let whole = caps.get(0).expect("group 0 always present");
            let fn_match = caps.get(1).expect("fn capture present");
            let fn_name = fn_match.as_str();
            // Line attribution maps to `fn_name` start (skip the boundary char).
            classify_js_call(
                source,
                fn_match.start(),
                whole.end(),
                fn_name,
                path,
                language,
                &mut out,
            );
        }
    }

    out
}

/// Classify a single candidate site. `fn_start` is where the fn name begins
/// (for line attribution); `args_start` is the position of the opening `(`
/// plus one (i.e. the `tail_start` after the `(`).
fn classify_js_call(
    source: &str,
    fn_start: usize,
    args_start: usize,
    fn_name: &str,
    path: &str,
    language: SourceLanguage,
    out: &mut DetectorOutput,
) {
    let line = line_number_for_offset(source, fn_start);
    let raw_snippet = truncate_snippet(line_at_offset(source, fn_start));

    // Try literal head: `"..."` or `'...'` followed by `,` or `)`.
    let slice = &source[args_start..];
    if let Some(caps) = CHILD_PROCESS_LITERAL_HEAD.captures(slice) {
        let raw = caps
            .get(1)
            .or_else(|| caps.get(2))
            .map(|g| g.as_str())
            .unwrap_or("");
        if let Some(binary) = extract_binary(fn_name, raw) {
            out.spawns.push(SubprocessCallOccurrence {
                binary,
                path: path.to_string(),
                line,
                language,
                resolved_via_dataflow: false,
            });
            return;
        }
        // Literal but empty / placeholder-y — still unresolved.
    }

    // Phase 8: try one-hop dataflow before emitting unresolved.
    let reason = classify_js_first_arg(slice);
    if let Some(expr) = js_first_arg_expr(slice) {
        let outcome = dataflow::resolve_one_hop(source, fn_start, language, &expr);
        match outcome {
            ResolveOutcome::Resolved { value } => {
                // Apply the `exec` first-token rule for resolved values too.
                if let Some(binary) = extract_binary(fn_name, &value) {
                    out.spawns.push(SubprocessCallOccurrence {
                        binary,
                        path: path.to_string(),
                        line,
                        language,
                        resolved_via_dataflow: true,
                    });
                    return;
                }
            }
            ResolveOutcome::Ambiguous => {
                out.unresolved.push(UnresolvedEdge {
                    source_path: path.to_string(),
                    source_line: line,
                    source_language: language,
                    edge_kind: "subprocess_spawn".to_string(),
                    reason: UnresolvedReason::AmbiguousAssignment,
                    raw_snippet,
                });
                return;
            }
            ResolveOutcome::ResolvedTemplate { .. } | ResolveOutcome::Unresolvable => {}
        }
    }
    out.unresolved.push(UnresolvedEdge {
        source_path: path.to_string(),
        source_line: line,
        source_language: language,
        edge_kind: "subprocess_spawn".to_string(),
        reason,
        raw_snippet,
    });
}

/// Build a substitution expression from a JS subprocess call's first arg
/// (the source text just after `(`).
fn js_first_arg_expr(slice: &str) -> Option<SubstituteExpr> {
    let head = slice.trim_start();
    if let Some(rest) = head.strip_prefix('`')
        && let Some(end) = rest.find('`')
    {
        let body = &rest[..end];
        let segs = dataflow::parse_js_template(body)?;
        return Some(SubstituteExpr::Template { segments: segs });
    }
    let stop = head.find([',', ')']).unwrap_or(head.len());
    let arg = head[..stop].trim();
    if arg.is_empty() {
        return None;
    }
    if arg.contains('+') {
        let parts = dataflow::parse_concat(arg)?;
        if parts.iter().any(|p| matches!(p, ConcatPart::Name(_))) {
            return Some(SubstituteExpr::Concat { parts });
        }
        return None;
    }
    let bytes = arg.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let first = bytes[0] as char;
    if !(first.is_alphabetic() || first == '_' || first == '$') {
        return None;
    }
    if arg.bytes().all(|b| {
        let c = b as char;
        c.is_alphanumeric() || c == '_' || c == '$'
    }) {
        return Some(SubstituteExpr::BareName(arg.to_string()));
    }
    None
}

fn classify_js_first_arg(args_slice: &str) -> UnresolvedReason {
    let head = args_slice.trim_start();
    // Template literal: starts with `` ` ``.
    if head.starts_with('`') {
        return UnresolvedReason::TemplateOrConcat;
    }
    // String concatenation: look for `+` in the head before the first comma
    // or closing paren (treating quoted regions naively — sufficient for the
    // shapes we see).
    let stop = head.find([',', ')']).unwrap_or(head.len());
    if head[..stop].contains('+') {
        return UnresolvedReason::TemplateOrConcat;
    }
    UnresolvedReason::NonLiteralFirstArg
}

/// `exec("bin --flag")` carries the whole command in the first arg; everyone
/// else carries just the binary. Tokenize on whitespace for `exec`, take as-is
/// for the rest, and drop empty results.
fn extract_binary(fn_name: &str, raw: &str) -> Option<String> {
    let candidate = if fn_name == "exec" {
        raw.split_whitespace().next().unwrap_or("")
    } else {
        raw
    };
    if candidate.is_empty() || candidate.contains('{') || candidate.contains('}') {
        return None;
    }
    Some(candidate.to_string())
}

fn has_child_process_import(source: &str) -> bool {
    source.contains("\"child_process\"") || source.contains("'child_process'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_prefixed_spawn() {
        let src = "child_process.spawn(\"leio-code\", [\"--help\"]);\n";
        let out = detect_js_subprocess_calls(src, "a.js", SourceLanguage::JavaScript);
        assert_eq!(out.spawns.len(), 1);
        assert_eq!(out.spawns[0].binary, "leio-code");
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn detects_exec_string_form_takes_first_token() {
        let src = "child_process.exec(\"leio-code --help\");\n";
        let out = detect_js_subprocess_calls(src, "a.js", SourceLanguage::JavaScript);
        assert_eq!(out.spawns.len(), 1);
        assert_eq!(out.spawns[0].binary, "leio-code");
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn skips_template_literal() {
        let src = "child_process.spawn(`bin-${x}`, []);\n";
        let out = detect_js_subprocess_calls(src, "a.js", SourceLanguage::JavaScript);
        assert!(out.spawns.is_empty());
        assert_eq!(out.unresolved.len(), 1);
        assert_eq!(out.unresolved[0].reason, UnresolvedReason::TemplateOrConcat);
    }

    #[test]
    fn skips_concat() {
        let src = "child_process.spawn(\"/usr/local/bin/\" + name, []);\n";
        let out = detect_js_subprocess_calls(src, "a.js", SourceLanguage::JavaScript);
        assert!(out.spawns.is_empty());
        assert_eq!(out.unresolved.len(), 1);
        assert_eq!(out.unresolved[0].reason, UnresolvedReason::TemplateOrConcat);
    }

    #[test]
    fn bare_form_requires_import() {
        // No import → bare form not even tried, no unresolved either.
        let no_import = "spawn(\"leio-code\", [\"--help\"]);\n";
        let with_import =
            "const { spawn } = require(\"child_process\");\nspawn(\"leio-code\", [\"--help\"]);\n";
        let out_no = detect_js_subprocess_calls(no_import, "a.js", SourceLanguage::JavaScript);
        assert!(out_no.spawns.is_empty());
        assert!(out_no.unresolved.is_empty());
        let out_yes = detect_js_subprocess_calls(with_import, "a.js", SourceLanguage::JavaScript);
        assert_eq!(out_yes.spawns.len(), 1);
        assert_eq!(out_yes.spawns[0].binary, "leio-code");
        assert!(out_yes.unresolved.is_empty());
    }
}
