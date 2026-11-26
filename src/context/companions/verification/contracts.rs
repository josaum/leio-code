//! Match an assertion in a test to a changed producer expectation from reachable history.
use super::*;

const MAX_CHANGED_FILES: usize = 12;
const MAX_PREIMAGES: usize = 24;

// A simple equality's expected side may be wrapped in a conversion/variant.
fn scalar(mut value: &str) -> Option<&str> {
    loop {
        value = value.trim();
        match ["json!", "Some", "Ok"].into_iter().find_map(|wrapper| {
            value
                .strip_prefix(wrapper)?
                .trim_start()
                .strip_prefix('(')?
                .strip_suffix(')')
        }) {
            Some(inner) => value = inner,
            None => break,
        }
    }
    let constant = value.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    (constant || is_literal(value)).then_some(value)
}

fn is_literal(value: &str) -> bool {
    matches!(value, "true" | "false")
        || value.starts_with('"') && serde_json::from_str::<String>(value).is_ok()
        || value.parse::<i64>().is_ok()
}

// Expand only explicit use paths, including groups and as aliases. Ignore globs.
fn imports(argument: Node<'_>, src: &str, out: &mut BTreeMap<String, BTreeSet<String>>) {
    let mut pending = vec![(String::new(), argument)];
    while let Some((prefix, node)) = pending.pop() {
        match node.kind() {
            "scoped_use_list" => {
                if let (Some(path), Some(list)) = (
                    node.child_by_field_name("path"),
                    node.child_by_field_name("list"),
                ) {
                    pending.push((format!("{prefix}{}::", text(path, src)), list));
                }
            }
            "use_list" => {
                let mut cursor = node.walk();
                pending.extend(
                    node.named_children(&mut cursor)
                        .map(|n| (prefix.clone(), n)),
                );
            }
            "identifier" | "scoped_identifier" | "use_as_clause" => {
                let imported = if node.kind() == "use_as_clause" {
                    let Some(path) = node.child_by_field_name("path") else {
                        continue;
                    };
                    path
                } else {
                    node
                };
                let path = text(imported, src);
                let alias = node
                    .child_by_field_name("alias")
                    .map(|n| text(n, src))
                    .unwrap_or_else(|| path.rsplit("::").next().unwrap_or(path));
                out.entry(alias.to_owned())
                    .or_default()
                    .insert(format!("{prefix}{path}"));
            }
            _ => {}
        }
    }
}

type Expectations = BTreeSet<(String, String)>;

#[derive(Default)]
struct Assertions {
    values: Expectations,
    selectors: BTreeSet<String>,
    constants: BTreeMap<String, Option<String>>,
    imports: BTreeMap<String, BTreeSet<String>>,
}

impl Assertions {
    fn resolved_values(&self) -> Expectations {
        self.values
            .iter()
            .filter_map(|(key, value)| {
                let literal = if is_literal(value) {
                    value
                } else {
                    self.constants.get(value)?.as_ref()?
                };
                Some((key.clone(), literal.clone()))
            })
            .collect()
    }

    fn parse(parser: &mut Parser, source: &str) -> Self {
        let mut result = Self::default();
        let Some(tree) = parser.parse(source, None) else {
            return result;
        };
        walk(tree.root_node(), &mut |node| match node.kind() {
            "const_item" => {
                if let (Some(name), Some(value)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("value"),
                ) {
                    let value = text(value, source);
                    let literal = is_literal(value).then_some(value);
                    result
                        .constants
                        .entry(text(name, source).to_owned())
                        .and_modify(|old| {
                            if old.as_deref() != literal {
                                *old = None;
                            }
                        })
                        .or_insert_with(|| literal.map(str::to_owned));
                }
            }
            "use_declaration" => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    imports(arg, source, &mut result.imports);
                }
            }
            "macro_invocation" => {
                if node
                    .child_by_field_name("macro")
                    .is_some_and(|m| text(m, source).rsplit("::").next() == Some("assert_eq"))
                    && let Some(args) = node.named_child(1)
                    && let Some((key, expected)) = assertion(args, source)
                {
                    result.selectors.insert(key.to_owned());
                    if let Some(value) = scalar(expected) {
                        result.values.insert((key.to_owned(), value.to_owned()));
                    }
                }
            }
            _ => {}
        });
        result
    }
}

// Require an observed field lookup and an expectation in the same assertion.
// A string elsewhere in a macro or in its optional failure message is insufficient.
fn assertion<'s>(args: Node<'_>, src: &'s str) -> Option<(&'s str, &'s str)> {
    if args.kind() != "token_tree" || args.has_error() {
        return None;
    }
    let mut cursor = args.walk();
    let mut commas = args.children(&mut cursor).filter(|n| n.kind() == ",");
    let split = commas.next()?;
    let end = commas
        .next()
        .map_or(args.end_byte().checked_sub(1)?, |n| n.start_byte());
    let mut keys = BTreeSet::new();
    walk(args, &mut |n| {
        if n.kind() == "string_literal"
            && n.end_byte() <= split.start_byte()
            && let Some(group) = n
                .parent()
                .filter(|g| g.kind() == "token_tree" && g.named_child_count() == 1)
            && let Some(previous) = group.prev_sibling()
            && ((group.child(0).is_some_and(|op| op.kind() == "[")
                && matches!(previous.kind(), "identifier" | "self" | "token_tree"))
                || (text(previous, src) == "get"
                    && previous.prev_sibling().is_some_and(|n| n.kind() == ".")))
        {
            keys.insert(text(n, src));
        }
    });
    (keys.len() == 1).then(|| (*keys.first().unwrap(), &src[split.end_byte()..end]))
}

struct ChangedContract {
    path: String,
    // Library import name and the module's qualified suffix relative to the crate.
    module: Option<(String, String)>,
    retired: Expectations,
    // Only constants that still have their previous value can be stale imports.
    constants: BTreeMap<String, String>,
}

fn library_module(root: &Path, dir: &Path, producer: &Path) -> Option<(String, String)> {
    let manifest = read_manifest(root, dir)?;
    let lib = manifest.get("lib");
    let name = lib
        .and_then(|v| v.get("name"))
        .or_else(|| manifest.get("package")?.get("name"))?
        .as_str()?
        .replace('-', "_");
    let entry = dir.join(
        lib.and_then(|v| v.get("path"))
            .and_then(toml::Value::as_str)
            .unwrap_or("src/lib.rs"),
    );
    if producer == entry {
        return Some((name, String::new()));
    }
    let rel = producer
        .strip_prefix(entry.parent()?)
        .ok()?
        .with_extension("");
    let rel = if rel.file_name()?.to_str()? == "mod" {
        rel.parent()?
    } else {
        &rel
    };
    let suffix = rel
        .iter()
        .filter_map(|s| s.to_str())
        .map(|s| format!("::{s}"))
        .collect();
    Some((name, suffix))
}

fn changed_contract(
    old: Assertions,
    now: Assertions,
    path: &str,
    module: Option<(String, String)>,
) -> Option<ChangedContract> {
    let current = now.resolved_values();
    let retired: Expectations = old
        .resolved_values()
        .into_iter()
        .filter(|pair| now.selectors.contains(&pair.0) && !current.contains(pair))
        .collect();
    if retired.is_empty() {
        return None;
    }
    let constants = old
        .constants
        .into_iter()
        .filter(|(name, v)| now.constants.get(name) == Some(v))
        .filter_map(|(name, v)| Some((name, v?)))
        .collect();
    Some(ChangedContract {
        path: path.to_owned(),
        module,
        retired,
        constants,
    })
}

impl ChangedContract {
    fn matches<'a>(
        &self,
        assertions: &'a Assertions,
        crate_name: Option<&str>,
    ) -> BTreeSet<&'a str> {
        assertions
            .values
            .iter()
            .filter_map(|(k, v)| {
                let expected = if let Some(local) = assertions.constants.get(v) {
                    local.as_ref()?
                } else if is_literal(v) {
                    v
                } else {
                    let (lib, module) = self.module.as_ref()?;
                    let prefix = format!("{}{module}::", crate_name.unwrap_or(lib));
                    let paths = assertions.imports.get(v)?;
                    if paths.len() != 1 {
                        return None;
                    }
                    self.constants.get(paths.first()?.strip_prefix(&prefix)?)?
                };
                self.retired
                    .contains(&(k.clone(), expected.clone()))
                    .then_some(k.as_str())
            })
            .collect()
    }
}

// Walk reachable first-parent history, taking each file's latest change. Skip
// broad commits as a whole and bound blob reads independently of repo size.
fn recent_changes(
    root: &Path,
    scope: &CargoScope,
    parser: &mut Parser,
) -> BTreeMap<PathBuf, Vec<ChangedContract>> {
    let mut result: BTreeMap<PathBuf, Vec<ChangedContract>> = BTreeMap::new();
    let Some(raw) = git(
        root,
        &[
            "log",
            "--first-parent",
            "--diff-merges=first-parent",
            "--max-count=8",
            "--format=%H",
            "--raw",
            "--no-abbrev",
            "-z",
            "--relative",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--diff-filter=M",
            "HEAD",
            "--",
            "*.rs",
        ],
    ) else {
        return result;
    };
    let mut manifests = HashMap::new();
    let mut seen = BTreeSet::new();
    let mut batches = Vec::new();
    let mut records = raw.split(|&b| b == 0);
    while let Some(record) = records.next() {
        let Ok(header) = std::str::from_utf8(record) else {
            continue;
        };
        let header = header.trim_start();
        if !header.starts_with(':') {
            if !header.is_empty() {
                batches.push(Vec::new());
            }
            continue;
        }
        let Some(name) = records.next() else { break };
        let Ok(path) = std::str::from_utf8(name) else {
            continue;
        };
        let pkg = package(root, Path::new(path), &mut manifests);
        if scope.packages.contains(pkg)
            && seen.insert(path)
            && let Some(id) = header.split_whitespace().nth(2)
            && let Some(batch) = batches.last_mut()
        {
            batch.push((pkg, path, id));
        }
    }
    let candidates = batches
        .into_iter()
        .filter(|batch| batch.len() <= MAX_CHANGED_FILES)
        .flatten();
    let mut preimages = 0;
    for (pkg, path, id) in candidates {
        let Some(current) =
            bounded_source(root, Path::new(path)).filter(|s| s.contains("assert_eq"))
        else {
            continue;
        };
        if preimages == MAX_PREIMAGES {
            break;
        }
        preimages += 1;
        let Some(size) = git(root, &["cat-file", "-s", id])
            .and_then(|b| std::str::from_utf8(&b).ok()?.trim().parse::<usize>().ok())
        else {
            continue;
        };
        if size > MAX_SOURCE_BYTES {
            continue;
        }
        let Some(old) = git(root, &["cat-file", "blob", id])
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|s| s.contains("assert_eq"))
        else {
            continue;
        };
        let prior = Assertions::parse(parser, &old);
        if prior.values.is_empty() {
            continue;
        }
        let now = Assertions::parse(parser, &current);
        if let Some(change) =
            changed_contract(prior, now, path, library_module(root, pkg, Path::new(path)))
        {
            result.entry(pkg.to_owned()).or_default().push(change);
        }
    }
    result
}

pub(in super::super) fn stale_expectations(
    index: &RepoIndex,
    root: &Path,
    seeds: &[String],
    scope: &CargoScope,
) -> Vec<(String, String)> {
    let mut results = vec![];
    if scope.packages.is_empty() {
        return results;
    }
    let Some(mut parser) = rust_parser() else {
        return results;
    };
    let changes = recent_changes(root, scope, &mut parser);
    if changes.is_empty() {
        return results;
    }
    let mut bytes = 0;
    let mut manifests = HashMap::new();
    for f in index.files.iter().filter(|f| {
        f.language == SourceLanguage::Rust
            && f.bytes <= MAX_SOURCE_BYTES
            && is_test_like_file_path(&f.path)
            && !seeds.contains(&f.path)
    }) {
        let pkg = package(root, Path::new(&f.path), &mut manifests);
        if !scope.packages.contains(pkg) {
            continue;
        }
        let applicable: Vec<_> = changes
            .iter()
            .filter_map(|(owner, batch)| {
                let alias = if owner == pkg {
                    None
                } else {
                    Some(
                        scope
                            .dependencies
                            .get(pkg)?
                            .iter()
                            .find(|(_, p)| *p == owner)?
                            .0
                            .as_str(),
                    )
                };
                Some(batch.iter().map(move |change| (alias, change)))
            })
            .flatten()
            .collect();
        if applicable.is_empty() {
            continue;
        }
        let Some(source) = bounded_source(root, Path::new(&f.path)) else {
            continue;
        };
        bytes += source.len();
        if bytes > MAX_SCAN_BYTES {
            return vec![];
        }
        if !applicable
            .iter()
            .any(|(_, c)| c.retired.iter().any(|(key, _)| source.contains(key)))
        {
            continue;
        }
        let consumer = Assertions::parse(&mut parser, &source);
        let evidence: BTreeSet<_> = applicable
            .iter()
            .filter(|(_, c)| c.path != f.path)
            .flat_map(|(alias, c)| {
                c.matches(&consumer, *alias)
                    .into_iter()
                    .map(|key| (&c.path, key))
            })
            .collect();
        if let Some((path, key)) = evidence.first() {
            results.push((
                f.path.clone(),
                format!("test still asserts a previous value of {key} changed in `{path}`"),
            ));
        }
    }
    results.sort();
    results
}
