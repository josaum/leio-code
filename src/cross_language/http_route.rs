//! HTTP route declarations across server frameworks.
//!
//! Each detector scans `source` for framework-specific route decorators or
//! builder calls where the route path is a string literal. Non-literal paths
//! (variables, f-strings, template literals) are intentionally skipped — we
//! never claim an edge we can't see. See `docs/cross-language-edges-design.md`
//! §5 for the broader HTTP edge story.
//!
//! Phase 6 adds path-parameter extraction. The framework-specific syntaxes
//! (`<int:id>`, `{id}`, `:id`) are parsed into a `path_params: Vec<String>`
//! and the route string itself is kept verbatim — `resolve_http_edges` does
//! segment-by-segment unification at match time, so we don't normalize the
//! syntaxes here.
//!
//! Conventions:
//! - The HTTP method is recorded as an uppercase string ("GET", "POST", ...);
//!   `"*"` is reserved for catch-all mounts (`router.use(...)`, axum
//!   `.route_service`-style patterns are not supported in v1).
//! - Single-quoted string literals are also recognized for JS/TS frameworks.
//! - Regex-only — no tree-sitter pass. Literal-only by design.
//!
//! The `regex` crate does not support backreferences, so JS/TS detectors use
//! an alternation between double- and single-quoted bodies; the capture index
//! picks whichever side matched.

use std::sync::LazyLock;

use regex::Regex;

use crate::model::{RouteRecord, SourceLanguage};

use super::line_number_for_offset;

// ---------------------------------------------------------------------------
// Flask
// ---------------------------------------------------------------------------

static FLASK_ROUTE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // @<obj>.route("/path"[, methods=["GET", "POST"]])
    Regex::new(r#"@\w+\.route\s*\(\s*"([^"$]+?)"(?:\s*,\s*methods\s*=\s*\[([^\]]+)\])?\s*\)"#)
        .expect("flask route regex compiles")
});

/// Flask path parameter: `<id>` or `<int:id>`. Captures only the name.
static FLASK_PARAM_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<(?:[^:<>]+:)?([^<>]+)>"#).expect("flask param regex compiles"));

/// Detect Flask `@app.route("/path", methods=[...])` declarations.
pub fn detect_flask_routes(source: &str, path: &str) -> Vec<RouteRecord> {
    let mut out = Vec::new();
    for caps in FLASK_ROUTE_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(route) = caps.get(1) else { continue };
        let route_str = route.as_str().to_string();
        if route_str.is_empty() || route_str.contains('{') {
            // Flask uses `<param>` syntax; literal `{` in the path is unusual
            // and would conflict with the FastAPI-style template syntax.
            continue;
        }
        let line = line_number_for_offset(source, whole.start());
        let path_params = extract_flask_params(&route_str);

        let methods = match caps.get(2) {
            Some(m) => parse_methods_list(m.as_str()),
            None => vec!["GET".to_string()],
        };
        for method in methods {
            out.push(RouteRecord {
                path: path.to_string(),
                line,
                method,
                route: route_str.clone(),
                framework: "flask".to_string(),
                language: SourceLanguage::Python,
                handler: None,
                auth_hint: None,
                path_params: path_params.clone(),
            });
        }
    }
    out
}

fn extract_flask_params(route: &str) -> Vec<String> {
    FLASK_PARAM_PATTERN
        .captures_iter(route)
        .filter_map(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
        .collect()
}

fn parse_methods_list(body: &str) -> Vec<String> {
    let mut methods = Vec::new();
    for piece in body.split(',') {
        let trimmed = piece.trim().trim_matches(|c| c == '"' || c == '\'').trim();
        if trimmed.is_empty() {
            continue;
        }
        methods.push(trimmed.to_ascii_uppercase());
    }
    if methods.is_empty() {
        methods.push("GET".to_string());
    }
    methods
}

// ---------------------------------------------------------------------------
// FastAPI
// ---------------------------------------------------------------------------

static FASTAPI_ROUTE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // `@router.get("/path"` — the router variable is captured so decorator
    // paths can be joined with the router's `APIRouter(prefix=...)`. The
    // path may be empty: `@router.get("")` on a prefixed router IS the
    // collection route itself.
    Regex::new(r#"@(\w+)\.(get|post|put|delete|patch)\s*\(\s*"([^"$]*?)""#)
        .expect("fastapi route regex compiles")
});

static FASTAPI_ROUTER_ASSIGN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^(\w+)\s*=\s*APIRouter\s*\("#).expect("fastapi router regex compiles")
});

static FASTAPI_PREFIX_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"prefix\s*=\s*"([^"]*)""#).expect("prefix regex compiles"));

/// Router-variable name -> mounted prefix, from `name = APIRouter(prefix=...)`.
/// Routers declared without a prefix (usually `app`/`api` app instances)
/// simply stay absent from the map and their decorator paths pass through.
fn fastapi_router_prefixes(source: &str) -> std::collections::HashMap<String, String> {
    let mut prefixes = std::collections::HashMap::new();
    for caps in FASTAPI_ROUTER_ASSIGN_PATTERN.captures_iter(source) {
        let (Some(name), Some(whole)) = (caps.get(1), caps.get(0)) else {
            continue;
        };
        // The prefix kwarg can sit on a later line inside the call body;
        // scan the balanced parenthesized region, not just one line.
        let bytes = source.as_bytes();
        let mut depth = 0usize;
        let mut end = whole.end();
        for (offset, byte) in bytes[whole.end() - 1..].iter().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = whole.end() - 1 + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &source[whole.end()..end];
        if let Some(prefix) = FASTAPI_PREFIX_PATTERN
            .captures(body)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
        {
            prefixes.insert(name.as_str().to_string(), prefix);
        }
    }
    prefixes
}

/// Join a router prefix and a decorator path into the mounted route:
/// `("/v2/widgets", "") -> "/v2/widgets"`, `("/v2/widgets", "/{id}") ->
/// "/v2/widgets/{id}"`, `(None, "health") -> "/health"`.
fn join_fastapi_route(prefix: Option<&str>, path: &str) -> Option<String> {
    let Some(prefix) = prefix else {
        if path.is_empty() {
            return None;
        }
        return Some(if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        });
    };
    if path.is_empty() {
        let trimmed = prefix.trim_end_matches('/');
        return if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if prefix.is_empty() {
        return Some(if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        });
    }
    let joined = format!(
        "{}/{}",
        prefix.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    Some(joined)
}

/// FastAPI/axum path parameter: `{name}`. Captures the name only.
static BRACE_PARAM_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\{([^{}]+)\}"#).expect("brace param regex compiles"));

/// Detect FastAPI `@app.get("/path")`, `.post(...)`, `.put(...)`, `.delete(...)`,
/// `.patch(...)` route decorators.
///
/// Phase 6: templated routes like `/users/{id}` are now kept and exposed via
/// `path_params`. Phase 5 skipped them to avoid false literal matches.
pub fn detect_fastapi_routes(source: &str, path: &str) -> Vec<RouteRecord> {
    let router_prefixes = fastapi_router_prefixes(source);
    let mut out = Vec::new();
    for caps in FASTAPI_ROUTE_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(router) = caps.get(1) else { continue };
        let Some(method) = caps.get(2) else { continue };
        let Some(route) = caps.get(3) else { continue };
        let Some(route_str) = join_fastapi_route(
            router_prefixes.get(router.as_str()).map(String::as_str),
            route.as_str(),
        ) else {
            continue;
        };
        let line = line_number_for_offset(source, whole.start());
        let path_params = extract_brace_params(&route_str);
        out.push(RouteRecord {
            path: path.to_string(),
            line,
            method: method.as_str().to_ascii_uppercase(),
            route: route_str,
            framework: "fastapi".to_string(),
            language: SourceLanguage::Python,
            handler: None,
            auth_hint: None,
            path_params,
        });
    }
    out
}

fn extract_brace_params(route: &str) -> Vec<String> {
    BRACE_PARAM_PATTERN
        .captures_iter(route)
        .filter_map(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// Express
// ---------------------------------------------------------------------------

static EXPRESS_ROUTE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // `app.get("/path", handler)` or `router.use('/path', ...)`.
    Regex::new(
        r#"\b(?:app|router)\.(get|post|put|delete|patch|use)\s*\(\s*(?:"([^"$]+?)"|'([^'$]+?)')"#,
    )
    .expect("express route regex compiles")
});

/// Express path parameter: `:name`. Captures the name; only matches on
/// `/:foo` boundaries to avoid swallowing protocol colons (`https://`).
static EXPRESS_PARAM_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|/):([A-Za-z_][A-Za-z0-9_]*)"#).expect("express param regex compiles")
});

/// Detect Express-style `app.<method>(...)` and `router.use(...)` routes.
pub fn detect_express_routes(
    source: &str,
    path: &str,
    language: SourceLanguage,
) -> Vec<RouteRecord> {
    let mut out = Vec::new();
    for caps in EXPRESS_ROUTE_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(verb) = caps.get(1) else { continue };
        let route = match caps.get(2).or_else(|| caps.get(3)) {
            Some(r) => r.as_str().to_string(),
            None => continue,
        };
        if route.is_empty() || route.contains('{') {
            continue;
        }
        let method = if verb.as_str() == "use" {
            "*".to_string()
        } else {
            verb.as_str().to_ascii_uppercase()
        };
        let line = line_number_for_offset(source, whole.start());
        let path_params = extract_express_params(&route);
        out.push(RouteRecord {
            path: path.to_string(),
            line,
            method,
            route,
            framework: "express".to_string(),
            language,
            handler: None,
            auth_hint: None,
            path_params,
        });
    }
    out
}

fn extract_express_params(route: &str) -> Vec<String> {
    EXPRESS_PARAM_PATTERN
        .captures_iter(route)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// axum
// ---------------------------------------------------------------------------

static AXUM_ROUTE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // .route("/path", get(handler))
    Regex::new(
        r#"\.route\s*\(\s*"([^"$]+?)"\s*,\s*(get|post|put|delete|patch)\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)"#,
    )
    .expect("axum route regex compiles")
});

static AXUM_MERGE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // .merge(module::path::router_fn())
    Regex::new(r#"\.merge\s*\(\s*([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*\("#)
        .expect("axum merge regex compiles")
});

static AXUM_CROSS_NEST_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // .nest("/prefix", module::path::router_fn(...)) — anything but an
    // inline `Router::new()` (those resolve in detect_axum_routes).
    Regex::new(
        r#"\.nest\s*\(\s*"([^"$]+?)"\s*,\s*([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*\("#,
    )
    .expect("axum cross nest regex compiles")
});

/// Resolve cross-file axum nests: `.nest("/prefix", module::router_fn())`
/// mounts routes that live in the file declaring `fn router_fn`. The callee
/// name is resolved against the indexed Rust symbols, every indexed route
/// from the declaring file gains the mounted prefix, and the http-edge
/// resolution then sees final paths.
///
/// Approximation (documented): when the declaring function only merges
/// routers built elsewhere, those other files do not receive the prefix —
/// following `merge` chains needs a real call graph.
pub fn mount_cross_file_axum_nests(
    root: &std::path::Path,
    files: &[crate::model::FileRecord],
    mut routes: Vec<RouteRecord>,
) -> Vec<RouteRecord> {
    use std::collections::{HashMap, HashSet};

    // fn name -> declaring files (generic names like `routes` collide across
    // modules, so resolution prefers the module path and only falls back to a
    // bare name when exactly one file declares it).
    let mut fn_files: HashMap<&str, Vec<&str>> = HashMap::new();
    for file in files {
        if file.language != SourceLanguage::Rust {
            continue;
        }
        for symbol in &file.symbols {
            fn_files
                .entry(symbol.name.as_str())
                .or_default()
                .push(file.path.as_str());
        }
    }

    let rust_files: Vec<&crate::model::FileRecord> = files
        .iter()
        .filter(|file| file.language == SourceLanguage::Rust)
        .collect();
    let declares_fn = |file: &str, fn_name: &str| {
        rust_files
            .iter()
            .any(|record| record.path == file && record.symbols.iter().any(|s| s.name == fn_name))
    };

    // `api::extract::routes` -> "api/extract" (module path without the fn).
    let module_suffixes = |callee: &str| -> Vec<String> {
        let segments: Vec<&str> = callee.split("::").collect();
        if segments.len() < 2 {
            return Vec::new();
        }
        let joined = segments[..segments.len() - 1].join("/");
        vec![format!("{joined}.rs"), format!("{joined}/mod.rs")]
    };

    let resolve_callee = |callee: &str| -> Option<String> {
        let fn_name = callee.rsplit("::").next().unwrap_or(callee);
        for suffix in module_suffixes(callee) {
            let normalized_suffix = format!("/{suffix}");
            for file in &rust_files {
                if file.path.ends_with(&normalized_suffix) && declares_fn(&file.path, fn_name) {
                    return Some(file.path.clone());
                }
            }
        }
        // Bare fn name: only unambiguous declarations resolve.
        match fn_files.get(fn_name) {
            Some(decls) if decls.len() == 1 => Some(decls[0].to_string()),
            _ => None,
        }
    };

    // Scan Rust sources for nest mounts and merge edges.
    let mut mounts: Vec<(String, String)> = Vec::new(); // (prefix, target file)
    let mut merge_edges: Vec<(String, String)> = Vec::new(); // (file, merged file)
    for file in &rust_files {
        let Ok(source) = std::fs::read_to_string(root.join(&file.path)) else {
            continue;
        };
        for caps in AXUM_CROSS_NEST_PATTERN.captures_iter(&source) {
            let (Some(prefix), Some(callee)) = (caps.get(1), caps.get(2)) else {
                continue;
            };
            if callee.as_str().split("::").next() == Some("Router") {
                continue;
            }
            if let Some(target) = resolve_callee(callee.as_str())
                && target != file.path
            {
                mounts.push((prefix.as_str().trim_end_matches('/').to_string(), target));
            }
        }
        for caps in AXUM_MERGE_PATTERN.captures_iter(&source) {
            let Some(callee) = caps.get(1) else { continue };
            if callee.as_str().split("::").next() == Some("Router") {
                continue;
            }
            if let Some(target) = resolve_callee(callee.as_str())
                && target != file.path
            {
                merge_edges.push((file.path.clone(), target));
            }
        }
    }
    if mounts.is_empty() {
        return routes;
    }

    let merges_from: HashMap<&str, Vec<&str>> = {
        let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
        for (from, to) in &merge_edges {
            map.entry(from.as_str()).or_default().push(to.as_str());
        }
        map
    };

    // First mount wins per file; a router mounted at two prefixes needs a
    // call graph to disambiguate.
    let mut mounted: HashMap<String, String> = HashMap::new();
    for (prefix, target) in mounts {
        // BFS through merge chains so routers merged inside the mounted
        // router receive the same prefix (transitively, cycle-safe).
        let mut queue = vec![target.clone()];
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(file) = queue.pop() {
            if !seen.insert(file.clone()) {
                continue;
            }
            mounted
                .entry(file.clone())
                .or_insert_with(|| prefix.clone());
            if let Some(next) = merges_from.get(file.as_str()) {
                for successor in next {
                    queue.push(successor.to_string());
                }
            }
        }
    }

    for record in &mut routes {
        if record.framework != "axum" {
            continue;
        }
        let Some(prefix) = mounted.get(&record.path) else {
            continue;
        };
        let joined = if record.route == "/" {
            prefix.clone()
        } else {
            format!("{prefix}/{}", record.route.trim_start_matches('/'))
        };
        if record.route != joined {
            record.route = joined;
            record.path_params = extract_brace_params(&record.route);
        }
    }
    routes
}
static AXUM_NEST_START_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // .nest("/prefix", Router::new()
    Regex::new(r#"\.nest\s*\(\s*"([^"$]+?)"\s*,\s*Router\s*::\s*new\s*\("#)
        .expect("axum nest start regex compiles")
});

static AXUM_AUTH_LAYER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // .route_layer(middleware::from_fn_with_state(.., jwt_auth_middleware))
    Regex::new(r#"\.route_layer\s*\(\s*middleware::from_fn(?:_with_state)?[^)]*\)"#)
        .expect("axum auth layer regex compiles")
});

/// One inline `.nest("/prefix", Router::new() ...)` span: routes captured
/// inside `[start, end)` carry the mounted prefix (recursively for nests
/// inside nests).
struct AxumNestSpan {
    start: usize,
    end: usize,
    prefix: String,
}

fn axum_nest_spans(source: &str) -> Vec<AxumNestSpan> {
    let bytes = source.as_bytes();
    let mut spans = Vec::new();
    for caps in AXUM_NEST_START_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(prefix) = caps.get(1) else { continue };
        // Walk the balanced parens of the whole `.nest(...)` call: start at
        // the match head so the call's own opening paren is counted, and end
        // where depth first returns to zero (the call's closing paren).
        let mut depth = 0usize;
        let mut end = source.len();
        for (offset, byte) in bytes[whole.start()..].iter().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = whole.start() + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        spans.push(AxumNestSpan {
            start: whole.start(),
            end,
            prefix: prefix.as_str().to_string(),
        });
    }
    spans
}

/// Mounted route for a route declared at `offset`: the innermost nest span
/// containing the offset contributes its prefix (and outer spans compose).
fn axum_mounted_route(spans: &[AxumNestSpan], offset: usize, literal: &str) -> String {
    let mut prefixes: Vec<&str> = spans
        .iter()
        .filter(|span| offset >= span.start && offset < span.end)
        .map(|span| span.prefix.as_str())
        .collect();
    prefixes.sort_by_key(|prefix| std::cmp::Reverse(prefix.len()));
    let mut route = String::new();
    for prefix in prefixes {
        route.push_str(prefix.trim_end_matches('/'));
    }
    if !literal.starts_with('/') {
        route.push('/');
    }
    route.push_str(literal);
    route
}

/// Auth hint from the builder chain: when a file scopes routes with
/// `.route_layer(middleware::from_fn*(... auth ...))`, routes declared
/// before that layer sit behind the auth middleware and routes after it
/// belong to the public router merged afterwards.
fn axum_auth_hint(source: &str, offset: usize) -> Option<&'static str> {
    let layer = AXUM_AUTH_LAYER_PATTERN.find_iter(source).find(|m| {
        let window = &m.as_str().to_ascii_lowercase();
        window.contains("jwt") || window.contains("auth")
    })?;
    if offset < layer.start() {
        Some("protected (route_layer)")
    } else {
        Some("public (post route_layer)")
    }
}

/// Detect axum `.route("/path", METHOD(handler))` builder calls.
///
/// Phase 6: templated routes like `/items/{slug}` are now kept and exposed
/// via `path_params`. Inline `.nest("/prefix", Router::new()...)` mounts
/// resolve to their full path, and a scoped auth `route_layer` marks the
/// routes it protects.
pub fn detect_axum_routes(source: &str, path: &str) -> Vec<RouteRecord> {
    let nest_spans = axum_nest_spans(source);
    let mut out = Vec::new();
    for caps in AXUM_ROUTE_PATTERN.captures_iter(source) {
        let Some(whole) = caps.get(0) else { continue };
        let Some(route) = caps.get(1) else { continue };
        let Some(method) = caps.get(2) else { continue };
        let route_str = route.as_str().to_string();
        if route_str.is_empty() {
            continue;
        }
        let line = line_number_for_offset(source, whole.start());
        let mounted = axum_mounted_route(&nest_spans, whole.start(), &route_str);
        let path_params = extract_brace_params(&mounted);
        let handler = caps.get(3).map(|m| m.as_str().to_string());
        let auth_hint = axum_auth_hint(source, whole.start()).map(str::to_string);
        out.push(RouteRecord {
            path: path.to_string(),
            line,
            method: method.as_str().to_ascii_uppercase(),
            route: mounted,
            framework: "axum".to_string(),
            language: SourceLanguage::Rust,
            handler,
            auth_hint,
            path_params,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Flask ----

    #[test]
    fn flask_basic_route_defaults_to_get() {
        let src = "@app.route(\"/users\")\ndef users(): pass\n";
        let hits = detect_flask_routes(src, "a.py");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].method, "GET");
        assert_eq!(hits[0].route, "/users");
        assert_eq!(hits[0].framework, "flask");
        assert_eq!(hits[0].line, 1);
        assert!(hits[0].path_params.is_empty());
    }

    #[test]
    fn flask_methods_post_only() {
        let src = "@app.route(\"/login\", methods=[\"POST\"])\ndef login(): pass\n";
        let hits = detect_flask_routes(src, "a.py");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].method, "POST");
        assert_eq!(hits[0].route, "/login");
    }

    #[test]
    fn flask_methods_multiple_expands_records() {
        let src = "@app.route(\"/x\", methods=[\"GET\", \"POST\"])\n";
        let hits = detect_flask_routes(src, "a.py");
        assert_eq!(hits.len(), 2);
        let methods: Vec<&str> = hits.iter().map(|h| h.method.as_str()).collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"POST"));
    }

    #[test]
    fn flask_blueprint_route_decorator_also_matches() {
        // Any `@<name>.route(...)` works — Flask Blueprints commonly use
        // `@bp.route(...)`.
        let src = "@bp.route(\"/items\")\n";
        let hits = detect_flask_routes(src, "a.py");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].route, "/items");
    }

    #[test]
    fn flask_path_params_extracted_with_type_prefix() {
        // `<int:id>` should yield path_param "id" (type prefix stripped).
        let src = "@app.route(\"/users/<int:id>\")\ndef get_user(id): pass\n";
        let hits = detect_flask_routes(src, "a.py");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].route, "/users/<int:id>");
        assert_eq!(hits[0].path_params, vec!["id".to_string()]);
    }

    #[test]
    fn flask_path_params_bare_name_also_extracted() {
        let src = "@app.route(\"/posts/<slug>\")\ndef get_post(slug): pass\n";
        let hits = detect_flask_routes(src, "a.py");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path_params, vec!["slug".to_string()]);
    }

    // ---- FastAPI ----

    #[test]
    fn fastapi_get_route() {
        let src = "@app.get(\"/items\")\ndef items(): pass\n";
        let hits = detect_fastapi_routes(src, "a.py");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].method, "GET");
        assert_eq!(hits[0].route, "/items");
        assert_eq!(hits[0].framework, "fastapi");
        assert!(hits[0].path_params.is_empty());
    }

    #[test]
    fn fastapi_post_route() {
        let src = "@router.post(\"/orders\")\ndef create_order(): pass\n";
        let hits = detect_fastapi_routes(src, "a.py");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].method, "POST");
        assert_eq!(hits[0].route, "/orders");
    }

    #[test]
    fn fastapi_path_param_route_now_recorded() {
        // Phase 5 skipped these; Phase 6 records them with extracted params.
        let src = "@app.get(\"/users/{uid}\")\ndef u(): pass\n";
        let hits = detect_fastapi_routes(src, "a.py");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].route, "/users/{uid}");
        assert_eq!(hits[0].path_params, vec!["uid".to_string()]);
    }

    // ---- Express ----

    #[test]
    fn express_post_route_double_quote() {
        let src = "app.post(\"/api/login\", handler)\n";
        let hits = detect_express_routes(src, "a.js", SourceLanguage::JavaScript);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].method, "POST");
        assert_eq!(hits[0].route, "/api/login");
        assert_eq!(hits[0].framework, "express");
    }

    #[test]
    fn express_router_use_is_catch_all() {
        let src = "router.use('/api', someMiddleware)\n";
        let hits = detect_express_routes(src, "a.ts", SourceLanguage::TypeScript);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].method, "*");
        assert_eq!(hits[0].route, "/api");
    }

    #[test]
    fn express_path_params_extracted() {
        let src = "app.get(\"/users/:id\", handler)\n";
        let hits = detect_express_routes(src, "a.js", SourceLanguage::JavaScript);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].route, "/users/:id");
        assert_eq!(hits[0].path_params, vec!["id".to_string()]);
    }

    // ---- axum ----

    #[test]
    fn axum_route_get_handler() {
        let src = "let app = Router::new().route(\"/health\", get(health_handler));\n";
        let hits = detect_axum_routes(src, "a.rs");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].method, "GET");
        assert_eq!(hits[0].route, "/health");
        assert_eq!(hits[0].framework, "axum");
    }

    #[test]
    fn axum_route_post_handler() {
        let src = ".route(\"/submit\", post(submit_handler))";
        let hits = detect_axum_routes(src, "a.rs");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].method, "POST");
        assert_eq!(hits[0].route, "/submit");
    }

    #[test]
    fn axum_path_params_extracted() {
        let src = ".route(\"/items/{slug}\", get(show_item))";
        let hits = detect_axum_routes(src, "a.rs");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].route, "/items/{slug}");
        assert_eq!(hits[0].path_params, vec!["slug".to_string()]);
    }

    #[test]
    fn axum_route_captures_handler() {
        let src = r#"
async fn chatwoot_webhook() -> impl IntoResponse {}

Router::new()
    .route("/webhooks/chatwoot", post(chatwoot_webhook))
    .route("/webhooks/chatwoot/:token", post(chatwoot_webhook_token))
"#;
        let hits = detect_axum_routes(src, "gateway/main.rs");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].handler.as_deref(), Some("chatwoot_webhook"));
        assert_eq!(hits[1].handler.as_deref(), Some("chatwoot_webhook_token"));
    }

    #[test]
    fn merge_chain_and_generic_fn_names_resolve_module_paths() {
        use crate::model::{FileRecord, SourceLanguage, SymbolOccurrence};

        let root = tempfile_dir();
        let src = root.join("src");
        std::fs::create_dir_all(src.join("api")).unwrap();

        // main.rs mounts TWO routers whose callees share the generic fn name
        // `routes` — only the module path disambiguates.
        std::fs::write(
            src.join("main.rs"),
            r#"
fn main() {
    let app = Router::new()
        .nest("/api/extract", api::extract::routes())
        .nest("/api/v1/config", api::config::routes());
}
"#,
        )
        .unwrap();
        std::fs::write(
            src.join("api/extract.rs"),
            r#"
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/", post(extract_handler))
}
"#,
        )
        .unwrap();
        std::fs::write(
            src.join("api/config.rs"),
            r#"
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/values", get(config_values))
}
"#,
        )
        .unwrap();

        let file_record = |path: &str, symbols: &[&str]| FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Rust,
            bytes: 0,
            modified_unix_ms: 0,
            symbols: symbols
                .iter()
                .map(|name| SymbolOccurrence {
                    name: name.to_string(),
                    kind: crate::model::SymbolKind::Function,
                    path: path.to_string(),
                    line: 0,
                    language: SourceLanguage::Rust,
                    qual_name: None,
                })
                .collect(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        };
        let files = vec![
            file_record("src/main.rs", &["main"]),
            file_record("src/api/extract.rs", &["routes", "extract_handler"]),
            file_record("src/api/config.rs", &["routes", "config_values"]),
        ];
        let routes = vec![
            RouteRecord {
                path: "src/api/extract.rs".to_string(),
                line: 3,
                method: "POST".to_string(),
                route: "/".to_string(),
                framework: "axum".to_string(),
                language: SourceLanguage::Rust,
                handler: Some("extract_handler".to_string()),
                auth_hint: None,
                path_params: Vec::new(),
            },
            RouteRecord {
                path: "src/api/config.rs".to_string(),
                line: 3,
                method: "GET".to_string(),
                route: "/values".to_string(),
                framework: "axum".to_string(),
                language: SourceLanguage::Rust,
                handler: Some("config_values".to_string()),
                auth_hint: None,
                path_params: Vec::new(),
            },
        ];

        let mounted = mount_cross_file_axum_nests(&root, &files, routes);
        let paths: Vec<&str> = mounted.iter().map(|r| r.route.as_str()).collect();
        // Root route mounts to the bare prefix (no trailing slash).
        assert!(paths.contains(&"/api/extract"), "paths={paths:?}");
        // The generic `routes` name did not cross-wire the config file.
        assert!(paths.contains(&"/api/v1/config/values"), "paths={paths:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cross_file_nest_mounts_target_file_routes() {
        use crate::model::{FileRecord, SourceLanguage, SymbolOccurrence};

        let root = tempfile_dir();
        let main_dir = root.join("src");
        std::fs::create_dir_all(&main_dir).unwrap();

        // main.rs nests the ops router under /ops_console.
        std::fs::write(
            main_dir.join("main.rs"),
            r#"
fn main() {
    let app = Router::new()
        .nest("/ops_console", ops_console::ops_routes(state))
        .nest("/api/cs", api::customer_service_routes());
}
"#,
        )
        .unwrap();
        // routes/mod.rs declares ops_routes AND the routes it returns.
        let routes_dir = main_dir.join("ops_console/routes");
        std::fs::create_dir_all(&routes_dir).unwrap();
        std::fs::write(
            routes_dir.join("mod.rs"),
            r#"
pub fn ops_routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/stats", get(stats_api))
        .route("/events", get(sse_handler))
}
"#,
        )
        .unwrap();

        let file_record = |path: &str, symbols: &[&str]| FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Rust,
            bytes: 0,
            modified_unix_ms: 0,
            symbols: symbols
                .iter()
                .map(|name| SymbolOccurrence {
                    name: name.to_string(),
                    kind: crate::model::SymbolKind::Function,
                    path: path.to_string(),
                    line: 0,
                    language: SourceLanguage::Rust,
                    qual_name: None,
                })
                .collect(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        };
        let files = vec![
            file_record("src/main.rs", &["main"]),
            file_record(
                "src/ops_console/routes/mod.rs",
                &["ops_routes", "stats_api"],
            ),
        ];
        let routes = vec![
            RouteRecord {
                path: "src/ops_console/routes/mod.rs".to_string(),
                line: 4,
                method: "GET".to_string(),
                route: "/api/stats".to_string(),
                framework: "axum".to_string(),
                language: SourceLanguage::Rust,
                handler: Some("stats_api".to_string()),
                auth_hint: None,
                path_params: Vec::new(),
            },
            RouteRecord {
                path: "src/ops_console/routes/mod.rs".to_string(),
                line: 5,
                method: "GET".to_string(),
                route: "/events".to_string(),
                framework: "axum".to_string(),
                language: SourceLanguage::Rust,
                handler: Some("sse_handler".to_string()),
                auth_hint: None,
                path_params: Vec::new(),
            },
        ];

        let mounted = mount_cross_file_axum_nests(&root, &files, routes);
        let paths: Vec<&str> = mounted.iter().map(|r| r.route.as_str()).collect();
        assert!(paths.contains(&"/ops_console/api/stats"), "paths={paths:?}");
        assert!(paths.contains(&"/ops_console/events"), "paths={paths:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("leio-cross-nest-{nanos}"))
    }

    #[test]
    fn axum_inline_nest_mounts_inner_routes() {
        let src = r#"
Router::new()
    .route("/api/stats", get(stats_api))
    .nest(
        "/api/ingest",
        Router::new()
            .route("/webhooks", get(ingest_webhooks_api))
            .route("/webhooks/:webhook_id", get(ingest_webhook_detail_api)),
    )
    .route("/api/whatsapp-registry", get(whatsapp_registry_api))
"#;
        let hits = detect_axum_routes(src, "ops_console/routes/mod.rs");
        let routes: Vec<&str> = hits.iter().map(|hit| hit.route.as_str()).collect();
        assert!(routes.contains(&"/api/stats"), "routes={routes:?}");
        assert!(
            routes.contains(&"/api/ingest/webhooks"),
            "routes={routes:?}"
        );
        assert!(
            routes.contains(&"/api/ingest/webhooks/:webhook_id"),
            "routes={routes:?}"
        );
        assert!(
            routes.contains(&"/api/whatsapp-registry"),
            "routes={routes:?}"
        );
        assert!(
            !routes.contains(&"/webhooks"),
            "bare inner route must not survive: {routes:?}"
        );
    }

    #[test]
    fn axum_auth_layer_splits_protected_from_public() {
        let src = r#"
let protected = Router::new()
    .route("/api/private/one", get(one))
    .route_layer(middleware::from_fn_with_state(
        Arc::new(JwtAuthState::new(cfg)),
        jwt_auth_middleware,
    ));
let app = Router::new()
    .route("/health", get(health))
    .merge(protected);
"#;
        let hits = detect_axum_routes(src, "main.rs");
        let by_route = |needle: &str| {
            hits.iter()
                .find(|hit| hit.route == needle)
                .unwrap_or_else(|| {
                    panic!(
                        "missing {needle} in {:?}",
                        hits.iter().map(|h| &h.route).collect::<Vec<_>>()
                    )
                })
        };
        assert_eq!(
            by_route("/api/private/one").auth_hint.as_deref(),
            Some("protected (route_layer)")
        );
        assert_eq!(
            by_route("/health").auth_hint.as_deref(),
            Some("public (post route_layer)")
        );
    }

    #[test]
    fn axum_without_auth_layer_has_no_hint() {
        let src = r#".route("/live", get(live))"#;
        let hits = detect_axum_routes(src, "a.rs");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].auth_hint, None);
    }

    #[test]
    fn fastapi_router_prefix_joins_decorator_paths() {
        let src = r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/phone-lines", tags=["phone-lines"])

@router.post("", status_code=201)
async def create_phone_line():
    ...

@router.get("/{phone_id}")
async def get_phone_line(phone_id: str):
    ...

@router.get("/{phone_id}/assignments")
async def list_assignments(phone_id: str):
    ...
"#;
        let hits = detect_fastapi_routes(src, "routers/phone_lines.py");
        let routes: Vec<&str> = hits.iter().map(|hit| hit.route.as_str()).collect();
        // The empty-path decorator IS the collection route: prefix alone.
        assert!(routes.contains(&"/v2/phone-lines"), "routes={routes:?}");
        assert!(
            routes.contains(&"/v2/phone-lines/{phone_id}"),
            "routes={routes:?}"
        );
        assert!(
            routes.contains(&"/v2/phone-lines/{phone_id}/assignments"),
            "routes={routes:?}"
        );
        assert!(
            hits.iter()
                .all(|hit| hit.path_params.contains(&"phone_id".to_string())
                    || hit.route == "/v2/phone-lines")
        );
    }

    #[test]
    fn fastapi_router_without_prefix_passes_through() {
        let src = r#"
from fastapi import APIRouter

router = APIRouter()

@router.get("/health")
async def health():
    ...
"#;
        let hits = detect_fastapi_routes(src, "a.py");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].route, "/health");
    }

    #[test]
    fn fastapi_app_decorators_are_untouched_by_router_prefixes() {
        let src = r#"
from fastapi import APIRouter

app_router = APIRouter(prefix="/v2/x")

@app.get("/live")
async def live():
    ...
"#;
        let hits = detect_fastapi_routes(src, "a.py");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].route, "/live");
    }

    #[test]
    fn fastapi_prefix_on_later_line_of_router_call() {
        let src = r#"
router = APIRouter(
    prefix="/v2/widgets",
    tags=["widgets"],
)
"#;
        let prefixes = fastapi_router_prefixes(src);
        assert_eq!(
            prefixes.get("router").map(String::as_str),
            Some("/v2/widgets")
        );
    }
}
