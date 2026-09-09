//! Cross-language edge detection.
//!
//! Per-language detectors live in submodules. Each one is line-anchored regex
//! scanning — cheap, no tree-sitter pass — and intentionally conservative so
//! we never claim an edge we can't see. See
//! `docs/cross-language-edges-design.md` §3 for the taxonomy.

pub mod binaries;
pub mod dataflow;
pub mod http_client;
pub mod http_route;
pub mod js_subprocess;
pub mod makefile;
pub mod npm_scripts;
pub mod python_subprocess;
pub mod rust_process;

pub use binaries::collect_binary_nodes;
pub use http_client::{
    HttpDetectorOutput, detect_js_http_calls, detect_python_http_calls, detect_rust_http_calls,
};
pub use http_route::{
    detect_axum_routes, detect_express_routes, detect_fastapi_routes, detect_flask_routes,
    mount_cross_file_axum_nests,
};
pub use js_subprocess::detect_js_subprocess_calls;
pub use makefile::detect_makefile_invocations;
pub use npm_scripts::detect_npm_script_invocations;
pub use python_subprocess::detect_python_subprocess_calls;
pub use rust_process::detect_rust_process_commands;

use crate::model::{
    BinaryNode, FileRecord, MatchKind, ResolvedHttpEdge, ResolvedSpawnEdge, RouteRecord,
    SubprocessCallOccurrence, UnresolvedEdge,
};

/// Bundle of detector outputs: resolved spawn occurrences plus unresolved
/// edges classified by reason. Each per-language detector returns this so
/// the indexer can aggregate both streams uniformly.
#[derive(Debug, Default, Clone)]
pub struct DetectorOutput {
    pub spawns: Vec<SubprocessCallOccurrence>,
    pub unresolved: Vec<UnresolvedEdge>,
}

/// Truncate a snippet to 120 chars for `UnresolvedEdge::raw_snippet`. Strips
/// surrounding whitespace first; replaces newlines with single spaces so the
/// snippet stays one line.
pub(crate) fn truncate_snippet(s: &str) -> String {
    let one_line: String = s
        .trim()
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    if one_line.chars().count() <= 120 {
        one_line
    } else {
        one_line.chars().take(120).collect()
    }
}

/// Extract the source line containing the given byte offset. Used by detectors
/// to capture `raw_snippet` for unresolved edges.
pub(crate) fn line_at_offset(source: &str, offset: usize) -> &str {
    let offset = offset.min(source.len());
    let start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = source[offset..]
        .find('\n')
        .map(|i| offset + i)
        .unwrap_or(source.len());
    &source[start..end]
}

/// Match each [`SubprocessCallOccurrence`] in `files` against the known
/// [`BinaryNode`] registry. Returns resolved edges with `confidence: 95`
/// for exact (literal) binary-name matches, or `confidence: 80` when the
/// binary name was reconstructed by the Phase 8 light-dataflow pass.
pub fn resolve_spawn_edges(
    files: &[FileRecord],
    binaries: &[BinaryNode],
) -> Vec<ResolvedSpawnEdge> {
    let mut out = Vec::new();
    for file in files {
        for call in &file.subprocess_calls {
            if let Some(bin) = binaries.iter().find(|b| b.name == call.binary) {
                let confidence = if call.resolved_via_dataflow { 80 } else { 95 };
                out.push(ResolvedSpawnEdge {
                    caller_path: call.path.clone(),
                    caller_line: call.line,
                    caller_language: call.language,
                    callee_name: bin.name.clone(),
                    callee_path: bin.path.clone(),
                    confidence,
                });
            }
        }
    }
    out
}

/// Match each [`HttpCallOccurrence`] on the indexed files against the known
/// [`RouteRecord`] registry, using tiered resolution (Phase 6):
///
/// 1. **Literal** (confidence 95) — exact string match on the route path.
/// 2. **Normalized** (85) — equal after trailing-slash + double-slash
///    normalization. Skipped if tier 1 already produced edges for this call.
/// 3. **Template** (70) — segment-by-segment unification where each route
///    segment is either a literal (must equal the URL segment) or a path
///    parameter (matches any single segment). Skipped if a higher tier
///    already matched.
/// 4. **TemplateMethodless** (60) — same as Template, but the caller's HTTP
///    method was unknown (`*`).
///
/// Tier walking stops on the first non-empty tier so we don't emit a 95 and
/// a 70 edge for the same call/route pair. Within a tier, **all** matching
/// routes produce edges (so ambiguity surfaces as multiple candidates).
///
/// Method filtering is consistent across tiers: a call with explicit method
/// must match the route's method (or the route's method is `*`). A call
/// with method `*` matches any route method but, when a template match
/// fires, downgrades to [`MatchKind::TemplateMethodless`] (confidence 60).
///
/// Template URLs (`is_template: true`) only match template routes — a
/// literal-to-template fallback would be too noisy. See
/// `docs/cross-language-edges-design.md` §5 for rationale.
///
/// [`HttpCallOccurrence`]: crate::model::HttpCallOccurrence
pub fn resolve_http_edges(files: &[FileRecord], routes: &[RouteRecord]) -> Vec<ResolvedHttpEdge> {
    let mut out = Vec::new();
    for file in files {
        for call in &file.http_calls {
            let Some(call_path) = url_path_component(&call.url) else {
                continue;
            };

            // Tier 1: literal-to-literal exact match. Only valid for
            // non-template URLs against non-template routes. Phase 8: when
            // the URL came from dataflow substitution, downgrade the
            // confidence band from Literal (95) to DataflowLiteral (80).
            if !call.is_template {
                let tier1: Vec<&RouteRecord> = routes
                    .iter()
                    .filter(|r| r.path_params.is_empty() && r.route == call_path)
                    .filter(|r| method_compatible(&call.method, &r.method))
                    .collect();
                if !tier1.is_empty() {
                    let (conf, kind) = if call.resolved_via_dataflow {
                        (80, MatchKind::DataflowLiteral)
                    } else {
                        (95, MatchKind::Literal)
                    };
                    for route in tier1 {
                        out.push(make_edge(call, route, conf, kind));
                    }
                    continue;
                }
            }

            // Tier 2: normalized literal-to-literal. Trailing slashes and
            // collapsed `//` only. Phase 8: when the URL came from
            // dataflow, still surface as DataflowLiteral (80) — we don't
            // model a separate "dataflow + normalized" band.
            if !call.is_template {
                let normalized_call = normalize_path(&call_path);
                let tier2: Vec<&RouteRecord> = routes
                    .iter()
                    .filter(|r| {
                        r.path_params.is_empty() && normalize_path(&r.route) == normalized_call
                    })
                    .filter(|r| method_compatible(&call.method, &r.method))
                    .collect();
                if !tier2.is_empty() {
                    let (conf, kind) = if call.resolved_via_dataflow {
                        (80, MatchKind::DataflowLiteral)
                    } else {
                        (85, MatchKind::Normalized)
                    };
                    for route in tier2 {
                        out.push(make_edge(call, route, conf, kind));
                    }
                    continue;
                }
            }

            // Tier 3/4: template unification. Template URLs only match
            // template routes; literal URLs may match template routes.
            // Phase 8: when the URL came from dataflow substitution, the
            // band is DataflowTemplate (75) — sits between normalized (85)
            // and template (70). Methodless calls still drop to 60.
            let tier3: Vec<&RouteRecord> = routes
                .iter()
                .filter(|r| !r.path_params.is_empty())
                .filter(|r| !(call.is_template && r.path_params.is_empty()))
                .filter(|r| method_compatible(&call.method, &r.method))
                .filter(|r| segments_unify(&r.route, &call_path, call.is_template))
                .collect();
            if !tier3.is_empty() {
                let (conf, kind) = if call.method == "*" {
                    (60, MatchKind::TemplateMethodless)
                } else if call.resolved_via_dataflow {
                    (75, MatchKind::DataflowTemplate)
                } else {
                    (70, MatchKind::Template)
                };
                for route in tier3 {
                    out.push(make_edge(call, route, conf, kind));
                }
            }
        }
    }
    out
}

fn make_edge(
    call: &crate::model::HttpCallOccurrence,
    route: &RouteRecord,
    confidence: u8,
    match_kind: MatchKind,
) -> ResolvedHttpEdge {
    ResolvedHttpEdge {
        caller_path: call.path.clone(),
        caller_line: call.line,
        caller_language: call.language,
        route_path: route.route.clone(),
        route_method: route.method.clone(),
        route_source_path: route.path.clone(),
        confidence,
        match_kind,
    }
}

/// `true` when the call's method is compatible with the route's method.
/// Either side using `*` is a wildcard.
fn method_compatible(call_method: &str, route_method: &str) -> bool {
    call_method == "*" || route_method == "*" || call_method == route_method
}

/// Collapse `//` runs and strip a single trailing `/` (but keep the leading
/// one). Intentionally minimal — anything fancier is template territory.
fn normalize_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut prev_slash = false;
    for c in p.chars() {
        if c == '/' {
            if !prev_slash {
                out.push(c);
            }
            prev_slash = true;
        } else {
            out.push(c);
            prev_slash = false;
        }
    }
    // Trim trailing slash unless the whole string is just "/".
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Segment-by-segment unification of a route template against a URL path.
///
/// Both inputs split on `/`. Lengths must match. Each route segment is either:
///
/// - a **parameter** (`<int:id>`, `{id}`, `:id`) — matches any URL segment;
/// - a **literal** — must equal the URL segment exactly.
///
/// When `url_is_template` is `true`, the URL's parameter segments are
/// allowed in any position (we don't try to match placeholder names — that
/// would over-constrain). The position must still align with a route
/// parameter segment.
fn segments_unify(route: &str, url: &str, url_is_template: bool) -> bool {
    let route_segs: Vec<&str> = route.split('/').collect();
    let url_segs: Vec<&str> = url.split('/').collect();
    if route_segs.len() != url_segs.len() {
        return false;
    }
    for (rs, us) in route_segs.iter().zip(url_segs.iter()) {
        let route_is_param = is_route_param_segment(rs);
        let url_is_param = url_is_template && is_url_param_segment(us);
        if route_is_param {
            // Route param matches any URL segment (literal or templated).
            continue;
        }
        if url_is_param {
            // Constraint from the contract: template URL segments must
            // align with route parameter segments. A template URL whose
            // placeholder lines up with a literal route segment is NOT a
            // match — the route author intended that segment to be a
            // fixed value.
            return false;
        }
        if rs != us {
            return false;
        }
    }
    true
}

/// Is this route segment a path-parameter slot? Recognizes Flask
/// (`<...>`), FastAPI/axum (`{...}`), and Express (`:name`).
fn is_route_param_segment(seg: &str) -> bool {
    let trimmed = seg.trim();
    if trimmed.is_empty() {
        return false;
    }
    (trimmed.starts_with('<') && trimmed.ends_with('>'))
        || (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || trimmed.starts_with(':')
}

/// Is this URL segment a template placeholder? Recognizes f-string
/// (`{name}`), JS template (`${name}`), and Rust `format!` (`{}` or
/// `{name}`) markers. Only used when the URL itself is templated.
fn is_url_param_segment(seg: &str) -> bool {
    let trimmed = seg.trim();
    if trimmed.is_empty() {
        return false;
    }
    // `${...}` (JS) or `{...}` / `{}` (Python f-string / Rust format!).
    if trimmed.starts_with("${") && trimmed.ends_with('}') {
        return true;
    }
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return true;
    }
    false
}

/// Strip the scheme + host from a URL so it can be matched against a route
/// path. Returns `None` when no path component is comparable to a route
/// (e.g. `"users"` with no leading slash).
///
/// Examples:
/// - `"https://api/foo/bar"` → `Some("/foo/bar")`
/// - `"/foo/bar"` → `Some("/foo/bar")`
/// - `"foo/bar"` → `None`
pub(crate) fn url_path_component(url: &str) -> Option<String> {
    let after_scheme = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => url,
    };
    let path = if after_scheme == url {
        // No scheme prefix: keep the original.
        url
    } else {
        // Scheme was stripped — take everything from the first `/`.
        match after_scheme.find('/') {
            Some(i) => &after_scheme[i..],
            None => "",
        }
    };
    if path.starts_with('/') {
        Some(path.to_string())
    } else {
        None
    }
}

/// 1-based line number for a byte offset into `source` — same convention as
/// the other occurrence types in `model.rs`.
pub(crate) fn line_number_for_offset(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BinaryNodeSource, SourceLanguage, SubprocessCallOccurrence};

    fn make_file(path: &str, calls: Vec<(&str, usize)>) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Python,
            bytes: 0,
            modified_unix_ms: 0,
            symbols: Vec::new(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: calls
                .into_iter()
                .map(|(binary, line)| SubprocessCallOccurrence {
                    binary: binary.to_string(),
                    path: path.to_string(),
                    line,
                    language: SourceLanguage::Python,
                    resolved_via_dataflow: false,
                })
                .collect(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        }
    }

    fn make_bin(name: &str, bin_path: &str) -> BinaryNode {
        BinaryNode {
            name: name.to_string(),
            path: bin_path.to_string(),
            source: BinaryNodeSource::CargoImplicit,
        }
    }

    #[test]
    fn exact_name_match_produces_edge() {
        let files = vec![make_file("caller.py", vec![("leio-code", 5)])];
        let binaries = vec![make_bin("leio-code", "src/main.rs")];
        let edges = resolve_spawn_edges(&files, &binaries);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].callee_name, "leio-code");
        assert_eq!(edges[0].caller_line, 5);
        assert_eq!(edges[0].confidence, 95);
    }

    #[test]
    fn no_match_produces_no_edge() {
        let files = vec![make_file("caller.py", vec![("unknown-bin", 3)])];
        let binaries = vec![make_bin("leio-code", "src/main.rs")];
        let edges = resolve_spawn_edges(&files, &binaries);
        assert!(edges.is_empty());
    }

    #[test]
    fn match_is_case_sensitive() {
        let files = vec![make_file("caller.py", vec![("Leio-Code", 1)])];
        let binaries = vec![make_bin("leio-code", "src/main.rs")];
        let edges = resolve_spawn_edges(&files, &binaries);
        assert!(edges.is_empty(), "match must be case-sensitive");
    }

    #[test]
    fn multiple_callers_same_binary() {
        let files = vec![
            make_file("a.py", vec![("leio-code", 2)]),
            make_file("b.py", vec![("leio-code", 7)]),
        ];
        let binaries = vec![make_bin("leio-code", "src/main.rs")];
        let edges = resolve_spawn_edges(&files, &binaries);
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn empty_subprocess_calls_no_edges() {
        let files = vec![make_file("a.py", vec![])];
        let binaries = vec![make_bin("leio-code", "src/main.rs")];
        let edges = resolve_spawn_edges(&files, &binaries);
        assert!(edges.is_empty());
    }

    // ---- url_path_component ----

    #[test]
    fn url_path_strips_scheme_and_host() {
        assert_eq!(
            url_path_component("https://api.example.com/users"),
            Some("/users".to_string())
        );
    }

    #[test]
    fn url_path_keeps_already_path() {
        assert_eq!(url_path_component("/users"), Some("/users".to_string()));
    }

    #[test]
    fn url_path_rejects_bare_segment() {
        assert!(url_path_component("users").is_none());
    }

    #[test]
    fn url_path_handles_host_with_no_path() {
        assert!(url_path_component("https://api.example.com").is_none());
    }

    // ---- resolve_http_edges ----

    fn http_file(path: &str, calls: Vec<crate::model::HttpCallOccurrence>) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Python,
            bytes: 0,
            modified_unix_ms: 0,
            symbols: Vec::new(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: calls,
            unresolved_edges: Vec::new(),
        }
    }

    fn http_call(
        path: &str,
        method: &str,
        url: &str,
        client: &str,
    ) -> crate::model::HttpCallOccurrence {
        crate::model::HttpCallOccurrence {
            path: path.to_string(),
            line: 1,
            method: method.to_string(),
            url: url.to_string(),
            language: SourceLanguage::Python,
            client: client.to_string(),
            is_template: false,
            resolved_via_dataflow: false,
        }
    }

    fn template_http_call(
        path: &str,
        method: &str,
        url: &str,
        client: &str,
    ) -> crate::model::HttpCallOccurrence {
        crate::model::HttpCallOccurrence {
            path: path.to_string(),
            line: 1,
            method: method.to_string(),
            url: url.to_string(),
            language: SourceLanguage::Python,
            client: client.to_string(),
            is_template: true,
            resolved_via_dataflow: false,
        }
    }

    fn route(path: &str, method: &str, route_str: &str, framework: &str) -> RouteRecord {
        RouteRecord {
            handler: None,
            auth_hint: None,
            path: path.to_string(),
            line: 1,
            method: method.to_string(),
            route: route_str.to_string(),
            framework: framework.to_string(),
            language: SourceLanguage::Python,
            path_params: Vec::new(),
        }
    }

    fn template_route(
        path: &str,
        method: &str,
        route_str: &str,
        framework: &str,
        params: Vec<&str>,
    ) -> RouteRecord {
        RouteRecord {
            handler: None,
            auth_hint: None,
            path: path.to_string(),
            line: 1,
            method: method.to_string(),
            route: route_str.to_string(),
            framework: framework.to_string(),
            language: SourceLanguage::Python,
            path_params: params.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn http_literal_match_emits_edge() {
        let files = vec![http_file(
            "caller.py",
            vec![http_call(
                "caller.py",
                "GET",
                "http://localhost/users",
                "requests",
            )],
        )];
        let routes = vec![route("server.py", "GET", "/users", "flask")];
        let edges = resolve_http_edges(&files, &routes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].route_path, "/users");
        assert_eq!(edges[0].confidence, 95);
        assert_eq!(edges[0].route_source_path, "server.py");
    }

    #[test]
    fn http_method_mismatch_drops_edge() {
        let files = vec![http_file(
            "caller.py",
            vec![http_call("caller.py", "POST", "/users", "requests")],
        )];
        let routes = vec![route("server.py", "GET", "/users", "flask")];
        let edges = resolve_http_edges(&files, &routes);
        assert!(
            edges.is_empty(),
            "POST call should not resolve to GET route"
        );
    }

    #[test]
    fn http_unknown_method_matches_any() {
        let files = vec![http_file(
            "caller.ts",
            vec![http_call("caller.ts", "*", "/items", "fetch")],
        )];
        let routes = vec![route("server.py", "GET", "/items", "fastapi")];
        let edges = resolve_http_edges(&files, &routes);
        assert_eq!(
            edges.len(),
            1,
            "unknown-method call should match any method"
        );
    }

    #[test]
    fn http_bare_segment_url_is_not_resolved() {
        let files = vec![http_file(
            "caller.py",
            vec![http_call("caller.py", "GET", "users", "requests")],
        )];
        let routes = vec![route("server.py", "GET", "/users", "flask")];
        let edges = resolve_http_edges(&files, &routes);
        assert!(edges.is_empty(), "non-path-prefixed URL must not resolve");
    }

    // ---- Phase 6: tiered resolution ----

    #[test]
    fn http_normalized_trailing_slash_match_yields_85() {
        // Client calls `/users/`, route declares `/users`. After
        // trailing-slash normalization they match — tier 2.
        let files = vec![http_file(
            "caller.py",
            vec![http_call("caller.py", "GET", "/users/", "requests")],
        )];
        let routes = vec![route("server.py", "GET", "/users", "flask")];
        let edges = resolve_http_edges(&files, &routes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].confidence, 85);
        assert_eq!(edges[0].match_kind, MatchKind::Normalized);
    }

    #[test]
    fn http_template_match_yields_70() {
        // Literal client URL `/users/42` against templated Flask route
        // `/users/<int:id>` — segment-by-segment unification, tier 3.
        let files = vec![http_file(
            "caller.py",
            vec![http_call("caller.py", "GET", "/users/42", "requests")],
        )];
        let routes = vec![template_route(
            "server.py",
            "GET",
            "/users/<int:id>",
            "flask",
            vec!["id"],
        )];
        let edges = resolve_http_edges(&files, &routes);
        assert_eq!(edges.len(), 1, "got: {:#?}", edges);
        assert_eq!(edges[0].confidence, 70);
        assert_eq!(edges[0].match_kind, MatchKind::Template);
        assert_eq!(edges[0].route_path, "/users/<int:id>");
    }

    #[test]
    fn http_methodless_template_match_yields_60() {
        // Bare fetch (method `*`) against templated FastAPI route.
        let files = vec![http_file(
            "caller.ts",
            vec![http_call("caller.ts", "*", "/users/42", "fetch")],
        )];
        let routes = vec![template_route(
            "server.py",
            "GET",
            "/users/{id}",
            "fastapi",
            vec!["id"],
        )];
        let edges = resolve_http_edges(&files, &routes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].confidence, 60);
        assert_eq!(edges[0].match_kind, MatchKind::TemplateMethodless);
    }

    #[test]
    fn http_template_url_matches_template_route() {
        // Templated client URL must align position-wise with template route.
        let files = vec![http_file(
            "caller.ts",
            vec![template_http_call(
                "caller.ts",
                "GET",
                "/users/${id}",
                "fetch",
            )],
        )];
        let routes = vec![template_route(
            "server.js",
            "GET",
            "/users/:uid",
            "express",
            vec!["uid"],
        )];
        let edges = resolve_http_edges(&files, &routes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].match_kind, MatchKind::Template);
    }

    #[test]
    fn http_template_url_does_not_match_literal_route() {
        // Per design: literal-to-template would be too noisy. Templated URLs
        // ONLY match templated routes.
        let files = vec![http_file(
            "caller.ts",
            vec![template_http_call(
                "caller.ts",
                "GET",
                "/users/${id}",
                "fetch",
            )],
        )];
        let routes = vec![route("server.py", "GET", "/users/42", "flask")];
        let edges = resolve_http_edges(&files, &routes);
        assert!(
            edges.is_empty(),
            "template URL must not match literal route"
        );
    }

    #[test]
    fn http_template_mismatched_segment_count_no_match() {
        // `/users/42/extra` cannot unify with `/users/{id}` — different
        // segment counts.
        let files = vec![http_file(
            "caller.py",
            vec![http_call("caller.py", "GET", "/users/42/extra", "requests")],
        )];
        let routes = vec![template_route(
            "server.py",
            "GET",
            "/users/{id}",
            "fastapi",
            vec!["id"],
        )];
        let edges = resolve_http_edges(&files, &routes);
        assert!(edges.is_empty());
    }

    #[test]
    fn http_template_mismatched_literal_segment_no_match() {
        // `/admins/42` vs `/users/{id}` — literal mismatch on first segment.
        let files = vec![http_file(
            "caller.py",
            vec![http_call("caller.py", "GET", "/admins/42", "requests")],
        )];
        let routes = vec![template_route(
            "server.py",
            "GET",
            "/users/{id}",
            "fastapi",
            vec!["id"],
        )];
        let edges = resolve_http_edges(&files, &routes);
        assert!(edges.is_empty());
    }

    #[test]
    fn http_literal_match_short_circuits_template() {
        // When a literal-tier match exists, we must NOT also emit a template
        // edge for the same call. Confidence stays 95.
        let files = vec![http_file(
            "caller.py",
            vec![http_call("caller.py", "GET", "/users", "requests")],
        )];
        let routes = vec![
            route("server.py", "GET", "/users", "flask"),
            template_route(
                "server2.py",
                "GET",
                "/{anything}",
                "fastapi",
                vec!["anything"],
            ),
        ];
        let edges = resolve_http_edges(&files, &routes);
        assert_eq!(edges.len(), 1, "tier walk should stop on first hit");
        assert_eq!(edges[0].confidence, 95);
        assert_eq!(edges[0].match_kind, MatchKind::Literal);
    }

    // ---- normalize_path direct ----

    #[test]
    fn normalize_path_strips_trailing_slash() {
        assert_eq!(normalize_path("/users/"), "/users");
        assert_eq!(normalize_path("/users"), "/users");
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn normalize_path_collapses_double_slashes() {
        assert_eq!(normalize_path("/a//b"), "/a/b");
        assert_eq!(normalize_path("//a///b//"), "/a/b");
    }
}
