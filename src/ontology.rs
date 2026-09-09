//! Ontology files as first-class index and formal-graph sources.
//!
//! Parses `.ttl`, `.owl`, `.jsonld`, and the other RDF serializations Oxigraph
//! already understands. Schema terms become [`SymbolOccurrence`] rows so
//! `find symbol` hits OWL/RDFS classes and properties; `owl:imports` becomes
//! a graph import edge. Instance data without an explicit schema type stays
//! in the SPARQL formal store, not the symbol index.
// Rust guideline compliant 2026-02-21

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

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

    let mut symbols = Vec::new();
    for (iri, kind) in &kinds {
        let local = local_name(iri);
        if local.is_empty() {
            continue;
        }
        let line = line_of(source, &[&local, iri]);
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
                let label_line = line_of(source, &[label, &local, iri]);
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
            let line = line_of(source, &[&iri, &local_name(&iri)]);
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

fn line_of(source: &str, needles: &[&str]) -> usize {
    for (idx, line) in source.lines().enumerate() {
        for needle in needles {
            if !needle.is_empty() && line.contains(needle) {
                return idx + 1;
            }
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

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
