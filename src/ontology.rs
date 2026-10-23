//! Ontology files as first-class index and formal-graph sources.
//!
//! Parses `.ttl`, `.owl`, `.jsonld`, and the other RDF serializations Oxigraph
//! already understands. Schema terms become [`SymbolOccurrence`] rows so
//! `find symbol` hits OWL/RDFS classes and properties; `owl:imports` becomes
//! a graph import edge. Instance data without an explicit schema type stays
//! in the SPARQL formal store, not the symbol index.
// Rust guideline compliant 2026-02-21

use std::collections::{BTreeSet, HashMap};
use std::io::Cursor;
use std::path::Path;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, AhoCorasickKind};
use oxigraph::io::{JsonLdProfileSet, RdfFormat, RdfParser};
use oxigraph::model::{NamedOrBlankNode, Term};

use crate::model::{SourceLanguage, SymbolKind, SymbolOccurrence};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_PROPERTY: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property";
const RDFS_CLASS: &str = "http://www.w3.org/2000/01/rdf-schema#Class";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const RDFS_SUBPROPERTY: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const OWL_NS: &str = "http://www.w3.org/2002/07/owl#";
const OWL_CLASS: &str = "http://www.w3.org/2002/07/owl#Class";
const OWL_DEPRECATED_CLASS: &str = "http://www.w3.org/2002/07/owl#DeprecatedClass";
const OWL_OBJECT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#ObjectProperty";
const OWL_DATATYPE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#DatatypeProperty";
const OWL_ANNOTATION_PROPERTY: &str = "http://www.w3.org/2002/07/owl#AnnotationProperty";
const OWL_FUNCTIONAL_PROPERTY: &str = "http://www.w3.org/2002/07/owl#FunctionalProperty";
const OWL_INVERSE_FUNCTIONAL_PROPERTY: &str =
    "http://www.w3.org/2002/07/owl#InverseFunctionalProperty";
const OWL_SYMMETRIC_PROPERTY: &str = "http://www.w3.org/2002/07/owl#SymmetricProperty";
const OWL_TRANSITIVE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#TransitiveProperty";
const OWL_NAMED_INDIVIDUAL: &str = "http://www.w3.org/2002/07/owl#NamedIndividual";
const OWL_ONTOLOGY: &str = "http://www.w3.org/2002/07/owl#Ontology";
const OWL_IMPORTS: &str = "http://www.w3.org/2002/07/owl#imports";
const SKOS_PREFLABEL: &str = "http://www.w3.org/2004/02/skos/core#prefLabel";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS_NS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema#";

/// `owl:imports` edge extracted from one ontology document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OntologyImport {
    pub iri: String,
    pub line: usize,
}

/// Schema terms and import edges from one RDF document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OntologyExtract {
    pub symbols: Vec<SymbolOccurrence>,
    pub imports: Vec<OntologyImport>,
}

/// True when `path` is an RDF/OWL serialization LEIO should treat as ontology.
pub fn is_rdf_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "ttl" | "owl" | "nt" | "nq" | "trig" | "jsonld" | "rdf" | "n3"
    )
}

/// Oxigraph format for `path`, sniffing `.owl` between RDF/XML and Turtle.
///
/// `.owl` is used for both Turtle-serialized OWL and RDF/XML. The sniff is
/// the leading non-BOM byte: XML always starts with `<`.
pub fn rdf_format_for(path: &Path, source: &str) -> RdfFormat {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "nt" => RdfFormat::NTriples,
        "nq" => RdfFormat::NQuads,
        "trig" => RdfFormat::TriG,
        "n3" => RdfFormat::N3,
        "jsonld" => RdfFormat::JsonLd {
            profile: JsonLdProfileSet::empty(),
        },
        "rdf" => RdfFormat::RdfXml,
        "owl" => sniff_owl_format(source),
        _ => RdfFormat::Turtle,
    }
}

/// True when `format` is Turtle-family and may contain RDF-star to rewrite.
pub fn format_needs_turtle_prepare(format: RdfFormat) -> bool {
    matches!(format, RdfFormat::Turtle | RdfFormat::TriG)
}

/// Parse `source` at `path` into schema symbols and `owl:imports`.
///
/// Parse failures yield an empty extract so a malformed ontology cannot abort
/// indexing of the rest of the tree.
pub fn extract_ontology(path: &str, source: &str) -> OntologyExtract {
    let mut kinds: HashMap<String, SymbolKind> = HashMap::new();
    let mut labels: HashMap<String, Vec<String>> = HashMap::new();
    let mut subclass_subjects: Vec<String> = Vec::new();
    let mut subproperty_subjects: Vec<String> = Vec::new();
    let mut import_iris: Vec<String> = Vec::new();

    for quad in parse_quads(path, source) {
        let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
            continue;
        };
        let subject_iri = subject.as_str().to_string();
        if is_vocab_iri(&subject_iri) {
            continue;
        }
        let pred = quad.predicate.as_str();
        if pred == RDF_TYPE {
            if let Term::NamedNode(ty) = &quad.object
                && let Some(kind) = classify_type(ty.as_str())
            {
                assign_kind(&mut kinds, &subject_iri, kind);
            }
            continue;
        }
        if pred == RDFS_LABEL || pred == SKOS_PREFLABEL {
            if let Term::Literal(lit) = &quad.object {
                let label = lit.value().trim();
                if !label.is_empty() {
                    labels
                        .entry(subject_iri)
                        .or_default()
                        .push(label.to_string());
                }
            }
            continue;
        }
        if pred == RDFS_SUBCLASS {
            subclass_subjects.push(subject_iri);
            continue;
        }
        if pred == RDFS_SUBPROPERTY {
            subproperty_subjects.push(subject_iri);
            continue;
        }
        if pred == OWL_IMPORTS
            && let Term::NamedNode(imported) = &quad.object
        {
            import_iris.push(imported.as_str().to_string());
        }
    }

    for iri in subclass_subjects {
        kinds.entry(iri).or_insert(SymbolKind::Class);
    }
    for iri in subproperty_subjects {
        kinds.entry(iri).or_insert(SymbolKind::Property);
    }

    let lines = FirstSourceLines::new(
        source,
        kinds
            .keys()
            .chain(import_iris.iter())
            .flat_map(|iri| [iri.clone(), local_name(iri)])
            .chain(
                labels
                    .iter()
                    .filter(|(iri, _)| kinds.contains_key(*iri))
                    .flat_map(|(_, names)| names.iter().cloned()),
            ),
    );

    let mut symbols = Vec::new();
    let mut subjects: Vec<_> = kinds.iter().collect();
    subjects.sort_unstable_by_key(|(iri, _)| *iri);
    for (iri, kind) in subjects {
        let local = local_name(iri);
        if local.is_empty() {
            continue;
        }
        let line = lines.line_of(&[&local, iri]);
        symbols.push(SymbolOccurrence {
            name: local.clone(),
            kind: *kind,
            path: path.to_string(),
            line,
            language: SourceLanguage::Rdf,
            qual_name: Some(iri.clone()),
        });
        if let Some(names) = labels.get(iri) {
            for label in names {
                if label == &local {
                    continue;
                }
                let label_line = lines.line_of(&[label, &local, iri]);
                symbols.push(SymbolOccurrence {
                    name: label.clone(),
                    kind: *kind,
                    path: path.to_string(),
                    line: label_line,
                    language: SourceLanguage::Rdf,
                    qual_name: Some(iri.clone()),
                });
            }
        }
    }

    let imports = import_iris
        .into_iter()
        .map(|iri| {
            let line = lines.line_of(&[&iri, &local_name(&iri)]);
            OntologyImport { iri, line }
        })
        .collect();

    OntologyExtract { symbols, imports }
}

fn sniff_owl_format(source: &str) -> RdfFormat {
    let trimmed = source.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with('<') {
        RdfFormat::RdfXml
    } else {
        RdfFormat::Turtle
    }
}

fn parse_quads(path: &str, source: &str) -> Vec<oxigraph::model::Quad> {
    let format = rdf_format_for(Path::new(path), source);
    let base = document_base_iri(path);
    let parser = match RdfParser::from_format(format).with_base_iri(&base) {
        Ok(parser) => parser,
        Err(_) => RdfParser::from_format(format),
    };
    parser
        .for_reader(Cursor::new(source.as_bytes()))
        .filter_map(Result::ok)
        .collect()
}

fn document_base_iri(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    }
}

fn classify_type(iri: &str) -> Option<SymbolKind> {
    match iri {
        OWL_CLASS | RDFS_CLASS | OWL_DEPRECATED_CLASS => Some(SymbolKind::Class),
        OWL_OBJECT_PROPERTY
        | OWL_DATATYPE_PROPERTY
        | OWL_ANNOTATION_PROPERTY
        | RDF_PROPERTY
        | OWL_FUNCTIONAL_PROPERTY
        | OWL_INVERSE_FUNCTIONAL_PROPERTY
        | OWL_SYMMETRIC_PROPERTY
        | OWL_TRANSITIVE_PROPERTY => Some(SymbolKind::Property),
        OWL_NAMED_INDIVIDUAL => Some(SymbolKind::Constant),
        OWL_ONTOLOGY => Some(SymbolKind::Module),
        _ => None,
    }
}

fn assign_kind(kinds: &mut HashMap<String, SymbolKind>, iri: &str, kind: SymbolKind) {
    match kinds.get(iri) {
        Some(existing) if kind_rank(*existing) >= kind_rank(kind) => {}
        _ => {
            kinds.insert(iri.to_string(), kind);
        }
    }
}

fn kind_rank(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Module => 4,
        SymbolKind::Class => 3,
        SymbolKind::Property => 2,
        SymbolKind::Constant => 1,
        _ => 0,
    }
}

fn is_vocab_iri(iri: &str) -> bool {
    iri.starts_with(RDF_NS)
        || iri.starts_with(RDFS_NS)
        || iri.starts_with(OWL_NS)
        || iri.starts_with(XSD_NS)
}

fn local_name(iri: &str) -> String {
    if let Some((_, rest)) = iri.rsplit_once('#')
        && !rest.is_empty()
    {
        return rest.to_string();
    }
    if let Some((_, rest)) = iri.rsplit_once('/')
        && !rest.is_empty()
    {
        return rest.to_string();
    }
    iri.to_string()
}

struct FirstSourceLines {
    lines: HashMap<String, usize>,
}

impl FirstSourceLines {
    fn new(source: &str, needles: impl IntoIterator<Item = String>) -> Self {
        Self::from_lines(source.lines(), needles)
    }

    fn from_lines<'a>(
        lines: impl Iterator<Item = &'a str>,
        needles: impl IntoIterator<Item = String>,
    ) -> Self {
        let patterns: Vec<_> = needles
            .into_iter()
            .filter(|needle| !needle.is_empty() && !needle.contains('\n'))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if patterns.is_empty() {
            return Self {
                lines: HashMap::new(),
            };
        }
        let matcher = AhoCorasickBuilder::new()
            .kind(Some(AhoCorasickKind::NoncontiguousNFA))
            .build(&patterns)
            .ok();
        Self::scan(lines, patterns, matcher.as_ref())
    }

    fn scan<'a>(
        lines: impl Iterator<Item = &'a str>,
        patterns: Vec<String>,
        matcher: Option<&AhoCorasick>,
    ) -> Self {
        let mut first_lines = vec![None; patterns.len()];
        let mut remaining = patterns.len();
        for (line_index, line) in lines.enumerate() {
            if let Some(matcher) = matcher {
                for matched in matcher.find_overlapping_iter(line) {
                    let first = &mut first_lines[matched.pattern().as_usize()];
                    if first.is_none() {
                        *first = Some(line_index + 1);
                        remaining -= 1;
                    }
                }
            } else {
                for (pattern, first) in patterns.iter().zip(&mut first_lines) {
                    if first.is_none() && line.contains(pattern) {
                        *first = Some(line_index + 1);
                        remaining -= 1;
                    }
                }
            }
            if remaining == 0 {
                break;
            }
        }
        Self {
            lines: patterns
                .into_iter()
                .zip(first_lines)
                .filter_map(|(pattern, line)| line.map(|line| (pattern, line)))
                .collect(),
        }
    }

    fn line_of(&self, needles: &[&str]) -> usize {
        needles
            .iter()
            .filter_map(|needle| self.lines.get(*needle).copied())
            .min()
            .unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Instant;

    fn legacy_line_of(source: &str, needles: &[&str]) -> usize {
        for (index, line) in source.lines().enumerate() {
            for needle in needles {
                if !needle.is_empty() && line.contains(needle) {
                    return index + 1;
                }
            }
        }
        1
    }

    #[test]
    fn first_lines_preserve_literal_first_match_semantics() {
        let sources = [
            "",
            "\n\n",
            "missing\nClaim suffix\nClaim\n",
            "\r\nalpha\r\nβeta\r\n終末\r\n",
            "alpha\rbeta\n",
            "long Claim label\nClaim\nhttp://example.org/Claim\n",
            "She hers\nhe\nhers\n",
            "\nCafé Cafe\u{301}\nCAFÉ\n",
            "before\nsecond\n",
            "aba\nbab\nabab\n",
            "left\nright",
        ];
        let groups: &[&[&str]] = &[
            &[""],
            &["missing", "Claim"],
            &["absent", "Claim"],
            &["Claim", "long Claim label", "http://example.org/Claim"],
            &["he"],
            &["hers", "She"],
            &["α", "βeta", "終末"],
            &["Café"],
            &["Cafe\u{301}", "CAFÉ"],
            &["\r"],
            &["alpha\r"],
            &["\n"],
            &["left\nright"],
            &["aba", "bab", "abab"],
            &["Claim", "Claim", ""],
            &["before", "second"],
            &["second", "before"],
            &[],
        ];
        for source in sources {
            let lines = FirstSourceLines::new(
                source,
                groups
                    .iter()
                    .flat_map(|group| group.iter().map(|needle| needle.to_string())),
            );
            for group in groups {
                assert_eq!(
                    lines.line_of(group),
                    legacy_line_of(source, group),
                    "{source:?} {group:?}"
                );
            }
        }
    }

    #[test]
    fn first_lines_match_generated_unicode_and_overlap_queries() {
        let alphabet = ['a', 'b', '\r', '\n', 'é', '終'];
        for seed in 0..128 {
            let mut state = seed + 1;
            let mut source = String::new();
            for _ in 0..96 {
                state = (state * 17 + 13) % 997;
                source.push(alphabet[state % alphabet.len()]);
            }
            let chars: Vec<_> = source.chars().collect();
            let mut needles = vec![String::new(), "absent".to_string()];
            for start in 0..chars.len() {
                for width in 1..=4 {
                    if start + width <= chars.len() {
                        needles.push(chars[start..start + width].iter().collect());
                    }
                }
            }
            let lines = FirstSourceLines::new(&source, needles.clone());
            for (index, needle) in needles.iter().enumerate() {
                let group = [
                    needle.as_str(),
                    needles[(index + 7) % needles.len()].as_str(),
                ];
                assert_eq!(lines.line_of(&group), legacy_line_of(&source, &group));
            }
        }
    }

    #[test]
    fn attribution_reads_each_source_line_once_independent_of_query_count() {
        let source = (0..2048)
            .map(|index| format!("item-{index:04}#\n"))
            .collect::<String>();
        let needles: Vec<_> = (0..2048)
            .map(|index| format!("item-{index:04}#"))
            .chain(["absent".to_string()])
            .collect();
        let reads = Cell::new(0);
        let lines = FirstSourceLines::from_lines(
            source.lines().inspect(|_| reads.set(reads.get() + 1)),
            needles.clone(),
        );
        assert_eq!(reads.get(), 2048);
        for (index, needle) in needles[..2048].iter().enumerate() {
            assert_eq!(lines.line_of(&[needle]), index + 1);
        }
        assert_eq!(lines.line_of(&["absent"]), 1);
        assert_eq!(reads.get(), 2048);
    }

    #[test]
    fn attribution_stops_when_all_patterns_have_first_lines() {
        let reads = Cell::new(0);
        let source = "\npattern\nnever needed\n";
        let lines = FirstSourceLines::from_lines(
            source.lines().inspect(|_| reads.set(reads.get() + 1)),
            ["pattern".to_string(), "pattern".to_string(), String::new()],
        );
        assert_eq!(reads.get(), 2);
        assert_eq!(lines.line_of(&["pattern"]), 2);
        let empty = FirstSourceLines::from_lines(
            source.lines().inspect(|_| reads.set(reads.get() + 1)),
            [String::new(), "\n".to_string()],
        );
        assert_eq!(reads.get(), 2);
        assert_eq!(empty.line_of(&[""]), 1);
    }

    #[test]
    fn unavailable_matcher_falls_back_without_losing_first_matches() {
        let source = "\r\nhers Claim\nCafé\nClaim\n";
        let patterns = ["hers", "he", "Claim", "Café", "absent"];
        let lines = FirstSourceLines::scan(
            source.lines(),
            patterns.iter().map(|needle| needle.to_string()).collect(),
            None,
        );
        for needle in patterns {
            assert_eq!(lines.line_of(&[needle]), legacy_line_of(source, &[needle]));
        }
    }

    #[test]
    fn extraction_keeps_legacy_symbol_label_and_import_lines() {
        for (path, source) in [
            ("ont/claim.ttl", TURTLE),
            ("ont/invoice.jsonld", JSONLD),
            ("ont/policy.owl", OWL_XML),
        ] {
            let extracted = extract_ontology(path, source);
            for symbol in &extracted.symbols {
                let iri = symbol.qual_name.as_deref().unwrap();
                assert_eq!(
                    symbol.line,
                    legacy_line_of(source, &[&symbol.name, &local_name(iri), iri])
                );
            }
            for imported in &extracted.imports {
                assert_eq!(
                    imported.line,
                    legacy_line_of(source, &[&imported.iri, &local_name(&imported.iri)])
                );
            }
        }
    }

    #[test]
    fn generated_ontology_preserves_all_attributed_rows() {
        let mut source = String::from(
            "@prefix owl: <http://www.w3.org/2002/07/owl#> .\r\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\r\n@prefix ex: <http://example.org/test#> .\r\n",
        );
        for index in 0..256 {
            source.push_str(&format!(
                "ex:Class{index:04} a owl:Class ; rdfs:label \"Café 終 {index:04}\" .\r\n"
            ));
        }
        let extracted = extract_ontology("ont/generated.ttl", &source);
        assert_eq!(extracted.symbols.len(), 512);
        for symbol in extracted.symbols {
            let iri = symbol.qual_name.as_deref().unwrap();
            assert_eq!(symbol.kind, SymbolKind::Class);
            assert_eq!(symbol.language, SourceLanguage::Rdf);
            assert_eq!(symbol.path, "ont/generated.ttl");
            assert_eq!(
                symbol.line,
                legacy_line_of(&source, &[&symbol.name, &local_name(iri), iri])
            );
        }
    }

    #[test]
    fn ontology_symbols_use_full_iri_order_without_reordering_aliases() {
        let prefixes = "@prefix owl: <http://www.w3.org/2002/07/owl#> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n";
        let alpha =
            "<http://z.example/Alpha> a owl:Class ; rdfs:label \"Alias B\", \"Alias A\" .\n";
        let zulu = "<http://a.example/Zulu> a owl:Class ; rdfs:label \"Zulu\", \"Zulu Alias\" .\n";
        let expected = [
            ("Zulu", "http://a.example/Zulu"),
            ("Zulu Alias", "http://a.example/Zulu"),
            ("Alpha", "http://z.example/Alpha"),
            ("Alias B", "http://z.example/Alpha"),
            ("Alias A", "http://z.example/Alpha"),
        ];
        for declarations in [[alpha, zulu], [zulu, alpha]] {
            let source = format!("{prefixes}{}{}", declarations[0], declarations[1]);
            for _ in 0..16 {
                let extracted = extract_ontology("ont/ordered.ttl", &source);
                let identities: Vec<_> = extracted
                    .symbols
                    .iter()
                    .map(|symbol| (symbol.name.as_str(), symbol.qual_name.as_deref().unwrap()))
                    .collect();
                assert_eq!(identities, expected);
                for symbol in &extracted.symbols {
                    let iri = symbol.qual_name.as_deref().unwrap();
                    assert_eq!(symbol.kind, SymbolKind::Class);
                    assert_eq!(symbol.language, SourceLanguage::Rdf);
                    assert_eq!(symbol.path, "ont/ordered.ttl");
                    assert_eq!(
                        symbol.line,
                        legacy_line_of(&source, &[&symbol.name, &local_name(iri), iri])
                    );
                }
            }
        }
    }

    #[test]
    fn measured_line_attribution_matches_the_reference() {
        let source = match std::env::var_os("LEIO_ONTOLOGY_PARITY_SOURCE") {
            Some(path) => std::fs::read_to_string(path).unwrap(),
            None => (0..1000)
                .map(|index| format!("item-{index:04}#\n"))
                .collect(),
        };
        let extracted = extract_ontology("ont/measured.ttl", &source);
        let groups: Vec<Vec<String>> = if extracted.symbols.is_empty() {
            (0..1000)
                .map(|index| vec![format!("item-{index:04}#")])
                .collect()
        } else {
            extracted
                .symbols
                .iter()
                .map(|symbol| {
                    let iri = symbol.qual_name.as_deref().unwrap();
                    vec![symbol.name.clone(), local_name(iri), iri.to_string()]
                })
                .chain(
                    extracted
                        .imports
                        .iter()
                        .map(|imported| vec![imported.iri.clone(), local_name(&imported.iri)]),
                )
                .collect()
        };
        let started = Instant::now();
        let lines = FirstSourceLines::new(&source, groups.iter().flatten().cloned());
        let actual: Vec<_> = groups
            .iter()
            .map(|group| lines.line_of(&group.iter().map(String::as_str).collect::<Vec<_>>()))
            .collect();
        let batched = started.elapsed();
        let started = Instant::now();
        let expected: Vec<_> = groups
            .iter()
            .map(|group| {
                legacy_line_of(
                    &source,
                    &group.iter().map(String::as_str).collect::<Vec<_>>(),
                )
            })
            .collect();
        let legacy = started.elapsed();
        assert_eq!(actual, expected);
        println!(
            "ontology-line-parity bytes={} lines={} symbols={} imports={} queries={} batched_ms={} legacy_ms={}",
            source.len(),
            source.lines().count(),
            extracted.symbols.len(),
            extracted.imports.len(),
            groups.len(),
            batched.as_millis(),
            legacy.as_millis()
        );
    }

    const TURTLE: &str = r#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix ex: <http://example.org/leio-ont#> .

<http://example.org/leio-ont> a owl:Ontology ;
    owl:imports <http://example.org/imported> .

ex:Claim a owl:Class ;
    rdfs:label "Insurance Claim" ;
    rdfs:subClassOf ex:Document .

ex:Document a owl:Class .

ex:hasAmount a owl:DatatypeProperty ;
    rdfs:domain ex:Claim .

ex:alpha a ex:Claim .
"#;

    const JSONLD: &str = r#"[{
  "@id": "http://example.org/leio-ont#Invoice",
  "@type": ["http://www.w3.org/2002/07/owl#Class"],
  "http://www.w3.org/2000/01/rdf-schema#label": [{"@value": "Invoice"}]
}]
"#;

    const OWL_XML: &str = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:owl="http://www.w3.org/2002/07/owl#"
         xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#">
  <owl:Class rdf:about="http://example.org/leio-ont#Policy">
    <rdfs:label>Policy</rdfs:label>
  </owl:Class>
</rdf:RDF>
"#;

    #[test]
    fn turtle_extracts_classes_properties_imports_not_instances() {
        let extracted = extract_ontology("ont/claim.ttl", TURTLE);
        let names: Vec<&str> = extracted.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Claim"), "{names:?}");
        assert!(names.contains(&"Insurance Claim"), "{names:?}");
        assert!(names.contains(&"Document"), "{names:?}");
        assert!(names.contains(&"hasAmount"), "{names:?}");
        assert!(!names.contains(&"alpha"), "{names:?}");
        assert_eq!(
            extracted
                .symbols
                .iter()
                .find(|s| s.name == "Claim")
                .map(|s| s.kind),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            extracted
                .symbols
                .iter()
                .find(|s| s.name == "hasAmount")
                .map(|s| s.kind),
            Some(SymbolKind::Property)
        );
        assert_eq!(extracted.imports.len(), 1);
        assert_eq!(extracted.imports[0].iri, "http://example.org/imported");
        assert_eq!(
            extracted
                .symbols
                .iter()
                .find(|s| s.name == "Claim")
                .and_then(|s| s.qual_name.as_deref()),
            Some("http://example.org/leio-ont#Claim")
        );
    }

    #[test]
    fn jsonld_extracts_class() {
        let extracted = extract_ontology("ont/invoice.jsonld", JSONLD);
        assert!(
            extracted.symbols.iter().any(|s| s.name == "Invoice"
                && s.kind == SymbolKind::Class
                && s.language == SourceLanguage::Rdf),
            "{:?}",
            extracted.symbols
        );
    }

    #[test]
    fn compact_jsonld_with_inline_context_extracts_class() {
        let compact = r#"{
  "@context": { "owl": "http://www.w3.org/2002/07/owl#" },
  "@id": "http://example.org/leio-ont#Receipt",
  "@type": "owl:Class"
}"#;
        let extracted = extract_ontology("ont/receipt.jsonld", compact);
        assert!(
            extracted
                .symbols
                .iter()
                .any(|s| s.name == "Receipt" && s.kind == SymbolKind::Class),
            "{:?}",
            extracted.symbols
        );
    }

    #[test]
    fn owl_rdfxml_extracts_class() {
        assert!(matches!(
            rdf_format_for(Path::new("ont/policy.owl"), OWL_XML),
            RdfFormat::RdfXml
        ));
        let extracted = extract_ontology("ont/policy.owl", OWL_XML);
        assert!(
            extracted
                .symbols
                .iter()
                .any(|s| s.name == "Policy" && s.kind == SymbolKind::Class),
            "{:?}",
            extracted.symbols
        );
    }

    #[test]
    fn owl_turtle_is_not_sniffed_as_xml() {
        assert!(matches!(
            rdf_format_for(Path::new("ont/claim.owl"), TURTLE),
            RdfFormat::Turtle
        ));
    }

    #[test]
    fn jsonld_extension_is_rdf() {
        assert!(is_rdf_path(Path::new("ont/invoice.jsonld")));
        assert!(is_rdf_path(Path::new("ont/claim.ttl")));
        assert!(is_rdf_path(Path::new("ont/policy.owl")));
        assert!(!is_rdf_path(Path::new("package.json")));
    }

    #[test]
    fn malformed_jsonld_does_not_abort() {
        let extracted = extract_ontology("ont/broken.jsonld", "{ this is not jsonld");
        assert!(extracted.symbols.is_empty());
        assert!(extracted.imports.is_empty());
    }
}
