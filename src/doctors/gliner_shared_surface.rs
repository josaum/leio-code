use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct GlinerSharedSurfaceDoctor;

impl Doctor for GlinerSharedSurfaceDoctor {
    fn name(&self) -> &'static str {
        "gliner-shared-surface"
    }

    fn description(&self) -> &'static str {
        "Checks that GLiNER consumers depend on the shared `gliner_core::GlinerEngine` surface instead of bypassing it with raw ONNX runtime wrappers."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_gliner_shared_surface(root)
    }
}

struct SurfaceCheck {
    relative_path: &'static str,
    required: &'static [(&'static str, &'static str)],
    forbidden: &'static [(&'static str, &'static str)],
}

const RAW_GLINER_IMPORTS: &[(&str, &str)] = &[
    (
        "use gliner_core::ort_inference::GlinerInference",
        "raw GlinerInference import resurfaced",
    ),
    (
        "use gliner_core::ort_inference::{GlinerInference",
        "raw GlinerInference grouped import resurfaced",
    ),
    (
        "GlinerInference::new(",
        "raw GlinerInference construction resurfaced",
    ),
    ("GlinerSession", "legacy GlinerSession surface resurfaced"),
    ("predict_ner(", "legacy predict_ner surface resurfaced"),
];

const SURFACE_CHECKS: &[SurfaceCheck] = &[
    SurfaceCheck {
        relative_path: "office-parsers-rs/gliner-core/src/lib.rs",
        required: &[
            (
                "pub mod engine;",
                "gliner-core exposes the shared engine module",
            ),
            (
                "pub use engine::GlinerEngine;",
                "gliner-core re-exports GlinerEngine as the canonical surface",
            ),
        ],
        forbidden: &[],
    },
    SurfaceCheck {
        relative_path: "office-parsers-rs/gliner-core/src/engine.rs",
        required: &[
            (
                "pub struct GlinerEngine",
                "shared GLiNER engine wrapper exists",
            ),
            (
                "fn shared_consumers_use_gliner_engine_surface()",
                "shared GLiNER surface has structural regression coverage",
            ),
        ],
        forbidden: &[],
    },
    SurfaceCheck {
        relative_path: "example-gateway/src/gliner.rs",
        required: &[
            (
                "use gliner_core::GlinerEngine as SharedGlinerEngine;",
                "gateway GLiNER wrapper composes the shared engine",
            ),
            (
                "engine: SharedGlinerEngine,",
                "gateway GLiNER wrapper stores the shared engine directly",
            ),
        ],
        forbidden: RAW_GLINER_IMPORTS,
    },
    SurfaceCheck {
        relative_path: "example-extractor/src-tauri/src/gliner.rs",
        required: &[(
            "pub struct GlinerFlightClient",
            "extractor uses the canonical GlinerFlightClient surface",
        )],
        forbidden: RAW_GLINER_IMPORTS,
    },
    SurfaceCheck {
        relative_path: "office-parsers-rs/gliner-fast-py/src/lib.rs",
        required: &[
            (
                "GlinerEngine as CoreGlinerEngine",
                "Python bindings compose the shared engine instead of raw ONNX inference",
            ),
            (
                "engine: CoreGlinerEngine,",
                "Python bindings store the shared engine directly",
            ),
        ],
        forbidden: RAW_GLINER_IMPORTS,
    },
    SurfaceCheck {
        relative_path: "office-parsers-rs/pdf-fast-core/examples/gliner-extract.rs",
        required: &[(
            "use gliner_core::GlinerEngine;",
            "pdf-fast example uses the shared GLiNER engine surface",
        )],
        forbidden: RAW_GLINER_IMPORTS,
    },
    SurfaceCheck {
        relative_path: "office-parsers-rs/examples/gliner_real_model.rs",
        required: &[(
            "use gliner_core::{GlinerEngine, GlinerRuntimeConfig};",
            "real-model example uses the shared GLiNER engine surface",
        )],
        forbidden: RAW_GLINER_IMPORTS,
    },
    SurfaceCheck {
        relative_path: "office-parsers-rs/gliner-core/README.md",
        required: &[(
            "use gliner_core::GlinerEngine;",
            "gliner-core README points consumers at the shared engine surface",
        )],
        forbidden: RAW_GLINER_IMPORTS,
    },
    SurfaceCheck {
        relative_path: "office-parsers-rs/GLINER_INTEGRATION.md",
        required: &[(
            "The canonical surface is now `gliner_core::GlinerEngine`.",
            "integration guide names the shared GLiNER surface explicitly",
        )],
        forbidden: RAW_GLINER_IMPORTS,
    },
    SurfaceCheck {
        relative_path: "office-parsers-rs/USAGE.md",
        required: &[(
            "Use `gliner_core::GlinerEngine`.",
            "usage guide names the shared GLiNER surface explicitly",
        )],
        forbidden: RAW_GLINER_IMPORTS,
    },
    SurfaceCheck {
        relative_path: "office-parsers-rs/README.md",
        required: &[(
            "use gliner_core::GlinerEngine;",
            "workspace README demonstrates the shared GLiNER surface",
        )],
        forbidden: RAW_GLINER_IMPORTS,
    },
    SurfaceCheck {
        relative_path: "cartridges/vigoros/triple_extractor.py",
        required: &[(
            "from example.gliner import",
            "VIGOROS extraction uses the shared Python GLiNER facade",
        )],
        forbidden: &[(
            "/api/extract",
            "VIGOROS extraction bypassed the shared GLiNER facade with a direct gateway route",
        )],
    },
];

fn inspect_surface(
    absolute_path: &Path,
    source: &str,
    required: &[(&str, &str)],
    forbidden: &[(&str, &str)],
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    for (needle, detail) in required {
        if let Some(line) = find_line(source, needle) {
            evidence.push(EvidenceItem {
                kind: "code".to_string(),
                path: absolute_path.display().to_string(),
                line: Some(line),
                detail: (*detail).to_string(),
            });
        } else {
            warnings.push(format!(
                "GLiNER shared-surface invariant missing in {}: {needle}",
                absolute_path.display()
            ));
        }
    }

    for (needle, detail) in forbidden {
        if let Some(line) = find_line(source, needle) {
            warnings.push(format!(
                "{detail} in {}:{} via `{needle}`",
                absolute_path.display(),
                line
            ));
        }
    }
}

pub fn doctor_gliner_shared_surface(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    for check in SURFACE_CHECKS {
        let absolute_path = root.join(check.relative_path);
        if let Some(source) = read_text(&absolute_path, &mut warnings) {
            inspect_surface(
                &absolute_path,
                &source,
                check.required,
                check.forbidden,
                &mut warnings,
                &mut evidence,
            );
        }
    }

    entities.push(json!({
        "doctor": "gliner-shared-surface",
        "checked_files": SURFACE_CHECKS
            .iter()
            .map(|check| check.relative_path)
            .collect::<Vec<_>>(),
        "warning_count": warnings.len(),
        "evidence_count": evidence.len(),
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_gliner_shared_surface"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "GLiNER shared engine surface looks intact across Rust, Python, gateway, extractor, and public docs".to_string()
        } else {
            format!(
                "GLiNER shared engine surface has {} warning(s) across runtime or documentation callsites",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.97 } else { 0.69 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "surface": "gliner-shared-surface",
            "checked_file_count": SURFACE_CHECKS.len(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::inspect_surface;

    #[test]
    fn inspect_surface_flags_missing_and_forbidden_needles() {
        let path = std::path::Path::new("/tmp/gliner.rs");
        let src = r#"
use gliner_core::ort_inference::GlinerInference;
pub struct GlinerModel;
"#;
        let mut warnings = Vec::new();
        let mut evidence = Vec::new();

        inspect_surface(
            path,
            src,
            &[("GlinerEngine", "shared engine is referenced")],
            &[(
                "use gliner_core::ort_inference::GlinerInference",
                "raw GlinerInference import resurfaced",
            )],
            &mut warnings,
            &mut evidence,
        );

        assert_eq!(evidence.len(), 0);
        assert_eq!(warnings.len(), 2);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("GLiNER shared-surface invariant missing"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("raw GlinerInference import resurfaced"))
        );
    }

    #[test]
    fn inspect_surface_emits_evidence_for_required_needles() {
        let path = std::path::Path::new("/tmp/gliner.rs");
        let src = "use gliner_core::GlinerEngine;\n";
        let mut warnings = Vec::new();
        let mut evidence = Vec::new();

        inspect_surface(
            path,
            src,
            &[(
                "use gliner_core::GlinerEngine;",
                "shared engine is referenced",
            )],
            &[],
            &mut warnings,
            &mut evidence,
        );

        assert!(warnings.is_empty());
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].line, Some(1));
    }
}
