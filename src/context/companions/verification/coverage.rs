//! Coverage leads for a wider bundle. These are direct-call observations,
//! not runtime coverage measurements.
use super::*;

mod fixtures;
mod formatting;
use fixtures::TestFixtures;

#[derive(Default)]
struct Callers<'a> {
    production: BTreeSet<&'a str>,
    functions: BTreeSet<(&'a str, usize)>,
    tests: BTreeSet<&'a str>,
}

#[derive(Default)]
struct Inventory<'a> {
    // Ambiguous free function names cannot establish a call to a specific file.
    owners: HashMap<String, Option<&'a str>>,
    public: BTreeMap<&'a str, BTreeSet<String>>,
    callers: HashMap<String, Callers<'a>>,
    fixtures: Vec<(usize, String, String)>,
}

fn test_attribute(item: Node<'_>, source: &str, include_cfg: bool) -> bool {
    let mut prev = item.prev_named_sibling();
    while let Some(attr) = prev {
        match attr.kind() {
            "attribute_item" => {
                let compact: String = text(attr, source).split_whitespace().collect();
                let attribute = compact
                    .trim_start_matches("#[")
                    .split(['(', ']'])
                    .next()
                    .unwrap_or("");
                if attribute.rsplit("::").next() == Some("test")
                    || (include_cfg && compact == "#[cfg(test)]")
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
        let mut fixtures = TestFixtures::default();
        walk(tree.root_node(), &mut |node| {
            if matches!(node.kind(), "function_item" | "mod_item")
                && test_end <= node.start_byte()
                && test_attribute(node, source, true)
            {
                test_end = node.end_byte();
            }
            let in_test = node.start_byte() < test_end;
            if let Some(name) = call_name(node, source) {
                let usage = self.callers.entry(name.to_owned()).or_default();
                if in_test {
                    usage.tests.insert(path);
                } else {
                    usage.production.insert(path);
                    if let Some(owner) = std::iter::successors(node.parent(), |n| n.parent())
                        .find(|n| n.kind() == "function_item")
                    {
                        usage.functions.insert((path, owner.start_byte()));
                    }
                }
            }
            if node.kind() != "function_item" {
                return;
            }
            if in_test {
                fixtures.observe(node, source);
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
        if let Some((count, reason)) = fixtures.strongest() {
            self.fixtures.push((count, path.to_owned(), reason));
        }
        Some(())
    }
}

pub(in super::super) fn coverage_gaps(
    index: &RepoIndex,
    root: &Path,
    scope: &CargoScope,
    expanded: bool,
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
    let mut format_inputs: BTreeMap<&Path, Vec<&str>> = BTreeMap::new();
    let mut bytes = 0;
    let mut inventory = Inventory::default();
    for f in files {
        if expanded {
            let package = package(root, Path::new(&f.path), &mut manifests);
            format_inputs.entry(package).or_default().push(&f.path);
        }
        let Some(source) = bounded_source(root, Path::new(&f.path)) else {
            return vec![];
        };
        bytes += source.len();
        if bytes > MAX_SCAN_BYTES || inventory.observe(&f.path, &mut parser, &source).is_none() {
            return vec![];
        }
    }
    let format = if expanded {
        formatting::drift(root, &format_inputs).unwrap_or_default()
    } else {
        Vec::new()
    };
    inventory.leads(format)
}

impl Inventory<'_> {
    fn leads(mut self, formatting: Vec<(String, String)>) -> Vec<(String, String)> {
        let mut results = vec![];
        let mut partial = vec![];
        for (path, names) in self.public {
            let mut callers = BTreeSet::new();
            let mut missing = vec![];
            let mut tested_peers = BTreeSet::new();
            let mut missing_uses = BTreeSet::new();
            for name in &names {
                if self.owners.get(name) != Some(&Some(path)) {
                    continue;
                }
                let Some(usage) = self.callers.get(name) else {
                    continue;
                };
                if usage.tests.contains(path) {
                    tested_peers
                        .extend(usage.functions.iter().copied().filter(|&(p, _)| p != path));
                }
                if usage.tests.is_empty() && usage.production.iter().any(|&p| p != path) {
                    callers.extend(usage.production.iter().copied().filter(|&p| p != path));
                    missing_uses
                        .extend(usage.functions.iter().copied().filter(|&(p, _)| p != path));
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
                let common = missing_uses.intersection(&tested_peers).count();
                if missing.len() == 1 && common > 0 {
                    let reason = format!(
                        "{evidence}; production callers shared with a locally tested public peer: {common}"
                    );
                    partial.push((common, path.to_owned(), reason));
                }
                results.push((callers.len(), missing.len(), path.to_owned(), evidence));
            }
        }
        results.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        let order = |a: &(usize, String, String), b: &(usize, String, String)| {
            b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1))
        };
        partial.sort_by(order);
        self.fixtures.sort_by(order);
        let mut fanout = results.into_iter().map(|(_, _, p, r)| (p, r));
        let mut peers = partial.into_iter().map(|(_, p, r)| (p, r));
        let mut fixtures = self.fixtures.into_iter().map(|(_, p, r)| (p, r));
        let mut format = formatting.into_iter();
        // Each evidence family gets a turn before any one family takes a second place.
        let mut leads = Vec::new();
        loop {
            let round = [fanout.next(), fixtures.next(), peers.next(), format.next()];
            if round.iter().all(Option::is_none) {
                break;
            }
            leads.extend(round.into_iter().flatten());
        }
        leads
    }
}

#[cfg(test)]
mod tests;
