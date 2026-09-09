use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct HealthAuditEmbeddingDoctor;

impl Doctor for HealthAuditEmbeddingDoctor {
    fn name(&self) -> &'static str {
        "health-audit-embedding"
    }

    fn description(&self) -> &'static str {
        "Enforces the Health Audit Arrow-only authority boundary: ontology parsing publishes local non-authoritative proposal packs and exposes no query-time retriever, embedding transport, or vector-store client."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_embedding(index, root)
    }
}

const ONTOLOGY_REL: &str = "cartridges/health_audit/ontology.py";
const REQUIREMENTS_REL: &str = "example-api/requirements.health-audit.txt";
const TASKS_REL: &str = "cartridges/health_audit/tasks.py";

const REQUIRED_ONTOLOGY_MARKERS: &[(&str, &str)] = &[
    (
        "publish_ontology_arrow_proposal_pack",
        "ontology parser publishes a typed Arrow proposal pack",
    ),
    (
        "publish_arrow_document_pack",
        "ontology parser delegates to the canonical Arrow document adapter",
    ),
    (
        "\"evidence_class\": \"PROPOSAL\"",
        "ontology records are explicitly proposals",
    ),
    (
        "\"authoritative\": False",
        "ontology proposal records cannot be authoritative",
    ),
    (
        "\"network_dependencies\": []",
        "ontology proposal publication declares no network dependency",
    ),
    (
        "\"embedding_model\": None",
        "ontology proposal publication declares no embedding model",
    ),
];

const FORBIDDEN_ONTOLOGY_MARKERS: &[&str] = &[
    "class OntologyRetriever",
    "def _embed_batch_",
    "def embed_chunks(",
    "example.core.vector",
    "pymilvus",
    "SentenceTransformerEngine",
];

pub fn doctor_health_audit_embedding(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    let ontology_src = read_text(&root.join(ONTOLOGY_REL), &mut warnings);
    let requirements_src = read_text(&root.join(REQUIREMENTS_REL), &mut warnings);
    let tasks_src = read_text(&root.join(TASKS_REL), &mut warnings);

    if let Some(src) = ontology_src.as_deref() {
        for (marker, detail) in REQUIRED_ONTOLOGY_MARKERS {
            match find_line(src, marker) {
                Some(line) => evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: ONTOLOGY_REL.to_string(),
                    line: Some(line),
                    detail: (*detail).to_string(),
                }),
                None => warnings.push(format!(
                    "{ONTOLOGY_REL}: missing Arrow-only authority marker `{marker}`"
                )),
            }
        }
        for marker in FORBIDDEN_ONTOLOGY_MARKERS {
            if let Some(line) = find_line(src, marker) {
                warnings.push(format!(
                    "{ONTOLOGY_REL}:{line}: forbidden model/vector runtime marker `{marker}`"
                ));
            }
        }
    }

    if let Some(src) = requirements_src.as_deref() {
        if let Some(line) = find_line(src, "pymilvus") {
            warnings.push(format!(
                "{REQUIREMENTS_REL}:{line}: dedicated Health Audit image must not install pymilvus"
            ));
        } else {
            evidence.push(EvidenceItem {
                kind: "dependency_boundary".to_string(),
                path: REQUIREMENTS_REL.to_string(),
                line: None,
                detail: "dedicated Health Audit requirements contain no pymilvus".to_string(),
            });
        }
    }

    if let Some(src) = tasks_src.as_deref() {
        let preauth_start = src.find("def preauth_tiss_task");
        let preauth_end = preauth_start.and_then(|start| {
            src[start + 1..]
                .find("\n@")
                .map(|offset| start + 1 + offset)
        });
        let preauth = preauth_start.map(|start| &src[start..preauth_end.unwrap_or(src.len())]);
        if preauth.is_some_and(|body| {
            body.contains("_text_to_embedding") || body.contains("OntologyRetriever")
        }) {
            warnings.push(format!(
                "{TASKS_REL}: preauth_tiss_task must remain deterministic and model-free"
            ));
        } else {
            evidence.push(EvidenceItem {
                kind: "hot_path".to_string(),
                path: TASKS_REL.to_string(),
                line: preauth_start.map(|offset| src[..offset].lines().count() + 1),
                detail: "preauth_tiss_task has no embedding or ontology retriever import"
                    .to_string(),
            });
        }
    }

    let arrow_only = ontology_src.as_deref().is_some_and(|src| {
        REQUIRED_ONTOLOGY_MARKERS
            .iter()
            .all(|(marker, _)| src.contains(marker))
            && FORBIDDEN_ONTOLOGY_MARKERS
                .iter()
                .all(|marker| !src.contains(marker))
    });

    let entities = vec![json!({
        "doctor": "health-audit-embedding",
        "authority_boundary": "arrow-only",
        "ontology_arrow_proposals": arrow_only,
        "query_time_retriever_absent": ontology_src
            .as_deref()
            .is_some_and(|src| !src.contains("class OntologyRetriever")),
        "model_transport_absent": ontology_src
            .as_deref()
            .is_some_and(|src| !src.contains("def _embed_batch_")),
        "dedicated_requirements_vector_free": requirements_src
            .as_deref()
            .is_some_and(|src| !src.contains("pymilvus")),
    })];

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_health_audit_embedding"),
        kind: "doctor".to_string(),
        summary: format!(
            "health-audit Arrow-only authority boundary checked: {} warnings, {} evidence items",
            warnings.len(),
            evidence.len()
        ),
        confidence: if warnings.is_empty() { 0.99 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::doctor_health_audit_embedding;
    use crate::model::RepoIndex;
    use std::fs;
    use tempfile::tempdir;

    fn empty_index() -> RepoIndex {
        RepoIndex {
            version: 0,
            root: String::new(),
            indexed_at: String::new(),
            files: Vec::new(),
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    fn write_fixture(root: &Path, ontology: &str, requirements: &str, tasks: &str) {
        let ontology_path = root.join(super::ONTOLOGY_REL);
        let requirements_path = root.join(super::REQUIREMENTS_REL);
        let tasks_path = root.join(super::TASKS_REL);
        fs::create_dir_all(ontology_path.parent().unwrap()).unwrap();
        fs::create_dir_all(requirements_path.parent().unwrap()).unwrap();
        fs::write(ontology_path, ontology).unwrap();
        fs::write(requirements_path, requirements).unwrap();
        fs::write(tasks_path, tasks).unwrap();
    }

    use std::path::Path;

    #[test]
    fn arrow_only_fixture_passes() {
        let dir = tempdir().unwrap();
        write_fixture(
            dir.path(),
            r#"
def publish_ontology_arrow_proposal_pack():
    publish_arrow_document_pack([])
    return {"evidence_class": "PROPOSAL", "authoritative": False,
            "network_dependencies": [], "embedding_model": None}
"#,
            "pyarrow==1\n",
            "def preauth_tiss_task():\n    return validate_tuss()\n",
        );
        let result = doctor_health_audit_embedding(&empty_index(), dir.path());
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn query_time_retriever_is_rejected() {
        let dir = tempdir().unwrap();
        write_fixture(
            dir.path(),
            r#"
class OntologyRetriever: pass
def publish_ontology_arrow_proposal_pack():
    publish_arrow_document_pack([])
    return {"evidence_class": "PROPOSAL", "authoritative": False,
            "network_dependencies": [], "embedding_model": None}
"#,
            "pyarrow==1\n",
            "def preauth_tiss_task():\n    return validate_tuss()\n",
        );
        let result = doctor_health_audit_embedding(&empty_index(), dir.path());
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("OntologyRetriever"))
        );
    }
}
