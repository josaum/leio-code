//! Rust subprocess-spawn detector (`std::process::Command::new("literal")`).
//!
//! Parallel to `super::detect_python_subprocess_calls` — same conservative
//! shape, same "never claim an edge we can't see" rule. We only emit a
//! resolved occurrence when the first argument to `Command::new(...)` is a
//! string literal we can read without evaluating anything.
//!
//! Phase 3 (P0 #2): non-literal first args and bare `Command::new(...)` with
//! no `use ... Command` import are now emitted as `UnresolvedEdge`s rather
//! than dropped silently.
//!
//! What we detect:
//!   Command::new("bin")                      // with `use std/tokio::process::Command`
//!   std::process::Command::new("bin")        // fully qualified
//!   tokio::process::Command::new("bin")      // fully qualified
//!
//! Unresolved:
//!   Command::new(var)                        // → NonLiteralFirstArg
//!   Command::new("foo")  // no use import    // → AmbiguousCommandImport
//!
//! Limitations:
//!   - Raw strings (`r#"..."#`) as the first arg: not detected (we only
//!     accept the plain `"..."` form).
//!   - Builder `.program(...)` mutators on an existing `Command`: not
//!     detected.

use std::sync::LazyLock;

use regex::Regex;

use crate::model::{SourceLanguage, SubprocessCallOccurrence, UnresolvedEdge, UnresolvedReason};

use super::dataflow::{self, ResolveOutcome, SubstituteExpr};
use super::{DetectorOutput, line_at_offset, line_number_for_offset, truncate_snippet};

/// `use std::process::Command` or `use tokio::process::Command`, including
/// nested `use std::process::{Command, …}`.
static USE_PROCESS_COMMAND_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"use\s+(?:std|tokio)::process::(?:\{[^}]*\bCommand\b[^}]*\}|Command)\b")
        .expect("use std/tokio::process::Command regex compiles")
});

/// Wide opening: any `Command::new(` (optionally fully qualified). Captures:
/// 1 = `std::process::` or `tokio::process::` (Some when fully qualified).
static COMMAND_NEW_OPENING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(std::process::|tokio::process::)?Command::new\s*\(")
        .expect("Command::new opening regex compiles")
});

/// Literal form: `Command::new("bin")` — the bin is a plain double-quoted
/// string with no `$` placeholder.
static COMMAND_NEW_LITERAL_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*"([^"$]+?)"\s*\)"#).expect("Command::new literal head regex compiles")
});

/// Detect Rust `Command::new(...)` candidates.
pub fn detect_rust_process_commands(source: &str, path: &str) -> DetectorOutput {
    let has_use_import = USE_PROCESS_COMMAND_PATTERN.is_match(source);

    let mut out = DetectorOutput::default();
    for caps in COMMAND_NEW_OPENING.captures_iter(source) {
        let whole = caps.get(0).expect("group 0 always present");
        let fully_qualified = caps.get(1).is_some();
        let line = line_number_for_offset(source, whole.start());
        let raw_snippet = truncate_snippet(line_at_offset(source, whole.start()));

        // Bare `Command::new(...)` with no `use` import is ambiguous: could
        // be a user-defined `Command`. Phase 3: emit as unresolved.
        if !fully_qualified && !has_use_import {
            out.unresolved.push(UnresolvedEdge {
                source_path: path.to_string(),
                source_line: line,
                source_language: SourceLanguage::Rust,
                edge_kind: "subprocess_spawn".to_string(),
                reason: UnresolvedReason::AmbiguousCommandImport,
                raw_snippet,
            });
            continue;
        }

        // Try the literal head on the args slice.
        let args_start = whole.end();
        let slice = &source[args_start..];
        if let Some(lit) = COMMAND_NEW_LITERAL_HEAD.captures(slice) {
            let binary = lit
                .get(1)
                .map(|g| g.as_str().to_string())
                .unwrap_or_default();
            if !binary.is_empty() {
                out.spawns.push(SubprocessCallOccurrence {
                    binary,
                    path: path.to_string(),
                    line,
                    language: SourceLanguage::Rust,
                    resolved_via_dataflow: false,
                });
                continue;
            }
        }

        // Phase 8: try one-hop dataflow on a bare-name first arg before
        // emitting unresolved. `Command::new(&url)` and `Command::new(url)`
        // are both common shapes; we strip the leading `&`.
        let stripped = slice.trim().trim_start_matches('&').trim();
        if let Some(name) = rust_bare_identifier(stripped) {
            let outcome = dataflow::resolve_one_hop(
                source,
                whole.start(),
                SourceLanguage::Rust,
                &SubstituteExpr::BareName(name),
            );
            match outcome {
                ResolveOutcome::Resolved { value } => {
                    out.spawns.push(SubprocessCallOccurrence {
                        binary: value,
                        path: path.to_string(),
                        line,
                        language: SourceLanguage::Rust,
                        resolved_via_dataflow: true,
                    });
                    continue;
                }
                ResolveOutcome::Ambiguous => {
                    out.unresolved.push(UnresolvedEdge {
                        source_path: path.to_string(),
                        source_line: line,
                        source_language: SourceLanguage::Rust,
                        edge_kind: "subprocess_spawn".to_string(),
                        reason: UnresolvedReason::AmbiguousAssignment,
                        raw_snippet,
                    });
                    continue;
                }
                ResolveOutcome::ResolvedTemplate { .. } | ResolveOutcome::Unresolvable => {}
            }
        }

        // Non-literal first arg.
        out.unresolved.push(UnresolvedEdge {
            source_path: path.to_string(),
            source_line: line,
            source_language: SourceLanguage::Rust,
            edge_kind: "subprocess_spawn".to_string(),
            reason: UnresolvedReason::NonLiteralFirstArg,
            raw_snippet,
        });
    }
    out
}

/// Return the identifier if `s` looks like a bare Rust identifier
/// (followed only by whitespace or `)`).
fn rust_bare_identifier(s: &str) -> Option<String> {
    let s = s.trim();
    // Stop at `)` or `,` or whitespace.
    let stop = s
        .find(|c: char| c == ')' || c == ',' || c.is_whitespace())
        .unwrap_or(s.len());
    let candidate = s[..stop].trim();
    if candidate.is_empty() {
        return None;
    }
    let bytes = candidate.as_bytes();
    let first = bytes[0] as char;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    if candidate.bytes().all(|b| {
        let c = b as char;
        c.is_alphanumeric() || c == '_'
    }) {
        Some(candidate.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_use_std_command_new() {
        let src = "use std::process::Command;\nCommand::new(\"leio-code\").arg(\"--help\");\n";
        let out = detect_rust_process_commands(src, "a.rs");
        assert_eq!(out.spawns.len(), 1);
        assert_eq!(out.spawns[0].binary, "leio-code");
        assert_eq!(out.spawns[0].line, 2);
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn detects_fully_qualified_std() {
        let src = "fn main() { std::process::Command::new(\"leio-code\"); }\n";
        let out = detect_rust_process_commands(src, "a.rs");
        assert_eq!(out.spawns.len(), 1);
        assert_eq!(out.spawns[0].binary, "leio-code");
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn skips_command_new_without_import() {
        // No `use std::process::Command;` in scope — could be a user type.
        let src = "fn main() { Command::new(\"foo\"); }\n";
        let out = detect_rust_process_commands(src, "a.rs");
        assert!(
            out.spawns.is_empty(),
            "ambiguous Command::new must not emit a resolved spawn"
        );
        assert_eq!(out.unresolved.len(), 1);
        assert_eq!(
            out.unresolved[0].reason,
            UnresolvedReason::AmbiguousCommandImport
        );
    }
}
