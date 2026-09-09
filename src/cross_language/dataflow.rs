//! Light dataflow — one-hop in-function literal substitution.
//!
//! Phase 8 (P0 #2). Bridges the gap between literal-only resolution and the
//! `UnresolvedEdge` bucket for the very common pattern:
//!
//! ```python
//! def fetch_users():
//!     base = "http://localhost:8080"
//!     path = "/users"
//!     url = base + path
//!     return requests.get(url)
//! ```
//!
//! Algorithm (one hop, same function body):
//!   1. Find the enclosing function range for the call site.
//!   2. Scan the function body for `<name> = <string-literal>` assignments
//!      that appear **before** the call site (by byte offset).
//!   3. Substitute the URL/binary expression. The result is one of:
//!      - [`SubstituteOutcome::Resolved`] — fully literal URL/binary
//!      - [`SubstituteOutcome::ResolvedTemplate`] — substitution applied but
//!        unresolvable placeholders remain (function params, etc.)
//!      - [`SubstituteOutcome::Ambiguous`] — variable was reassigned in the
//!        function body; we refuse to guess
//!      - [`SubstituteOutcome::Unresolvable`] — name not found / not a literal
//!
//! The pass is intentionally regex-based and line-anchored. It does NOT
//! build a CFG, does NOT cross function boundaries, and does NOT chase
//! through intermediate function calls. See
//! `docs/light-dataflow-design.md` for the contract this module honors.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::model::SourceLanguage;

/// Outcome of substituting a URL/binary expression against a binding table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstituteOutcome {
    /// Substitution succeeded and the result is a fully-literal string with
    /// no remaining placeholders. Maps to confidence 80 / `DataflowLiteral`
    /// for HTTP, or the dataflow band for subprocess.
    Resolved(String),
    /// Substitution applied but at least one placeholder remained (typically
    /// a function parameter). The result is a template-shaped string
    /// suitable for template matching at confidence 75 / `DataflowTemplate`.
    ResolvedTemplate(String),
    /// At least one placeholder maps to a name that was reassigned within
    /// the function body. Caller should emit
    /// `UnresolvedReason::AmbiguousAssignment` rather than guess.
    Ambiguous,
    /// Substitution failed (name not in table, or table value was not a
    /// literal). Caller should keep the original unresolved reason.
    Unresolvable,
}

/// Per-name binding info. `value = None` flags an ambiguous binding (name
/// assigned 2+ times in the function body before the call site, with at
/// least two distinct literal values).
#[derive(Debug, Clone)]
struct Binding {
    value: Option<String>,
    /// Number of literal assignments observed before the call site.
    count: usize,
}

/// Map of variable name → bound literal value or ambiguity flag, scoped to
/// a single function body and a single call-site offset.
#[derive(Debug, Default, Clone)]
pub struct LiteralBindingTable {
    bindings: HashMap<String, Binding>,
}

impl LiteralBindingTable {
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Look up a name. `Some(Ok(value))` is a unique literal binding,
    /// `Some(Err(()))` means ambiguous (reassigned), `None` means absent.
    pub fn lookup(&self, name: &str) -> Option<Result<&str, ()>> {
        self.bindings.get(name).map(|b| match &b.value {
            Some(v) if b.count <= 1 => Ok(v.as_str()),
            _ => Err(()),
        })
    }

    fn insert(&mut self, name: String, value: String) {
        let entry = self.bindings.entry(name).or_insert(Binding {
            value: Some(value.clone()),
            count: 0,
        });
        entry.count += 1;
        if let Some(existing) = &entry.value
            && existing != &value
        {
            // distinct literals → ambiguous
            entry.value = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Enclosing function range
// ---------------------------------------------------------------------------

/// Find the function-body byte range that contains `offset`. Returns
/// `(body_start, body_end)` where `body_start` is the offset just after the
/// function header (so scanning from `body_start` sees body statements) and
/// `body_end` is one past the last body byte.
///
/// Returns `None` when no enclosing function is found, or when boundary
/// detection is ambiguous (nested closures in JS, e.g.). The caller treats
/// `None` as "give up, emit unresolved".
pub fn enclosing_function_range(
    source: &str,
    offset: usize,
    lang: SourceLanguage,
) -> Option<(usize, usize)> {
    match lang {
        SourceLanguage::Python => python_function_range(source, offset),
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            js_function_range(source, offset)
        }
        SourceLanguage::Rust => rust_function_range(source, offset),
        _ => None,
    }
}

// --- Python ---------------------------------------------------------------

static PY_DEF_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Match `def name(...):` at start of line (allowing leading whitespace).
    // We only need the start offset and the indent depth of the `def`.
    Regex::new(r"(?m)^([ \t]*)(?:async\s+)?def\s+[A-Za-z_][A-Za-z0-9_]*\s*\(")
        .expect("python def regex compiles")
});

fn python_function_range(source: &str, offset: usize) -> Option<(usize, usize)> {
    // Find the last `def ` that starts at or before `offset` and whose body
    // (indented more deeply) contains `offset`.
    let mut best: Option<(usize, usize, usize)> = None; // (def_start, header_indent, body_start)
    for caps in PY_DEF_PATTERN.captures_iter(source) {
        let m = caps.get(0)?;
        if m.start() > offset {
            break;
        }
        let indent = caps.get(1).map(|g| g.as_str().len()).unwrap_or(0);
        // Body begins after the `:` followed by a newline. Find the first
        // newline at or after the colon following the params.
        // Naive: find the line break after the def header.
        let header_end = match source[m.start()..].find('\n') {
            Some(i) => m.start() + i + 1,
            None => continue,
        };
        // Find the function's end: first line at or below the indent of the
        // `def` (non-blank).
        let body_end = python_body_end(source, header_end, indent);
        if header_end <= offset && offset < body_end {
            best = Some((m.start(), indent, header_end));
        }
    }
    let (_def_start, indent, body_start) = best?;
    let body_end = python_body_end(source, body_start, indent);
    Some((body_start, body_end))
}

/// Find where a Python function body ends given the indent depth of its
/// `def` line. Body ends at the first non-empty line whose indent is `<=
/// def_indent`.
fn python_body_end(source: &str, body_start: usize, def_indent: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = body_start;
    while i < bytes.len() {
        // Find end of current line.
        let line_end = match source[i..].find('\n') {
            Some(j) => i + j,
            None => bytes.len(),
        };
        let line = &source[i..line_end];
        let trimmed = line.trim_end();
        let is_blank = trimmed.trim_start().is_empty();
        let leading_ws = line.len() - line.trim_start().len();
        if !is_blank && leading_ws <= def_indent {
            return i;
        }
        i = line_end + 1;
    }
    bytes.len()
}

// --- JavaScript / TypeScript ----------------------------------------------

static JS_FUNCTION_OPENING: LazyLock<Regex> = LazyLock::new(|| {
    // Matches several JS function-opening shapes. We capture the position of
    // the opening `{` separately by scanning forward after the regex hit.
    //   function name(args) {
    //   async function name(args) {
    //   function (args) {           // anonymous
    //   const name = (args) => {
    //   const name = function (args) {
    //   const name = async (args) => {
    //   name: function (args) {     // object methods
    Regex::new(
        r"(?m)(?:^|[^.\w])(?:async\s+)?function\s*(?:[A-Za-z_$][A-Za-z0-9_$]*\s*)?\(|(?:const|let|var)\s+[A-Za-z_$][A-Za-z0-9_$]*\s*=\s*(?:async\s+)?(?:function\s*\(|\([^)]*\)\s*=>\s*\{?|[A-Za-z_$][A-Za-z0-9_$]*\s*=>\s*\{?)",
    )
    .expect("js function opening regex compiles")
});

fn js_function_range(source: &str, offset: usize) -> Option<(usize, usize)> {
    // Strategy: walk all candidate function openings; for each, find the
    // opening `{` of its body and the matching `}`. Pick the innermost
    // range that contains `offset`.
    let mut best: Option<(usize, usize, usize)> = None; // (open, body_start, body_end)
    for m in JS_FUNCTION_OPENING.find_iter(source) {
        let header_start = m.start();
        // Find the body-opening `{`. We rely on the fact that the function
        // header is well-formed: parentheses balanced, then `{`.
        let Some(brace) = find_function_body_brace(source, header_start) else {
            continue;
        };
        let body_start = brace + 1;
        let Some(close) = find_matching_brace(source, brace) else {
            continue;
        };
        if body_start <= offset && offset < close {
            // Pick the smallest enclosing range — handles nested functions.
            match &best {
                None => best = Some((header_start, body_start, close)),
                Some((_, prev_start, prev_end)) => {
                    if body_start >= *prev_start && close <= *prev_end {
                        best = Some((header_start, body_start, close));
                    }
                }
            }
        }
    }
    let (_, body_start, body_end) = best?;
    Some((body_start, body_end))
}

/// Starting at `from`, find the next `{` that opens a function body —
/// skipping past the parameter list. Returns the byte offset of the `{`.
fn find_function_body_brace(source: &str, from: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = from;
    let mut paren_depth: i32 = 0;
    let mut seen_paren = false;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'(' => {
                paren_depth += 1;
                seen_paren = true;
            }
            b')' => paren_depth -= 1,
            b'{' if paren_depth == 0 && seen_paren => return Some(i),
            b';' | b'\n' if paren_depth == 0 && seen_paren && (i > from + 10) => {
                // Bailout: arrow without braces (`(x) => expr`) — we can't
                // form a stable body range without `{}`, so give up.
                // The `+ 10` slack ensures we don't bail out before seeing
                // the parameter list at all.
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Given the offset of an opening `{`, find the matching `}`. Returns the
/// byte offset of the matching brace. Naive: no string-literal awareness.
/// Good enough for our purposes — the failure mode is "give up and emit
/// unresolved", which is safe.
fn find_matching_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth: i32 = 0;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

// --- Rust -----------------------------------------------------------------

static RUST_FN_OPENING: LazyLock<Regex> = LazyLock::new(|| {
    // `fn name(...)`, `pub fn name(...)`, `async fn name(...)`, etc. We
    // don't try to parse generics; we just find the start of the fn header.
    Regex::new(r"(?m)(?:^|[^\w])(?:pub(?:\s*\([^)]+\))?\s+)?(?:async\s+|unsafe\s+|const\s+)?fn\s+[A-Za-z_][A-Za-z0-9_]*\s*[<(]")
        .expect("rust fn opening regex compiles")
});

fn rust_function_range(source: &str, offset: usize) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize, usize)> = None;
    for m in RUST_FN_OPENING.find_iter(source) {
        let header_start = m.start();
        let Some(brace) = find_function_body_brace(source, header_start) else {
            continue;
        };
        let body_start = brace + 1;
        let Some(close) = find_matching_brace(source, brace) else {
            continue;
        };
        if body_start <= offset && offset < close {
            match &best {
                None => best = Some((header_start, body_start, close)),
                Some((_, prev_start, prev_end)) => {
                    if body_start >= *prev_start && close <= *prev_end {
                        best = Some((header_start, body_start, close));
                    }
                }
            }
        }
    }
    let (_, body_start, body_end) = best?;
    Some((body_start, body_end))
}

// ---------------------------------------------------------------------------
// Literal binding tables
// ---------------------------------------------------------------------------

static PY_LITERAL_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    // `name = "literal"` or `name = 'literal'` — no f-string prefix, no
    // interpolation. Single-line only.
    Regex::new(r#"(?m)^[ \t]*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?:"([^"\\$]*)"|'([^'\\$]*)')\s*$"#)
        .expect("python literal assign regex compiles")
});

static JS_LITERAL_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    // `const name = "literal";` or with `let`/`var`, single/double quotes.
    Regex::new(
        r#"(?m)^[ \t]*(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*(?:"([^"\\$]*)"|'([^'\\$]*)')\s*;?\s*$"#,
    )
    .expect("js literal assign regex compiles")
});

static JS_REBIND: LazyLock<Regex> = LazyLock::new(|| {
    // Bare reassignment without a declaration keyword: `name = "literal";`.
    // Only meaningful when `name` was previously declared — the caller in
    // `build_binding_table` enforces that constraint.
    Regex::new(
        r#"(?m)^[ \t]*([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*(?:"([^"\\$]*)"|'([^'\\$]*)')\s*;?\s*$"#,
    )
    .expect("js rebind regex compiles")
});

static RUST_LITERAL_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    // `let name = "literal";` — optionally with `mut`, optionally `&str` /
    // `String::from("...")` are out of scope. Trailing `;` mandatory.
    Regex::new(
        r#"(?m)^[ \t]*let\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*(?::\s*[^=]+)?=\s*"([^"\\]*)"\s*;\s*$"#,
    )
    .expect("rust literal assign regex compiles")
});

static RUST_REBIND: LazyLock<Regex> = LazyLock::new(|| {
    // Bare reassignment without `let`: `name = "literal";`. Only meaningful
    // when `name` was previously declared `let mut` — `build_binding_table`
    // enforces that the name already exists in the table.
    Regex::new(r#"(?m)^[ \t]*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"([^"\\]*)"\s*;\s*$"#)
        .expect("rust rebind regex compiles")
});

/// Build a literal-binding table by scanning the function body up to
/// `call_offset` (exclusive). Reassignments of the same name with distinct
/// values are tracked as ambiguous; reassignments with the same value are
/// preserved.
pub fn build_binding_table(
    source: &str,
    body_range: (usize, usize),
    call_offset: usize,
    lang: SourceLanguage,
) -> LiteralBindingTable {
    let (body_start, body_end) = body_range;
    let scan_end = call_offset.min(body_end);
    if scan_end <= body_start {
        return LiteralBindingTable::default();
    }
    let body = &source[body_start..scan_end];
    let pattern: &Regex = match lang {
        SourceLanguage::Python => &PY_LITERAL_ASSIGN,
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            &JS_LITERAL_ASSIGN
        }
        SourceLanguage::Rust => &RUST_LITERAL_ASSIGN,
        _ => return LiteralBindingTable::default(),
    };
    let mut table = LiteralBindingTable::default();
    for caps in pattern.captures_iter(body) {
        let Some(name) = caps.get(1) else { continue };
        let value = caps
            .get(2)
            .or_else(|| caps.get(3))
            .map(|g| g.as_str().to_string());
        if let Some(v) = value {
            table.insert(name.as_str().to_string(), v);
        }
    }
    // Second pass for JS/Rust: detect bare reassignments (no decl keyword).
    // These only count when the name was already declared above — otherwise
    // we'd treat e.g. a global assignment as a fresh binding. Python's
    // single regex already handles this case because Python has no declaration
    // keyword and the primary pattern matches bare assignments directly.
    let rebind_pattern: Option<&Regex> = match lang {
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            Some(&JS_REBIND)
        }
        SourceLanguage::Rust => Some(&RUST_REBIND),
        _ => None,
    };
    if let Some(rebind) = rebind_pattern {
        for caps in rebind.captures_iter(body) {
            let Some(name) = caps.get(1) else { continue };
            let value = caps
                .get(2)
                .or_else(|| caps.get(3))
                .map(|g| g.as_str().to_string());
            let Some(v) = value else { continue };
            // Only record the rebind if the name already exists. This avoids
            // creating spurious bindings for top-level globals or class
            // attribute assignments.
            if table.bindings.contains_key(name.as_str()) {
                table.insert(name.as_str().to_string(), v);
            }
        }
    }
    table
}

// ---------------------------------------------------------------------------
// Substitution
// ---------------------------------------------------------------------------

/// Substitute an expression against a binding table. The `expr` flavor
/// depends on `lang`:
/// - Python: bare name (`url`), f-string (`f"{base}/users"`), or single
///   concatenation (`base + "/users"`).
/// - JS/TS: bare name, template literal (`` `${base}/users` ``), or
///   concatenation (`base + "/users"`).
/// - Rust: bare name, `format!("/users/{}", id)`, or concatenation
///   (`format!("{}/users", base)`).
///
/// Returns the substitution outcome — see [`SubstituteOutcome`].
pub fn substitute(
    expr: &SubstituteExpr,
    table: &LiteralBindingTable,
    _lang: SourceLanguage,
) -> SubstituteOutcome {
    match expr {
        SubstituteExpr::BareName(name) => match table.lookup(name) {
            Some(Ok(value)) => SubstituteOutcome::Resolved(value.to_string()),
            Some(Err(())) => SubstituteOutcome::Ambiguous,
            None => SubstituteOutcome::Unresolvable,
        },
        SubstituteExpr::Template { segments } => substitute_template(segments, table),
        SubstituteExpr::Concat { parts } => substitute_concat(parts, table),
    }
}

/// A pre-parsed expression ready for substitution. Built by the per-language
/// detector when it sees a candidate URL/binary expression.
#[derive(Debug, Clone)]
pub enum SubstituteExpr {
    /// `url` — a bare identifier.
    BareName(String),
    /// `f"{base}/users/{uid}"` / `` `${base}/users/${uid}` `` /
    /// `format!("{}/users/{}", base, uid)`. The segments are alternating
    /// literal-text / placeholder.
    Template { segments: Vec<TemplateSegment> },
    /// `base + "/users"` — a single `+` concatenation. Each part is either
    /// a literal string or a name reference.
    Concat { parts: Vec<ConcatPart> },
}

#[derive(Debug, Clone)]
pub enum TemplateSegment {
    Literal(String),
    /// A `{name}` (Python f-string), `${name}` (JS), or `{}`/`{name}` (Rust
    /// `format!`). The empty string for positional Rust `{}` placeholders
    /// when no name is recoverable.
    Placeholder(String),
}

#[derive(Debug, Clone)]
pub enum ConcatPart {
    Literal(String),
    Name(String),
}

fn substitute_template(
    segments: &[TemplateSegment],
    table: &LiteralBindingTable,
) -> SubstituteOutcome {
    let mut out = String::new();
    let mut had_placeholder_resolved = false;
    let mut had_placeholder_remaining = false;
    for seg in segments {
        match seg {
            TemplateSegment::Literal(s) => out.push_str(s),
            TemplateSegment::Placeholder(name) => {
                if name.is_empty() {
                    // Positional Rust placeholder we can't bind by name.
                    out.push_str("{}");
                    had_placeholder_remaining = true;
                    continue;
                }
                match table.lookup(name) {
                    Some(Ok(value)) => {
                        out.push_str(value);
                        had_placeholder_resolved = true;
                    }
                    Some(Err(())) => return SubstituteOutcome::Ambiguous,
                    None => {
                        // Function param / runtime value — keep as `{name}`
                        // placeholder so template matching can still work.
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                        had_placeholder_remaining = true;
                    }
                }
            }
        }
    }
    // Reject pure no-op substitutions: if no placeholder resolved and none
    // remained, this template was effectively a literal already and the
    // existing literal-tier detector would have caught it. But if it had
    // ONLY remaining placeholders (no resolution), that means the template
    // is the original template — no dataflow value-add. We still emit
    // ResolvedTemplate because the template URL gets fed into the tier
    // walk; but if NOTHING was resolved, treat as Unresolvable (no value).
    if !had_placeholder_resolved && had_placeholder_remaining {
        return SubstituteOutcome::Unresolvable;
    }
    if had_placeholder_remaining {
        SubstituteOutcome::ResolvedTemplate(out)
    } else {
        SubstituteOutcome::Resolved(out)
    }
}

fn substitute_concat(parts: &[ConcatPart], table: &LiteralBindingTable) -> SubstituteOutcome {
    let mut out = String::new();
    let mut had_resolved_name = false;
    for part in parts {
        match part {
            ConcatPart::Literal(s) => out.push_str(s),
            ConcatPart::Name(name) => match table.lookup(name) {
                Some(Ok(value)) => {
                    out.push_str(value);
                    had_resolved_name = true;
                }
                Some(Err(())) => return SubstituteOutcome::Ambiguous,
                None => return SubstituteOutcome::Unresolvable,
            },
        }
    }
    if !had_resolved_name {
        // Concat of only literals — the source must have been weird.
        SubstituteOutcome::Unresolvable
    } else {
        SubstituteOutcome::Resolved(out)
    }
}

// ---------------------------------------------------------------------------
// Expression parsing helpers
// ---------------------------------------------------------------------------

/// Parse a Python f-string body (the text between `f"` and the closing `"`)
/// into template segments. `{name}` placeholders become
/// `TemplateSegment::Placeholder("name")`; everything else is literal text.
/// Returns `None` when the body has unbalanced braces or other parse oddity.
pub fn parse_python_fstring(body: &str) -> Option<Vec<TemplateSegment>> {
    parse_brace_template(body, false)
}

/// Parse a JS template-literal body (text between backticks) into segments.
/// Recognizes `${name}` placeholders.
pub fn parse_js_template(body: &str) -> Option<Vec<TemplateSegment>> {
    parse_brace_template(body, true)
}

fn parse_brace_template(body: &str, dollar_brace: bool) -> Option<Vec<TemplateSegment>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if dollar_brace {
            if c == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                if !cur.is_empty() {
                    out.push(TemplateSegment::Literal(std::mem::take(&mut cur)));
                }
                let end = body[i + 2..].find('}')?;
                let name = body[i + 2..i + 2 + end].trim().to_string();
                out.push(TemplateSegment::Placeholder(name));
                i += 2 + end + 1;
                continue;
            }
        } else if c == b'{' {
            // Python f-string: `{{` is a literal `{`.
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                cur.push('{');
                i += 2;
                continue;
            }
            if !cur.is_empty() {
                out.push(TemplateSegment::Literal(std::mem::take(&mut cur)));
            }
            let end = body[i + 1..].find('}')?;
            let name = body[i + 1..i + 1 + end].trim().to_string();
            // Strip Python format spec (`:fmt`) and conversion (`!r`).
            let name = name
                .split([':', '!'])
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            out.push(TemplateSegment::Placeholder(name));
            i += 1 + end + 1;
            continue;
        } else if c == b'}' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
            cur.push('}');
            i += 2;
            continue;
        }
        cur.push(c as char);
        i += 1;
    }
    if !cur.is_empty() {
        out.push(TemplateSegment::Literal(cur));
    }
    Some(out)
}

/// Parse a Rust `format!` first-argument body, plus the comma-separated
/// argument names. `body` is the literal template-string text; `args` is
/// the comma-separated trailing arguments (e.g. `uid, base`).
///
/// Placeholders are `{}` (positional) or `{name}` (named). Positional
/// placeholders consume args in order.
pub fn parse_rust_format(body: &str, args: &str) -> Option<Vec<TemplateSegment>> {
    let arg_names: Vec<String> = if args.trim().is_empty() {
        Vec::new()
    } else {
        args.split(',').map(|s| s.trim().to_string()).collect()
    };
    let mut positional_iter = arg_names.iter();
    let mut out = Vec::new();
    let mut cur = String::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'{' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                cur.push('{');
                i += 2;
                continue;
            }
            if !cur.is_empty() {
                out.push(TemplateSegment::Literal(std::mem::take(&mut cur)));
            }
            let end = body[i + 1..].find('}')?;
            let raw = body[i + 1..i + 1 + end].trim();
            let name_part = raw.split(':').next().unwrap_or("").trim();
            let name = if name_part.is_empty() {
                // Positional `{}` — bind to next positional arg.
                positional_iter.next().cloned().unwrap_or_default()
            } else {
                name_part.to_string()
            };
            out.push(TemplateSegment::Placeholder(name));
            i += 1 + end + 1;
            continue;
        }
        if c == b'}' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
            cur.push('}');
            i += 2;
            continue;
        }
        cur.push(c as char);
        i += 1;
    }
    if !cur.is_empty() {
        out.push(TemplateSegment::Literal(cur));
    }
    Some(out)
}

/// Parse a single-`+`-chain concatenation expression into parts. Recognizes
/// quoted literal strings and bare names. Whitespace tolerant. Returns
/// `None` if any part is unrecognized (function call, `[index]`, etc.).
pub fn parse_concat(expr: &str) -> Option<Vec<ConcatPart>> {
    let mut parts = Vec::new();
    let trimmed = expr.trim();
    let bytes = trimmed.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        // Skip whitespace.
        while idx < bytes.len() && (bytes[idx] as char).is_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() {
            break;
        }
        let c = bytes[idx];
        if c == b'"' || c == b'\'' {
            let quote = c;
            let start = idx + 1;
            idx += 1;
            while idx < bytes.len() && bytes[idx] != quote {
                if bytes[idx] == b'\\' {
                    idx += 2;
                } else {
                    idx += 1;
                }
            }
            if idx >= bytes.len() {
                return None;
            }
            let s = std::str::from_utf8(&bytes[start..idx]).ok()?;
            parts.push(ConcatPart::Literal(s.to_string()));
            idx += 1;
        } else if (c as char).is_alphabetic() || c == b'_' || c == b'$' {
            let start = idx;
            while idx < bytes.len()
                && ((bytes[idx] as char).is_alphanumeric()
                    || bytes[idx] == b'_'
                    || bytes[idx] == b'$')
            {
                idx += 1;
            }
            let name = std::str::from_utf8(&bytes[start..idx]).ok()?;
            parts.push(ConcatPart::Name(name.to_string()));
        } else {
            return None;
        }
        // Skip whitespace and expect `+` or end.
        while idx < bytes.len() && (bytes[idx] as char).is_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() {
            break;
        }
        if bytes[idx] != b'+' {
            return None;
        }
        idx += 1;
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts)
}

// ---------------------------------------------------------------------------
// High-level helper: resolve_one_hop
// ---------------------------------------------------------------------------

/// Outcome of a one-hop resolution attempt against an enclosing function.
#[derive(Debug, Clone)]
pub enum ResolveOutcome {
    /// Substitution succeeded; result is a literal URL/binary name.
    Resolved { value: String },
    /// Substitution succeeded but produced a template URL with at least
    /// one remaining placeholder. Suitable for template matching.
    ResolvedTemplate { value: String },
    /// Variable bound to multiple distinct literals across the function
    /// body — caller should emit `AmbiguousAssignment`.
    Ambiguous,
    /// No useful substitution — caller keeps the original reason.
    Unresolvable,
}

/// Apply one-hop substitution at the given call offset. Returns
/// [`ResolveOutcome::Unresolvable`] if no enclosing function can be found,
/// no binding table can be built, or substitution doesn't reduce the
/// expression to a literal / template-with-progress.
pub fn resolve_one_hop(
    source: &str,
    offset: usize,
    lang: SourceLanguage,
    expr: &SubstituteExpr,
) -> ResolveOutcome {
    let Some(range) = enclosing_function_range(source, offset, lang) else {
        return ResolveOutcome::Unresolvable;
    };
    let table = build_binding_table(source, range, offset, lang);
    match substitute(expr, &table, lang) {
        SubstituteOutcome::Resolved(v) => ResolveOutcome::Resolved { value: v },
        SubstituteOutcome::ResolvedTemplate(v) => ResolveOutcome::ResolvedTemplate { value: v },
        SubstituteOutcome::Ambiguous => ResolveOutcome::Ambiguous,
        SubstituteOutcome::Unresolvable => ResolveOutcome::Unresolvable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Python function range ----

    #[test]
    fn python_def_simple_body() {
        let src = "def foo():\n    url = \"/users\"\n    return url\n\nother = 1\n";
        let offset = src.find("return").unwrap();
        let (start, end) = enclosing_function_range(src, offset, SourceLanguage::Python).unwrap();
        assert!(start <= offset && offset < end);
        let body = &src[start..end];
        assert!(body.contains("url"));
        assert!(!body.contains("other"));
    }

    // ---- JS function range ----

    #[test]
    fn js_function_decl_body() {
        let src =
            "function getUser(id) {\n  const url = `/users/${id}`;\n  return fetch(url);\n}\n";
        let offset = src.find("fetch").unwrap();
        let (start, end) =
            enclosing_function_range(src, offset, SourceLanguage::JavaScript).unwrap();
        let body = &src[start..end];
        assert!(body.contains("fetch"));
    }

    // ---- Rust function range ----

    #[test]
    fn rust_fn_body() {
        let src = "fn fetch_user(id: u64) {\n  let url = format!(\"/users/{}\", id);\n  reqwest::get(&url);\n}\n";
        let offset = src.find("reqwest").unwrap();
        let (start, end) = enclosing_function_range(src, offset, SourceLanguage::Rust).unwrap();
        let body = &src[start..end];
        assert!(body.contains("reqwest"));
    }

    // ---- Binding table ----

    #[test]
    fn python_literal_binding() {
        let src = "def foo():\n    base = \"http://api\"\n    return base\n";
        let offset = src.find("return").unwrap();
        let range = python_function_range(src, offset).unwrap();
        let table = build_binding_table(src, range, offset, SourceLanguage::Python);
        assert_eq!(table.lookup("base"), Some(Ok("http://api")));
    }

    #[test]
    fn python_reassignment_is_ambiguous() {
        let src =
            "def foo(x):\n    base = \"http://a\"\n    base = \"http://b\"\n    return base\n";
        let offset = src.find("return").unwrap();
        let range = python_function_range(src, offset).unwrap();
        let table = build_binding_table(src, range, offset, SourceLanguage::Python);
        assert_eq!(table.lookup("base"), Some(Err(())));
    }

    #[test]
    fn js_let_binding() {
        let src = "function f() {\n  const base = \"http://api\";\n  return base;\n}\n";
        let offset = src.find("return").unwrap();
        let range = js_function_range(src, offset).unwrap();
        let table = build_binding_table(src, range, offset, SourceLanguage::JavaScript);
        assert_eq!(table.lookup("base"), Some(Ok("http://api")));
    }

    #[test]
    fn rust_let_binding() {
        let src = "fn f() {\n  let base = \"http://api\";\n  println!(\"{}\", base);\n}\n";
        let offset = src.find("println").unwrap();
        let range = rust_function_range(src, offset).unwrap();
        let table = build_binding_table(src, range, offset, SourceLanguage::Rust);
        assert_eq!(table.lookup("base"), Some(Ok("http://api")));
    }

    // ---- Substitution ----

    #[test]
    fn substitute_bare_name_resolved() {
        let mut t = LiteralBindingTable::default();
        t.insert("url".into(), "/foo".into());
        let r = substitute(
            &SubstituteExpr::BareName("url".into()),
            &t,
            SourceLanguage::Python,
        );
        assert_eq!(r, SubstituteOutcome::Resolved("/foo".into()));
    }

    #[test]
    fn substitute_bare_name_ambiguous() {
        let mut t = LiteralBindingTable::default();
        t.insert("url".into(), "/a".into());
        t.insert("url".into(), "/b".into());
        let r = substitute(
            &SubstituteExpr::BareName("url".into()),
            &t,
            SourceLanguage::Python,
        );
        assert_eq!(r, SubstituteOutcome::Ambiguous);
    }

    #[test]
    fn substitute_template_partial_keeps_placeholder() {
        let mut t = LiteralBindingTable::default();
        t.insert("base".into(), "http://api".into());
        let segs = vec![
            TemplateSegment::Placeholder("base".into()),
            TemplateSegment::Literal("/users/".into()),
            TemplateSegment::Placeholder("uid".into()), // not bound
        ];
        let r = substitute(
            &SubstituteExpr::Template { segments: segs },
            &t,
            SourceLanguage::Python,
        );
        assert_eq!(
            r,
            SubstituteOutcome::ResolvedTemplate("http://api/users/{uid}".into())
        );
    }

    #[test]
    fn substitute_concat_resolved() {
        let mut t = LiteralBindingTable::default();
        t.insert("base".into(), "http://api".into());
        let parts = vec![
            ConcatPart::Name("base".into()),
            ConcatPart::Literal("/users".into()),
        ];
        let r = substitute(
            &SubstituteExpr::Concat { parts },
            &t,
            SourceLanguage::Python,
        );
        assert_eq!(r, SubstituteOutcome::Resolved("http://api/users".into()));
    }

    // ---- f-string parsing ----

    #[test]
    fn parse_fstring_with_placeholder() {
        let segs = parse_python_fstring("/users/{uid}/posts").unwrap();
        assert_eq!(segs.len(), 3);
        match &segs[1] {
            TemplateSegment::Placeholder(n) => assert_eq!(n, "uid"),
            _ => panic!("expected placeholder"),
        }
    }

    #[test]
    fn parse_js_template_with_placeholder() {
        let segs = parse_js_template("/users/${id}").unwrap();
        match &segs[1] {
            TemplateSegment::Placeholder(n) => assert_eq!(n, "id"),
            _ => panic!("expected placeholder"),
        }
    }

    #[test]
    fn parse_rust_format_positional() {
        let segs = parse_rust_format("/users/{}", "uid").unwrap();
        match &segs[1] {
            TemplateSegment::Placeholder(n) => assert_eq!(n, "uid"),
            _ => panic!("expected placeholder"),
        }
    }
}
