//! Coverage leads for a wider bundle. These are direct-call observations,
//! not runtime coverage measurements.
use super::*;

#[derive(Default)]
struct Callers<'a> {
    production: BTreeSet<&'a str>,
    tested: bool,
}

#[derive(Default)]
struct Inventory<'a> {
    // Ambiguous free function names cannot establish a call to a specific file.
    owners: HashMap<String, Option<&'a str>>,
    public: BTreeMap<&'a str, BTreeSet<String>>,
    callers: HashMap<String, Callers<'a>>,
}

fn test_attribute(item: Node<'_>, source: &str) -> bool {
    let mut prev = item.prev_named_sibling();
    while let Some(attr) = prev {
        match attr.kind() {
            "attribute_item" => {
                let compact: String = text(attr, source).split_whitespace().collect();
                if matches!(compact.as_str(), "#[cfg(test)]" | "#[test]")
                    || compact.ends_with("::test]")
                {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = attr.prev_named_sibling();
    }
    false
}

fn call_name<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    let target = match node.kind() {
        "call_expression" => {
            let f = node.child_by_field_name("function")?;
            match f.kind() {
                "generic_function" => f.child_by_field_name("function")?,
                _ => f,
            }
        }
        // Rust macro arguments are token trees, even for nested function calls
        // in assertions. A member access or a macro name isn't a free function.
        "token_tree" if node.child(0)?.kind() == "(" => {
            let f = node.prev_sibling()?;
            if f.kind() != "identifier" || f.prev_sibling().is_some_and(|s| s.kind() == ".") {
                return None;
            }
            f
        }
        _ => return None,
    };
    match target.kind() {
        "identifier" => Some(text(target, source)),
        "scoped_identifier" => Some(text(target.child_by_field_name("name")?, source)),
        _ => None,
    }
}

impl<'a> Inventory<'a> {
    fn observe(&mut self, path: &'a str, parser: &mut Parser, source: &str) -> Option<()> {
        let tree = parser.parse(source, None)?;
        let mut test_end = if is_test_like_file_path(path) {
            usize::MAX
        } else {
            0
        };
        walk(tree.root_node(), &mut |node| {
            if matches!(node.kind(), "function_item" | "mod_item")
                && test_end <= node.start_byte()
                && test_attribute(node, source)
            {
                test_end = node.end_byte();
            }
            let in_test = node.start_byte() < test_end;
            if let Some(name) = call_name(node, source) {
                let usage = self.callers.entry(name.to_owned()).or_default();
                if in_test {
                    usage.tested = true;
                } else {
                    usage.production.insert(path);
                }
            }
            if in_test || node.kind() != "function_item" {
                return;
            }
            let module_level = node.parent().is_some_and(|parent| {
                parent.kind() == "source_file"
                    || parent.kind() == "declaration_list"
                        && parent.parent().is_some_and(|n| n.kind() == "mod_item")
            });
            if module_level && let Some(name) = node.child_by_field_name("name") {
                let name = text(name, source).trim();
                if name.is_empty() {
                    return;
                }
                self.owners
                    .entry(name.to_owned())
                    .and_modify(|p| *p = None)
                    .or_insert(Some(path));
                if node
                    .named_child(0)
                    .is_some_and(|n| n.kind() == "visibility_modifier")
                {
                    self.public.entry(path).or_default().insert(name.to_owned());
                }
            }
        });
        Some(())
    }
}

pub(in super::super) fn coverage_gaps(
    index: &RepoIndex,
    root: &Path,
    scope: &CargoScope,
) -> Vec<(String, String)> {
    if scope.packages.is_empty() {
        return vec![];
    }
    let mut manifests = HashMap::new();
    let files: Vec<_> = index
        .files
        .iter()
        .filter(|f| {
            f.language == SourceLanguage::Rust
                && scope
                    .packages
                    .contains(package(root, Path::new(&f.path), &mut manifests))
        })
        .collect();
    // A partial scan cannot establish the absence of direct test calls.
    if files.iter().any(|f| f.bytes > MAX_SOURCE_BYTES)
        || files.iter().map(|f| f.bytes).sum::<usize>() > MAX_SCAN_BYTES
    {
        return vec![];
    }
    let Some(mut parser) = rust_parser() else {
        return vec![];
    };
    let mut bytes = 0;
    let mut inventory = Inventory::default();
    for f in files {
        let Some(source) = bounded_source(root, Path::new(&f.path)) else {
            return vec![];
        };
        bytes += source.len();
        if bytes > MAX_SCAN_BYTES || inventory.observe(&f.path, &mut parser, &source).is_none() {
            return vec![];
        }
    }
    let mut results = vec![];
    for (path, names) in inventory.public {
        let mut callers = BTreeSet::new();
        let mut missing = vec![];
        for name in &names {
            if inventory.owners.get(name) != Some(&Some(path)) {
                continue;
            }
            if let Some(usage) = inventory.callers.get(name)
                && !usage.tested
                && usage.production.iter().any(|&p| p != path)
            {
                callers.extend(usage.production.iter().copied().filter(|&p| p != path));
                missing.push(name.as_str());
            }
        }
        if !missing.is_empty() {
            let evidence = format!(
                "public calls with no direct test call observed ({} external caller files): {}",
                callers.len(),
                missing
                    .iter()
                    .take(4)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            results.push((callers.len(), missing.len(), path.to_owned(), evidence));
        }
    }
    results.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    results
        .into_iter()
        .map(|(_, _, path, reason)| (path, reason))
        .collect()
}
