//! Evidence for companions when the named file has little co-edit history.
use super::*;
use crate::model::SymbolKind;
use std::process::Command;

const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
];

fn package<'a>(root: &Path, file: &'a Path, cache: &mut HashMap<PathBuf, bool>) -> &'a Path {
    file.ancestors()
        .skip(1)
        .find(|dir| {
            *cache.entry(dir.to_path_buf()).or_insert_with(|| {
                MANIFESTS
                    .iter()
                    .any(|name| root.join(dir).join(name).is_file())
            })
        })
        .unwrap_or(Path::new(""))
}

pub(super) fn live_file(root: &Path, relative: &Path) -> Option<PathBuf> {
    if !relative
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    let path = root.join(relative).canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

fn type_name(symbol: &SymbolOccurrence) -> bool {
    matches!(
        symbol.kind,
        SymbolKind::Struct
            | SymbolKind::Class
            | SymbolKind::Enum
            | SymbolKind::Trait
            | SymbolKind::Interface
            | SymbolKind::TypeAlias
    )
}

// Keep one test and one source collaborator. Require whole module-name tokens in
// test definitions, or multiple distinct sibling types mentioned by the seed.
fn verification_candidates(
    index: &RepoIndex,
    root: &Path,
    seeds: &[String],
) -> Vec<(String, String)> {
    static IDENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[A-Za-z_][A-Za-z_0-9]*").unwrap());
    let mut manifests = HashMap::new();
    let mut best_test = None;
    let mut best_types = None;
    for seed in seeds.iter().take(4) {
        let path = Path::new(seed);
        let scope = package(root, path, &mut manifests);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let needle = tokenize_keep_stops(stem);
        let mut owners: HashMap<&str, Option<&str>> = HashMap::new();
        for f in &index.files {
            let other = Path::new(&f.path);
            if other.parent() == path.parent() && other.extension() == path.extension() {
                for sym in f.symbols.iter().filter(|s| type_name(s)) {
                    owners
                        .entry(&sym.name)
                        .and_modify(|p| {
                            if *p != Some(&f.path) {
                                *p = None;
                            }
                        })
                        .or_insert(Some(&f.path));
                }
            }
            if seeds.contains(&f.path)
                || needle.is_empty()
                || matches!(stem, "main" | "lib" | "index" | "mod" | "init")
                || !is_test_like_file_path(&f.path)
                || !other.starts_with(scope)
                || package(root, other, &mut manifests) != scope
            {
                continue;
            }
            let hits = f
                .symbols
                .iter()
                .filter(|s| {
                    matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
                        && tokenize_keep_stops(&s.name)
                            .windows(needle.len())
                            .any(|w| w == needle)
                })
                .count();
            if hits > 0 {
                retain_best(
                    &mut best_test,
                    hits,
                    &f.path,
                    format!("same-package test definitions mentioning `{stem}` ({hits})"),
                );
            }
        }
        let Some(source) = source_prefix(root, path) else {
            continue;
        };
        let names: HashSet<_> = IDENT.find_iter(&source).map(|m| m.as_str()).collect();
        let mut hits: HashMap<&str, Vec<&str>> = HashMap::new();
        for name in names {
            if let Some(Some(owner)) = owners.get(name)
                && !seeds.iter().any(|s| s == owner)
            {
                hits.entry(owner).or_default().push(name);
            }
        }
        for (owner, mut names) in hits {
            if names.len() >= 2 {
                names.sort_unstable();
                retain_best(
                    &mut best_types,
                    names.len(),
                    owner,
                    format!(
                        "named file `{seed}` mentions sibling types {}",
                        names[..names.len().min(5)].join(", ")
                    ),
                );
            }
        }
    }
    [best_test, best_types]
        .into_iter()
        .flatten()
        .map(|(_, path, reason)| (path, reason))
        .collect()
}

fn retain_best(
    best: &mut Option<(usize, String, String)>,
    hits: usize,
    path: &str,
    reason: String,
) {
    if best
        .as_ref()
        .is_none_or(|(n, p, _)| hits > *n || (hits == *n && path < p.as_str()))
    {
        *best = Some((hits, path.to_owned(), reason));
    }
}

fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let result = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    result.status.success().then_some(result.stdout)
}

// A recurring change description predicts its frequently edited artifacts even
// when a particular seed has never been paired with them in a focused commit.
fn recurring_candidate(
    root: &Path,
    description: &str,
    seeds: &[String],
) -> Option<(String, String)> {
    static SUBJECT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9_-]*(?:\([\w./-]+\))?!?:\s+").unwrap());
    let subject = description
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .trim_end();
    if !SUBJECT.is_match(subject) || task_tokens(subject).len() < 3 {
        return None;
    }
    let log = git(
        root,
        &[
            "log",
            "--no-merges",
            "-100",
            "--format=%H%x09%s",
            "HEAD",
            "--",
            ".",
        ],
    )?;
    let log = String::from_utf8_lossy(&log);
    let commits: Vec<_> = log
        .lines()
        .filter_map(|line| {
            let (sha, title) = line.split_once('\t')?;
            subject
                .eq_ignore_ascii_case(title.trim_end_matches(['.', '!', '?']))
                .then_some(sha)
        })
        .take(8)
        .collect();
    if commits.len() < 2 {
        return None;
    }
    let mut counts = HashMap::new();
    let mut eligible = 0;
    for sha in commits {
        let output = git(
            root,
            &[
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-only",
                "--no-renames",
                "--relative",
                "-z",
                "-r",
                sha,
                "--",
                ".",
            ],
        )?;
        let paths: Vec<_> = output
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .take(65)
            .collect();
        if paths.is_empty() || paths.len() > 64 {
            continue;
        }
        eligible += 1;
        for path in paths {
            if let Ok(path) = std::str::from_utf8(path)
                && !seeds.iter().any(|s| s == path)
            {
                *counts.entry(path.to_owned()).or_insert(0usize) += 1;
            }
        }
    }
    let (path, count) = counts
        .into_iter()
        .filter(|(p, n)| *n >= 2 && 2 * n >= eligible && live_file(root, Path::new(p)).is_some())
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))?;
    Some((
        path,
        format!("changed in {count}/{eligible} reachable commits with the same subject"),
    ))
}

/// If a whole named directory fits, return its tracked members, including
/// documentation and manifests omitted by the language index.
fn named_directory_members(
    root: &Path,
    task: &str,
    seeds: &[String],
    budget: usize,
) -> Vec<(String, String)> {
    let mut members = std::collections::BTreeMap::new();
    for dir in ExplicitReferences::from_task(task)
        .paths
        .into_keys()
        .collect::<BTreeSet<_>>()
    {
        let relative = Path::new(&dir);
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            || !seeds.iter().any(|s| {
                Path::new(s)
                    .parent()
                    .is_some_and(|p| p.starts_with(relative))
            })
        {
            continue;
        }
        let Some(bytes) = git(root, &["--literal-pathspecs", "ls-files", "-z", "--", &dir]) else {
            continue;
        };
        let paths = bytes.split(|b| *b == 0).filter(|b| !b.is_empty());
        for path in paths.filter_map(|b| std::str::from_utf8(b).ok()) {
            if !seeds.iter().any(|s| s == path) && live_file(root, Path::new(path)).is_some() {
                members.insert(
                    path.to_owned(),
                    format!("tracked member of named directory `{dir}`"),
                );
                if members.len() > budget {
                    return Vec::new();
                }
            }
        }
    }
    members.into_iter().collect()
}

fn source_prefix(root: &Path, file: &Path) -> Option<String> {
    let source = fs::File::open(live_file(root, file)?).ok()?;
    let mut bytes = Vec::new();
    source.take(96 * 1024).read_to_end(&mut bytes).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

// Distinguish consumers of the requested interface from unrelated files
// sharing its basename. For coverage requests, also measure how much of
// each sibling's callable interface the named entry point mentions.
fn interface_companions(
    index: &RepoIndex,
    root: &Path,
    seeds: &[String],
    terms: &[String],
) -> Vec<(String, String)> {
    static QUALIFIED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b([A-Za-z_][A-Za-z_0-9]*)\s*(?:::|\.)\s*").unwrap());
    let coverage = terms.iter().any(|t| t == "coverage");
    let mut candidates = Vec::new();
    for seed in seeds.iter().take(4) {
        let file = Path::new(seed);
        let mut siblings: Vec<_> = index
            .files
            .iter()
            .filter(|f| {
                let p = Path::new(&f.path);
                p.parent() == file.parent() && p.extension() == file.extension()
            })
            .collect();
        // Large flat directories require indexed references. Do not silently
        // give an arbitrary prefix of such a directory preferential treatment.
        if siblings.len() > 128 {
            continue;
        }
        let Some(owner) = siblings.iter().find(|f| f.path == *seed) else {
            continue;
        };
        let other_names: HashSet<_> = siblings
            .iter()
            .filter(|f| f.path != *seed)
            .flat_map(|f| f.symbols.iter().map(|s| s.name.as_str()))
            .collect();
        let mut names: Vec<_> = owner
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .filter(|n| {
                n.len() >= 4
                    && !other_names.contains(n)
                    && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
            .collect();
        names.sort_unstable();
        names.dedup();
        let focus: HashSet<_> = names
            .iter()
            .copied()
            .filter(|name| {
                task_tokens(name)
                    .iter()
                    .any(|w| w.len() >= 3 && terms.contains(w))
            })
            .collect();
        let matcher = if focus.is_empty() {
            None
        } else {
            aho_corasick::AhoCorasick::builder()
                .match_kind(aho_corasick::MatchKind::LeftmostLongest)
                .build(&names)
                .ok()
        };
        let mut calls: HashMap<String, HashSet<String>> = HashMap::new();
        if coverage && let Some(text) = source_prefix(root, file) {
            for m in QUALIFIED.captures_iter(&text) {
                let rest = &text[m.get(0).unwrap().end()..];
                let len = rest
                    .bytes()
                    .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                    .count();
                if len > 0 {
                    calls
                        .entry(m[1].to_owned())
                        .or_default()
                        .insert(rest[..len].to_owned());
                }
            }
        }
        if matcher.is_none() && calls.is_empty() {
            continue;
        }
        siblings.retain(|s| !seeds.contains(&s.path));
        for other in siblings {
            let path = Path::new(&other.path);
            if let Some(module) = path.file_stem().and_then(|s| s.to_str())
                && let Some(refs) = calls.get(module)
            {
                let defs: HashSet<_> = other
                    .symbols
                    .iter()
                    .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
                    .map(|s| s.name.as_str())
                    .collect();
                let hits = defs.iter().filter(|s| refs.contains(**s)).count();
                if hits >= 2 {
                    candidates.push((100 * hits / defs.len(), hits, other.path.clone(),
                        format!("named file `{seed}` mentions {hits}/{} indexed callables qualified by `{module}`", defs.len())));
                }
            }
            let Some(matcher) = &matcher else { continue };
            let Some(text) = source_prefix(root, path) else {
                continue;
            };
            let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
            let hits: BTreeSet<_> = matcher
                .find_iter(&text)
                .filter(|m| {
                    (m.start() == 0 || !word(text.as_bytes()[m.start() - 1]))
                        && (m.end() == text.len() || !word(text.as_bytes()[m.end()]))
                })
                .map(|m| names[m.pattern().as_usize()])
                .collect();
            let focused = hits.iter().filter(|n| focus.contains(**n)).count();
            if hits.len() >= 2 && focused > 0 {
                candidates.push((
                    focused,
                    hits.len(),
                    other.path.clone(),
                    format!(
                        "mentions definitions unique to sibling `{seed}`: {}",
                        hits.iter().take(5).copied().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|c| seen.insert(c.2.clone()))
        .take(2)
        .map(|(_, _, p, r)| (p, r))
        .collect()
}

/// Resolve a local inline link as a repository-relative Git path.
fn local_link_path(document: &Path, destination: &str) -> Option<PathBuf> {
    let target = destination.split(['#', '?']).next()?;
    if target.is_empty() || target.split('/').next()?.contains(':') {
        return None;
    }
    let mut path = document.parent()?.to_path_buf();
    for part in Path::new(target).components() {
        match part {
            std::path::Component::Normal(name) => path.push(name),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if path.pop() => {}
            _ => return None,
        }
    }
    Some(path)
}

fn paragraph_reference<'a>(
    paragraph: &str,
    document: &Path,
    needles: &BTreeSet<&'a str>,
    keys: &BTreeSet<&str>,
) -> Option<&'a str> {
    static DESTINATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\]\(\s*(?:<([^>\n]+)>|([^)\s<>]+))").expect("valid inline link")
    });
    let priority = |token: &&'a str| (keys.contains(*token), *token);
    let literal = |text: &str| {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/' | '.')))
            .map(|t| t.trim_end_matches('.').trim_start_matches("./"))
            .filter_map(|t| needles.get(t).copied())
            .max_by_key(priority)
    };
    if document.extension().and_then(|e| e.to_str()) != Some("md") {
        return literal(paragraph);
    }
    let mut best = None;
    let mut start = 0;
    for captures in DESTINATION.captures_iter(paragraph) {
        let link = captures.get(0).unwrap();
        let destination = captures
            .get(1)
            .or_else(|| captures.get(2))
            .unwrap()
            .as_str();
        let target = local_link_path(document, destination);
        let reference = target
            .as_ref()
            .and_then(|p| p.to_str())
            .and_then(|p| needles.get(p).copied())
            .filter(|p| !keys.contains(p));
        best = [best, literal(&paragraph[start..link.start()]), reference]
            .into_iter()
            .flatten()
            .max_by_key(priority);
        start = link.end();
    }
    best.into_iter()
        .chain(literal(&paragraph[start..]))
        .max_by_key(priority)
}

fn matching_documents(root: &Path, needles: &BTreeSet<&str>) -> Option<Vec<String>> {
    let mut args = vec!["grep", "--threads=1", "-l", "-z", "-I", "-F"];
    for needle in needles {
        args.extend(["-e", *needle]);
    }
    args.extend(["--", "*.md", "*.rst", "*.adoc"]);
    let output = git(root, &args).unwrap_or_default();
    let paths: Vec<_> = output
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .take(33)
        .filter_map(|b| std::str::from_utf8(b).ok().map(str::to_owned))
        .collect();
    (paths.len() <= 32).then_some(paths)
}

/// Documentation can be a consumer of a configuration interface even when it
/// has no language-index row. Require a literal backlink or a task-relevant
/// configuration key and corroborate ambiguous backlinks in their own paragraph.
fn documentation_companion(
    index: &RepoIndex,
    root: &Path,
    seeds: &[String],
    terms: &[String],
) -> Option<(String, String)> {
    let mut needles = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for seed in seeds.iter().take(4) {
        needles.insert(seed.as_str());
        if let Some(record) = index.files.iter().find(|f| f.path == *seed) {
            for env in &record.env_vars {
                if task_tokens(&env.name)
                    .iter()
                    .any(|t| t.len() >= 3 && terms.contains(t))
                {
                    keys.insert(env.name.as_str());
                }
            }
        }
    }
    let seed_terms: HashSet<_> = seeds.iter().flat_map(|s| task_tokens(s)).collect();
    let topic: HashSet<_> = terms
        .iter()
        .filter(|t| t.len() >= 3 && !seed_terms.contains(*t))
        .collect();
    if keys.is_empty() && topic.len() < 2 {
        return None;
    }
    needles.extend(keys.iter().take(16).copied());
    // Basenames find links written relative to the document. If the expanded
    // search exceeds the scan budget, retain the narrower path/key search.
    let mut search = needles.clone();
    search.extend(
        seeds
            .iter()
            .take(4)
            .filter_map(|seed| Path::new(seed).file_name()?.to_str()),
    );
    let paths = match matching_documents(root, &search) {
        Some(paths) => paths,
        None if search != needles => matching_documents(root, &needles)?,
        None => return None,
    };
    let mut best = None;
    for path in &paths {
        if seeds.contains(path) {
            continue;
        }
        let Some(text) = source_prefix(root, Path::new(path)) else {
            continue;
        };
        // A soft line break stays within a paragraph. Blank lines separate
        // evidence even with CRLF endings or Markdown indentation.
        static PARAGRAPH_BREAK: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\r?\n[ \t]*\r?\n").expect("valid paragraph break"));
        for paragraph in PARAGRAPH_BREAK.split(&text) {
            let Some(needle) = paragraph_reference(paragraph, Path::new(path), &needles, &keys)
            else {
                continue;
            };
            let shared = task_tokens(paragraph)
                .iter()
                .filter(|t| topic.contains(t))
                .count();
            let key = keys.contains(needle);
            if key || shared >= 2 {
                retain_best(
                    &mut best,
                    shared + usize::from(key) * 2,
                    path,
                    if key {
                        format!("documents configuration key `{needle}` used by a named file")
                    } else {
                        format!("references `{needle}` in a paragraph matching {shared} task terms")
                    },
                );
            }
        }
    }
    best.map(|(_, p, r)| (p, r))
}

pub(super) fn add_task_companions(
    files: &mut Vec<ScoredFile>,
    index: &RepoIndex,
    root: &Path,
    task: &str,
    seeds: &[String],
    limit: usize,
    history: bool,
) {
    let budget = limit.saturating_sub(seeds.len());
    if budget == 0 {
        return;
    }
    let Ok(root) = root.canonicalize() else {
        return;
    };
    let terms = task_tokens(task);
    let verify = terms
        .iter()
        .any(|t| matches!(t.as_str(), "ci" | "coverage" | "test" | "tests" | "testing"));
    let mut evidence = named_directory_members(&root, task, seeds, budget);
    let has_directory = !evidence.is_empty();
    if evidence.len() < budget {
        if verify {
            evidence.extend(verification_candidates(index, &root, seeds));
        }
        if history && let Some(candidate) = recurring_candidate(&root, task, seeds) {
            evidence.push(candidate);
        }
    }
    if !has_directory {
        evidence.extend(interface_companions(index, &root, seeds, &terms));
        if let Some(doc) = documentation_companion(index, &root, seeds, &terms) {
            evidence.push(doc);
        }
    }
    let mut seen = HashSet::new();
    evidence.retain(|(path, _)| seen.insert(path.clone()));
    let ceiling = files.iter().map(|f| f.score).max().unwrap_or(0);
    for (i, (path, reason)) in evidence.into_iter().take(budget).enumerate() {
        if live_file(&root, Path::new(&path)).is_none() {
            continue;
        }
        let existing = files.iter().find(|f| f.path == path).map(|f| f.score);
        upsert_scored_file(
            files,
            index,
            &path,
            ceiling + (budget - i) as i64 - existing.unwrap_or(0),
            &reason,
        );
        if existing.is_none()
            && let Some(new) = files.last_mut()
        {
            new.graph_proximity = 0;
        }
    }
}

/// Reserve the last available slot for a corroborated relationship. Exact
/// references and earlier results keep their place. A cold context query
/// can use indexed binary edges without constructing a graph synchronously.
pub(super) fn add_linked_companion(
    files: &mut Vec<ScoredFile>,
    index: &RepoIndex,
    root: &Path,
    seeds: &[String],
    terms: &[String],
    cache: Option<&CodeGraphQueryCache>,
    limit: usize,
) -> Option<String> {
    let protected = files
        .iter()
        .filter(|f| f.explicit_match != ExplicitMatch::None)
        .count();
    if protected >= limit {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let graph_dir = crate::code_graph::default_code_graph_output_dir(&root);
    let cache = cache.filter(|_| {
        crate::code_graph::default_code_graph_manifest_path(&graph_dir).is_file()
            && crate::code_graph::code_graph_refresh_reason(index, &graph_dir).is_none()
    });
    let verify = terms
        .iter()
        .any(|t| matches!(t.as_str(), "test" | "tests" | "testing" | "ci" | "coverage"));
    let mut links: std::collections::BTreeMap<String, BTreeSet<String>> = Default::default();
    let mut manifests = HashMap::new();
    for seed in seeds.iter().take(4) {
        let scope = package(&root, Path::new(seed), &mut manifests);
        let mut add = |path: &str, reason: String| {
            if !seeds.iter().any(|s| s == path)
                && (package(&root, Path::new(path), &mut manifests) != scope
                    || (verify && is_test_like_file_path(path)))
            {
                links.entry(path.to_string()).or_default().insert(reason);
            }
        };
        if verify {
            for edge in index
                .cross_language
                .resolved_spawns
                .iter()
                .filter(|e| e.callee_path == *seed && is_test_like_file_path(&e.caller_path))
            {
                add(
                    &edge.caller_path,
                    format!(
                        "indexed invocation of binary `{}` from test line {}",
                        edge.callee_name, edge.caller_line
                    ),
                );
            }
            for binary in index.cross_language.binaries.iter().filter(|b| {
                b.path == *seed
                    && matches!(
                        b.source,
                        crate::model::BinaryNodeSource::CargoImplicit
                            | crate::model::BinaryNodeSource::CargoBin
                    )
            }) {
                let marker = format!("env!(\"CARGO_BIN_EXE_{}\")", binary.name);
                for f in index.files.iter().filter(|f| {
                    is_test_like_file_path(&f.path)
                        && f.path.ends_with(".rs")
                        && Path::new(&f.path).starts_with(scope)
                }) {
                    // Cargo exposes this variable to integration tests of the defining package.
                    if package(&root, Path::new(&f.path), &mut HashMap::new()) == scope
                        && source_prefix(&root, Path::new(&f.path))
                            .is_some_and(|t| t.contains(&marker))
                    {
                        add(
                            &f.path,
                            format!(
                                "integration test references Cargo binary `{}` defined by `{seed}`",
                                binary.name
                            ),
                        );
                    }
                }
            }
        }
        let Some(cache) = cache else { continue };
        for imp in cache
            .importers_by_target_path
            .get(seed)
            .into_iter()
            .flatten()
        {
            if cache
                .file_import_details
                .get(&imp.file_iri)
                .into_iter()
                .flatten()
                .any(|detail| {
                    detail.resolution_kind == "resolved"
                        && detail.candidate_paths.as_slice() == std::slice::from_ref(seed)
                        && detail.raw == imp.raw_import
                })
            {
                add(&imp.path, format!("resolved import of named file `{seed}`"));
            }
        }
        for iri in cache.file_lookup_path.get(seed).into_iter().flatten() {
            for detail in cache.file_import_details.get(iri).into_iter().flatten() {
                if detail.resolution_kind == "resolved"
                    && let [path] = detail.candidate_paths.as_slice()
                {
                    add(path, format!("named file `{seed}` imports `{path}`"));
                }
            }
            for sym in cache.file_symbols.get(iri).into_iter().flatten() {
                for caller in cache.callers_by_symbol.get(&sym.iri).into_iter().flatten() {
                    add(
                        &caller.path,
                        format!(
                            "indexed caller `{}` of `{}` in `{seed}`",
                            caller.name, sym.name
                        ),
                    );
                }
            }
        }
    }
    let mut best = None;
    for (path, reasons) in links {
        let words = task_tokens(&path);
        let overlap = terms
            .iter()
            .filter(|t| t.len() >= 3 && words.contains(t))
            .count();
        if overlap == 0 && !(verify && is_test_like_file_path(&path)) && reasons.len() < 2 {
            continue;
        }
        if live_file(&root, Path::new(&path)).is_some() {
            retain_best(
                &mut best,
                overlap + reasons.len() + usize::from(verify && is_test_like_file_path(&path)),
                &path,
                reasons.into_iter().take(3).collect::<Vec<_>>().join("; "),
            );
        }
    }
    let (_, path, reason) = best?;
    let mut scores: Vec<_> = files
        .iter()
        .filter(|f| f.explicit_match == ExplicitMatch::None)
        .map(|f| f.score)
        .collect();
    scores.sort_unstable_by(|a, b| b.cmp(a));
    let floor = scores.get(limit - protected - 1).copied().unwrap_or(0) + 1;
    if let Some(f) = files.iter_mut().find(|f| f.path == path) {
        f.score = f.score.max(floor);
        f.reasons.insert(reason);
        return None;
    }
    upsert_scored_file(files, index, &path, floor, &reason);
    files.last_mut()?.graph_proximity = 0;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_named_directory_respects_budget_and_git_membership() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join("plugins")).unwrap();
        for p in ["driver.sh", "guide.md", "schema.rst", "scratch.txt"] {
            fs::write(root.join("plugins").join(p), "").unwrap();
        }
        git(&root, &["init", "-q"]).unwrap();
        git(
            &root,
            &[
                "add",
                "plugins/driver.sh",
                "plugins/guide.md",
                "plugins/schema.rst",
            ],
        )
        .unwrap();
        let seeds = vec!["plugins/driver.sh".to_owned()];
        let task = "retire the `./plugins` directory";
        assert!(named_directory_members(&root, task, &seeds, 1).is_empty());
        let paths: Vec<_> = named_directory_members(&root, task, &seeds, 2)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(paths, ["plugins/guide.md", "plugins/schema.rst"]);
    }

    #[test]
    fn recurring_subject_uses_only_reachable_commits_and_live_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let run = |args: &[&str]| git(&root, args).expect("fixture git command");
        run(&["init", "-q"]);
        let subject = "build(catalog): refresh derived inventory";
        for n in 0..3 {
            fs::write(root.join("inventory.csv"), n.to_string()).unwrap();
            fs::write(root.join(format!("input{n}")), "source").unwrap();
            run(&["add", "."]);
            run(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--no-gpg-sign",
                "-qm",
                subject,
            ]);
        }
        let seeds = vec!["input0".to_owned()];
        let result = recurring_candidate(&root, subject, &seeds).unwrap();
        assert_eq!(result.0, "inventory.csv");
        assert!(result.1.contains("3/3"));
        assert!(
            recurring_candidate(
                &root,
                "build(catalog): refresh external dependencies",
                &seeds
            )
            .is_none()
        );
        run(&["checkout", "--detach", "-q", "HEAD~2"]);
        assert!(recurring_candidate(&root, subject, &seeds).is_none());
        run(&["checkout", "--detach", "-q", "@{-1}"]);
        fs::remove_file(root.join("inventory.csv")).unwrap();
        assert!(recurring_candidate(&root, subject, &seeds).is_none());
    }
}
