//! Python `subprocess.<fn>(["literal-binary-name", ...])` detector.
//!
//! What we detect:
//!   subprocess.run(["bin", ...])
//!   subprocess.Popen(["bin", ...])
//!   subprocess.check_output(["bin", ...])
//!   subprocess.check_call(["bin", ...])
//!   subprocess.call(["bin", ...])
//!
//! Phase 3 (P0 #2): candidates that aren't a plain string literal — variables,
//! f-strings, `.format()`, `shell=True` — no longer drop silently; they're
//! emitted as [`UnresolvedEdge`]s with a classified reason.
//!
//! Limitations (recorded as unresolved where applicable rather than guessing):
//!   - Variables, f-strings, `.format()`, Path objects → `NonLiteralFirstArg`
//!     or `TemplateOrConcat`.
//!   - `shell=True` calls → `ShellInvocation`.
//!   - Multi-line list literals: only the line carrying the opening `[` is
//!     considered. A future tree-sitter-aided pass can fix this.
//!
//! The `regex` crate does not support backreferences, so we use a single
//! pattern with two quote-style alternations and pick whichever capture
//! matched.

use std::sync::LazyLock;

use regex::Regex;

use crate::model::{SourceLanguage, SubprocessCallOccurrence, UnresolvedEdge, UnresolvedReason};

use super::dataflow::{self, ConcatPart, ResolveOutcome, SubstituteExpr};
use super::{DetectorOutput, line_at_offset, line_number_for_offset, truncate_snippet};

/// Wide opening: any `subprocess.<fn>(` call, regardless of first arg shape.
/// Used to find candidate sites before deciding whether they're literal
/// (emit a `SubprocessCallOccurrence`) or unresolved.
static SUBPROCESS_OPENING_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"subprocess\.(?:run|Popen|check_output|check_call|call)\s*\(")
        .expect("subprocess opening regex compiles")
});

/// Literal form: first list element is a single- or double-quoted string with
/// no interpolation sigils.
static SUBPROCESS_LITERAL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^subprocess\.(?:run|Popen|check_output|check_call|call)\s*\(\s*\[\s*(?:"([^"$]+?)"|'([^'$]+?)')"#,
    )
    .expect("subprocess literal regex compiles")
});

/// Detect Python `subprocess.<fn>(...)` candidates.
///
/// Each opening is classified:
/// - Literal string list head → `SubprocessCallOccurrence`.
/// - `shell=True` anywhere in the call body → `UnresolvedEdge { ShellInvocation }`.
/// - First arg starts with `f"`/`f'` or contains `{...}` → `TemplateOrConcat`.
/// - Anything else (variable, expression) → `NonLiteralFirstArg`.
pub fn detect_python_subprocess_calls(source: &str, path: &str) -> DetectorOutput {
    let mut out = DetectorOutput::default();
    for m in SUBPROCESS_OPENING_PATTERN.find_iter(source) {
        let start = m.start();
        let line = line_number_for_offset(source, start);
        let raw_snippet = truncate_snippet(line_at_offset(source, start));

        // Find the matching close paren (naive: first `)` after the opening).
        // Same limitation the previous code had — accepted as scope.
        let tail_start = m.end();
        let tail_end = source[tail_start..]
            .find(')')
            .map(|i| tail_start + i)
            .unwrap_or(source.len());
        let call_body = &source[tail_start..tail_end];

        if call_body.contains("shell=True") {
            out.unresolved.push(UnresolvedEdge {
                source_path: path.to_string(),
                source_line: line,
                source_language: SourceLanguage::Python,
                edge_kind: "subprocess_spawn".to_string(),
                reason: UnresolvedReason::ShellInvocation,
                raw_snippet,
            });
            continue;
        }

        // Try the literal pattern against the slice starting at the call.
        let slice = &source[start..];
        if let Some(caps) = SUBPROCESS_LITERAL_PATTERN.captures(slice) {
            let binary = caps
                .get(1)
                .or_else(|| caps.get(2))
                .map(|g| g.as_str().to_string());
            if let Some(binary) = binary
                && !binary.is_empty()
                && !binary.contains('{')
                && !binary.contains('}')
            {
                out.spawns.push(SubprocessCallOccurrence {
                    binary,
                    path: path.to_string(),
                    line,
                    language: SourceLanguage::Python,
                    resolved_via_dataflow: false,
                });
                continue;
            }
        }

        // Phase 8: before emitting unresolved, try one-hop dataflow on
        // the first arg. If it resolves to a literal binary name we get a
        // proper SubprocessCallOccurrence at confidence band 80.
        let reason = classify_python_first_arg(call_body);
        let expr = python_first_arg_expr(call_body);
        if let Some(expr) = expr {
            let outcome = dataflow::resolve_one_hop(source, start, SourceLanguage::Python, &expr);
            match outcome {
                ResolveOutcome::Resolved { value } => {
                    out.spawns.push(SubprocessCallOccurrence {
                        binary: value,
                        path: path.to_string(),
                        line,
                        language: SourceLanguage::Python,
                        resolved_via_dataflow: true,
                    });
                    continue;
                }
                ResolveOutcome::Ambiguous => {
                    out.unresolved.push(UnresolvedEdge {
                        source_path: path.to_string(),
                        source_line: line,
                        source_language: SourceLanguage::Python,
                        edge_kind: "subprocess_spawn".to_string(),
                        reason: UnresolvedReason::AmbiguousAssignment,
                        raw_snippet,
                    });
                    continue;
                }
                ResolveOutcome::ResolvedTemplate { .. } | ResolveOutcome::Unresolvable => {
                    // Fall through to the original unresolved emission.
                }
            }
        }
        out.unresolved.push(UnresolvedEdge {
            source_path: path.to_string(),
            source_line: line,
            source_language: SourceLanguage::Python,
            edge_kind: "subprocess_spawn".to_string(),
            reason,
            raw_snippet,
        });
    }
    out
}

/// Extract a substitution expression from a Python subprocess call body
/// (the source text between `(` and `)`). Returns `None` if the shape isn't
/// one of the patterns we recognize.
fn python_first_arg_expr(body: &str) -> Option<SubstituteExpr> {
    let trimmed = body.trim_start();
    // Strip optional list bracket.
    let head = if let Some(rest) = trimmed.strip_prefix('[') {
        rest.trim_start()
    } else {
        trimmed
    };
    // f-string.
    if let Some(rest) = head.strip_prefix("f\"")
        && let Some(end) = rest.find('"')
    {
        let body = &rest[..end];
        let segs = dataflow::parse_python_fstring(body)?;
        return Some(SubstituteExpr::Template { segments: segs });
    }
    if let Some(rest) = head.strip_prefix("f'")
        && let Some(end) = rest.find('\'')
    {
        let body = &rest[..end];
        let segs = dataflow::parse_python_fstring(body)?;
        return Some(SubstituteExpr::Template { segments: segs });
    }
    // Find the first-arg substring: up to `,` or `]` or `)` at top level.
    let stop = head.find([',', ']']).unwrap_or(head.len());
    let arg = head[..stop].trim();
    if arg.is_empty() {
        return None;
    }
    // Concat?
    if arg.contains('+') {
        let parts = dataflow::parse_concat(arg)?;
        if parts.iter().any(|p| matches!(p, ConcatPart::Name(_))) {
            return Some(SubstituteExpr::Concat { parts });
        }
        return None;
    }
    // Bare identifier?
    let bytes = arg.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let first = bytes[0] as char;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    if arg.bytes().all(|b| {
        let c = b as char;
        c.is_alphanumeric() || c == '_'
    }) {
        return Some(SubstituteExpr::BareName(arg.to_string()));
    }
    None
}

/// Classify the first-arg shape for a Python `subprocess.<fn>(...)` call. The
/// input is the substring after the opening `(` up to (exclusive) the closing
/// `)`. Heuristics — not a Python parser.
fn classify_python_first_arg(body: &str) -> UnresolvedReason {
    let trimmed = body.trim_start();
    // List form: `[...`
    let head = if let Some(rest) = trimmed.strip_prefix('[') {
        rest.trim_start()
    } else {
        trimmed
    };
    // f-string: `f"..."` / `f'...'` / `rf"..."` etc.
    if head.starts_with("f\"")
        || head.starts_with("f'")
        || head.starts_with("rf\"")
        || head.starts_with("rf'")
    {
        return UnresolvedReason::TemplateOrConcat;
    }
    // Literal that contains `{...}` — `.format()` placeholder style. Look at
    // the first token: if it's a plain quoted string with `{`, treat as
    // TemplateOrConcat.
    if (head.starts_with('"') || head.starts_with('\'')) && head.contains('{') {
        return UnresolvedReason::TemplateOrConcat;
    }
    // String concatenation: head contains `+` before the first comma-or-bracket.
    let stop = head.find([',', ']']).unwrap_or(head.len());
    if head[..stop].contains('+') {
        return UnresolvedReason::TemplateOrConcat;
    }
    UnresolvedReason::NonLiteralFirstArg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_subprocess_run_double_quoted() {
        let src = "import subprocess\nsubprocess.run([\"leio-code\", \"--help\"])\n";
        let out = detect_python_subprocess_calls(src, "a.py");
        assert_eq!(out.spawns.len(), 1);
        assert_eq!(out.spawns[0].binary, "leio-code");
        assert_eq!(out.spawns[0].line, 2);
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn detects_subprocess_run_single_quoted() {
        let src = "subprocess.run(['leio-code', '--help'])\n";
        let out = detect_python_subprocess_calls(src, "a.py");
        assert_eq!(out.spawns.len(), 1);
        assert_eq!(out.spawns[0].binary, "leio-code");
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn skips_format_string_first_arg() {
        let src = "subprocess.run([f\"bin-{suffix}\", \"--help\"])\n";
        let out = detect_python_subprocess_calls(src, "a.py");
        assert!(out.spawns.is_empty());
        assert_eq!(out.unresolved.len(), 1);
        assert_eq!(out.unresolved[0].reason, UnresolvedReason::TemplateOrConcat);
    }
}
