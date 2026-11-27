//! Look for a fixture shared by tests that reuses a clock-derived temporary path.
use super::{Node, text, walk};
use std::collections::{BTreeMap, BTreeSet};

fn compact(node: Node<'_>, source: &str) -> String {
    text(node, source).split_whitespace().collect()
}

// A clock or process ID is shared by concurrent tests. Only accept these known
// inputs, including simple time conversions. Unknown identifiers may supply a
// unique suffix, so an unfamiliar expression contributes no finding.
fn only_clock_inputs(
    expr: Node<'_>,
    source: &str,
    clocks: &BTreeSet<String>,
    captured: bool,
) -> bool {
    let mut ids = BTreeSet::new();
    let mut closure_params = BTreeSet::new();
    walk(expr, &mut |n| {
        if matches!(
            n.kind(),
            "identifier" | "type_identifier" | "field_identifier"
        ) {
            ids.insert(text(n, source));
            if n.parent().is_some_and(|p| p.kind() == "closure_parameters")
                || (n.prev_sibling().is_some_and(|p| p.kind() == "|")
                    && n.next_sibling().is_some_and(|p| p.kind() == "|"))
            {
                closure_params.insert(text(n, source));
            }
        }
    });
    let clock = captured
        || (ids.contains("SystemTime") && ids.contains("now"))
        || ids.iter().any(|name| clocks.contains(*name));
    clock
        && ids.iter().all(|name| {
            clocks.contains(*name)
                || closure_params.contains(name)
                || matches!(
                    *name,
                    "format"
                        | "std"
                        | "time"
                        | "SystemTime"
                        | "now"
                        | "UNIX_EPOCH"
                        | "duration_since"
                        | "as_secs"
                        | "as_millis"
                        | "as_micros"
                        | "as_nanos"
                        | "map"
                        | "expect"
                        | "unwrap"
                        | "unwrap_or_default"
                        | "unwrap_or"
                        | "process"
                        | "id"
                )
        })
}

fn clock_name(expr: Node<'_>, source: &str, clocks: &BTreeSet<String>) -> bool {
    if expr.kind() != "macro_invocation"
        || !expr
            .child_by_field_name("macro")
            .is_some_and(|n| text(n, source) == "format")
    {
        return false;
    }
    let Some(args) = expr.named_child(1) else {
        return false;
    };
    let Some(fmt) = args.named_child(0) else {
        return false;
    };
    if fmt.kind() != "string_literal" {
        return false;
    }
    let format = text(fmt, source).replace("{{", "").replace("}}", "");
    if format.contains('$') {
        return false;
    }
    let mut captured_clock = false;
    for field in format.split('{').skip(1) {
        let Some((spec, _)) = field.split_once('}') else {
            return false;
        };
        let name = spec.split(':').next().unwrap_or("");
        if !name.is_empty() && !name.bytes().all(|b| b.is_ascii_digit()) {
            if !clocks.contains(name) {
                return false;
            }
            captured_clock = true;
        }
    }
    // With only implicit captures, the macro has no expression tokens after
    // its format string. With explicit arguments also rule out unknown inputs.
    only_clock_inputs(args, source, clocks, captured_clock)
}

fn fixture_directory(mut expr: Node<'_>, source: &str, paths: &BTreeSet<String>) -> bool {
    loop {
        match expr.kind() {
            "identifier" => return paths.contains(text(expr, source)),
            "reference_expression" => {
                let Some(value) = expr.child_by_field_name("value") else {
                    return false;
                };
                expr = value;
            }
            "call_expression" => {
                let Some(method) = expr.child_by_field_name("function") else {
                    return false;
                };
                if !method
                    .child_by_field_name("field")
                    .is_some_and(|n| text(n, source) == "join")
                {
                    return false;
                }
                let Some(segment) = expr
                    .child_by_field_name("arguments")
                    .and_then(|args| args.named_child(0))
                else {
                    return false;
                };
                let value = text(segment, source).trim_matches('"');
                if segment.kind() != "string_literal"
                    || value.contains('\\')
                    || !std::path::Path::new(value)
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)))
                {
                    return false;
                }
                let Some(parent) = method.child_by_field_name("value") else {
                    return false;
                };
                expr = parent;
            }
            _ => return false,
        }
    }
}

fn shared_path(body: Node<'_>, source: &str) -> bool {
    let mut clocks = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut created = false;
    let mut cursor = body.walk();
    for stmt in body.named_children(&mut cursor) {
        if stmt.kind() == "let_declaration"
            && let (Some(name), Some(value)) = (
                stmt.child_by_field_name("pattern"),
                stmt.child_by_field_name("value"),
            )
            && name.kind() == "identifier"
        {
            let variable = text(name, source);
            let clock = only_clock_inputs(value, source, &clocks, false);
            clocks.remove(variable);
            paths.remove(variable);
            if clock {
                clocks.insert(variable.to_owned());
            }
            if value.kind() == "call_expression"
                && let Some(func) = value.child_by_field_name("function")
                && func.kind() == "field_expression"
                && func
                    .child_by_field_name("field")
                    .is_some_and(|n| text(n, source) == "join")
                && func.child_by_field_name("value").is_some_and(|n| {
                    matches!(
                        compact(n, source).as_str(),
                        "std::env::temp_dir()" | "env::temp_dir()" | "temp_dir()"
                    )
                })
                && let Some(name_expr) = value
                    .child_by_field_name("arguments")
                    .and_then(|a| a.named_child(0))
                && clock_name(name_expr, source, &clocks)
            {
                paths.insert(variable.to_owned());
            }
        }
        if paths.is_empty() {
            continue;
        }
        let mut exclusive = false;
        walk(stmt, &mut |n| {
            if n.kind() != "call_expression" {
                return;
            }
            let Some(arg) = n
                .child_by_field_name("arguments")
                .and_then(|a| a.named_child(0))
            else {
                return;
            };
            if fixture_directory(arg, source, &paths) {
                let Some(function) = n.child_by_field_name("function") else {
                    return;
                };
                match compact(function, source).as_str() {
                    "fs::create_dir_all" | "std::fs::create_dir_all" => created = true,
                    "fs::create_dir" | "std::fs::create_dir" => exclusive = true,
                    _ => {}
                }
            }
        });
        if exclusive {
            return false;
        }
    }
    created
}

#[derive(Default)]
pub(super) struct TestFixtures {
    helpers: BTreeMap<(usize, String), Option<bool>>,
    calls: BTreeMap<(usize, String), BTreeSet<usize>>,
}

impl TestFixtures {
    pub(super) fn observe(&mut self, function: Node<'_>, source: &str) {
        let (Some(name), Some(body)) = (
            function.child_by_field_name("name"),
            function.child_by_field_name("body"),
        ) else {
            return;
        };
        let Some(module) = function.parent() else {
            return;
        };
        if !matches!(module.kind(), "source_file" | "declaration_list")
            || module.kind() == "declaration_list"
                && !module.parent().is_some_and(|n| n.kind() == "mod_item")
        {
            return;
        }
        let scope = module.id();
        let body_source = text(body, source);
        self.helpers
            .entry((scope, text(name, source).to_owned()))
            .and_modify(|v| *v = None)
            .or_insert_with(|| {
                Some(
                    body_source.contains("temp_dir")
                        && body_source.contains("create_dir_all")
                        && shared_path(body, source),
                )
            });
        if !super::test_attribute(function, source, false) {
            return;
        }
        walk(body, &mut |n| {
            if n.kind() == "call_expression"
                && let Some(callee) = n.child_by_field_name("function")
                && callee.kind() == "identifier"
            {
                self.calls
                    .entry((scope, text(callee, source).to_owned()))
                    .or_default()
                    .insert(function.start_byte());
            }
        });
    }

    pub(super) fn strongest(&self) -> Option<(usize, String)> {
        let candidates = self.helpers.iter().filter_map(|(key, observed)| {
            let count = self.calls.get(key)?.len();
            (*observed == Some(true) && count > 1).then_some((count, &key.1))
        });
        let (count, helper) = candidates.max_by(|(a_count, a_name), (b_count, b_name)| {
            a_count.cmp(b_count).then_with(|| b_name.cmp(a_name))
        })?;
        let reason = format!(
            "test fixture `{helper}` is called by {count} tests and creates a clock-named temporary directory non-exclusively"
        );
        Some((count, reason))
    }
}
