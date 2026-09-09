use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct HealthAuditSentinelDoctor;

impl Doctor for HealthAuditSentinelDoctor {
    fn name(&self) -> &'static str {
        "health-audit-sentinel"
    }

    fn description(&self) -> &'static str {
        "Checks Sentinel health-audit ingest wiring: fixture corpus, TUSS reference, tenant-safe PDF scratch space, single ingest endpoint, OCR role separation, Flight probe, and regression coverage."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_sentinel(index, root)
    }
}

fn pdf_scratch_is_ephemeral(router_src: &str, text_extract_src: &str) -> bool {
    let ephemeral_marker = "tempfile.TemporaryDirectory(prefix=\"health-audit-pdf-\")";
    let durable_markers = [
        "_CONTRACT_UPLOADS_DIR / \".tmp\"",
        "contract_uploads_dir / \".tmp\"",
    ];

    router_src.contains(ephemeral_marker)
        && text_extract_src.contains(ephemeral_marker)
        && !durable_markers
            .iter()
            .any(|marker| router_src.contains(marker) || text_extract_src.contains(marker))
}

pub fn doctor_health_audit_sentinel(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let router_path = root.join("cartridges/health_audit/router.py");
    let sentinel_routes_path = root.join("cartridges/health_audit/routes/sentinel.py");
    let sentinel_runtime_path = root.join("cartridges/health_audit/services/sentinel_runtime.py");
    let sentinel_ingest_path = root.join("cartridges/health_audit/services/sentinel_ingest.py");
    let contracts_service_path = root.join("cartridges/health_audit/services/contracts.py");
    let text_extract_path = root.join("cartridges/health_audit/services/text_extract.py");
    let audit_service_path = root.join("cartridges/health_audit/services/audit.py");
    let config_path = root.join("cartridges/health_audit/config.py");
    let contracts_path = root.join("example-api/example/flight/contracts.py");
    let tests_path = root.join("example-api/example/tests/api/test_health_audit_cartridge.py");
    let api_dockerfile_path = root.join("example-api/Dockerfile");
    let health_compose_path = root.join("example-api/docker-compose.health-audit.yml");
    let health_profile_path = root.join("deploy/profiles/health_audit.env");
    let gateway_dockerfile_path = root.join("example-gateway/Dockerfile");
    let sentinel_target_path = root.join("deploy/targets/sentinel.toml");
    let deploy_common_path = root.join("deploy/lib/common.sh");
    let deploy_script_path = root.join("deploy/scripts/deploy.sh");
    let pull_restart_path = root.join("deploy/scripts/pull-and-restart.sh");
    let smoke_script_path = root.join("deploy/scripts/compose_smoke.sh");
    let tuss_db_path = root.join("example-api/data/tuss_reference.duckdb");
    let fixture_path = root.join("cartridges/health_audit/xml - reunião - 06.03.2026");
    let fixture_ascii_path = root.join("cartridges/health_audit/xml - reuniao - 06.03.2026");
    let raw_data_path = root.join("cartridges/health_audit/raw-data");

    let router_src = read_text(&router_path, &mut warnings);
    let sentinel_routes_src = read_text(&sentinel_routes_path, &mut warnings);
    let sentinel_runtime_src = read_text(&sentinel_runtime_path, &mut warnings);
    let sentinel_ingest_src = read_text(&sentinel_ingest_path, &mut warnings);
    let contracts_service_src = read_text(&contracts_service_path, &mut warnings);
    let text_extract_src = read_text(&text_extract_path, &mut warnings);
    let audit_service_src = read_text(&audit_service_path, &mut warnings);
    let config_src = read_text(&config_path, &mut warnings);
    let contracts_src = read_text(&contracts_path, &mut warnings);
    let tests_src = read_text(&tests_path, &mut warnings);
    let api_dockerfile_src = read_text(&api_dockerfile_path, &mut warnings);
    let health_compose_src = read_text(&health_compose_path, &mut warnings);
    let health_profile_src = read_text(&health_profile_path, &mut warnings);
    let gateway_dockerfile_src = read_text(&gateway_dockerfile_path, &mut warnings);
    let sentinel_target_src = read_text(&sentinel_target_path, &mut warnings);
    let deploy_common_src = read_text(&deploy_common_path, &mut warnings);
    let deploy_script_src = read_text(&deploy_script_path, &mut warnings);
    let pull_restart_src = read_text(&pull_restart_path, &mut warnings);
    let smoke_script_src = read_text(&smoke_script_path, &mut warnings);

    let fixture_exists = fixture_path.exists() || fixture_ascii_path.exists();
    if !fixture_exists {
        warnings.push(
            "missing Sentinel fixture corpus: expected `cartridges/health_audit/xml - reunião - 06.03.2026` or ASCII-normalized variant".to_string(),
        );
    } else {
        evidence.push(EvidenceItem {
            kind: "fixture".to_string(),
            path: if fixture_path.exists() {
                fixture_path.display().to_string()
            } else {
                fixture_ascii_path.display().to_string()
            },
            line: None,
            detail: "Sentinel fixture corpus present".to_string(),
        });
    }

    if !tuss_db_path.exists() {
        evidence.push(EvidenceItem {
            kind: "bootstrap_missing".to_string(),
            path: tuss_db_path.display().to_string(),
            line: None,
            detail:
                "gitignored processed TUSS reference database absent in clean checkout; validate with a bootstrapped Health Audit data volume"
                    .to_string(),
        });
    } else {
        evidence.push(EvidenceItem {
            kind: "data".to_string(),
            path: tuss_db_path.display().to_string(),
            line: None,
            detail: "processed TUSS reference database present".to_string(),
        });
    }

    if !raw_data_path.exists() {
        evidence.push(EvidenceItem {
            kind: "bootstrap_missing".to_string(),
            path: raw_data_path.display().to_string(),
            line: None,
            detail:
                "gitignored Sentinel raw-data corpus absent in clean checkout; deploy/runtime validation owns material data presence"
                    .to_string(),
        });
    } else {
        evidence.push(EvidenceItem {
            kind: "data".to_string(),
            path: raw_data_path.display().to_string(),
            line: None,
            detail: "Sentinel raw-data corpus present".to_string(),
        });
    }

    if router_src.is_some() {
        let sentinel_sources = [
            (router_path.as_path(), router_src.as_deref()),
            (
                sentinel_routes_path.as_path(),
                sentinel_routes_src.as_deref(),
            ),
            (
                sentinel_runtime_path.as_path(),
                sentinel_runtime_src.as_deref(),
            ),
            (
                sentinel_ingest_path.as_path(),
                sentinel_ingest_src.as_deref(),
            ),
            (
                contracts_service_path.as_path(),
                contracts_service_src.as_deref(),
            ),
            (audit_service_path.as_path(), audit_service_src.as_deref()),
        ];
        for (needle, detail) in [
            (
                "@router.post(\"/sentinel/ingest\"",
                "single Sentinel ingest endpoint registered",
            ),
            (
                "_run_xml_glosa_audit(",
                "XML glosa path routed through shared runtime-rule evaluator",
            ),
            (
                "_publish_sentinel_documents_to_arrow(",
                "Sentinel ingest publishes immutable Arrow document packs",
            ),
            (
                "flight_call_options(",
                "Flight/gRPC probe uses shared auth contract",
            ),
            (
                "_ingest_sentinel_raw_data_directory(",
                "Sentinel ingest can bulk-load raw-data corpus",
            ),
            (
                "_extract_contract_document_via_example_extractor_flight(",
                "contract ingestion can fall back to the canonical Example extractor Flight service when the API container has no local binary",
            ),
        ] {
            if let Some((path, line)) = sentinel_sources
                .iter()
                .filter_map(|(path, maybe_src)| {
                    maybe_src.and_then(|candidate| {
                        find_line(candidate, needle).map(|line| (*path, line))
                    })
                })
                .next()
            {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!("missing expected Sentinel wiring: {needle}"));
            }
        }
        for forbidden in [
            "example.core.vector.milvus",
            "_ingest_sentinel_documents_to_milvus",
            "_ingest_raw_data_runtime_documents_to_milvus",
        ] {
            if sentinel_sources
                .iter()
                .any(|(_, maybe_src)| maybe_src.is_some_and(|src| src.contains(forbidden)))
            {
                warnings.push(format!(
                    "forbidden Health Audit vector-store wiring remains active: {forbidden}"
                ));
            }
        }
    }

    if let Some(src) = config_src.as_deref() {
        for (needle, detail) in [
            (
                "EXAMPLE_EXTRACTOR_CONTRACT_BIN",
                "health_audit config accepts the canonical extractor binary env name",
            ),
            (
                "EXAMPLE_EXTRACTOR_TIMEOUT_SEC",
                "health_audit config accepts the canonical extractor timeout env name",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: config_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "health_audit config missing canonical extractor env bridge: {needle}"
                ));
            }
        }
    }

    let pdf_scratch_is_ephemeral = router_src
        .as_deref()
        .zip(text_extract_src.as_deref())
        .is_some_and(|(router, service)| pdf_scratch_is_ephemeral(router, service));
    if !pdf_scratch_is_ephemeral {
        warnings.push(
            "Health Audit PDF extraction writes scratch state below the durable uploads root; even an empty flat `.tmp` directory blocks tenant ownership attestation"
                .to_string(),
        );
    } else if let Some(src) = text_extract_src.as_deref()
        && let Some(line) = find_line(
            src,
            "tempfile.TemporaryDirectory(prefix=\"health-audit-pdf-\")",
        )
    {
        evidence.push(EvidenceItem {
            kind: "tenant_isolation".to_string(),
            path: text_extract_path.display().to_string(),
            line: Some(line),
            detail: "PDF parsing scratch files stay process-ephemeral and outside durable tenant uploads"
                .to_string(),
        });
    }

    let flight_headers_encode_bytes = contracts_src.as_deref().is_some_and(|src| {
        src.contains("def _to_flight_header_pair(")
            && src.contains("key.encode(\"utf-8\") if isinstance(key, str) else key")
            && src.contains("value.encode(\"utf-8\") if isinstance(value, str) else value")
    });
    if !flight_headers_encode_bytes {
        warnings.push(
            "shared Flight auth contract does not normalize header keys/values to bytes before constructing `pyarrow.flight.FlightCallOptions`, so OCR and other Flight probes can fail at runtime with `expected bytes, str found`"
                .to_string(),
        );
    } else if let Some(src) = contracts_src.as_deref() {
        for (needle, detail) in [
            (
                "def _to_flight_header_pair(",
                "shared Flight auth contract normalizes headers to PyArrow-safe bytes",
            ),
            (
                "headers.append(_to_flight_header_pair(\"authorization\", authorization))",
                "Flight authorization header uses the shared bytes-normalization path",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: contracts_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    if let Some(src) = tests_src.as_deref() {
        for (needle, detail) in [
            (
                "test_sentinel_ingest_runs_contract_audit_and_arrow_publication",
                "regression test covers Arrow-native Sentinel ingest",
            ),
            (
                "test_sentinel_ingest_reads_and_publishes_raw_data_directory",
                "regression test covers raw-data Arrow publication",
            ),
            (
                "test_xml_glosa_refuses_ungoverned_runtime_bundle_rules",
                "regression test proves ungoverned runtime bundles cannot promote glosas",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "test".to_string(),
                    path: tests_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "missing expected Sentinel regression coverage: {needle}"
                ));
            }
        }
    }

    let health_audit_runtime_target = api_dockerfile_src.as_deref().is_some_and(|src| {
        src.contains("FROM deps AS health-audit-source")
            && src.contains("FROM python:3.12-slim-trixie AS health-audit-runtime")
            && (src.contains("COPY cartridges/health_audit/ ./cartridges/health_audit/")
                || src.contains("COPY --link cartridges/health_audit/ ./cartridges/health_audit/"))
            && src.contains("! grep -R -E \"cartridges\\\\.gohosptwin|gohosptwin|CAPTA\" ./cartridges/health_audit")
            && !src.contains("COPY cartridges/ /out/cartridges/")
    });
    if !health_audit_runtime_target {
        warnings.push(
            "example-api Dockerfile is missing the tenant-scoped Health Audit runtime target; customer deploys must not ship the broad cartridge tree or source syncs"
                .to_string(),
        );
    } else if let Some(src) = api_dockerfile_src.as_deref()
        && let Some(line) = find_line(src, "FROM python:3.12-slim-trixie AS health-audit-runtime")
    {
        evidence.push(EvidenceItem {
            kind: "build".to_string(),
            path: api_dockerfile_path.display().to_string(),
            line: Some(line),
            detail: "Health Audit API image has a tenant-scoped runtime target".to_string(),
        });
    }

    let health_compose_is_standalone = health_compose_src.as_deref().is_some_and(|src| {
        src.contains("x-health-audit-env:")
            && src.contains("image: ${EXAMPLE_API_IMAGE:-jquant/example-health-audit-api:latest}")
            && src.contains("target: health-audit-runtime")
            && src.contains("health-audit-ollama:")
            && !src.contains("jai-pay:")
            && !src.contains("worker-sara:")
            && !src.contains("worker-liz:")
            && !src.contains("worker-pratique:")
            && !src.contains("worker-chatwoot:")
            && !src.contains("generativelanguage.googleapis.com")
            && !src.contains("api.openai.com")
    });
    if !health_compose_is_standalone {
        warnings.push(
            "Health Audit compose is not a standalone tenant file or still contains broad platform services/defaults"
                .to_string(),
        );
    } else if let Some(src) = health_compose_src.as_deref()
        && let Some(line) = find_line(src, "x-health-audit-env:")
    {
        evidence.push(EvidenceItem {
            kind: "deploy".to_string(),
            path: health_compose_path.display().to_string(),
            line: Some(line),
            detail: "Health Audit compose is standalone and tenant-scoped".to_string(),
        });
    }

    let api_worker_use_flight = health_profile_src.as_deref().is_some_and(|src| {
        src.contains("HEALTH_AUDIT_OCR_FLIGHT_URL=grpc://ocr-sidecar:9485")
            && src.contains("CONTRACT_EXTRACTOR_DISABLE_FLIGHT_OCR=0")
    }) && health_compose_src.as_deref().is_some_and(|src| {
        src.contains(
            "HEALTH_AUDIT_OCR_FLIGHT_URL: ${HEALTH_AUDIT_OCR_FLIGHT_URL:-grpc://ocr-sidecar:9485}",
        ) && src.contains(
            "CONTRACT_EXTRACTOR_DISABLE_FLIGHT_OCR: ${CONTRACT_EXTRACTOR_DISABLE_FLIGHT_OCR:-0}",
        )
    });
    let sidecar_breaks_flight_recursion = health_compose_src
        .as_deref()
        .is_some_and(|src| src.contains("      CONTRACT_EXTRACTOR_DISABLE_FLIGHT_OCR: \"1\""));
    let sidecar_runs_local_oar = health_compose_src.as_deref().is_some_and(|src| {
        src.contains("      EXAMPLE_EXTRACTOR_OAR_OCR_BIN: /app/oar-ocr-cli")
            && src.contains("      EXAMPLE_OAR_OCR_BIN: /app/oar-ocr-cli")
            && src.contains("      CONTRACT_EXTRACTOR_DISABLE_LOCAL_OAR_OCR: \"0\"")
            && src.contains("      CONTRACT_EXTRACTOR_DISABLE_OAR_OCR: \"0\"")
    });
    if !api_worker_use_flight || !sidecar_breaks_flight_recursion || !sidecar_runs_local_oar {
        warnings.push(
            "Health Audit scanned-contract OCR roles are incoherent: API/worker must use Flight while ocr-sidecar blocks recursive Flight and enables the bundled local OAR binary"
                .to_string(),
        );
    } else {
        if let Some(src) = health_profile_src.as_deref()
            && let Some(line) = find_line(src, "CONTRACT_EXTRACTOR_DISABLE_FLIGHT_OCR=0")
        {
            evidence.push(EvidenceItem {
                kind: "deploy".to_string(),
                path: health_profile_path.display().to_string(),
                line: Some(line),
                detail: "Health Audit API and worker delegate scanned-contract OCR over Flight"
                    .to_string(),
            });
        }
        if let Some(src) = health_compose_src.as_deref()
            && let Some(line) =
                find_line(src, "      EXAMPLE_EXTRACTOR_OAR_OCR_BIN: /app/oar-ocr-cli")
        {
            evidence.push(EvidenceItem {
                kind: "deploy".to_string(),
                path: health_compose_path.display().to_string(),
                line: Some(line),
                detail: "Health Audit OCR sidecar terminates Flight recursion and runs bundled local OAR"
                    .to_string(),
            });
        }
    }

    let gateway_copies_extractor_binary = gateway_dockerfile_src.as_deref().is_some_and(|src| {
        src.contains("COPY --from=extractor-builder /tmp/contract_extract_json /app/contract_extract_json")
            || src.contains("COPY --link --from=extractor-builder /tmp/contract_extract_json /app/contract_extract_json")
            || src.contains(
                "COPY --from=extractor-builder /app/example-extractor/src-tauri/target/release/contract_extract_json /app/contract_extract_json",
            )
    });
    let gateway_copies_platform_workspace = gateway_dockerfile_src.as_deref().is_some_and(|src| {
        src.contains("COPY --link example-platform/ /example-platform/")
            || (src.contains("COPY example-platform/Cargo.toml /example-platform/Cargo.toml")
                && src.contains(
                    "COPY example-platform/flight-contracts /example-platform/flight-contracts",
                )
                && src
                    .contains("COPY example-platform/example-core /example-platform/example-core"))
    });
    if !gateway_copies_extractor_binary {
        warnings.push(
            "example-gateway Dockerfile does not copy `contract_extract_json` into the runtime image, so `ocr-sidecar` cannot satisfy the canonical extractor contract"
                .to_string(),
        );
    }
    if !gateway_copies_platform_workspace {
        warnings.push(
            "example-gateway Dockerfile does not copy the `example-platform` workspace bits needed by the gateway `flight-contracts` path dependency, so a fresh gateway image build can fail even when cached images keep prod healthy"
                .to_string(),
        );
    }
    if let Some(src) = gateway_dockerfile_src.as_deref() {
        for (needle, detail) in [
            (
                "COPY --from=extractor-builder /tmp/contract_extract_json /app/contract_extract_json",
                "gateway runtime image carries the canonical contract extractor binary",
            ),
            (
                "COPY --from=extractor-builder /app/example-extractor/src-tauri/target/release/contract_extract_json /app/contract_extract_json",
                "gateway runtime image carries the canonical contract extractor binary",
            ),
            (
                "COPY example-platform/flight-contracts /example-platform/flight-contracts",
                "gateway build context includes the flight-contracts path dependency workspace",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "build".to_string(),
                    path: gateway_dockerfile_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    let sentinel_uses_targeted_smoke = sentinel_target_src
        .as_deref()
        .is_some_and(|src| src.contains("smoke_suite = \"./scripts/compose_smoke.sh sentinel\""));
    let smoke_only_checks_health_urls = smoke_script_src.as_deref().is_some_and(|src| {
        src.contains("done < <(target_manifest_values \"$TARGET_NAME\" \"health_checks\")")
            && !src.contains("/v2/auth/login")
            && !src.contains("/v2/auth/register")
            && !src.contains("/v2/health-audit/sentinel/ingest")
            && !src.contains("/v2/health-audit/audit/xml-glosa")
    });
    if !sentinel_uses_targeted_smoke || smoke_only_checks_health_urls {
        warnings.push(
            "Sentinel deploy smoke contract only exercises anonymous health checks; it does not authenticate and verify `/v2/health-audit/sentinel/ingest` or runtime XML glosa routes"
                .to_string(),
        );
    }
    if let Some(src) = sentinel_target_src.as_deref()
        && let Some(line) = find_line(src, "smoke_suite = \"./scripts/compose_smoke.sh sentinel\"")
    {
        evidence.push(EvidenceItem {
            kind: "deploy".to_string(),
            path: sentinel_target_path.display().to_string(),
            line: Some(line),
            detail: "Sentinel target uses its own authenticated smoke suite".to_string(),
        });
    }
    if let Some(src) = smoke_script_src.as_deref()
        && let Some(line) = find_line(
            src,
            "done < <(target_manifest_values \"$TARGET_NAME\" \"health_checks\")",
        )
    {
        evidence.push(EvidenceItem {
                kind: "deploy".to_string(),
                path: smoke_script_path.display().to_string(),
                line: Some(line),
                detail:
                    "generic compose smoke only curls manifest health_checks and does not run authenticated route assertions"
                        .to_string(),
            });
    }

    let deploy_syncs_health_audit_source = deploy_script_src
        .as_deref()
        .is_some_and(|src| src.contains("sync_to_vm \"$ROOT/cartridges/health_audit\""));
    let pull_restart_syncs_health_audit_source = pull_restart_src
        .as_deref()
        .is_some_and(|src| src.contains("sync_to_vm \"$ROOT/cartridges/health_audit\""));
    if deploy_syncs_health_audit_source || pull_restart_syncs_health_audit_source {
        warnings.push(
            "Health Audit deploy scripts sync cartridge source to the VM; customer deploys must ship published images and minimal runtime config only"
                .to_string(),
        );
    }
    let deploy_syncs_only_env = deploy_script_src.as_deref().is_some_and(|src| {
        src.contains("sync_to_vm \"$DEFAULTS_ENV\" \"/opt/example/\"")
            && src.contains("sync_to_vm \"$env_path\" \"/opt/example/\"")
            && !src.contains("cartridges/health_audit")
            && !src.contains("runtime-cartridges/health_audit")
    });
    let pull_restart_syncs_only_env = pull_restart_src.as_deref().is_some_and(|src| {
        src.contains("sync_to_vm \"$DEFAULTS_ENV\" \"/opt/example/\"")
            && src.contains("sync_to_vm \"$ENV_PATH\" \"/opt/example/\"")
            && !src.contains("cartridges/health_audit")
            && !src.contains("runtime-cartridges/health_audit")
    });
    if let Some(src) = deploy_script_src.as_deref() {
        for needle in [
            "sync_to_vm \"$DEFAULTS_ENV\" \"/opt/example/\"",
            "sync_to_vm \"$env_path\" \"/opt/example/\"",
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "deploy".to_string(),
                    path: deploy_script_path.display().to_string(),
                    line: Some(line),
                    detail: "backend deploy syncs env files to the VM".to_string(),
                });
            }
        }
    }
    if deploy_syncs_only_env && pull_restart_syncs_only_env {
        evidence.push(EvidenceItem {
            kind: "deploy".to_string(),
            path: deploy_script_path.display().to_string(),
            line: find_line(
                deploy_script_src.as_deref().unwrap_or_default(),
                "sync_to_vm \"$DEFAULTS_ENV\" \"/opt/example/\"",
            ),
            detail: "backend deploy avoids syncing Health Audit cartridge source to customer VMs"
                .to_string(),
        });
    }
    let raw_data_sync_is_opt_in = deploy_common_src
        .as_deref()
        .is_some_and(|src| src.contains("HEALTH_AUDIT_SYNC_RAW_DATA_ON_DEPLOY"));
    if !raw_data_sync_is_opt_in {
        warnings.push(
            "Health Audit raw-data sync is not explicitly opt-in; customer packages should not move local fixture data by default"
                .to_string(),
        );
    } else if let Some(src) = deploy_common_src.as_deref()
        && let Some(line) = find_line(src, "HEALTH_AUDIT_SYNC_RAW_DATA_ON_DEPLOY")
    {
        evidence.push(EvidenceItem {
            kind: "deploy".to_string(),
            path: deploy_common_path.display().to_string(),
            line: Some(line),
            detail: "Health Audit raw-data transfer is explicit opt-in".to_string(),
        });
    }
    if let Some(src) = pull_restart_src.as_deref() {
        for needle in [
            "sync_to_vm \"$DEFAULTS_ENV\" \"/opt/example/\"",
            "sync_to_vm \"$ENV_PATH\" \"/opt/example/\"",
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "deploy".to_string(),
                    path: pull_restart_path.display().to_string(),
                    line: Some(line),
                    detail: "pull-and-restart syncs env files to the VM before restarting"
                        .to_string(),
                });
            }
        }
    }

    let flight_probe_wired = [
        router_src.as_deref(),
        sentinel_routes_src.as_deref(),
        sentinel_runtime_src.as_deref(),
        sentinel_ingest_src.as_deref(),
    ]
    .iter()
    .any(|maybe_src| maybe_src.is_some_and(|src| src.contains("flight_call_options(")));

    let contract_extractor_flight_fallback = [
        router_src.as_deref(),
        sentinel_routes_src.as_deref(),
        sentinel_runtime_src.as_deref(),
        sentinel_ingest_src.as_deref(),
    ]
    .iter()
    .any(|maybe_src| {
        maybe_src.is_some_and(|src| {
            src.contains("_extract_contract_document_via_example_extractor_flight(")
        })
    });

    entities.push(json!({
        "doctor": "health-audit-sentinel",
        "fixture_present": fixture_exists,
        "tuss_reference_present": tuss_db_path.exists(),
        "raw_data_present": raw_data_path.exists(),
        "endpoint_wired": router_src
            .as_deref()
            .is_some_and(|src| src.contains("@router.post(\"/sentinel/ingest\""))
            || sentinel_routes_src
                .as_deref()
                .is_some_and(|src| src.contains("@router.post(\"/sentinel/ingest\"")),
        "flight_probe_wired": flight_probe_wired,
        "contract_extractor_flight_fallback": contract_extractor_flight_fallback,
        "flight_headers_encode_bytes": flight_headers_encode_bytes,
        "pdf_scratch_is_ephemeral": pdf_scratch_is_ephemeral,
        "config_bridges_canonical_extractor_env": config_src
            .as_deref()
            .is_some_and(|src| src.contains("EXAMPLE_EXTRACTOR_CONTRACT_BIN")),
        "health_audit_runtime_target": health_audit_runtime_target,
        "health_compose_is_standalone": health_compose_is_standalone,
        "api_worker_use_ocr_flight": api_worker_use_flight,
        "sidecar_breaks_ocr_flight_recursion": sidecar_breaks_flight_recursion,
        "sidecar_runs_local_oar": sidecar_runs_local_oar,
        "gateway_image_copies_extractor_binary": gateway_copies_extractor_binary,
        "gateway_image_copies_platform_workspace": gateway_copies_platform_workspace,
        "regression_tests_present": tests_src.as_deref().is_some_and(|src| {
            src.contains("test_sentinel_ingest_runs_contract_audit_and_arrow_publication")
                && src.contains("test_sentinel_ingest_reads_and_publishes_raw_data_directory")
                && src.contains("test_xml_glosa_refuses_ungoverned_runtime_bundle_rules")
        }),
        "deploy_smoke_covers_ingest": sentinel_uses_targeted_smoke && !smoke_only_checks_health_urls,
        "deploy_avoids_runtime_source_sync": !deploy_syncs_health_audit_source
            && !pull_restart_syncs_health_audit_source,
        "raw_data_sync_is_opt_in": raw_data_sync_is_opt_in,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_health_audit_sentinel"),
        kind: "doctor".to_string(),
        summary: format!(
            "health-audit Sentinel readiness checked: {} warnings, {} evidence items",
            warnings.len(),
            evidence.len()
        ),
        confidence: if warnings.is_empty() { 0.97 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::pdf_scratch_is_ephemeral;

    #[test]
    fn accepts_process_ephemeral_pdf_scratch_space() {
        let source = r#"
with tempfile.TemporaryDirectory(prefix="health-audit-pdf-") as temp_dir_name:
    pdf_path = Path(temp_dir_name) / "contract.pdf"
"#;

        assert!(pdf_scratch_is_ephemeral(source, source));
    }

    #[test]
    fn rejects_pdf_scratch_below_durable_uploads() {
        let router = r#"temp_dir = _CONTRACT_UPLOADS_DIR / ".tmp""#;
        let service = r#"temp_dir = contract_uploads_dir / ".tmp""#;

        assert!(!pdf_scratch_is_ephemeral(router, service));
    }
}
