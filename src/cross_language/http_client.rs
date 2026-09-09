//! HTTP client call detectors.
//!
//! Each function returns one [`HttpCallOccurrence`] per call site. Literal
//! URLs (Phase 5) are detected separately from templated URLs (Phase 6) so
//! the matching layer can pick the right strategy:
//!
//! - **Literal** — `requests.get("/users")`, `fetch("/items")` — exact
//!   string. `is_template: false`.
//! - **Templated** — `requests.get(f"/users/{uid}")`,
//!   `` fetch(`/users/${id}`) ``, `reqwest::get(format!("/users/{}", uid))`
//!   — placeholder-bearing URL. `is_template: true`.
//! - **Dataflow** (Phase 8) — `requests.get(url)` where `url` is a literal
//!   bound earlier in the function body, or a template whose placeholders
//!   bind to literals. The detector substitutes the value and sets
//!   `resolved_via_dataflow: true`. Tier-walk in `resolve_http_edges`
//!   downgrades the confidence band to 80 / `DataflowLiteral` or 75 /
//!   `DataflowTemplate`.
//!
//! Conventions mirror `http_route.rs`:
//! - Methods are uppercase ASCII; `"*"` means "method unknown" (e.g. a bare
//!   `fetch("/foo")` without options).
//! - The character class `[^"$]` keeps interpolation tokens out of literal
//!   strings; the templated detectors use the inverse — they only fire on
//!   strings that DO carry a placeholder.

use std::sync::LazyLock;

use regex::Regex;

use crate::model::{HttpCallOccurrence, SourceLanguage, UnresolvedEdge, UnresolvedReason};

use super::dataflow::{self, ConcatPart, ResolveOutcome, SubstituteExpr};
use super::{line_at_offset, line_number_for_offset, truncate_snippet};

/// HTTP detector output: resolved (or partially-resolved) call sites plus
/// unresolved edges classified by reason. Phase 8 introduced the unresolved
/// stream for HTTP — pre-Phase-8 detectors silently dropped dynamic URLs.
#[derive(Debug, Default, Clone)]
pub struct HttpDetectorOutput {
    pub http_calls: Vec<HttpCallOccurrence>,
    pub unresolved: Vec<UnresolvedEdge>,
}

// ---------------------------------------------------------------------------
// Python: requests / httpx
// ---------------------------------------------------------------------------

static PYTHON_HTTP_CLIENT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(requests|httpx)\.(get|post|put|delete|patch)\s*\(\s*"([^"$]+?)""#)
        .expect("python http client regex compiles")
});

/// Python f-string variant: `requests.get(f"/users/{uid}")`. The `f` prefix
/// gates this detector; the body may contain `{name}` placeholders.
static PYTHON_HTTP_FSTRING_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(requests|httpx)\.(get|post|put|delete|patch)\s*\(\s*f"([^"]+?)""#)
        .expect("python http f-string regex compiles")
});

/// Wide opener used to find dataflow-eligible Python HTTP calls — anything
/// shape `requests.<method>(...)` whose first arg isn't already covered by
/// the literal / f-string patterns. We require the first arg to not start
/// with a `"`/`'`/`f"` so the literal & f-string detectors stay in charge
/// of their cases.
static PYTHON_HTTP_WIDE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(requests|httpx)\.(get|post|put|delete|patch)\s*\("#)
        .expect("python http wide regex compiles")
});

/// Detect Python `requests.<method>(...)` / `httpx.<method>(...)` calls,
/// both literal and f-string forms. The f-string form sets `is_template`.
/// Phase 8: also detects bare-name and concatenation forms and tries
/// one-hop dataflow substitution; on success the call records the
/// substituted URL with `resolved_via_dataflow: true`.
pub fn detect_python_http_calls(source: &str, path: &str) -> HttpDetectorOutput {
    let mut out = HttpDetectorOutput::default();

    // Track byte offsets of literal/f-string call openings so the wide
    // pattern doesn't double-emit for the same call site.
    let mut covered: Vec<(usize, usize)> = Vec::new();

    for caps in PYTHON_HTTP_CLIENT_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(client) = caps.get(1) else { continue };
        let Some(method) = caps.get(2) else { continue };
        let Some(url) = caps.get(3) else { continue };
        let url_str = url.as_str().to_string();
        if url_str.is_empty() || url_str.contains('{') {
            continue;
        }
        let line = line_number_for_offset(source, whole.start());
        covered.push((whole.start(), whole.end()));
        out.http_calls.push(HttpCallOccurrence {
            path: path.to_string(),
            line,
            method: method.as_str().to_ascii_uppercase(),
            url: url_str,
            language: SourceLanguage::Python,
            client: client.as_str().to_string(),
            is_template: false,
            resolved_via_dataflow: false,
        });
    }

    // f-string form. If dataflow can resolve any of the placeholders, emit
    // a substituted occurrence instead of the raw template.
    for caps in PYTHON_HTTP_FSTRING_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(client) = caps.get(1) else { continue };
        let Some(method) = caps.get(2) else { continue };
        let Some(url) = caps.get(3) else { continue };
        let url_str = url.as_str().to_string();
        if url_str.is_empty() || !url_str.contains('{') {
            continue;
        }
        covered.push((whole.start(), whole.end()));
        let line = line_number_for_offset(source, whole.start());
        let raw_snippet = || truncate_snippet(line_at_offset(source, whole.start()));
        let segs = dataflow::parse_python_fstring(&url_str).unwrap_or_default();
        let outcome = if segs.is_empty() {
            ResolveOutcome::Unresolvable
        } else {
            dataflow::resolve_one_hop(
                source,
                whole.start(),
                SourceLanguage::Python,
                &SubstituteExpr::Template {
                    segments: segs.clone(),
                },
            )
        };
        match outcome {
            ResolveOutcome::Resolved { value } => {
                out.http_calls.push(HttpCallOccurrence {
                    path: path.to_string(),
                    line,
                    method: method.as_str().to_ascii_uppercase(),
                    url: value,
                    language: SourceLanguage::Python,
                    client: client.as_str().to_string(),
                    is_template: false,
                    resolved_via_dataflow: true,
                });
            }
            ResolveOutcome::ResolvedTemplate { value } => {
                out.http_calls.push(HttpCallOccurrence {
                    path: path.to_string(),
                    line,
                    method: method.as_str().to_ascii_uppercase(),
                    url: value,
                    language: SourceLanguage::Python,
                    client: client.as_str().to_string(),
                    is_template: true,
                    resolved_via_dataflow: true,
                });
            }
            ResolveOutcome::Ambiguous => {
                out.unresolved.push(UnresolvedEdge {
                    source_path: path.to_string(),
                    source_line: line,
                    source_language: SourceLanguage::Python,
                    edge_kind: "http_call".to_string(),
                    reason: UnresolvedReason::AmbiguousAssignment,
                    raw_snippet: raw_snippet(),
                });
            }
            ResolveOutcome::Unresolvable => {
                // Keep the original templated-URL emission so tier-3
                // template matching can still try.
                out.http_calls.push(HttpCallOccurrence {
                    path: path.to_string(),
                    line,
                    method: method.as_str().to_ascii_uppercase(),
                    url: url_str.clone(),
                    language: SourceLanguage::Python,
                    client: client.as_str().to_string(),
                    is_template: true,
                    resolved_via_dataflow: false,
                });
            }
        }
    }

    // Phase 8: wide-form dataflow path — bare name or concat first arg.
    for caps in PYTHON_HTTP_WIDE_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        // Skip if covered by a literal / f-string capture.
        if covered
            .iter()
            .any(|(s, e)| *s <= whole.start() && whole.start() < *e)
        {
            continue;
        }
        let Some(client) = caps.get(1) else { continue };
        let Some(method) = caps.get(2) else { continue };
        let line = line_number_for_offset(source, whole.start());
        let raw_snippet = truncate_snippet(line_at_offset(source, whole.start()));
        // Extract the first argument source text — naive: from end of opener
        // to next `,` or `)` at top level.
        let arg_text = first_arg_text(&source[whole.end()..]);
        let expr = if let Some(name) = bare_identifier(&arg_text) {
            Some(SubstituteExpr::BareName(name))
        } else if let Some(parts) = dataflow::parse_concat(&arg_text) {
            // Concat expr — only useful if it references at least one name.
            if parts.iter().any(|p| matches!(p, ConcatPart::Name(_))) {
                Some(SubstituteExpr::Concat { parts })
            } else {
                None
            }
        } else {
            None
        };
        let Some(expr) = expr else {
            continue;
        };
        let outcome =
            dataflow::resolve_one_hop(source, whole.start(), SourceLanguage::Python, &expr);
        emit_dataflow_http_outcome(
            outcome,
            &mut out,
            path,
            line,
            method.as_str(),
            SourceLanguage::Python,
            client.as_str(),
            raw_snippet,
            matches!(expr, SubstituteExpr::BareName(_)),
        );
    }

    out
}

// ---------------------------------------------------------------------------
// JS / TS: fetch + axios
// ---------------------------------------------------------------------------

static JS_FETCH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // fetch("/foo") or fetch('/foo'). Template-literal URLs (backticks) are
    // handled separately below.
    Regex::new(r#"\bfetch\s*\(\s*(?:"([^"$]+?)"|'([^'$]+?)')"#).expect("js fetch regex compiles")
});

static JS_AXIOS_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\baxios\.(get|post|put|delete|patch)\s*\(\s*(?:"([^"$]+?)"|'([^'$]+?)')"#)
        .expect("js axios regex compiles")
});

/// JS template-literal fetch: `` fetch(`/users/${id}`) ``. Body must contain
/// `${...}` placeholder, otherwise it's a glorified literal and we'd be
/// claiming a template-edge falsely.
static JS_FETCH_TEMPLATE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\bfetch\s*\(\s*`([^`]+?)`"#).expect("js fetch template regex compiles")
});

/// JS template-literal axios: `` axios.get(`/users/${id}`) ``.
static JS_AXIOS_TEMPLATE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\baxios\.(get|post|put|delete|patch)\s*\(\s*`([^`]+?)`"#)
        .expect("js axios template regex compiles")
});

/// Phase 8 wide opener: any `fetch(` or `axios.<method>(`.
static JS_HTTP_WIDE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(fetch|axios\.(?:get|post|put|delete|patch))\s*\("#)
        .expect("js http wide regex compiles")
});

/// Detect JS/TS `fetch(...)` and `axios.<method>(...)` calls. Both literal
/// (quoted) and template-literal (backticked) forms are recognized; the
/// templated form sets `is_template`. Phase 8 adds bare-name and concat
/// detection via dataflow.
pub fn detect_js_http_calls(
    source: &str,
    path: &str,
    language: SourceLanguage,
) -> HttpDetectorOutput {
    let mut out = HttpDetectorOutput::default();
    let mut covered: Vec<(usize, usize)> = Vec::new();

    for caps in JS_FETCH_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let url = match caps.get(1).or_else(|| caps.get(2)) {
            Some(u) => u.as_str().to_string(),
            None => continue,
        };
        if url.is_empty() || url.contains('{') {
            continue;
        }
        let line = line_number_for_offset(source, whole.start());
        covered.push((whole.start(), whole.end()));
        out.http_calls.push(HttpCallOccurrence {
            path: path.to_string(),
            line,
            method: "*".to_string(),
            url,
            language,
            client: "fetch".to_string(),
            is_template: false,
            resolved_via_dataflow: false,
        });
    }

    for caps in JS_AXIOS_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(method) = caps.get(1) else { continue };
        let url = match caps.get(2).or_else(|| caps.get(3)) {
            Some(u) => u.as_str().to_string(),
            None => continue,
        };
        if url.is_empty() || url.contains('{') {
            continue;
        }
        let line = line_number_for_offset(source, whole.start());
        covered.push((whole.start(), whole.end()));
        out.http_calls.push(HttpCallOccurrence {
            path: path.to_string(),
            line,
            method: method.as_str().to_ascii_uppercase(),
            url,
            language,
            client: "axios".to_string(),
            is_template: false,
            resolved_via_dataflow: false,
        });
    }

    // Template-literal fetch. Try dataflow substitution.
    for caps in JS_FETCH_TEMPLATE_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(url) = caps.get(1) else { continue };
        let url_str = url.as_str().to_string();
        if url_str.is_empty() || !url_str.contains("${") {
            continue;
        }
        covered.push((whole.start(), whole.end()));
        let line = line_number_for_offset(source, whole.start());
        let raw_snippet = truncate_snippet(line_at_offset(source, whole.start()));
        let segs = dataflow::parse_js_template(&url_str).unwrap_or_default();
        let outcome = if segs.is_empty() {
            ResolveOutcome::Unresolvable
        } else {
            dataflow::resolve_one_hop(
                source,
                whole.start(),
                language,
                &SubstituteExpr::Template { segments: segs },
            )
        };
        emit_template_outcome(
            outcome,
            &mut out,
            path,
            line,
            "*",
            language,
            "fetch",
            url_str,
            raw_snippet,
        );
    }

    // Template-literal axios.
    for caps in JS_AXIOS_TEMPLATE_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(method) = caps.get(1) else { continue };
        let Some(url) = caps.get(2) else { continue };
        let url_str = url.as_str().to_string();
        if url_str.is_empty() || !url_str.contains("${") {
            continue;
        }
        covered.push((whole.start(), whole.end()));
        let line = line_number_for_offset(source, whole.start());
        let raw_snippet = truncate_snippet(line_at_offset(source, whole.start()));
        let segs = dataflow::parse_js_template(&url_str).unwrap_or_default();
        let outcome = if segs.is_empty() {
            ResolveOutcome::Unresolvable
        } else {
            dataflow::resolve_one_hop(
                source,
                whole.start(),
                language,
                &SubstituteExpr::Template { segments: segs },
            )
        };
        emit_template_outcome(
            outcome,
            &mut out,
            path,
            line,
            method.as_str(),
            language,
            "axios",
            url_str,
            raw_snippet,
        );
    }

    // Phase 8: wide-form dataflow — bare name or concat.
    for caps in JS_HTTP_WIDE_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        if covered
            .iter()
            .any(|(s, e)| *s <= whole.start() && whole.start() < *e)
        {
            continue;
        }
        let Some(api) = caps.get(1) else { continue };
        let api_str = api.as_str();
        let (client, method) = if api_str.starts_with("axios.") {
            ("axios", api_str.trim_start_matches("axios.").to_string())
        } else {
            ("fetch", "*".to_string())
        };
        let line = line_number_for_offset(source, whole.start());
        let raw_snippet = truncate_snippet(line_at_offset(source, whole.start()));
        let arg_text = first_arg_text(&source[whole.end()..]);
        let expr = if let Some(name) = bare_identifier(&arg_text) {
            Some(SubstituteExpr::BareName(name))
        } else if let Some(parts) = dataflow::parse_concat(&arg_text) {
            if parts.iter().any(|p| matches!(p, ConcatPart::Name(_))) {
                Some(SubstituteExpr::Concat { parts })
            } else {
                None
            }
        } else {
            None
        };
        let Some(expr) = expr else {
            continue;
        };
        let outcome = dataflow::resolve_one_hop(source, whole.start(), language, &expr);
        emit_dataflow_http_outcome(
            outcome,
            &mut out,
            path,
            line,
            &method,
            language,
            client,
            raw_snippet,
            matches!(expr, SubstituteExpr::BareName(_)),
        );
    }

    out
}

// ---------------------------------------------------------------------------
// Rust: reqwest
// ---------------------------------------------------------------------------

static RUST_REQWEST_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // reqwest::get("...") or reqwest::Client::new().get("...")
    Regex::new(r#"reqwest::(?:Client::new\(\)\.)?(get|post|put|delete|patch)\s*\(\s*"([^"$]+?)""#)
        .expect("rust reqwest regex compiles")
});

/// Rust `format!`-wrapped reqwest call: `reqwest::get(format!("/users/{}", uid))`.
/// The `{}` placeholder is the marker. Captures the format-string body and
/// the trailing args list.
static RUST_REQWEST_FORMAT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"reqwest::(?:Client::new\(\)\.)?(get|post|put|delete|patch)\s*\(\s*(?:&\s*)?format!\s*\(\s*"([^"]+?)"\s*(?:,\s*([^)]*))?\)"#,
    )
    .expect("rust reqwest format regex compiles")
});

/// Phase 8 wide opener: `reqwest::<method>(` or `reqwest::Client::new().<method>(`.
static RUST_REQWEST_WIDE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"reqwest::(?:Client::new\(\)\.)?(get|post|put|delete|patch)\s*\("#)
        .expect("rust reqwest wide regex compiles")
});

/// Detect Rust `reqwest::<method>(...)` and `reqwest::Client::new().<method>(...)`
/// calls, both literal and `format!`-wrapped. Phase 8 adds bare-name and
/// concat detection plus dataflow substitution for `format!` placeholders
/// bound to literals.
pub fn detect_rust_http_calls(source: &str, path: &str) -> HttpDetectorOutput {
    let mut out = HttpDetectorOutput::default();
    let mut covered: Vec<(usize, usize)> = Vec::new();

    for caps in RUST_REQWEST_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(method) = caps.get(1) else { continue };
        let Some(url) = caps.get(2) else { continue };
        let url_str = url.as_str().to_string();
        if url_str.is_empty() || url_str.contains('{') {
            continue;
        }
        let line = line_number_for_offset(source, whole.start());
        covered.push((whole.start(), whole.end()));
        out.http_calls.push(HttpCallOccurrence {
            path: path.to_string(),
            line,
            method: method.as_str().to_ascii_uppercase(),
            url: url_str,
            language: SourceLanguage::Rust,
            client: "reqwest".to_string(),
            is_template: false,
            resolved_via_dataflow: false,
        });
    }

    // format! wrapper — try dataflow on the format args.
    for caps in RUST_REQWEST_FORMAT_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(method) = caps.get(1) else { continue };
        let Some(url) = caps.get(2) else { continue };
        let url_str = url.as_str().to_string();
        if url_str.is_empty() || !url_str.contains('{') {
            continue;
        }
        covered.push((whole.start(), whole.end()));
        let args = caps.get(3).map(|g| g.as_str()).unwrap_or("");
        let line = line_number_for_offset(source, whole.start());
        let raw_snippet = truncate_snippet(line_at_offset(source, whole.start()));
        let segs = dataflow::parse_rust_format(&url_str, args).unwrap_or_default();
        let outcome = if segs.is_empty() {
            ResolveOutcome::Unresolvable
        } else {
            dataflow::resolve_one_hop(
                source,
                whole.start(),
                SourceLanguage::Rust,
                &SubstituteExpr::Template { segments: segs },
            )
        };
        emit_template_outcome(
            outcome,
            &mut out,
            path,
            line,
            method.as_str(),
            SourceLanguage::Rust,
            "reqwest",
            url_str,
            raw_snippet,
        );
    }

    // Phase 8: wide-form dataflow — bare name (e.g. `reqwest::get(&url)`).
    for caps in RUST_REQWEST_WIDE_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        if covered
            .iter()
            .any(|(s, e)| *s <= whole.start() && whole.start() < *e)
        {
            continue;
        }
        let Some(method) = caps.get(1) else { continue };
        let line = line_number_for_offset(source, whole.start());
        let raw_snippet = truncate_snippet(line_at_offset(source, whole.start()));
        let arg_text = first_arg_text(&source[whole.end()..]);
        // Rust often uses `&url` — strip the leading `&`.
        let stripped = arg_text.trim().trim_start_matches('&').trim();
        let expr = bare_identifier(stripped).map(SubstituteExpr::BareName);
        let Some(expr) = expr else {
            continue;
        };
        let outcome = dataflow::resolve_one_hop(source, whole.start(), SourceLanguage::Rust, &expr);
        emit_dataflow_http_outcome(
            outcome,
            &mut out,
            path,
            line,
            method.as_str(),
            SourceLanguage::Rust,
            "reqwest",
            raw_snippet,
            true,
        );
    }

    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the first argument's source text from the slice that starts just
/// after an opening `(`. Stops at the matching `,` or `)` at top level.
/// Quote- and bracket-aware to handle nested calls/strings.
fn first_arg_text(after_paren: &str) -> String {
    let bytes = after_paren.as_bytes();
    let mut depth_paren: i32 = 0;
    let mut depth_brack: i32 = 0;
    let mut depth_brace: i32 = 0;
    let mut in_quote: Option<u8> = None;
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = in_quote {
            if c == b'\\' && i + 1 < bytes.len() {
                out.push(c as char);
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == q {
                in_quote = None;
            }
            out.push(c as char);
            i += 1;
            continue;
        }
        match c {
            b'"' | b'\'' | b'`' => in_quote = Some(c),
            b'(' => depth_paren += 1,
            b')' => {
                if depth_paren == 0 && depth_brack == 0 && depth_brace == 0 {
                    break;
                }
                depth_paren -= 1;
            }
            b'[' => depth_brack += 1,
            b']' => depth_brack -= 1,
            b'{' => depth_brace += 1,
            b'}' => depth_brace -= 1,
            b',' if depth_paren == 0 && depth_brack == 0 && depth_brace == 0 => break,
            _ => {}
        }
        out.push(c as char);
        i += 1;
    }
    out.trim().to_string()
}

/// Return the identifier if `arg` is a bare name (optionally preceded by `&`
/// for Rust borrowing). Returns `None` for anything else.
fn bare_identifier(arg: &str) -> Option<String> {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return None;
    }
    let trimmed = trimmed.trim_start_matches('&').trim();
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let first = bytes[0] as char;
    if !(first.is_alphabetic() || first == '_' || first == '$') {
        return None;
    }
    for &b in bytes {
        let c = b as char;
        if !(c.is_alphanumeric() || c == '_' || c == '$') {
            return None;
        }
    }
    Some(trimmed.to_string())
}

/// Emit the outcome of a dataflow-resolved templated call. Falls back to
/// the raw templated URL when substitution didn't progress.
#[allow(clippy::too_many_arguments)]
fn emit_template_outcome(
    outcome: ResolveOutcome,
    out: &mut HttpDetectorOutput,
    path: &str,
    line: usize,
    method: &str,
    language: SourceLanguage,
    client: &str,
    fallback_url: String,
    raw_snippet: String,
) {
    match outcome {
        ResolveOutcome::Resolved { value } => {
            out.http_calls.push(HttpCallOccurrence {
                path: path.to_string(),
                line,
                method: method.to_ascii_uppercase(),
                url: value,
                language,
                client: client.to_string(),
                is_template: false,
                resolved_via_dataflow: true,
            });
        }
        ResolveOutcome::ResolvedTemplate { value } => {
            out.http_calls.push(HttpCallOccurrence {
                path: path.to_string(),
                line,
                method: method.to_ascii_uppercase(),
                url: value,
                language,
                client: client.to_string(),
                is_template: true,
                resolved_via_dataflow: true,
            });
        }
        ResolveOutcome::Ambiguous => {
            out.unresolved.push(UnresolvedEdge {
                source_path: path.to_string(),
                source_line: line,
                source_language: language,
                edge_kind: "http_call".to_string(),
                reason: UnresolvedReason::AmbiguousAssignment,
                raw_snippet,
            });
        }
        ResolveOutcome::Unresolvable => {
            // No substitution progress — fall back to the raw templated
            // URL. The Phase 6 template-matching path takes over at
            // confidence 70 / `MatchKind::Template`.
            out.http_calls.push(HttpCallOccurrence {
                path: path.to_string(),
                line,
                method: method.to_ascii_uppercase(),
                url: fallback_url,
                language,
                client: client.to_string(),
                is_template: true,
                resolved_via_dataflow: false,
            });
        }
    }
}

/// Emit the outcome of a dataflow attempt on a bare-name or concat first
/// argument. Unlike the template path, there is no useful fallback URL —
/// `Unresolvable` becomes `UnresolvedEdge` with the conservative reason.
#[allow(clippy::too_many_arguments)]
fn emit_dataflow_http_outcome(
    outcome: ResolveOutcome,
    out: &mut HttpDetectorOutput,
    path: &str,
    line: usize,
    method: &str,
    language: SourceLanguage,
    client: &str,
    raw_snippet: String,
    bare_name_form: bool,
) {
    match outcome {
        ResolveOutcome::Resolved { value } => {
            out.http_calls.push(HttpCallOccurrence {
                path: path.to_string(),
                line,
                method: method.to_ascii_uppercase(),
                url: value,
                language,
                client: client.to_string(),
                is_template: false,
                resolved_via_dataflow: true,
            });
        }
        ResolveOutcome::ResolvedTemplate { value } => {
            out.http_calls.push(HttpCallOccurrence {
                path: path.to_string(),
                line,
                method: method.to_ascii_uppercase(),
                url: value,
                language,
                client: client.to_string(),
                is_template: true,
                resolved_via_dataflow: true,
            });
        }
        ResolveOutcome::Ambiguous => {
            out.unresolved.push(UnresolvedEdge {
                source_path: path.to_string(),
                source_line: line,
                source_language: language,
                edge_kind: "http_call".to_string(),
                reason: UnresolvedReason::AmbiguousAssignment,
                raw_snippet,
            });
        }
        ResolveOutcome::Unresolvable => {
            let reason = if bare_name_form {
                UnresolvedReason::NonLiteralFirstArg
            } else {
                UnresolvedReason::TemplateOrConcat
            };
            out.unresolved.push(UnresolvedEdge {
                source_path: path.to_string(),
                source_line: line,
                source_language: language,
                edge_kind: "http_call".to_string(),
                reason,
                raw_snippet,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Python: requests / httpx ----

    #[test]
    fn requests_get_literal_url() {
        let src = "import requests\nrequests.get(\"http://example.com/users\")\n";
        let out = detect_python_http_calls(src, "a.py");
        assert_eq!(out.http_calls.len(), 1);
        assert_eq!(out.http_calls[0].method, "GET");
        assert_eq!(out.http_calls[0].url, "http://example.com/users");
        assert_eq!(out.http_calls[0].client, "requests");
        assert!(!out.http_calls[0].is_template);
        assert!(out.unresolved.is_empty());
    }

    #[test]
    fn httpx_post_literal_url() {
        let src = "httpx.post(\"/api/items\", json={\"k\": 1})\n";
        let out = detect_python_http_calls(src, "a.py");
        assert_eq!(out.http_calls.len(), 1);
        assert_eq!(out.http_calls[0].method, "POST");
        assert_eq!(out.http_calls[0].client, "httpx");
        assert_eq!(out.http_calls[0].url, "/api/items");
    }

    #[test]
    fn requests_fstring_url_with_unbound_placeholder_kept_as_template() {
        // `uid` is a function param → unbound. Substitution makes no
        // progress, so the detector falls back to the raw template URL
        // and Phase 6 takes over at confidence 70.
        let src = "def f(uid):\n    requests.get(f\"/users/{uid}\")\n";
        let out = detect_python_http_calls(src, "a.py");
        let calls: Vec<_> = out
            .http_calls
            .iter()
            .filter(|h| h.client == "requests")
            .collect();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].is_template);
        assert_eq!(calls[0].url, "/users/{uid}");
        assert!(!calls[0].resolved_via_dataflow);
    }

    // ---- JS / TS: fetch / axios ----

    #[test]
    fn fetch_double_quoted_path() {
        let src = "fetch(\"/items\")\n";
        let out = detect_js_http_calls(src, "a.ts", SourceLanguage::TypeScript);
        assert_eq!(out.http_calls.len(), 1);
        assert_eq!(out.http_calls[0].method, "*");
        assert_eq!(out.http_calls[0].url, "/items");
        assert_eq!(out.http_calls[0].client, "fetch");
        assert!(!out.http_calls[0].is_template);
    }

    #[test]
    fn fetch_template_literal_url_with_unbound_param() {
        let src = "function f(id) {\n  fetch(`/users/${id}`);\n}\n";
        let out = detect_js_http_calls(src, "a.ts", SourceLanguage::TypeScript);
        let calls: Vec<_> = out
            .http_calls
            .iter()
            .filter(|h| h.client == "fetch")
            .collect();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].is_template);
        assert_eq!(calls[0].url, "/users/${id}");
    }

    #[test]
    fn axios_get_single_quoted_url() {
        let src = "axios.get('/items')\n";
        let out = detect_js_http_calls(src, "a.js", SourceLanguage::JavaScript);
        let axios_hits: Vec<_> = out
            .http_calls
            .iter()
            .filter(|h| h.client == "axios")
            .collect();
        assert_eq!(axios_hits.len(), 1);
        assert_eq!(axios_hits[0].method, "GET");
        assert_eq!(axios_hits[0].url, "/items");
    }

    // ---- Rust: reqwest ----

    #[test]
    fn reqwest_free_function_get() {
        let src = "let r = reqwest::get(\"http://api.local/health\").await?;\n";
        let out = detect_rust_http_calls(src, "a.rs");
        assert_eq!(out.http_calls.len(), 1);
        assert_eq!(out.http_calls[0].method, "GET");
        assert_eq!(out.http_calls[0].url, "http://api.local/health");
        assert_eq!(out.http_calls[0].client, "reqwest");
    }

    #[test]
    fn reqwest_client_new_get() {
        let src = "let r = reqwest::Client::new().get(\"/health\").send().await?;\n";
        let out = detect_rust_http_calls(src, "a.rs");
        assert_eq!(out.http_calls.len(), 1);
        assert_eq!(out.http_calls[0].method, "GET");
        assert_eq!(out.http_calls[0].url, "/health");
    }

    #[test]
    fn reqwest_format_macro_with_unbound_arg_kept_as_template() {
        // No literal binding for `uid` (it's a function param), so the
        // dataflow pass makes no progress and the raw template URL is
        // preserved. Tier walk treats this as Phase 6 Template (70).
        let src = "fn f(uid: u64) { let _ = reqwest::get(format!(\"/users/{}\", uid)).await?; }\n";
        let out = detect_rust_http_calls(src, "a.rs");
        let calls: Vec<_> = out
            .http_calls
            .iter()
            .filter(|h| h.client == "reqwest")
            .collect();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].is_template);
        assert_eq!(calls[0].url, "/users/{}");
        assert!(!calls[0].resolved_via_dataflow);
    }

    // ---- Phase 8: dataflow ----

    #[test]
    fn python_bare_name_resolves_via_dataflow() {
        let src = "def f():\n    url = \"/users\"\n    requests.get(url)\n";
        let out = detect_python_http_calls(src, "a.py");
        let calls: Vec<_> = out
            .http_calls
            .iter()
            .filter(|h| h.resolved_via_dataflow)
            .collect();
        assert_eq!(calls.len(), 1, "should resolve via dataflow: {:#?}", out);
        assert_eq!(calls[0].url, "/users");
    }
}
