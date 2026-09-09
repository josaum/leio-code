//! Query-time reverse-import matching.
//!
//! Index time stores each edge once: exact raw in `importers_by_raw`, resolved
//! files in `importers_by_target_path`, and specifier/name on
//! `file_import_details`. Stuffing last-segment aliases (`json`, `Path`, `os`,
//! `auth`) into `importers_by_raw` made `importers-of` work for Python and
//! collide on example. Matching happens here against those existing fields.

use std::collections::BTreeSet;

use crate::code_graph::looks_like_python_module_spec;

const SOURCE_SUFFIXES: [&str; 32] = [
    ".py", ".rs", ".ts", ".tsx", ".js", ".cs", ".csx", ".razor", ".cshtml", ".go", ".c", ".h",
    ".cc", ".cpp", ".cxx", ".hpp", ".hh", ".hxx", ".sh", ".bash", ".java", ".kt", ".kts", ".html",
    ".htm", ".css", ".swift", ".ttl", ".owl", ".jsonld", ".rdf", ".n3",
];

/// Identity-preserving aliases for one `importers-of` needle.
///
/// Path ↔ dotted Python module, with/without a source suffix. Does not add
/// the last path segment: that is what collided on short names in a monorepo.
pub(crate) fn import_query_aliases(needle: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let normalized = normalize_importer_needle(needle);
    if normalized.is_empty() {
        return keys;
    }
    keys.insert(normalized.clone());
    let without_ext = strip_source_suffix(&normalized);
    let has_source_suffix = without_ext != normalized.as_str();
    if normalized.contains('/') || has_source_suffix {
        keys.insert(without_ext.to_string());
        keys.insert(without_ext.replace('/', "."));
        if !has_source_suffix {
            keys.insert(format!("{normalized}.py"));
        }
    } else if normalized.contains('.')
        && !normalized.contains("::")
        && looks_like_python_module_spec(&normalized)
    {
        let as_path = normalized.replace('.', "/");
        keys.insert(as_path.clone());
        keys.insert(format!("{as_path}.py"));
    }
    keys
}

/// True when the needle itself is a file path, not a module or symbol name.
pub(crate) fn needle_looks_like_path(needle: &str) -> bool {
    let normalized = normalize_importer_needle(needle);
    normalized.contains('/') || strip_source_suffix(&normalized) != normalized.as_str()
}

pub(crate) fn normalize_importer_needle(needle: &str) -> String {
    let trimmed = needle.trim().trim_matches(|ch| matches!(ch, '"' | '\''));
    let mut out = trimmed.replace('\\', "/");
    while let Some(stripped) = out.strip_prefix("./") {
        out = stripped.to_string();
    }
    out
}

/// How a stored import edge hits the query aliases, if at all.
pub(crate) fn import_detail_hit_kind(
    specifiers: &[String],
    imported_names: &[String],
    candidate_paths: &[String],
    needles: &BTreeSet<String>,
) -> Option<&'static str> {
    if specifiers
        .iter()
        .any(|spec| needles.contains(&normalize_importer_needle(spec)))
    {
        return Some("specifier");
    }
    if imported_names
        .iter()
        .any(|name| needles.contains(&normalize_importer_needle(name)))
    {
        return Some("imported-name");
    }
    if candidate_paths.iter().any(|path| {
        let normalized = normalize_importer_needle(path);
        needles.contains(&normalized) || needles.contains(strip_source_suffix(&normalized))
    }) {
        return Some("canonical-file");
    }
    None
}

/// Collapse several hit kinds into the `match_kind` field agents see.
pub(crate) fn collapse_import_match_kinds(kinds: &BTreeSet<&'static str>) -> &'static str {
    if kinds.len() == 1 {
        kinds.iter().copied().next().unwrap_or("import-lookup")
    } else {
        "import-lookup"
    }
}

fn strip_source_suffix(path: &str) -> &str {
    for suffix in SOURCE_SUFFIXES {
        if let Some(stripped) = path.strip_suffix(suffix) {
            return stripped;
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_query_aliases_preserve_module_identity() {
        let dotted = import_query_aliases("cartridges.c4gym.seed_cobranca");
        assert!(dotted.contains("cartridges.c4gym.seed_cobranca"));
        assert!(dotted.contains("cartridges/c4gym/seed_cobranca"));
        assert!(dotted.contains("cartridges/c4gym/seed_cobranca.py"));
        assert!(
            !dotted.contains("seed_cobranca"),
            "must not add last-segment aliases: {dotted:?}"
        );

        let path = import_query_aliases("cartridges/c4gym/seed_cobranca.py");
        assert!(path.contains("cartridges/c4gym/seed_cobranca.py"));
        assert!(path.contains("cartridges.c4gym.seed_cobranca"));
        assert!(
            !path.contains("seed_cobranca.py"),
            "must not add basename aliases: {path:?}"
        );
    }

    #[test]
    fn import_detail_hit_kind_matches_specifier_and_name_not_stem() {
        let specifiers = ["cartridges.c4gym.seed_cobranca".to_string()];
        let names = ["seed_c4_cobranca_agent".to_string()];
        let paths = ["cartridges/c4gym/seed_cobranca.py".to_string()];

        assert_eq!(
            import_detail_hit_kind(
                &specifiers,
                &names,
                &paths,
                &import_query_aliases("cartridges.c4gym.seed_cobranca")
            ),
            Some("specifier")
        );
        assert_eq!(
            import_detail_hit_kind(
                &specifiers,
                &names,
                &paths,
                &import_query_aliases("seed_c4_cobranca_agent")
            ),
            Some("imported-name")
        );
        assert_eq!(
            import_detail_hit_kind(
                &specifiers,
                &names,
                &paths,
                &import_query_aliases("cartridges/c4gym/seed_cobranca.py")
            ),
            Some("specifier")
        );
        assert_eq!(
            import_detail_hit_kind(
                &specifiers,
                &names,
                &paths,
                &import_query_aliases("seed_cobranca")
            ),
            None
        );
    }
}
