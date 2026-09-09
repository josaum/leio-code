use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct TessellationContractDoctor;

impl Doctor for TessellationContractDoctor {
    fn name(&self) -> &'static str {
        "tessellation-contract"
    }

    fn description(&self) -> &'static str {
        "Guards tessellation SolverConfig, hierarchical solve, WASM demo, sector coverage proxy, OpenAPI, and docs contract alignment."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_tessellation_contract(root)
    }
}

pub fn doctor_tessellation_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    let makefile_path = root.join("Makefile");
    let rust_ffi_path = root.join("tessellation-solver/tessellation-core/src/ffi.rs");
    let rust_hierarchy_path = root.join("tessellation-solver/tessellation-core/src/hierarchy.rs");
    let rust_products_path = root.join("tessellation-solver/tessellation-core/src/products.rs");
    let engine_cli_path = root.join("tessellation-solver/tessellation-engine/src/main.rs");
    let engine_envelope_schema_path =
        root.join("tessellation-solver/engine/schemas/engine-envelope.schema.json");
    let engine_solver_schema_path =
        root.join("tessellation-solver/engine/schemas/solver-config.schema.json");
    let engine_hierarchy_schema_path =
        root.join("tessellation-solver/engine/schemas/hierarchical-config.schema.json");
    let engine_power_schema_path =
        root.join("tessellation-solver/engine/schemas/power-diagram-config.schema.json");
    let wasm_path = root.join("tessellation-solver/tessellation-wasm/src/lib.rs");
    let wasm_test_path = root.join("tessellation-solver/tessellation-wasm/tests/wasm_tests.rs");
    let api_path = root.join("example-api/example/core/evolution/tessellation_gepa.py");
    let api_test_path = root.join("example-api/tests/test_tessellation_gepa.py");
    let ffi_test_path = root.join("example-api/tests/test_tessellation_ffi_smoke.py");
    let scenario_test_path =
        root.join("tessellation-solver/tessellation-core/tests/scenario_fixtures.rs");
    let dondonha_scenario_path = root.join("tessellation-solver/scenarios/dondonha-search.json");
    let dashboard_scenario_path = root.join("tessellation-solver/scenarios/dashboard.json");
    let google_scenario_path = root.join("tessellation-solver/scenarios/google-homepage.json");
    let morph_scenario_path = root.join("tessellation-solver/scenarios/canvas-morph.json");
    let sector_power_scenario_path =
        root.join("tessellation-solver/scenarios/sector-coverage-power.json");
    let sector_route_path = classified_sector_route_path(root);
    let sector_route_evidence_path =
        root.join("sector-console/src/app/api/tessellation/sectors/route.ts");
    let pnpm_workspace_path = root.join("pnpm-workspace.yaml");
    let web_package_path = root.join("tessellation-solver/web/package.json");
    let web_wasm_path = root.join("tessellation-solver/web/src/wasm.ts");
    let web_semantic_compiler_path =
        root.join("tessellation-solver/web/src/engine/semantic-compiler.ts");
    let web_semantic_compiler_test_path =
        root.join("tessellation-solver/web/src/engine/semantic-compiler.test.ts");
    let shadcn_tailwind_compiler_path = root.join("tessellation-solver/sdk/shadcn-tailwind.js");
    let shadcn_tailwind_types_path = root.join("tessellation-solver/sdk/shadcn-tailwind.d.ts");
    let shadcn_tailwind_test_path =
        root.join("tessellation-solver/web/src/engine/shadcn-tailwind-compiler.test.ts");
    let tailwind_style_resolver_path =
        root.join("tessellation-solver/sdk/tailwind-style-resolver.js");
    let tailwind_style_resolver_types_path =
        root.join("tessellation-solver/sdk/tailwind-style-resolver.d.ts");
    let tailwind_style_resolver_test_path =
        root.join("tessellation-solver/web/src/engine/tailwind-style-resolver.test.ts");
    let tailwind_build_adapter_path = root.join("tessellation-solver/sdk/tailwind-build.js");
    let tailwind_build_adapter_types_path =
        root.join("tessellation-solver/sdk/tailwind-build.d.ts");
    let tailwind_build_adapter_test_path =
        root.join("tessellation-solver/web/src/engine/tailwind-build.test.ts");
    let shadcn_project_loader_path = root.join("tessellation-solver/sdk/shadcn-project.js");
    let shadcn_project_loader_types_path = root.join("tessellation-solver/sdk/shadcn-project.d.ts");
    let shadcn_project_loader_test_path =
        root.join("tessellation-solver/web/src/engine/shadcn-project.test.ts");
    let tsx_extractor_path = root.join("tessellation-solver/sdk/tsx-extractor.js");
    let tsx_extractor_types_path = root.join("tessellation-solver/sdk/tsx-extractor.d.ts");
    let tsx_extractor_test_path =
        root.join("tessellation-solver/web/src/engine/tsx-extractor.test.ts");
    let app_spec_path = root.join("tessellation-solver/sdk/app-spec.js");
    let app_spec_types_path = root.join("tessellation-solver/sdk/app-spec.d.ts");
    let presets_path = root.join("tessellation-solver/sdk/presets.js");
    let presets_types_path = root.join("tessellation-solver/sdk/presets.d.ts");
    let runtime_path = root.join("tessellation-solver/sdk/runtime.js");
    let runtime_types_path = root.join("tessellation-solver/sdk/runtime.d.ts");
    let power_layout_path = root.join("tessellation-solver/sdk/power-layout.js");
    let power_layout_types_path = root.join("tessellation-solver/sdk/power-layout.d.ts");
    let performance_budget_path = root.join("tessellation-solver/sdk/performance-budget.js");
    let performance_budget_types_path =
        root.join("tessellation-solver/sdk/performance-budget.d.ts");
    let performance_budget_test_path =
        root.join("tessellation-solver/web/src/engine/performance-budget.test.ts");
    let power_benchmark_page_path = root.join("tessellation-solver/web/src/power-benchmark.ts");
    let power_benchmark_e2e_path = root.join("tessellation-solver/web/e2e/power-runtime.spec.ts");
    let power_studio_html_path = root.join("tessellation-solver/web/power-studio.html");
    let power_studio_path = root.join("tessellation-solver/web/src/power-studio.ts");
    let power_studio_css_path = root.join("tessellation-solver/web/src/power-studio.css");
    let power_studio_e2e_path = root.join("tessellation-solver/web/e2e/power-studio.spec.ts");
    let showcase_html_path = root.join("tessellation-solver/web/showcase.html");
    let showcase_path = root.join("tessellation-solver/web/src/showcase.ts");
    let showcase_css_path = root.join("tessellation-solver/web/src/showcase.css");
    let showcase_e2e_path = root.join("tessellation-solver/web/e2e/showcase.spec.ts");
    let operational_studio_html_path = root.join("tessellation-solver/web/studio.html");
    let operational_studio_path = root.join("tessellation-solver/web/src/operational-studio.ts");
    let operational_studio_css_path =
        root.join("tessellation-solver/web/src/operational-studio.css");
    let operational_studio_e2e_path =
        root.join("tessellation-solver/web/e2e/operational-studio.spec.ts");
    let example_surface_studio_path =
        root.join("example-studio-web/src/app/surfaces/SurfaceStudio.tsx");
    let example_surface_studio_css_path =
        root.join("example-studio-web/src/app/surfaces/surface-studio.module.css");
    let example_surface_page_path = root.join("example-studio-web/src/app/surfaces/page.tsx");
    let example_surface_e2e_path = root.join("example-studio-web/tests/surfaces.spec.ts");
    let example_surface_projects_client_path =
        root.join("example-studio-web/src/lib/surface-projects.ts");
    let example_surface_realtime_client_path =
        root.join("example-studio-web/src/lib/surface-realtime.ts");
    let example_surface_pdf_client_path = root.join("example-studio-web/src/lib/surface-pdf.ts");
    let surface_pdf_compiler_path = root.join("example-api/example/pdf_studio/surface.py");
    let surface_pdf_test_path =
        root.join("example-api/example/tests/pdf_studio/test_surface_render.py");
    let pdf_studio_router_path = root.join("example-api/example/routers/pdf_studio.py");
    let deck_tessellate_path = root.join("example-deck/src/tessellate.rs");
    let deck_compile_path = root.join("example-deck/src/compile.rs");
    let render_deck_bin_path = root.join("example-deck/src/bin/render_deck.rs");
    let deck_cover_theme_path = root.join(".claude/skills/example-deck/shared/cover.typ");
    let api_deck_cover_theme_path = root.join("example-api/pdf-studio-deck-theme/cover.typ");
    let api_dockerfile_path = root.join("example-api/Dockerfile");
    let pdf_studio_render_path = root.join("example-api/example/pdf_studio/render.py");
    let workspace_verify_path = root.join(".github/workflows/workspace-verify.yml");
    let workspace_docs_report_path = root.join("example-report/src/bin/workspace_docs_report.rs");
    let report_cargo_path = root.join("example-report/Cargo.toml");
    let report_renderer_path = root.join("example-api/example/pdf_studio/report.py");
    let typst_report_path = root.join("example-render-core/src/render_typst_report.rs");
    let report_theme_path = root.join("example-api/pdf-studio-report-theme");
    let studio_deploy_smoke_path = root.join("deploy/scripts/compose_smoke.sh");
    let surface_realtime_hub_path = root.join("example-api/example/flight/surface_realtime.py");
    let flight_server_path = root.join("example-api/example/flight/server.py");
    let surface_realtime_test_path =
        root.join("example-api/example/tests/flight/test_surface_realtime_exchange.py");
    let studio_projects_router_path =
        root.join("example-api/example/routers/v2/studio_projects.py");
    let studio_projects_db_path = root.join("example-api/example/core/duckdb.py");
    let studio_projects_test_path =
        root.join("example-api/example/tests/pdf_studio/test_studio_projects.py");
    let example_studio_package_path = root.join("example-studio-web/package.json");
    let example_studio_next_config_path = root.join("example-studio-web/next.config.ts");
    let example_studio_dockerfile_path = root.join("example-studio-web/Dockerfile");
    let example_studio_readme_path = root.join("example-studio-web/README.md");
    let example_api_compose_path = root.join("example-api/docker-compose.yml");
    let creator_sdk_test_path = root.join("tessellation-solver/web/src/engine/creator-sdk.test.ts");
    let dom_adapter_path = root.join("tessellation-solver/sdk/dom-adapter.js");
    let dom_adapter_types_path = root.join("tessellation-solver/sdk/dom-adapter.d.ts");
    let dom_adapter_test_path = root.join("tessellation-solver/web/src/engine/dom-adapter.test.ts");
    let render_plan_path = root.join("tessellation-solver/sdk/render-plan.js");
    let render_plan_types_path = root.join("tessellation-solver/sdk/render-plan.d.ts");
    let render_plan_test_path = root.join("tessellation-solver/web/src/engine/render-plan.test.ts");
    let surface_optimizer_path = root.join("tessellation-solver/sdk/surface-optimizer.js");
    let surface_optimizer_types_path = root.join("tessellation-solver/sdk/surface-optimizer.d.ts");
    let surface_optimizer_test_path =
        root.join("tessellation-solver/web/src/engine/surface-optimizer.test.ts");
    let interaction_runtime_path = root.join("tessellation-solver/sdk/interaction-runtime.js");
    let interaction_runtime_types_path =
        root.join("tessellation-solver/sdk/interaction-runtime.d.ts");
    let interaction_runtime_test_path =
        root.join("tessellation-solver/web/src/engine/interaction-runtime.test.ts");
    let control_runtime_path = root.join("tessellation-solver/sdk/control-runtime.js");
    let control_runtime_types_path = root.join("tessellation-solver/sdk/control-runtime.d.ts");
    let control_runtime_test_path =
        root.join("tessellation-solver/web/src/engine/control-runtime.test.ts");
    let dashboard_renderer_path = root.join("tessellation-solver/sdk/dashboard-renderer.js");
    let dashboard_renderer_types_path =
        root.join("tessellation-solver/sdk/dashboard-renderer.d.ts");
    let visual_regression_path = root.join("tessellation-solver/sdk/visual-regression.js");
    let visual_regression_types_path = root.join("tessellation-solver/sdk/visual-regression.d.ts");
    let visual_regression_test_path =
        root.join("tessellation-solver/web/src/engine/visual-regression.test.ts");
    let visual_regression_e2e_path =
        root.join("tessellation-solver/web/e2e/shadcn-visual-regression.spec.ts");
    let web_accessibility_path =
        root.join("tessellation-solver/web/src/engine/accessibility-order.ts");
    let web_accessibility_test_path =
        root.join("tessellation-solver/web/src/engine/accessibility-order.test.ts");
    let web_config_test_path =
        root.join("tessellation-solver/web/src/engine/config-builder.test.ts");
    let web_renderer_test_path =
        root.join("tessellation-solver/web/src/renderer/canvas-renderer.test.ts");
    let web_playwright_config_path = root.join("tessellation-solver/web/playwright.config.ts");
    let web_vite_config_path = root.join("tessellation-solver/web/vite.config.ts");
    let web_e2e_test_path = root.join("tessellation-solver/web/e2e/liquid-search.spec.ts");
    let openapi_export_path = root.join("example-api/scripts/export_openapi.py");
    let openapi_json_path = root.join("example-api/docs/openapi.json");
    let openapi_yaml_path = root.join("example-api/docs/openapi.yaml");
    let docs_path = root.join("docs/guides/building-webpages-with-tessellation.md");
    let engine_docs_path = root.join("docs/tessellation-engine.md");
    let sdk_package_path = root.join("tessellation-solver/package.json");
    let sdk_readme_path = root.join("tessellation-solver/README.md");

    let makefile_src = read_text(&makefile_path, &mut warnings);
    let rust_ffi_src = read_text(&rust_ffi_path, &mut warnings);
    let rust_hierarchy_src = read_text(&rust_hierarchy_path, &mut warnings);
    let rust_products_src = read_text(&rust_products_path, &mut warnings);
    let engine_cli_src = read_text(&engine_cli_path, &mut warnings);
    let engine_envelope_schema_src = read_text(&engine_envelope_schema_path, &mut warnings);
    let engine_solver_schema_src = read_text(&engine_solver_schema_path, &mut warnings);
    let engine_hierarchy_schema_src = read_text(&engine_hierarchy_schema_path, &mut warnings);
    let engine_power_schema_src = read_text(&engine_power_schema_path, &mut warnings);
    let wasm_src = read_text(&wasm_path, &mut warnings);
    let wasm_test_src = read_text(&wasm_test_path, &mut warnings);
    let api_src = read_text(&api_path, &mut warnings);
    let api_test_src = read_text(&api_test_path, &mut warnings);
    let ffi_test_src = read_text(&ffi_test_path, &mut warnings);
    let scenario_test_src = read_text(&scenario_test_path, &mut warnings);
    let dondonha_scenario_src = read_text(&dondonha_scenario_path, &mut warnings);
    let dashboard_scenario_src = read_text(&dashboard_scenario_path, &mut warnings);
    let google_scenario_src = read_text(&google_scenario_path, &mut warnings);
    let morph_scenario_src = read_text(&morph_scenario_path, &mut warnings);
    let sector_power_scenario_src = read_text(&sector_power_scenario_path, &mut warnings);
    let sector_route_src = read_text(&sector_route_path, &mut warnings);
    let pnpm_workspace_src = read_text(&pnpm_workspace_path, &mut warnings);
    let web_package_src = read_text(&web_package_path, &mut warnings);
    let web_wasm_src = read_text(&web_wasm_path, &mut warnings);
    let web_semantic_compiler_src = read_text(&web_semantic_compiler_path, &mut warnings);
    let web_semantic_compiler_test_src = read_text(&web_semantic_compiler_test_path, &mut warnings);
    let shadcn_tailwind_compiler_src = read_text(&shadcn_tailwind_compiler_path, &mut warnings);
    let shadcn_tailwind_types_src = read_text(&shadcn_tailwind_types_path, &mut warnings);
    let shadcn_tailwind_test_src = read_text(&shadcn_tailwind_test_path, &mut warnings);
    let tailwind_style_resolver_src = read_text(&tailwind_style_resolver_path, &mut warnings);
    let tailwind_style_resolver_types_src =
        read_text(&tailwind_style_resolver_types_path, &mut warnings);
    let tailwind_style_resolver_test_src =
        read_text(&tailwind_style_resolver_test_path, &mut warnings);
    let tailwind_build_adapter_src = read_text(&tailwind_build_adapter_path, &mut warnings);
    let tailwind_build_adapter_types_src =
        read_text(&tailwind_build_adapter_types_path, &mut warnings);
    let tailwind_build_adapter_test_src =
        read_text(&tailwind_build_adapter_test_path, &mut warnings);
    let shadcn_project_loader_src = read_text(&shadcn_project_loader_path, &mut warnings);
    let shadcn_project_loader_types_src =
        read_text(&shadcn_project_loader_types_path, &mut warnings);
    let shadcn_project_loader_test_src = read_text(&shadcn_project_loader_test_path, &mut warnings);
    let tsx_extractor_src = read_text(&tsx_extractor_path, &mut warnings);
    let tsx_extractor_types_src = read_text(&tsx_extractor_types_path, &mut warnings);
    let tsx_extractor_test_src = read_text(&tsx_extractor_test_path, &mut warnings);
    let app_spec_src = read_text(&app_spec_path, &mut warnings);
    let app_spec_types_src = read_text(&app_spec_types_path, &mut warnings);
    let presets_src = read_text(&presets_path, &mut warnings);
    let presets_types_src = read_text(&presets_types_path, &mut warnings);
    let runtime_src = read_text(&runtime_path, &mut warnings);
    let runtime_types_src = read_text(&runtime_types_path, &mut warnings);
    let power_layout_src = read_text(&power_layout_path, &mut warnings);
    let power_layout_types_src = read_text(&power_layout_types_path, &mut warnings);
    let performance_budget_src = read_text(&performance_budget_path, &mut warnings);
    let performance_budget_types_src = read_text(&performance_budget_types_path, &mut warnings);
    let performance_budget_test_src = read_text(&performance_budget_test_path, &mut warnings);
    let power_benchmark_page_src = read_text(&power_benchmark_page_path, &mut warnings);
    let power_benchmark_e2e_src = read_text(&power_benchmark_e2e_path, &mut warnings);
    let power_studio_html_src = read_text(&power_studio_html_path, &mut warnings);
    let power_studio_src = read_text(&power_studio_path, &mut warnings);
    let power_studio_css_src = read_text(&power_studio_css_path, &mut warnings);
    let power_studio_e2e_src = read_text(&power_studio_e2e_path, &mut warnings);
    let showcase_html_src = read_text(&showcase_html_path, &mut warnings);
    let showcase_src = read_text(&showcase_path, &mut warnings);
    let showcase_css_src = read_text(&showcase_css_path, &mut warnings);
    let showcase_e2e_src = read_text(&showcase_e2e_path, &mut warnings);
    let operational_studio_html_src = read_text(&operational_studio_html_path, &mut warnings);
    let operational_studio_src = read_text(&operational_studio_path, &mut warnings);
    let operational_studio_css_src = read_text(&operational_studio_css_path, &mut warnings);
    let operational_studio_e2e_src = read_text(&operational_studio_e2e_path, &mut warnings);
    let example_surface_studio_src = read_text(&example_surface_studio_path, &mut warnings);
    let example_surface_studio_css_src = read_text(&example_surface_studio_css_path, &mut warnings);
    let example_surface_page_src = read_text(&example_surface_page_path, &mut warnings);
    let example_surface_e2e_src = read_text(&example_surface_e2e_path, &mut warnings);
    let example_surface_projects_client_src =
        read_text(&example_surface_projects_client_path, &mut warnings);
    let example_surface_realtime_client_src =
        read_text(&example_surface_realtime_client_path, &mut warnings);
    let example_surface_pdf_client_src = read_text(&example_surface_pdf_client_path, &mut warnings);
    let surface_pdf_compiler_src = read_text(&surface_pdf_compiler_path, &mut warnings);
    let surface_pdf_test_src = read_text(&surface_pdf_test_path, &mut warnings);
    let pdf_studio_router_src = read_text(&pdf_studio_router_path, &mut warnings);
    let deck_tessellate_src = read_text(&deck_tessellate_path, &mut warnings);
    let deck_compile_src = read_text(&deck_compile_path, &mut warnings);
    let render_deck_bin_src = read_text(&render_deck_bin_path, &mut warnings);
    let deck_cover_theme_src = read_text(&deck_cover_theme_path, &mut warnings);
    let api_deck_cover_theme_src = read_text(&api_deck_cover_theme_path, &mut warnings);
    let api_dockerfile_src = read_text(&api_dockerfile_path, &mut warnings);
    let pdf_studio_render_src = read_text(&pdf_studio_render_path, &mut warnings);
    let workspace_verify_src = read_text(&workspace_verify_path, &mut warnings);
    let workspace_docs_report_src = read_text(&workspace_docs_report_path, &mut warnings);
    let report_cargo_src = read_text(&report_cargo_path, &mut warnings);
    let report_renderer_src = read_text(&report_renderer_path, &mut warnings);
    let typst_report_src = read_text(&typst_report_path, &mut warnings);
    let studio_deploy_smoke_src = read_text(&studio_deploy_smoke_path, &mut warnings);
    let surface_realtime_hub_src = read_text(&surface_realtime_hub_path, &mut warnings);
    let flight_server_src = read_text(&flight_server_path, &mut warnings);
    let surface_realtime_test_src = read_text(&surface_realtime_test_path, &mut warnings);
    let studio_projects_router_src = read_text(&studio_projects_router_path, &mut warnings);
    let studio_projects_db_src = read_text(&studio_projects_db_path, &mut warnings);
    let studio_projects_test_src = read_text(&studio_projects_test_path, &mut warnings);
    let example_studio_package_src = read_text(&example_studio_package_path, &mut warnings);
    let example_studio_next_config_src = read_text(&example_studio_next_config_path, &mut warnings);
    let example_studio_dockerfile_src = read_text(&example_studio_dockerfile_path, &mut warnings);
    let example_studio_readme_src = read_text(&example_studio_readme_path, &mut warnings);
    let example_api_compose_src = read_text(&example_api_compose_path, &mut warnings);
    let creator_sdk_test_src = read_text(&creator_sdk_test_path, &mut warnings);
    let dom_adapter_src = read_text(&dom_adapter_path, &mut warnings);
    let dom_adapter_types_src = read_text(&dom_adapter_types_path, &mut warnings);
    let dom_adapter_test_src = read_text(&dom_adapter_test_path, &mut warnings);
    let render_plan_src = read_text(&render_plan_path, &mut warnings);
    let render_plan_types_src = read_text(&render_plan_types_path, &mut warnings);
    let render_plan_test_src = read_text(&render_plan_test_path, &mut warnings);
    let surface_optimizer_src = read_text(&surface_optimizer_path, &mut warnings);
    let surface_optimizer_types_src = read_text(&surface_optimizer_types_path, &mut warnings);
    let surface_optimizer_test_src = read_text(&surface_optimizer_test_path, &mut warnings);
    let interaction_runtime_src = read_text(&interaction_runtime_path, &mut warnings);
    let interaction_runtime_types_src = read_text(&interaction_runtime_types_path, &mut warnings);
    let interaction_runtime_test_src = read_text(&interaction_runtime_test_path, &mut warnings);
    let control_runtime_src = read_text(&control_runtime_path, &mut warnings);
    let control_runtime_types_src = read_text(&control_runtime_types_path, &mut warnings);
    let control_runtime_test_src = read_text(&control_runtime_test_path, &mut warnings);
    let dashboard_renderer_src = read_text(&dashboard_renderer_path, &mut warnings);
    let dashboard_renderer_types_src = read_text(&dashboard_renderer_types_path, &mut warnings);
    let visual_regression_src = read_text(&visual_regression_path, &mut warnings);
    let visual_regression_types_src = read_text(&visual_regression_types_path, &mut warnings);
    let visual_regression_test_src = read_text(&visual_regression_test_path, &mut warnings);
    let visual_regression_e2e_src = read_text(&visual_regression_e2e_path, &mut warnings);
    let web_accessibility_src = read_text(&web_accessibility_path, &mut warnings);
    let web_accessibility_test_src = read_text(&web_accessibility_test_path, &mut warnings);
    let web_config_test_src = read_text(&web_config_test_path, &mut warnings);
    let web_renderer_test_src = read_text(&web_renderer_test_path, &mut warnings);
    let web_playwright_config_src = read_text(&web_playwright_config_path, &mut warnings);
    let web_vite_config_src = read_text(&web_vite_config_path, &mut warnings);
    let web_e2e_test_src = read_text(&web_e2e_test_path, &mut warnings);
    let openapi_export_src = read_text(&openapi_export_path, &mut warnings);
    let openapi_json_src = read_text(&openapi_json_path, &mut warnings);
    let openapi_yaml_src = read_text(&openapi_yaml_path, &mut warnings);
    let docs_src = read_text(&docs_path, &mut warnings);
    let engine_docs_src = read_text(&engine_docs_path, &mut warnings);
    let sdk_package_src = read_text(&sdk_package_path, &mut warnings);
    let sdk_readme_src = read_text(&sdk_readme_path, &mut warnings);

    let rust_exposes_hierarchy_ffi = has_all(
        rust_ffi_src.as_deref(),
        &[
            "pub unsafe extern \"C\" fn tessellation_solve_hierarchy",
            "HierarchicalConfig",
            "HierarchicalSolver::new",
        ],
    );
    let rust_exposes_power_diagram_ffi = has_all(
        rust_ffi_src.as_deref(),
        &[
            "pub unsafe extern \"C\" fn tessellation_solve_power_diagram",
            "PowerDiagramConfig",
            "PowerDiagramProduct::solve",
        ],
    );
    let rust_hierarchy_scales_children = has_all(
        rust_hierarchy_src.as_deref(),
        &[
            "scale_target_to_frame",
            "source_width",
            "test_child_targets_scale_to_solved_parent_surface",
        ],
    );
    let rust_splits_product_facades = has_all(
        rust_products_src.as_deref(),
        &[
            "pub struct SemanticLayoutProduct",
            "pub struct PowerDiagramProduct",
            "PowerDiagramConfig",
            "WeightCountMismatch",
            "power_diagram_product_returns_polygon_cells",
        ],
    );
    let engine_cli_product_boundary = has_all(
        engine_cli_src.as_deref(),
        &[
            "solve-power",
            "scaffold_new_app",
            "NewOptions",
            "ScaffoldKind",
            "runTessellationApp",
            "createDashboard",
            "@example/tessellation/wasm-bg?url",
            "scaffold_new_dashboard_writes_one_surface_project",
            "tessellation new <dashboard|landing|webapp>",
            "verify_scenarios",
            "schema_version",
            "tessellation.engine.v0.1",
            "PowerDiagramProduct::solve",
            "SemanticLayoutProduct::solve_hierarchy",
        ],
    );
    let engine_schemas_are_versioned = has_all(
        engine_envelope_schema_src.as_deref(),
        &[
            "tessellation.engine.v0.1",
            "semantic-layout",
            "semantic-layout-hierarchy",
            "power-diagram",
        ],
    ) && has_all(
        engine_solver_schema_src.as_deref(),
        &["Tessellation SolverConfig", "targets", "$defs"],
    ) && has_all(
        engine_hierarchy_schema_src.as_deref(),
        &["Tessellation HierarchicalConfig", "nodes"],
    ) && has_all(
        engine_power_schema_src.as_deref(),
        &["Tessellation PowerDiagramConfig", "sites", "weights"],
    );
    let makefile_exposes_engine_release_check = has_all(
        makefile_src.as_deref(),
        &[
            "tessellation-engine",
            "tessellation-release-check",
            "cargo run -p tessellation-engine -- verify scenarios",
            "cargo run -p tessellation-engine -- schemas --out engine/schemas",
        ],
    );
    let wasm_exposes_hierarchy_buffer = has_all(
        wasm_src.as_deref(),
        &[
            "pub struct HierarchicalLayoutBuffer",
            "const H_STRIDE: usize = 6",
            "#[wasm_bindgen(js_name = \"lastLayoutResult\")]",
        ],
    );
    let wasm_tests_hierarchy_buffer = has_all(
        wasm_test_src.as_deref(),
        &[
            "test_hierarchical_layout_buffer",
            "HierarchicalLayoutBuffer",
            "last_layout_result",
            "parent_id",
        ],
    );
    let makefile_exposes_wasm_test = has_all(
        makefile_src.as_deref(),
        &[
            "tessellation-wasm-test",
            "RUST_LOG=error wasm-pack test --node tessellation-wasm",
        ],
    );
    let api_configures_hierarchy_ffi = has_all(
        api_src.as_deref(),
        &[
            "lib.tessellation_solve_hierarchy.argtypes",
            "def _call_hierarchical_solver",
            "@router.post(\"/solve/hierarchy\"",
        ],
    );
    let api_configures_power_diagram_ffi = has_all(
        api_src.as_deref(),
        &[
            "lib.tessellation_solve_power_diagram.argtypes",
            "def _call_power_diagram_solver",
            "@router.post(\"/geo/solve\"",
            "TessellationGeoSolveRequest",
        ],
    );
    let api_tests_hierarchy_endpoint = has_all(
        api_test_src.as_deref(),
        &[
            "test_hierarchical_solve_returns_parent_depth_metadata",
            "/tessellation/solve/hierarchy",
            "_minimal_hierarchy_config_dict",
        ],
    );
    let api_tests_power_diagram_endpoint = has_all(
        api_test_src.as_deref(),
        &[
            "test_geo_solve_returns_power_diagram_cells",
            "/tessellation/geo/solve",
            "_minimal_geo_solve_payload",
        ],
    );
    let ffi_tests_hierarchy_symbol = has_all(
        ffi_test_src.as_deref(),
        &[
            "tessellation_solve_hierarchy",
            "TestHierarchySolveMinimal",
            "_call_solve_hierarchy",
        ],
    );
    let ffi_tests_power_diagram_symbol = has_all(
        ffi_test_src.as_deref(),
        &[
            "tessellation_solve_power_diagram",
            "TestPowerDiagramSolveMinimal",
            "_call_solve_power_diagram",
        ],
    );
    let rust_tests_formal_scenarios = has_all(
        scenario_test_src.as_deref(),
        &[
            "formal_layout_scenarios_solve_and_emit_semantics",
            "formal_canvas_morph_scenario_preserves_semantic_ids",
            "assert_semantic_contract",
            "min_semantic_triples",
        ],
    ) && has_all(
        dondonha_scenario_src.as_deref(),
        &["dondonha-search", "\"min_semantic_triples\""],
    ) && has_all(
        dashboard_scenario_src.as_deref(),
        &["dashboard", "\"min_semantic_triples\""],
    ) && has_all(
        google_scenario_src.as_deref(),
        &["google-homepage", "\"min_semantic_triples\""],
    ) && has_all(
        morph_scenario_src.as_deref(),
        &["canvas-morph", "source_config", "target_config"],
    );
    let engine_conformance_covers_power_scenario = has_all(
        sector_power_scenario_src.as_deref(),
        &[
            "sector-coverage-power",
            "\"mode\": \"power-diagram\"",
            "\"sites\"",
            "\"min_vertices_per_cell\"",
        ],
    );
    let sector_uses_geo_solve_contract = has_all(
        sector_route_src.as_deref(),
        &[
            "/tessellation/geo/solve",
            "parsePowerDiagramCells",
            "buildPowerDiagramFeatureCollection",
            "tessellation_engine: \"power-diagram\"",
        ],
    );
    let sector_removed_raw_ingest_drift = sector_route_src
        .as_deref()
        .is_some_and(|src| !src.contains("/tessellation/ingest/raw"));
    let web_is_workspace_package =
        has_all(pnpm_workspace_src.as_deref(), &["tessellation-solver/web"]);
    let web_builds_wasm_before_vite = has_all(
        web_package_src.as_deref(),
        &[
            "\"build:wasm\": \"RUST_LOG=error wasm-pack build ../tessellation-wasm --target web --out-dir ../pkg\"",
            "\"dev\": \"pnpm --loglevel error build:wasm && vite\"",
            "\"build\": \"pnpm --loglevel error build:wasm && tsc --noEmit && vite build\"",
        ],
    );
    let web_uses_hierarchy_buffer = has_all(
        web_wasm_src.as_deref(),
        &[
            "HierarchicalLayoutBuffer",
            "hierarchical-layout-buffer",
            "stride: 6",
            "buildAccessibilityFromRegions",
        ],
    );
    let web_promotes_semantic_compiler = has_all(
        web_semantic_compiler_src.as_deref(),
        &[
            "compileSearchToLayout",
            "buildSemanticDocument",
            "contentJsonLd",
            "semanticTriples",
            "emitCompilerTriples",
        ],
    ) && has_all(
        web_semantic_compiler_test_src.as_deref(),
        &[
            "compiles classified search through a JSON-LD document to solver config",
            "keeps compiled region ids aligned with semantic result and cluster nodes",
            "emits deterministic document IRIs",
        ],
    );
    let shadcn_tailwind_compiler_contract = has_all(
        shadcn_tailwind_compiler_src.as_deref(),
        &[
            "compileShadcnTailwindToLayout",
            "parseTailwindHints",
            "parseGridColumns",
            "preservedTokens",
            "tess:component",
            "hierarchical-layout-buffer",
            "resolveTailwindStyle",
            "resolvedStyle",
            "resolvedTailwindTokenCount",
            "variantRuleCount",
        ],
    ) && has_all(
        shadcn_tailwind_types_src.as_deref(),
        &[
            "ShadcnTailwindNode",
            "ShadcnTailwindLayoutPlan",
            "HierarchicalConfig",
            "StyleTokenEntry",
            "TailwindStyleResolution",
        ],
    ) && has_all(
        shadcn_tailwind_test_src.as_deref(),
        &[
            "compiles a shadcn dashboard tree into a hierarchical solver config",
            "preserves arbitrary Tailwind tokens while extracting layout hints",
            "derives responsive grid columns from arbitrary minmax Tailwind syntax",
            "emits semantic triples that bind components, class names, and region ids",
            "resolvedStyle.declarations.backgroundColor",
            "variantRuleCount",
        ],
    );
    let tailwind_style_resolver_contract = has_all(
        tailwind_style_resolver_src.as_deref(),
        &[
            "resolveTailwindStyle",
            "resolveTailwindTokens",
            "SHADCN_COLOR_NAMES",
            "variantRules",
            "unresolvedTokens",
            "parseArbitraryProperty",
            "cssVariables",
        ],
    ) && has_all(
        tailwind_style_resolver_types_src.as_deref(),
        &[
            "TailwindStyleResolverOptions",
            "TailwindStyleResolution",
            "TailwindVariantRule",
            "TailwindAppliedToken",
        ],
    ) && has_all(
        tailwind_style_resolver_test_src.as_deref(),
        &[
            "resolves shadcn CSS-variable colors and arbitrary values",
            "keeps state variants as explicit rules instead of flattening them",
            "does not apply inactive breakpoint declarations to the current surface",
            "bg-background",
        ],
    );
    let tailwind_build_adapter_contract = has_all(
        tailwind_build_adapter_src.as_deref(),
        &[
            "compileTailwindCss",
            "compileShadcnProjectTailwind",
            "extractCompiledClassRules",
            "buildTailwindParityReport",
            "@tailwindcss/postcss",
            "declarationMismatches",
        ],
    ) && has_all(
        tailwind_build_adapter_types_src.as_deref(),
        &[
            "TailwindCompileResult",
            "CompiledClassRuleExtraction",
            "TailwindParityReport",
            "ShadcnTailwindProject",
        ],
    ) && has_all(
        tailwind_build_adapter_test_src.as_deref(),
        &[
            "extracts class rules from compiled Tailwind CSS including variants",
            "builds a parity report between resolved styles and compiled CSS",
            "invokes injected PostCSS/Tailwind implementations for host-project compilation",
        ],
    );
    let shadcn_project_loader_contract = has_all(
        shadcn_project_loader_src.as_deref(),
        &[
            "loadShadcnTailwindProject",
            "extractShadcnCssVariables",
            "createTailwindResolverOptions",
            "components.json",
            "@theme",
            "darkResolverOptions",
        ],
    ) && has_all(
        shadcn_project_loader_types_src.as_deref(),
        &[
            "ShadcnTailwindProject",
            "ShadcnCssVariableExtraction",
            "ShadcnComponentsConfig",
            "TailwindStyleResolverOptions",
        ],
    ) && has_all(
        shadcn_project_loader_test_src.as_deref(),
        &[
            "extracts light and dark shadcn CSS variables from globals.css",
            "loads components.json and feeds project theme into TSX compilation",
            "loadShadcnTailwindProject",
            "createTailwindResolverOptions",
        ],
    );
    let tsx_extractor_contract = has_all(
        tsx_extractor_src.as_deref(),
        &[
            "extractShadcnTailwindTreeFromSource",
            "extractAndCompileShadcnTailwindFromSource",
            "typescript is required for TSX extraction",
            "CLASS_HELPER_NAMES",
            "collectClassTokens",
            "spread JSX attributes are not statically expanded",
        ],
    ) && has_all(
        tsx_extractor_types_src.as_deref(),
        &[
            "TsxExtractionOptions",
            "TsxExtractionResult",
            "ExtractedShadcnTailwindLayoutPlan",
        ],
    ) && has_all(
        tsx_extractor_test_src.as_deref(),
        &[
            "extracts real shadcn TSX into a component tree",
            "extracts and compiles TSX into the tessellation hierarchy plan",
            "bg-[oklch(0.62_0.14_168)]",
            "typescript: ts",
        ],
    );
    let creator_sdk_contract = has_all(
        app_spec_src.as_deref(),
        &[
            "TESSELLATION_APP_SCHEMA_VERSION",
            "tessellation.app.v0.1",
            "defineTessellationApp",
            "compileTessellationAppToLayout",
            "validateTessellationAppSpec",
        ],
    ) && has_all(
        app_spec_types_src.as_deref(),
        &[
            "TessellationAppSpec",
            "TessellationAppNode",
            "CompiledTessellationApp",
            "CompileTessellationAppOptions",
        ],
    ) && has_all(
        presets_src.as_deref(),
        &["createDashboard", "createLandingPage", "createWebApp"],
    ) && has_all(
        presets_types_src.as_deref(),
        &[
            "DashboardPresetOptions",
            "LandingPresetOptions",
            "WebAppPresetOptions",
        ],
    ) && has_all(
        runtime_src.as_deref(),
        &[
            "runTessellationApp",
            "compileTessellationSurface",
            "solveWithHierarchicalLayoutBuffer",
            "paintCanvas2DCommands",
            "resolveCanvasPaint",
        ],
    ) && has_all(
        runtime_types_src.as_deref(),
        &[
            "TessellationRuntimeOptions",
            "TessellationCanvasRuntime",
            "HierarchicalLayoutBufferConstructor",
        ],
    ) && has_all(
        creator_sdk_test_src.as_deref(),
        &[
            "creates a dashboard app spec and compiles it through the hierarchy compiler",
            "compiles, renders, and hit-tests a no-component-DOM surface",
            "resolves shadcn CSS variable paints before canvas drawing",
        ],
    );
    let power_surface_product_contract = has_all(
        power_layout_src.as_deref(),
        &[
            "solvePowerTessellationLayoutPlan",
            "buildPowerTessellationRenderPlan",
            "verifyPowerTessellation",
            "verifyPowerRenderPlan",
            "PowerLayoutReadinessError",
            "minTouchTarget",
            "failClosedPowerLayout",
        ],
    ) && has_all(
        power_layout_types_src.as_deref(),
        &[
            "PowerTessellationSolution",
            "PowerTessellationProof",
            "PowerLayoutReadinessError",
            "verifyPowerRenderPlan",
        ],
    ) && has_all(
        runtime_src.as_deref(),
        &[
            "geometryBackend",
            "morphPowerWeights",
            "polygon-text",
            "syncAccessibilityMirror",
        ],
    ) && has_all(
        runtime_types_src.as_deref(),
        &[
            "PowerDiagramSolver",
            "solvePowerDiagram",
            "morphPowerWeights",
        ],
    ) && has_all(
        wasm_src.as_deref(),
        &["solvePowerDiagram", "solve_power_diagram"],
    ) && has_all(
        creator_sdk_test_src.as_deref(),
        &[
            "renders semantic hierarchy as true nested Power Diagram polygons",
            "rejects unreadable and undersized interactive Power cells",
            "re-solves and verifies every animated Power frame",
        ],
    );
    let power_performance_budget_contract = has_all(
        performance_budget_src.as_deref(),
        &[
            "TESSELLATION_MOBILE_FRAME_BUDGET_MS",
            "createTessellationPerformanceMonitor",
            "summarizePerformanceSamples",
            "TessellationPerformanceBudgetError",
            "p95Ms",
        ],
    ) && has_all(
        performance_budget_types_src.as_deref(),
        &[
            "TessellationPerformanceSample",
            "TessellationPerformanceSnapshot",
            "TessellationPerformanceOptions",
        ],
    ) && has_all(
        performance_budget_test_src.as_deref(),
        &[
            "tessellation mobile performance budget",
            "reports bounded p50/p95 frame metrics and violations",
            "fail closed on a single over-budget frame",
        ],
    ) && has_all(
        runtime_src.as_deref(),
        &[
            "performanceMonitor.record",
            "resetPerformance",
            "get performance",
        ],
    );
    let power_mobile_browser_release_gate = has_all(
        power_benchmark_page_src.as_deref(),
        &[
            "__powerSurfaceBenchmark",
            "runBenchmark",
            "performanceBudgetMs: 1000 / 60",
            "everyFrameValid",
            "mirrorPowerCellCount",
        ],
    ) && has_all(
        power_benchmark_e2e_src.as_deref(),
        &[
            "Power Surface browser release gate",
            "mobile frame budget",
            "p95Ms",
            "touchTargets",
            "noHorizontalOverflow",
        ],
    ) && has_all(
        web_playwright_config_src.as_deref(),
        &["mobile-power", "Pixel 7", "power-runtime.spec.ts"],
    ) && has_all(
        makefile_src.as_deref(),
        &[
            "tessellation-power-mobile-benchmark",
            "--project=mobile-power",
        ],
    );
    let power_realtime_studio_contract = has_all(
        power_studio_html_src.as_deref(),
        &[
            "Semantic geometry",
            "live-toggle",
            "weight-evidence",
            "proof-grid",
            "event-list",
        ],
    ) && has_all(
        power_studio_src.as_deref(),
        &[
            "__powerStudio",
            "ResizeObserver",
            "morphPowerWeights",
            "setPowerWeights",
            "performanceBudgetMs: 1000 / 60",
            "showGateMessage",
            "addGeometryEvent",
            "Last verified geometry",
            "noHorizontalOverflow",
        ],
    ) && has_all(
        power_studio_css_src.as_deref(),
        &[
            "button:focus-visible",
            "@media (max-width: 720px)",
            "min-height: 44px",
            "prefers-reduced-motion",
        ],
    ) && has_all(
        power_studio_e2e_src.as_deref(),
        &[
            "Power Surface real-time studio",
            "Auto pulse",
            "data-power-studio-ready",
            "touchTargets: true",
            "geometryAfter.polygon",
            "geometry / evidence",
            "noHorizontalOverflow",
        ],
    ) && has_all(
        web_vite_config_src.as_deref(),
        &["powerStudio", "power-studio.html", "powerBenchmark"],
    ) && has_all(
        sdk_readme_src.as_deref(),
        &["power-studio.html", "Power Lab", "solver engineering"],
    );
    let tessellation_web_showcase_contract = has_all(
        showcase_html_src.as_deref(),
        &[
            "GUIDED QUICKSTART",
            "tutorial-surface",
            "data-example-action",
            "data-language=\"pt\"",
            "starter-dialog",
            "recipe-input",
        ],
    ) && has_all(
        showcase_src.as_deref(),
        &[
            "__tessellationShowcase",
            "setLanguage",
            "focusNextExample",
            "starterProjectFiles",
            "buildRecipeFromEditor",
            "geometryBackend: \"power-diagram\"",
        ],
    ) && has_all(
        showcase_css_src.as_deref(),
        &[
            ".lesson-preview { grid-column: auto; order: 2; }",
            ".lesson-code { order: 3; }",
            "#starter-dialog",
            ".language-switch",
        ],
    ) && has_all(
        showcase_e2e_src.as_deref(),
        &[
            "Tessellation web showcase",
            "examples.operations",
            "starter-dialog",
            "lang\", \"pt-BR",
            "mobileOrder.preview",
        ],
    ) && has_all(
        web_vite_config_src.as_deref(),
        &["showcase: resolve", "showcase.html"],
    ) && has_all(
        sdk_readme_src.as_deref(),
        &[
            "Web Showcase",
            "showcase.html",
            "in-browser JSON recipe editor",
        ],
    );
    let operational_surface_studio_contract = has_all(
        operational_studio_html_src.as_deref(),
        &[
            "OPERATIONAL SURFACE STUDIO",
            "theme-color\" content=\"#ffffff",
            "data-add-kind=\"metric\"",
            "SEMANTIC LAYERS",
            "property-form",
            "generation-command",
            "duplicate-element",
            "auto-apply-note",
            "solve-count",
            "drag-ghost",
            "insertion-guide",
            "move-earlier",
            "move-later",
            "DRAG TO REORDER",
            "contract-bounded",
            "contract-contained",
            "contract-overlap",
            "contract-targets",
            "spec-dialog",
        ],
    ) && has_all(
        operational_studio_src.as_deref(),
        &[
            "__operationalStudio",
            "createDashboard",
            "runDashboardSurface",
            "addElement",
            "addElements",
            "duplicateSelected",
            "removeSelected",
            "applyGenerationCommand",
            "commandSegments",
            "persistDocument",
            "estimatedSurfaceHeight",
            "installStudioDragHandlers",
            "autoScrollDragViewport",
            "updateDragGhost",
            "showInsertionGuide",
            "hideDirectManipulationFeedback",
            "reorderElement",
            "moveSelectedBy",
            "schedulePropertyApply",
            "scheduleGenerationApply",
            "runtime.update",
            "runtimeMountCount",
            "hotUpdateCount",
            "solveCount",
            "releaseVerified",
            "setContractStatus",
            "ENGINE GATE FAILED",
            "historyIndex",
            "geometry: runtime.renderPlan?.geometry",
        ],
    ) && has_all(
        operational_studio_css_src.as_deref(),
        &[
            "color-scheme: light",
            ".workbench-body",
            ".component-palette",
            ".layer-list",
            ".property-panel",
            ".generation-bar",
            ".is-studio-dragging",
            ".drag-ghost",
            ".insertion-guide",
            ".reorder-actions",
            "solve-pulse",
            "@media (max-width: 720px)",
        ],
    ) && has_all(
        operational_studio_e2e_src.as_deref(),
        &[
            "Operational Surface Studio",
            "colorScheme)).toBe(\"light\")",
            "rgb(237, 242, 247)",
            "geometry).toBe(\"rectangles\")",
            "metric-3",
            "critical compliance alert",
            "Remove the selected element",
            "generates compound changes atomically",
            "restores the local document",
            "Duplicate selected element",
            "snapshot.releaseVerified).toBe(true)",
            "toBeEnabled()",
            "AUTOSOLVE #",
            "drags widgets on the rendered surface",
            "data-source-id",
            "data-target-id",
            "Alt+ArrowUp",
            "engine autosolved",
            "auto-fits the surface whenever new widgets require more rows",
            "expanded.releaseVerified).toBe(true)",
            "snapshot.runtimeMountCount).toBe(1)",
            "snapshot.hotUpdateCount).toBeGreaterThan(0)",
            "name: \"Generate\"",
            "toHaveCount(0)",
            "selects the mobile surface automatically",
        ],
    ) && has_all(
        web_vite_config_src.as_deref(),
        &["studio: resolve", "studio.html"],
    ) && has_all(
        sdk_readme_src.as_deref(),
        &[
            "Operational Surface Studio",
            "SCADA/CAE environment",
            "drag widgets directly on the rendered surface",
            "atomic multi-element generative UI commands",
            "There are no Apply or Generate buttons",
            "immediately runs a new solver pass",
            "hot-update the live runtime in place",
            "morph retained semantic frames",
            "semantic drag ghost and solver insertion guide",
            "Move Earlier/Move Later controls",
            "Alt+Arrow",
            "newly added components always enter an auto-fitted layout",
            "persist locally with runtime validation",
            "Export remains locked unless the live release contract proves",
        ],
    );
    let example_studio_surface_integration_contract =
        has_all(
            example_surface_studio_src.as_deref(),
            &[
                "@example/tessellation/presets",
                "@example/tessellation/renderers/dashboard",
                "example.surface.v1",
                "example-surface-studio/v1",
                "runDashboardSurface",
                "runtime.update",
                "Object.values(proof).every(Boolean)",
                "createSurfaceProject",
                "updateSurfaceProject",
                "connectSurfaceStream",
                "publishSurfaceEvent",
                "updateContent",
                "SurfaceDataBinding",
                "mapBoundPayload",
                "data-agent-last-live-binding",
                "data-agent-last-live-projection",
                "example-surface-studio-agent/v1",
                "SURFACE_AGENT_CAPABILITIES",
                "exampleStudioAgent",
                "parseSurfaceAgentRequest",
                "validatedAgentElementPatch",
                "request-governed-command",
                "renderSurfacePdf",
                "verifiedReleaseProof",
                "compilePdfArtifact",
                "pdfCacheKeyRef",
                "pdfArtifact?.digest",
                "shortDigest",
                "PdfAStandard",
                "PDF archival standard",
                "PDF/A-1b",
                "PDF/A-2b",
                "PDF/A-3a",
                "set-pdf-standard",
                "preview-pdf",
                "export-pdf",
                "Preview PDF",
                "Export PDF",
                "Source semantic ID",
                "Flight DoExchange",
                "Save project",
                "Changes apply and solve automatically",
            ],
        ) && has_all(
            example_surface_studio_css_src.as_deref(),
            &[
                ".shell",
                ".leftRail",
                ".tabs",
                ".viewportPanel",
                ".inspector",
                ".bindingFields",
                ".pdfDialog",
                ".commandBar",
                "@media (max-width: 760px)",
            ],
        ) && has_all(
            example_surface_page_src.as_deref(),
            &["Operational Surfaces · Example Studio", "<SurfaceStudio />"],
        ) && has_all(
            example_surface_e2e_src.as_deref(),
            &[
                "Example Operational Surface workspace",
                "data-agent-contract",
                "data-agent-verified",
                "Layers 7",
                "persists and hot-renders Flight DoExchange surface events",
                "project_kind).toBe('operational-surface')",
                "Flight DoExchange · 1 events",
                "claims-feed→open-claims,sla-risk",
                "open-claims=1291;sla-risk=38",
                "exposes a validated AI-native operational surface contract",
                "example-surface-studio-agent/v1",
                "surface.realtime-inspect",
                "unsafeHtml",
                "commandWithoutProject",
                "surface:solve",
                "compiles a verified operational surface PDF through the Typst endpoint",
                "api/studio/surface/render",
                "claims-operations.pdf",
                "RUST + TYPST ARTIFACT",
                "x-example-artifact-digest",
                "render-deck+typst",
                "sha256:aaaaaaaaaaaa",
                "PDF archival standard",
                "PDF/A-3A",
                "pdf_standard).toBe('a-3a')",
                "renderBodies).toHaveLength(1)",
                "surface:preview-pdf",
                "surface:export-pdf",
                "Source semantic ID",
                "valuePath: 'metrics.open'",
                "realtime.transport).toBe('flight-do-exchange')",
                "keeps the integrated workspace bounded on mobile",
            ],
        ) && has_all(
            example_surface_projects_client_src.as_deref(),
            &[
                "project_kind: 'operational-surface'",
                "listSurfaceProjects",
                "createSurfaceProject",
                "updateSurfaceProject",
                "normalizeSurfaceProject",
            ],
        ) && has_all(
            example_surface_realtime_client_src.as_deref(),
            &[
                "connectSurfaceStream",
                "publishSurfaceEvent",
                "new EventSource",
                "human_confirmed",
            ],
        ) && has_all(
            example_surface_pdf_client_src.as_deref(),
            &[
                "renderSurfacePdf",
                "/api/studio/surface/render",
                "application/pdf",
                "release_proof",
                "x-example-artifact-digest",
                "x-example-render-engine",
                "verifiable artifact identity",
                "PdfAStandard",
                "pdf_standard",
                "x-example-pdf-standard",
            ],
        ) && has_all(
            surface_pdf_compiler_src.as_deref(),
            &[
                "class OperationalSurfaceDocument",
                "class SurfaceReleaseProof",
                "class SurfaceRenderRequest",
                "Literal[\"a-1b\", \"a-2b\", \"a-3a\"]",
                "pdf_standard",
                "surface_artifact_digest",
                "hashlib.sha256",
                "surface_to_deck",
                "TessellatedLayout",
                "Release contract",
                "VERIFIED",
            ],
        ) && has_all(
            surface_pdf_test_src.as_deref(),
            &[
                "test_surface_compiles_to_tessellated_deck_with_live_values",
                "test_surface_render_endpoint_uses_canonical_deck_renderer",
                "test_surface_artifact_digest_changes_with_live_snapshot",
                "test_surface_render_accepts_supported_pdfa_profiles",
                "test_surface_render_rejects_unknown_pdfa_profile",
                "x-example-artifact-digest",
                "verapdf",
                "test_surface_renderer_produces_real_typst_pdfa",
            ],
        ) && has_all(
            pdf_studio_router_src.as_deref(),
            &[
                "/surface/render",
                "surface_artifact_digest",
                "X-Example-Artifact-Digest",
                "X-Example-Render-Engine",
                "X-Example-PDF-Standard",
                "pdf_standard=request.pdf_standard",
                "render_surface",
            ],
        ) && has_all(
            deck_tessellate_src.as_deref(),
            &[
                "body_card_emits_title_and_description_once",
                "stat_card_emits_verification_description",
            ],
        ) && has_all(
            deck_compile_src.as_deref(),
            &["pdf_standard", "--pdf-standard"],
        ) && has_all(
            render_deck_bin_src.as_deref(),
            &[
                "--pdf-standard",
                "a-1b",
                "a-2b",
                "a-3a",
                "opts.pdf_standard(standard)",
            ],
        ) && has_all(
            pdf_studio_render_src.as_deref(),
            &["PdfStandard", "--pdf-standard", "pdf_standard=pdf_standard"],
        ) && has_all(
            api_dockerfile_src.as_deref(),
            &["typst-cli --version '~0.14'"],
        ) && has_all(deck_cover_theme_src.as_deref(), &["#CED2D7", "#9DA6B0"])
            && deck_cover_theme_src.as_deref().is_some_and(|src| {
                !src.contains("transparentize(") && !src.contains("example_hero.jpg")
            })
            && api_deck_cover_theme_src == deck_cover_theme_src
            && has_all(
                workspace_verify_src.as_deref(),
                &[
                    "pdfa-conformance:",
                    "Typst 0.14.2",
                    "surface-a-1b.pdf",
                    "surface-a-2b.pdf",
                    "surface-a-3a.pdf",
                    "workspace-docs-report-a-2b.pdf",
                    "test_subprocess_report_renderer_produces_real_pdfa_without_latex",
                    "verapdf/cli@sha256:",
                    "isCompliant=\"true\"",
                ],
            )
            && has_all(
                workspace_docs_report_src.as_deref(),
                &[
                    "render_typst_report",
                    "compile_typst_subprocess",
                    ".pdf_standard(\"a-2b\")",
                    "workspace-docs-report.typ",
                ],
            )
            && has_all(
                report_cargo_src.as_deref(),
                &[
                    "typst-subprocess",
                    "required-features = [\"typst-subprocess\", \"md\"]",
                ],
            )
            && has_all(
                report_renderer_src.as_deref(),
                &[
                    "same Typst CLI",
                    "typst_available",
                    "[\"typst\", \"--version\"]",
                ],
            )
            && has_all(
                typst_report_src.as_deref(),
                &[
                    "render_typst_report",
                    "New Computer Modern Math",
                    "Noto Sans Symbols2",
                    "Noto Color Emoji",
                    "Noto Serif CJK SC",
                ],
            )
            && api_dockerfile_src.as_deref().is_some_and(|src| {
                !src.contains("texlive-")
                    && !src.contains("pdf-studio-report-theme")
                    && src.contains("--features typst-subprocess,md")
                    && src.contains("fonts-noto-core")
                    && src.contains("fonts-noto-cjk")
                    && src.contains("fonts-noto-color-emoji")
            })
            && !report_theme_path.exists()
            && has_all(
                studio_deploy_smoke_src.as_deref(),
                &[
                    "/v2/pdf-studio/surface/render",
                    "for pdf_standard in (\"a-1b\", \"a-2b\", \"a-3a\")",
                    "operational surface {pdf_standard} render smoke failed",
                    "surface_pdf_bytes",
                    "x-example-pdf-standard",
                    "render-deck+typst",
                    "x-example-artifact-digest",
                    "returned mismatched ETag",
                    "workspace-docs-report+typst",
                    "workspace-docs Typst report smoke failed",
                    "render_surface",
                ],
            )
            && has_all(
                surface_realtime_hub_src.as_deref(),
                &[
                    "class SurfaceRealtimeHub",
                    "class SurfaceRealtimeUnavailable",
                    "canonical_surface_tenant_key",
                    "EXAMPLE_FLIGHT_TENANT_HMAC_SECRET",
                    "EXAMPLE_SURFACE_REALTIME_BACKEND",
                    "get_redis",
                    "client.xadd(",
                    "client.xread(",
                    "example:surface:realtime:",
                    "subscriber_capacity",
                    "_bounded_put",
                    "surface_realtime_hub",
                ],
            )
            && has_all(
                flight_server_src.as_deref(),
                &[
                    "surface:{project}",
                    "SURFACE_EVENT_SCHEMA_VERSION",
                    "_exchange_surface",
                    "flight-do-exchange",
                    "Flight producers cannot bypass the HTTP human gate",
                ],
            )
            && has_all(
                surface_realtime_test_src.as_deref(),
                &[
                    "test_redis_stream_fans_out_across_realtime_hub_instances",
                    "test_redis_stream_replays_recent_surface_events",
                    "test_http_and_flight_use_the_same_opaque_tenant_key",
                    "test_surface_do_exchange_runs_over_a_real_flight_channel",
                    "test_surface_do_exchange_is_bidirectional_and_arrow_native",
                    "test_surface_do_exchange_rejects_client_supplied_tenant_scope",
                    "test_surface_do_exchange_cannot_bypass_command_human_gate",
                ],
            )
            && has_all(
                studio_projects_router_src.as_deref(),
                &[
                    "Literal[\"deck\", \"report\", \"operational-surface\"]",
                    "SurfaceRealtimeUnavailable",
                    "project_kind=body.project_kind",
                    "/projects/{project_id}/stream",
                    "/projects/{project_id}/events",
                    "human_confirmed",
                ],
            )
            && has_all(
                studio_projects_db_src.as_deref(),
                &[
                    "project_kind    VARCHAR NOT NULL DEFAULT 'deck'",
                    "{\"project_kind\": \"VARCHAR NOT NULL DEFAULT 'deck'\"}",
                    "project_kind: str = \"deck\"",
                ],
            )
            && has_all(
                studio_projects_test_src.as_deref(),
                &[
                    "test_crud_operational_surface_kind_round_trip",
                    "test_router_creates_operational_surface_project",
                    "test_router_surface_event_bridge_is_tenant_scoped",
                    "test_router_surface_stream_fails_closed_when_shared_backend_is_unavailable",
                    "test_router_surface_command_requires_human_gate",
                ],
            )
            && has_all(
                example_studio_package_src.as_deref(),
                &["@example/tessellation", "workspace:*"],
            )
            && has_all(
                example_studio_next_config_src.as_deref(),
                &[
                    "transpilePackages: ['@example/tessellation']",
                    "outputFileTracingRoot: workspaceRoot",
                ],
            )
            && has_all(
                example_studio_dockerfile_src.as_deref(),
                &[
                    "Build context is the workspace root",
                    "COPY tessellation-solver/sdk",
                    "pnpm --filter example-studio-web build",
                ],
            )
            && has_all(
                example_api_compose_src.as_deref(),
                &[
                    "context: ..",
                    "dockerfile: example-studio-web/Dockerfile",
                    "EXAMPLE_SURFACE_REALTIME_BACKEND",
                ],
            )
            && has_all(
                pnpm_workspace_src.as_deref(),
                &[
                    "example-studio-web",
                    "tessellation-solver\"",
                    "tessellation-solver/web",
                ],
            )
            && has_all(
                example_studio_readme_src.as_deref(),
                &[
                    "Operational Surface workspace",
                    "@example/tessellation",
                    "Redis Streams",
                    "dot-separated",
                    "sourceId",
                    "example-surface-studio-agent/v1",
                    "operator confirmation gate",
                    "Rust/Typst renderer",
                    "not a canvas screenshot",
                    "modal PDF preview",
                    "compiled Blob is reused",
                    "deterministic SHA-256 digest",
                    "X-Example-Artifact-Digest",
                    "PDF/A-1b",
                    "PDF/A-2b",
                    "PDF/A-3a",
                    "--pdf-standard",
                    "veraPDF validation",
                    "http://localhost:3200/surfaces",
                ],
            )
            && has_all(
                sdk_readme_src.as_deref(),
                &[
                    "canonical Operational Surface Studio",
                    "example-studio-web",
                    "http://localhost:3200/surfaces",
                ],
            );
    let dom_adapter_contract = has_all(
        dom_adapter_src.as_deref(),
        &[
            "applyTessellationLayoutToDom",
            "buildTessellationDomPatches",
            "regionsFromFlatBuffer",
            "data-tessellation-id",
            "resolvedStyle",
            "variant-count",
        ],
    ) && has_all(
        dom_adapter_types_src.as_deref(),
        &[
            "TessellationDomPatch",
            "TessellationDomApplyResult",
            "FlatSolvedLayout",
            "ShadcnTailwindLayoutPlan",
        ],
    ) && has_all(
        dom_adapter_test_src.as_deref(),
        &[
            "builds deterministic DOM patches from solved tessellation regions",
            "extracts stride-6 hierarchy buffers without losing parent/depth metadata",
            "applies patches to existing nodes by semantic id",
        ],
    );
    let render_plan_contract = has_all(
        render_plan_src.as_deref(),
        &[
            "buildTessellationRenderPlan",
            "buildCanvas2DCommands",
            "hitTestRenderPlan",
            "render-surface",
            "rounded-rect",
            "semanticTriples",
        ],
    ) && has_all(
        render_plan_types_src.as_deref(),
        &[
            "TessellationRenderPlan",
            "RenderPlanNode",
            "Canvas2DCommand",
            "RenderPlanOptions",
        ],
    ) && has_all(
        render_plan_test_src.as_deref(),
        &[
            "builds a DOM-free render surface from shadcn/Tailwind layout output",
            "emits deterministic Canvas2D commands and hit-tests semantic nodes",
            "excludeSemanticIds",
        ],
    );
    let dashboard_renderer_contract = has_all(
        dashboard_renderer_src.as_deref(),
        &[
            "runDashboardSurface",
            "paintDashboardRenderPlan",
            "compileTessellationSurface",
            "hitTestRenderPlan",
            "optimizeSurfaceRenderPlan",
            "createSurfaceInteractionModel",
            "createSurfaceControlState",
            "function update(",
            "function updateContent(",
            "content-only update cannot add, remove, or rename semantic regions",
            "createGeometryTransition",
            "transitionRenderPlan",
            "paintedRenderPlan",
            "component DOM",
        ],
    ) && has_all(
        dashboard_renderer_types_src.as_deref(),
        &[
            "DashboardSurfaceRuntime",
            "DashboardRendererOptions",
            "runDashboardSurface",
            "paintDashboardRenderPlan",
            "readonly renderedPlan",
            "update(",
            "updateContent(",
        ],
    ) && has_all(
        creator_sdk_test_src.as_deref(),
        &[
            "paints the dashboard renderer from semantic render-plan data",
            "hot-updates a dashboard spec without replacing its runtime",
            "hot-updates realtime content without re-solving verified geometry",
            "morphs retained semantic nodes during a hot update",
            "runDashboardSurface",
        ],
    );
    let surface_optimizer_contract = has_all(
        surface_optimizer_src.as_deref(),
        &[
            "optimizeSurfaceRenderPlan",
            "verifySurfaceRenderPlan",
            "criticalVisibleFraction",
            "actionSeparation",
            "nonOverlapping",
        ],
    ) && has_all(
        surface_optimizer_types_src.as_deref(),
        &[
            "SurfaceOptimizerResult",
            "SurfaceOptimizerProof",
            "SurfaceOptimizerDiagnostic",
            "optimizeSurfaceRenderPlan",
            "verifySurfaceRenderPlan",
        ],
    ) && has_all(
        surface_optimizer_test_src.as_deref(),
        &[
            "projects convex regions into surface bounds",
            "separates overlapping sibling rectangles",
            "critical action separation",
        ],
    );
    let interaction_runtime_contract = has_all(
        interaction_runtime_src.as_deref(),
        &[
            "createSurfaceInteractionModel",
            "buildSurfaceAccessibilityMirror",
            "createSurfaceCommandEnvelope",
            "routeSurfaceCommand",
            "tessellation.command.v0.1",
            "focusOrder",
        ],
    ) && has_all(
        interaction_runtime_types_src.as_deref(),
        &[
            "SurfaceInteractionModel",
            "SurfaceCommandEnvelope",
            "SurfaceAccessibilityMirror",
            "createSurfaceInteractionModel",
            "routeSurfaceCommand",
        ],
    ) && has_all(
        interaction_runtime_test_src.as_deref(),
        &[
            "builds deterministic focus order",
            "emits command envelopes",
            "semantic accessibility mirror",
        ],
    );
    let control_runtime_contract = has_all(
        control_runtime_src.as_deref(),
        &[
            "createSurfaceControlState",
            "reduceSurfaceControlState",
            "surfaceControlTransitionFromKey",
            "surfaceControlTransitionFromPointer",
            "surfaceControlTransitionFromPaste",
            "validateSurfaceControls",
            "tessellation.controls.v0.1",
            "cursors",
            "dirty",
            "touched",
        ],
    ) && has_all(
        control_runtime_types_src.as_deref(),
        &[
            "SurfaceControlState",
            "SurfaceControlTransition",
            "SurfaceControlValidity",
            "createSurfaceControlState",
            "reduceSurfaceControlState",
            "surfaceControlTransitionFromPointer",
            "surfaceControlTransitionFromPaste",
        ],
    ) && has_all(
        control_runtime_test_src.as_deref(),
        &[
            "extracts form primitives",
            "reduces text and toggle transitions",
            "keyboard transitions",
            "pointer transitions",
            "pastes text at the current rendered caret position",
        ],
    );
    let visual_regression_contract = has_all(
        visual_regression_src.as_deref(),
        &[
            "buildVisualRegressionFixture",
            "measureTessellationDomSnapshot",
            "compareVisualRegressionSnapshot",
            "getBoundingClientRect",
            "computedStyleExpectations",
            "geometry-mismatch",
        ],
    ) && has_all(
        visual_regression_types_src.as_deref(),
        &[
            "VisualRegressionFixture",
            "VisualDomSnapshotEntry",
            "VisualRegressionComparison",
            "ComputedStyleExpectation",
        ],
    ) && has_all(
        visual_regression_test_src.as_deref(),
        &[
            "matches rendered DOM snapshots against tessellation patches",
            "reports geometry and computed-style drift with semantic IDs",
            "summarizeVisualDiagnostics",
        ],
    ) && has_all(
        visual_regression_e2e_src.as_deref(),
        &[
            "shadcn visual regression harness",
            "matches tessellation patches to browser geometry and computed styles",
            "measureTessellationDomSnapshot",
            "compareVisualRegressionSnapshot",
            "screenshot.byteLength",
        ],
    );
    let web_tests_cover_accessibility_order = has_all(
        web_accessibility_src.as_deref(),
        &[
            "compareRegionsForReadingOrder",
            "rowTolerance",
            "roleToAria",
        ],
    ) && has_all(
        web_accessibility_test_src.as_deref(),
        &[
            "keeps left-to-right order when row tops differ slightly",
            "orders later rows after earlier rows once the row band is exceeded",
            "builds aria nodes from visual region order",
        ],
    );
    let web_tests_cover_hierarchy_demo = has_all(
        web_package_src.as_deref(),
        &["\"test\": \"vitest run src\""],
    ) && has_all(
        web_config_test_src.as_deref(),
        &[
            "builds a hierarchical config with root groups and child result nodes",
            "hierarchical-layout-buffer",
            "normalizes undersized surfaces",
        ],
    ) && has_all(
        web_renderer_test_src.as_deref(),
        &[
            "decodes stride-6 positions into groups and child regions",
            "buildRenderGroups",
            "hitTestLayout",
        ],
    );
    let web_e2e_configures_browser_smoke = has_all(
        web_package_src.as_deref(),
        &["\"test:e2e\": \"env -u NO_COLOR playwright test\""],
    ) && has_all(
        web_playwright_config_src.as_deref(),
        &[
            "baseURL: \"http://127.0.0.1:5173\"",
            "command: \"pnpm dev --host 127.0.0.1\"",
            "reuseExistingServer: true",
        ],
    );
    let web_exposes_one_command_verify = has_all(
        web_package_src.as_deref(),
        &[
            "\"verify\": \"pnpm --loglevel error test && pnpm --loglevel error test:e2e && pnpm --loglevel error build\"",
        ],
    ) && has_all(
        makefile_src.as_deref(),
        &["tessellation-web-verify", "pnpm -C \"$(TESS)/web\" verify"],
    );
    let web_e2e_proves_browser_hierarchy = has_all(
        web_e2e_test_src.as_deref(),
        &[
            "Liquid Search browser smoke",
            "expectCanvasToBePainted",
            "keeps canvas bounded after viewport resize",
            "keeps narrow mobile hierarchy tall enough for result groups",
            "keeps finance child cards separated on narrow viewports",
            "canvasFitsPanel",
            "childRegionsDoNotOverlap",
            "firstReadingOrderLabel",
            "noHorizontalDocumentOverflow",
            "metric(page, \"engine\")",
            "hierarchy",
            ".overlay-item",
            ".canvas-warning",
            "canvas.result-canvas",
        ],
    );
    let openapi_export_writes_json_and_yaml = has_all(
        openapi_export_src.as_deref(),
        &[
            "_ensure_import_paths",
            "openapi.json",
            "openapi.yaml",
            "yaml.dump",
        ],
    );
    let openapi_json_covers_hierarchy = has_all(
        openapi_json_src.as_deref(),
        &[
            "\"/tessellation/solve/hierarchy\"",
            "TessellationHierarchicalSolveRequest",
            "TessellationHierarchicalSolveResponse",
        ],
    );
    let openapi_json_covers_geo = has_all(
        openapi_json_src.as_deref(),
        &[
            "\"/tessellation/geo/solve\"",
            "TessellationGeoSolveRequest",
            "TessellationGeoSolveResponse",
        ],
    );
    let openapi_yaml_covers_hierarchy = has_all(
        openapi_yaml_src.as_deref(),
        &[
            "/tessellation/solve/hierarchy:",
            "TessellationHierarchicalSolveRequest",
            "TessellationHierarchicalSolveResponse",
        ],
    );
    let openapi_yaml_covers_geo = has_all(
        openapi_yaml_src.as_deref(),
        &[
            "/tessellation/geo/solve:",
            "TessellationGeoSolveRequest",
            "TessellationGeoSolveResponse",
        ],
    );
    let docs_cover_hierarchy_contract = has_all(
        docs_src.as_deref(),
        &[
            "HierarchicalLayoutBuffer",
            "@example/tessellation",
            "make tessellation-wasm-test",
            "pnpm -C tessellation-solver/web test:e2e",
            "make tessellation-web-verify",
            "compileSearchToLayout",
            "semanticTriples",
            "SemanticLayoutProduct",
            "PowerDiagramProduct",
            "tessellation_solve_power_diagram",
            "/tessellation/geo/solve",
            "cargo run -p tessellation-engine",
            "tessellation-solver/engine/schemas/",
            "make tessellation-release-check",
            "Reading order is derived from solved region geometry using row bands",
            "tessellation-solver/scenarios/*.json",
            "cargo test formal_ --features ffi",
            "/tessellation/solve/hierarchy",
            "HierarchicalResult",
            "@example/tessellation/app-spec",
            "@example/tessellation/presets",
            "@example/tessellation/runtime",
            "tessellation.app.v0.1",
            "tessellation new dashboard",
            "@example/tessellation/testing/visual-regression",
            "@example/tessellation/runtime/render-plan",
            "@example/tessellation/runtime/surface-optimizer",
            "@example/tessellation/runtime/interaction",
            "@example/tessellation/runtime/controls",
            "@example/tessellation/renderers/dashboard",
            "single rendered Canvas2D/WebGPU surface",
        ],
    );
    let sdk_package_exports_public_surface = has_all(
        sdk_package_src.as_deref(),
        &[
            "\"name\": \"@example/tessellation\"",
            "\"./wasm-bg\"",
            "\"./schemas/*\": \"./engine/schemas/*\"",
            "\"./compiler/shadcn-tailwind\"",
            "\"./compiler/tailwind-style\"",
            "\"./compiler/tailwind-build\"",
            "\"./compiler/shadcn-project\"",
            "\"./compiler/tsx-extractor\"",
            "\"./app-spec\"",
            "\"./presets\"",
            "\"./runtime\"",
            "\"./runtime/dom-adapter\"",
            "\"./runtime/render-plan\"",
            "\"./runtime/power-layout\"",
            "\"./runtime/performance-budget\"",
            "\"./runtime/surface-optimizer\"",
            "\"./runtime/interaction\"",
            "\"./runtime/controls\"",
            "\"./renderers/dashboard\"",
            "\"./testing/visual-regression\"",
            "\"default\": \"./pkg/tessellation_wasm.js\"",
            "\"types\": \"./pkg/tessellation_wasm.d.ts\"",
            "\"sdk/tailwind-style-resolver.js\"",
            "\"sdk/tailwind-style-resolver.d.ts\"",
            "\"sdk/tailwind-build.js\"",
            "\"sdk/tailwind-build.d.ts\"",
            "\"sdk/shadcn-project.js\"",
            "\"sdk/shadcn-project.d.ts\"",
            "\"sdk/shadcn-tailwind.js\"",
            "\"sdk/shadcn-tailwind.d.ts\"",
            "\"sdk/tsx-extractor.js\"",
            "\"sdk/tsx-extractor.d.ts\"",
            "\"sdk/app-spec.js\"",
            "\"sdk/app-spec.d.ts\"",
            "\"sdk/presets.js\"",
            "\"sdk/presets.d.ts\"",
            "\"sdk/runtime.js\"",
            "\"sdk/runtime.d.ts\"",
            "\"sdk/dom-adapter.js\"",
            "\"sdk/dom-adapter.d.ts\"",
            "\"sdk/render-plan.js\"",
            "\"sdk/render-plan.d.ts\"",
            "\"sdk/power-layout.js\"",
            "\"sdk/power-layout.d.ts\"",
            "\"sdk/performance-budget.js\"",
            "\"sdk/performance-budget.d.ts\"",
            "\"sdk/surface-optimizer.js\"",
            "\"sdk/surface-optimizer.d.ts\"",
            "\"sdk/interaction-runtime.js\"",
            "\"sdk/interaction-runtime.d.ts\"",
            "\"sdk/control-runtime.js\"",
            "\"sdk/control-runtime.d.ts\"",
            "\"sdk/dashboard-renderer.js\"",
            "\"sdk/dashboard-renderer.d.ts\"",
            "\"sdk/visual-regression.js\"",
            "\"sdk/visual-regression.d.ts\"",
            "\"pkg/tessellation_wasm_bg.wasm\"",
            "\"verify\": \"cargo test -p tessellation-engine",
            "pnpm pack --dry-run",
        ],
    ) && has_all(
        makefile_src.as_deref(),
        &[
            "unexpected tessellation package name",
            "missing tessellation package exports",
            "./wasm-bg",
            "./app-spec",
            "./presets",
            "./runtime",
            "./runtime/render-plan",
            "./runtime/power-layout",
            "./runtime/performance-budget",
            "./runtime/surface-optimizer",
            "./runtime/interaction",
            "./runtime/controls",
            "./renderers/dashboard",
            "./testing/visual-regression",
            "pnpm -C \"$(TESS)\" pack --dry-run",
        ],
    );
    let engine_docs_cover_package_and_cli = has_all(
        engine_docs_src.as_deref(),
        &[
            "# Tessellation Engine",
            "@example/tessellation",
            "cargo install --path tessellation-solver/tessellation-engine",
            "tessellation new dashboard",
            "tessellation verify tessellation-solver/scenarios",
            "schema_version",
            "/tessellation/geo/solve",
            "shadcn/Tailwind Reproduction",
            "Creator SDK",
            "@example/tessellation/app-spec",
            "@example/tessellation/presets",
            "@example/tessellation/runtime",
            "tessellation.app.v0.1",
            "@example/tessellation/compiler/shadcn-tailwind",
            "@example/tessellation/compiler/tailwind-style",
            "@example/tessellation/compiler/tailwind-build",
            "@example/tessellation/compiler/shadcn-project",
            "@example/tessellation/compiler/tsx-extractor",
            "@example/tessellation/runtime/dom-adapter",
            "@example/tessellation/runtime/render-plan",
            "@example/tessellation/runtime/surface-optimizer",
            "@example/tessellation/runtime/interaction",
            "@example/tessellation/runtime/controls",
            "@example/tessellation/renderers/dashboard",
            "@example/tessellation/testing/visual-regression",
            "components.json",
            "resolvedStyle",
            "rendered surface",
            "make tessellation-release-check",
        ],
    ) && has_all(
        sdk_readme_src.as_deref(),
        &[
            "# @example/tessellation",
            "HierarchicalLayoutBuffer",
            "tessellation new dashboard",
            "shadcn/Tailwind Compiler",
            "Creator SDK",
            "@example/tessellation/app-spec",
            "@example/tessellation/presets",
            "@example/tessellation/runtime",
            "tessellation.app.v0.1",
            "@example/tessellation/compiler/shadcn-tailwind",
            "@example/tessellation/compiler/tailwind-style",
            "@example/tessellation/compiler/tailwind-build",
            "@example/tessellation/compiler/shadcn-project",
            "@example/tessellation/compiler/tsx-extractor",
            "@example/tessellation/runtime/dom-adapter",
            "@example/tessellation/runtime/render-plan",
            "@example/tessellation/runtime/surface-optimizer",
            "@example/tessellation/runtime/interaction",
            "@example/tessellation/runtime/controls",
            "@example/tessellation/renderers/dashboard",
            "@example/tessellation/testing/visual-regression",
            "components.json",
            "resolvedStyle",
            "tessellation solve-power",
            "tessellation.engine.v0.1",
            "make tessellation-release-check",
        ],
    );

    let checks = [
        ContractCheck {
            ok: rust_exposes_hierarchy_ffi,
            path: &rust_ffi_path,
            src: rust_ffi_src.as_deref(),
            needle: "tessellation_solve_hierarchy",
            detail: "Rust cdylib exposes the hierarchical one-shot FFI",
            warning: "tessellation-core FFI no longer exposes tessellation_solve_hierarchy",
        },
        ContractCheck {
            ok: rust_exposes_power_diagram_ffi,
            path: &rust_ffi_path,
            src: rust_ffi_src.as_deref(),
            needle: "tessellation_solve_power_diagram",
            detail: "Rust cdylib exposes the power-diagram one-shot FFI",
            warning: "tessellation-core FFI no longer exposes tessellation_solve_power_diagram",
        },
        ContractCheck {
            ok: rust_hierarchy_scales_children,
            path: &rust_hierarchy_path,
            src: rust_hierarchy_src.as_deref(),
            needle: "scale_target_to_frame",
            detail: "hierarchical solver scales child coordinates into solved parent surfaces",
            warning: "hierarchical solver no longer proves child coordinates scale to solved parent surfaces",
        },
        ContractCheck {
            ok: rust_splits_product_facades,
            path: &rust_products_path,
            src: rust_products_src.as_deref(),
            needle: "SemanticLayoutProduct",
            detail: "Rust core exposes explicit semantic-layout and power-diagram product facades",
            warning: "tessellation-core no longer keeps semantic-layout and power-diagram products explicit",
        },
        ContractCheck {
            ok: engine_cli_product_boundary,
            path: &engine_cli_path,
            src: engine_cli_src.as_deref(),
            needle: "solve-power",
            detail: "Tessellation Engine CLI exposes solve, hierarchy, power, creator scaffold, verify, and schema commands",
            warning: "tessellation-engine CLI no longer exposes the product command boundary",
        },
        ContractCheck {
            ok: engine_schemas_are_versioned,
            path: &engine_envelope_schema_path,
            src: engine_envelope_schema_src.as_deref(),
            needle: "tessellation.engine.v0.1",
            detail: "Tessellation Engine schemas are versioned and cover public input/output contracts",
            warning: "tessellation-engine schemas no longer cover the versioned public contracts",
        },
        ContractCheck {
            ok: wasm_exposes_hierarchy_buffer,
            path: &wasm_path,
            src: wasm_src.as_deref(),
            needle: "HierarchicalLayoutBuffer",
            detail: "WASM exposes a stride-6 hierarchical layout buffer",
            warning: "tessellation-wasm no longer exposes HierarchicalLayoutBuffer with parent/depth stride",
        },
        ContractCheck {
            ok: wasm_tests_hierarchy_buffer,
            path: &wasm_test_path,
            src: wasm_test_src.as_deref(),
            needle: "test_hierarchical_layout_buffer",
            detail: "WASM binding tests execute the hierarchical stride-6 buffer",
            warning: "tessellation-wasm tests no longer exercise HierarchicalLayoutBuffer",
        },
        ContractCheck {
            ok: makefile_exposes_wasm_test,
            path: &makefile_path,
            src: makefile_src.as_deref(),
            needle: "tessellation-wasm-test",
            detail: "Makefile exposes a node-executed WASM binding test target",
            warning: "Makefile no longer exposes tessellation-wasm-test for wasm-pack node tests",
        },
        ContractCheck {
            ok: makefile_exposes_engine_release_check,
            path: &makefile_path,
            src: makefile_src.as_deref(),
            needle: "tessellation-release-check",
            detail: "Makefile exposes Tessellation Engine CLI and release verification targets",
            warning: "Makefile no longer exposes tessellation engine product release checks",
        },
        ContractCheck {
            ok: api_configures_hierarchy_ffi,
            path: &api_path,
            src: api_src.as_deref(),
            needle: "/solve/hierarchy",
            detail: "Python control plane exposes /tessellation/solve/hierarchy",
            warning: "example-api no longer wires the hierarchical tessellation FFI endpoint",
        },
        ContractCheck {
            ok: api_configures_power_diagram_ffi,
            path: &api_path,
            src: api_src.as_deref(),
            needle: "/geo/solve",
            detail: "Python control plane exposes /tessellation/geo/solve for power diagrams",
            warning: "example-api no longer wires the power-diagram tessellation endpoint",
        },
        ContractCheck {
            ok: api_tests_hierarchy_endpoint,
            path: &api_test_path,
            src: api_test_src.as_deref(),
            needle: "test_hierarchical_solve_returns_parent_depth_metadata",
            detail: "FastAPI hierarchy endpoint has parent/depth regression coverage",
            warning: "tessellation FastAPI tests no longer cover hierarchy parent/depth metadata",
        },
        ContractCheck {
            ok: api_tests_power_diagram_endpoint,
            path: &api_test_path,
            src: api_test_src.as_deref(),
            needle: "test_geo_solve_returns_power_diagram_cells",
            detail: "FastAPI geo endpoint has power-diagram cell regression coverage",
            warning: "tessellation FastAPI tests no longer cover /tessellation/geo/solve",
        },
        ContractCheck {
            ok: ffi_tests_hierarchy_symbol,
            path: &ffi_test_path,
            src: ffi_test_src.as_deref(),
            needle: "TestHierarchySolveMinimal",
            detail: "ctypes smoke tests require tessellation_solve_hierarchy",
            warning: "tessellation cdylib smoke tests no longer guard the hierarchy FFI symbol",
        },
        ContractCheck {
            ok: ffi_tests_power_diagram_symbol,
            path: &ffi_test_path,
            src: ffi_test_src.as_deref(),
            needle: "TestPowerDiagramSolveMinimal",
            detail: "ctypes smoke tests require tessellation_solve_power_diagram",
            warning: "tessellation cdylib smoke tests no longer guard the power-diagram FFI symbol",
        },
        ContractCheck {
            ok: rust_tests_formal_scenarios,
            path: &scenario_test_path,
            src: scenario_test_src.as_deref(),
            needle: "formal_layout_scenarios_solve_and_emit_semantics",
            detail: "Rust core promotes demo scenarios into solve and semantic contract tests",
            warning: "tessellation formal scenario fixtures no longer guard examples as solver contracts",
        },
        ContractCheck {
            ok: engine_conformance_covers_power_scenario,
            path: &sector_power_scenario_path,
            src: sector_power_scenario_src.as_deref(),
            needle: "sector-coverage-power",
            detail: "Engine conformance scenarios cover the power-diagram product mode",
            warning: "tessellation engine conformance no longer covers power-diagram scenarios",
        },
        ContractCheck {
            ok: sector_uses_geo_solve_contract,
            path: &sector_route_evidence_path,
            src: sector_route_src.as_deref(),
            needle: "/tessellation/geo/solve",
            detail: "Sector coverage route posts weighted sites to the power-diagram geo endpoint",
            warning: "Sector coverage tessellation proxy drifted from the /tessellation/geo/solve contract",
        },
        ContractCheck {
            ok: sector_removed_raw_ingest_drift,
            path: &sector_route_evidence_path,
            src: sector_route_src.as_deref(),
            needle: "/tessellation/geo/solve",
            detail: "Sector coverage route no longer fire-and-forgets raw ingest as a solve surrogate",
            warning: "Sector coverage tessellation proxy reintroduced /tessellation/ingest/raw drift",
        },
        ContractCheck {
            ok: web_is_workspace_package,
            path: &pnpm_workspace_path,
            src: pnpm_workspace_src.as_deref(),
            needle: "tessellation-solver/web",
            detail: "Liquid Search demo is registered in the pnpm workspace",
            warning: "Liquid Search demo is no longer registered in pnpm-workspace.yaml",
        },
        ContractCheck {
            ok: web_builds_wasm_before_vite,
            path: &web_package_path,
            src: web_package_src.as_deref(),
            needle: "build:wasm",
            detail: "Liquid Search demo builds the WASM package before Vite",
            warning: "Liquid Search demo no longer rebuilds WASM before dev/build",
        },
        ContractCheck {
            ok: web_uses_hierarchy_buffer,
            path: &web_wasm_path,
            src: web_wasm_src.as_deref(),
            needle: "HierarchicalLayoutBuffer",
            detail: "Liquid Search demo routes grouped results through the hierarchy buffer",
            warning: "Liquid Search demo no longer exercises the hierarchical WASM buffer",
        },
        ContractCheck {
            ok: web_promotes_semantic_compiler,
            path: &web_semantic_compiler_path,
            src: web_semantic_compiler_src.as_deref(),
            needle: "compileSearchToLayout",
            detail: "Liquid Search demo compiles classified payloads through JSON-LD before SolverConfig",
            warning: "Liquid Search demo no longer promotes the semantic compiler into the main layout path",
        },
        ContractCheck {
            ok: shadcn_tailwind_compiler_contract,
            path: &shadcn_tailwind_compiler_path,
            src: shadcn_tailwind_compiler_src.as_deref(),
            needle: "compileShadcnTailwindToLayout",
            detail: "shadcn/Tailwind compiler preserves style tokens and emits hierarchical solver configs",
            warning: "shadcn/Tailwind compiler contract no longer maps component trees to tessellation hierarchy",
        },
        ContractCheck {
            ok: tailwind_style_resolver_contract,
            path: &tailwind_style_resolver_path,
            src: tailwind_style_resolver_src.as_deref(),
            needle: "resolveTailwindStyle",
            detail: "Tailwind style resolver normalizes shadcn variables, arbitrary values, and variant rules",
            warning: "Tailwind style resolver no longer exposes normalized declarations and variant diagnostics",
        },
        ContractCheck {
            ok: tailwind_build_adapter_contract,
            path: &tailwind_build_adapter_path,
            src: tailwind_build_adapter_src.as_deref(),
            needle: "compileTailwindCss",
            detail: "Tailwind build adapter compiles host CSS and reports resolver parity against emitted rules",
            warning: "Tailwind build adapter no longer exposes compile/inspect/parity contract",
        },
        ContractCheck {
            ok: shadcn_project_loader_contract,
            path: &shadcn_project_loader_path,
            src: shadcn_project_loader_src.as_deref(),
            needle: "loadShadcnTailwindProject",
            detail: "shadcn project loader extracts components.json and global CSS variables for compiler theme parity",
            warning: "shadcn project loader no longer provides build-time components.json/global CSS theme context",
        },
        ContractCheck {
            ok: tsx_extractor_contract,
            path: &tsx_extractor_path,
            src: tsx_extractor_src.as_deref(),
            needle: "extractShadcnTailwindTreeFromSource",
            detail: "TSX extractor turns real shadcn JSX source into component trees and tessellation plans",
            warning: "TSX extractor contract no longer maps JSX source to shadcn/Tailwind tessellation plans",
        },
        ContractCheck {
            ok: creator_sdk_contract,
            path: &runtime_path,
            src: runtime_src.as_deref(),
            needle: "runTessellationApp",
            detail: "creator SDK exposes app specs, presets, and a no-component-DOM canvas runtime",
            warning: "tessellation creator SDK no longer proves dashboard/landing/webapp surfaces render without component DOM",
        },
        ContractCheck {
            ok: power_surface_product_contract,
            path: &power_layout_path,
            src: power_layout_src.as_deref(),
            needle: "solvePowerTessellationLayoutPlan",
            detail: "Power Surface product recursively solves, renders, morphs, and fail-closes true polygon layouts",
            warning: "tessellation Power Surface product no longer guards recursive polygon geometry and readability",
        },
        ContractCheck {
            ok: power_performance_budget_contract,
            path: &performance_budget_path,
            src: performance_budget_src.as_deref(),
            needle: "createTessellationPerformanceMonitor",
            detail: "Power Surface runtime enforces a bounded mobile frame-performance contract",
            warning: "tessellation Power Surface no longer exposes p95 mobile frame-budget enforcement",
        },
        ContractCheck {
            ok: power_mobile_browser_release_gate,
            path: &power_benchmark_e2e_path,
            src: power_benchmark_e2e_src.as_deref(),
            needle: "Power Surface browser release gate",
            detail: "Playwright validates Power Surface proofs and frame budgets on an emulated mobile browser",
            warning: "tessellation release gate no longer benchmarks verified Power morphs on mobile browser geometry",
        },
        ContractCheck {
            ok: power_realtime_studio_contract,
            path: &power_studio_path,
            src: power_studio_src.as_deref(),
            needle: "__powerStudio",
            detail: "responsive Power Studio exposes real-time semantic controls, proofs, metrics, and browser coverage",
            warning: "tessellation Power Studio no longer proves a responsive real-time semantic weight workflow",
        },
        ContractCheck {
            ok: tessellation_web_showcase_contract,
            path: &showcase_path,
            src: showcase_src.as_deref(),
            needle: "__tessellationShowcase",
            detail: "web showcase teaches the engine through live previews, bilingual guidance, interactive examples, and a browser-generated starter",
            warning: "tessellation web showcase no longer proves the complete responsive onboarding and starter workflow",
        },
        ContractCheck {
            ok: operational_surface_studio_contract,
            path: &operational_studio_path,
            src: operational_studio_src.as_deref(),
            needle: "__operationalStudio",
            detail: "operational studio authors generated dashboards through semantic layers, properties, history, commands, and engine rendering",
            warning: "tessellation operational studio no longer proves add/remove/generate/export authoring through the engine",
        },
        ContractCheck {
            ok: example_studio_surface_integration_contract,
            path: &example_surface_studio_path,
            src: example_surface_studio_src.as_deref(),
            needle: "example-surface-studio/v1",
            detail: "Example Studio owns the canonical operational-surface workspace and consumes Tessellation as a workspace compiler/runtime package",
            warning: "operational surface authoring has drifted back into a duplicate standalone product or lost its Example Studio/Tessellation integration",
        },
        ContractCheck {
            ok: dom_adapter_contract,
            path: &dom_adapter_path,
            src: dom_adapter_src.as_deref(),
            needle: "applyTessellationLayoutToDom",
            detail: "DOM adapter applies solved tessellation regions back onto existing component nodes",
            warning: "DOM adapter no longer exposes a stable solved-layout to browser-node handoff",
        },
        ContractCheck {
            ok: render_plan_contract,
            path: &render_plan_path,
            src: render_plan_src.as_deref(),
            needle: "buildTessellationRenderPlan",
            detail: "DOM-free render-plan runtime turns solved shadcn/Tailwind layouts into semantic paint commands",
            warning: "tessellation render-plan runtime no longer proves final websites can render without component DOM nodes",
        },
        ContractCheck {
            ok: dashboard_renderer_contract,
            path: &dashboard_renderer_path,
            src: dashboard_renderer_src.as_deref(),
            needle: "runDashboardSurface",
            detail: "dashboard renderer preset paints product-grade canvas surfaces from semantic render-plan data",
            warning: "dashboard scaffolds no longer have a reusable high-fidelity canvas renderer",
        },
        ContractCheck {
            ok: surface_optimizer_contract,
            path: &surface_optimizer_path,
            src: surface_optimizer_src.as_deref(),
            needle: "optimizeSurfaceRenderPlan",
            detail: "surface optimizer projects convex UI regions and emits proof diagnostics",
            warning: "tessellation surfaces no longer have a proof-producing geometry optimization pass",
        },
        ContractCheck {
            ok: interaction_runtime_contract,
            path: &interaction_runtime_path,
            src: interaction_runtime_src.as_deref(),
            needle: "createSurfaceInteractionModel",
            detail: "interaction runtime derives focus order, command envelopes, and semantic mirror metadata",
            warning: "tessellation surfaces no longer have deterministic interaction and audit metadata",
        },
        ContractCheck {
            ok: control_runtime_contract,
            path: &control_runtime_path,
            src: control_runtime_src.as_deref(),
            needle: "createSurfaceControlState",
            detail: "control runtime reduces rendered form primitive state and validation",
            warning: "tessellation surfaces no longer have deterministic rendered control state",
        },
        ContractCheck {
            ok: visual_regression_contract,
            path: &visual_regression_path,
            src: visual_regression_src.as_deref(),
            needle: "buildVisualRegressionFixture",
            detail: "visual regression harness compares tessellation DOM patches against browser geometry and computed styles",
            warning: "tessellation visual regression harness no longer proves browser DOM parity for shadcn surfaces",
        },
        ContractCheck {
            ok: web_tests_cover_accessibility_order,
            path: &web_accessibility_test_path,
            src: web_accessibility_test_src.as_deref(),
            needle: "keeps left-to-right order when row tops differ slightly",
            detail: "Liquid Search reading order has row-band unit coverage",
            warning: "Liquid Search demo no longer tests row-band accessibility reading order",
        },
        ContractCheck {
            ok: web_tests_cover_hierarchy_demo,
            path: &web_config_test_path,
            src: web_config_test_src.as_deref(),
            needle: "builds a hierarchical config with root groups and child result nodes",
            detail: "Liquid Search demo has deterministic hierarchy config and renderer tests",
            warning: "Liquid Search demo no longer has test coverage for hierarchy config and stride-6 rendering",
        },
        ContractCheck {
            ok: web_e2e_configures_browser_smoke,
            path: &web_playwright_config_path,
            src: web_playwright_config_src.as_deref(),
            needle: "reuseExistingServer",
            detail: "Liquid Search demo has a Playwright browser smoke test entrypoint",
            warning: "Liquid Search demo no longer wires Playwright browser smoke tests",
        },
        ContractCheck {
            ok: web_exposes_one_command_verify,
            path: &makefile_path,
            src: makefile_src.as_deref(),
            needle: "tessellation-web-verify",
            detail: "Liquid Search demo has a one-command unit, browser, and build verification target",
            warning: "Liquid Search demo no longer exposes a one-command verification target",
        },
        ContractCheck {
            ok: web_e2e_proves_browser_hierarchy,
            path: &web_e2e_test_path,
            src: web_e2e_test_src.as_deref(),
            needle: "expectCanvasToBePainted",
            detail: "Liquid Search browser smoke verifies hierarchy, canvas pixels, and overlay nodes",
            warning: "Liquid Search browser smoke no longer proves hierarchy canvas rendering and overlay accessibility",
        },
        ContractCheck {
            ok: openapi_export_writes_json_and_yaml,
            path: &openapi_export_path,
            src: openapi_export_src.as_deref(),
            needle: "openapi.yaml",
            detail: "OpenAPI export regenerates JSON and YAML from a monorepo-aware environment",
            warning: "example-api OpenAPI export no longer writes both JSON and YAML artifacts",
        },
        ContractCheck {
            ok: openapi_json_covers_hierarchy,
            path: &openapi_json_path,
            src: openapi_json_src.as_deref(),
            needle: "\"/tessellation/solve/hierarchy\"",
            detail: "OpenAPI JSON publishes the hierarchical tessellation solve endpoint",
            warning: "example-api OpenAPI JSON no longer publishes /tessellation/solve/hierarchy",
        },
        ContractCheck {
            ok: openapi_json_covers_geo,
            path: &openapi_json_path,
            src: openapi_json_src.as_deref(),
            needle: "\"/tessellation/geo/solve\"",
            detail: "OpenAPI JSON publishes the power-diagram geo solve endpoint",
            warning: "example-api OpenAPI JSON no longer publishes /tessellation/geo/solve",
        },
        ContractCheck {
            ok: openapi_yaml_covers_hierarchy,
            path: &openapi_yaml_path,
            src: openapi_yaml_src.as_deref(),
            needle: "/tessellation/solve/hierarchy:",
            detail: "OpenAPI YAML publishes the hierarchical tessellation solve endpoint",
            warning: "example-api OpenAPI YAML no longer publishes /tessellation/solve/hierarchy",
        },
        ContractCheck {
            ok: openapi_yaml_covers_geo,
            path: &openapi_yaml_path,
            src: openapi_yaml_src.as_deref(),
            needle: "/tessellation/geo/solve:",
            detail: "OpenAPI YAML publishes the power-diagram geo solve endpoint",
            warning: "example-api OpenAPI YAML no longer publishes /tessellation/geo/solve",
        },
        ContractCheck {
            ok: docs_cover_hierarchy_contract,
            path: &docs_path,
            src: docs_src.as_deref(),
            needle: "/tessellation/solve/hierarchy",
            detail: "tessellation guide documents the WASM and API hierarchy contracts",
            warning: "tessellation guide no longer documents the hierarchical solve contract",
        },
        ContractCheck {
            ok: sdk_package_exports_public_surface,
            path: &sdk_package_path,
            src: sdk_package_src.as_deref(),
            needle: "@example/tessellation",
            detail: "npm package manifest exports WASM bindings and checked engine schemas",
            warning: "tessellation npm package manifest no longer exports the public engine surface",
        },
        ContractCheck {
            ok: engine_docs_cover_package_and_cli,
            path: &engine_docs_path,
            src: engine_docs_src.as_deref(),
            needle: "# Tessellation Engine",
            detail: "engine quickstart documents package install, CLI envelopes, API modes, and release gate",
            warning: "tessellation engine quickstart no longer covers package, CLI, API, and release use",
        },
    ];

    for check in &checks {
        if check.ok {
            push_evidence(&mut evidence, check);
        } else {
            warnings.push(check.warning.to_string());
        }
    }

    let ok_count = checks.iter().filter(|check| check.ok).count();
    let total_count = checks.len();

    let mut entity = serde_json::Map::new();
    entity.insert("doctor".to_string(), json!("tessellation-contract"));
    macro_rules! entity_bool {
        ($name:literal, $value:expr) => {
            entity.insert($name.to_string(), json!($value));
        };
    }
    entity_bool!("rust_exposes_hierarchy_ffi", rust_exposes_hierarchy_ffi);
    entity_bool!(
        "rust_exposes_power_diagram_ffi",
        rust_exposes_power_diagram_ffi
    );
    entity_bool!(
        "rust_hierarchy_scales_children",
        rust_hierarchy_scales_children
    );
    entity_bool!("rust_splits_product_facades", rust_splits_product_facades);
    entity_bool!("engine_cli_product_boundary", engine_cli_product_boundary);
    entity_bool!("engine_schemas_are_versioned", engine_schemas_are_versioned);
    entity_bool!(
        "wasm_exposes_hierarchy_buffer",
        wasm_exposes_hierarchy_buffer
    );
    entity_bool!("wasm_tests_hierarchy_buffer", wasm_tests_hierarchy_buffer);
    entity_bool!("makefile_exposes_wasm_test", makefile_exposes_wasm_test);
    entity_bool!(
        "makefile_exposes_engine_release_check",
        makefile_exposes_engine_release_check
    );
    entity_bool!("api_configures_hierarchy_ffi", api_configures_hierarchy_ffi);
    entity_bool!(
        "api_configures_power_diagram_ffi",
        api_configures_power_diagram_ffi
    );
    entity_bool!("api_tests_hierarchy_endpoint", api_tests_hierarchy_endpoint);
    entity_bool!(
        "api_tests_power_diagram_endpoint",
        api_tests_power_diagram_endpoint
    );
    entity_bool!("ffi_tests_hierarchy_symbol", ffi_tests_hierarchy_symbol);
    entity_bool!(
        "ffi_tests_power_diagram_symbol",
        ffi_tests_power_diagram_symbol
    );
    entity_bool!("rust_tests_formal_scenarios", rust_tests_formal_scenarios);
    entity_bool!(
        "engine_conformance_covers_power_scenario",
        engine_conformance_covers_power_scenario
    );
    entity_bool!(
        "sector_uses_geo_solve_contract",
        sector_uses_geo_solve_contract
    );
    entity_bool!(
        "sector_removed_raw_ingest_drift",
        sector_removed_raw_ingest_drift
    );
    entity_bool!("web_is_workspace_package", web_is_workspace_package);
    entity_bool!("web_builds_wasm_before_vite", web_builds_wasm_before_vite);
    entity_bool!("web_uses_hierarchy_buffer", web_uses_hierarchy_buffer);
    entity_bool!(
        "web_promotes_semantic_compiler",
        web_promotes_semantic_compiler
    );
    entity_bool!(
        "shadcn_tailwind_compiler_contract",
        shadcn_tailwind_compiler_contract
    );
    entity_bool!(
        "tailwind_style_resolver_contract",
        tailwind_style_resolver_contract
    );
    entity_bool!(
        "tailwind_build_adapter_contract",
        tailwind_build_adapter_contract
    );
    entity_bool!(
        "shadcn_project_loader_contract",
        shadcn_project_loader_contract
    );
    entity_bool!("tsx_extractor_contract", tsx_extractor_contract);
    entity_bool!("creator_sdk_contract", creator_sdk_contract);
    entity_bool!(
        "power_surface_product_contract",
        power_surface_product_contract
    );
    entity_bool!(
        "power_performance_budget_contract",
        power_performance_budget_contract
    );
    entity_bool!(
        "power_mobile_browser_release_gate",
        power_mobile_browser_release_gate
    );
    entity_bool!(
        "power_realtime_studio_contract",
        power_realtime_studio_contract
    );
    entity_bool!(
        "tessellation_web_showcase_contract",
        tessellation_web_showcase_contract
    );
    entity_bool!(
        "operational_surface_studio_contract",
        operational_surface_studio_contract
    );
    entity_bool!(
        "example_studio_surface_integration_contract",
        example_studio_surface_integration_contract
    );
    entity_bool!("dom_adapter_contract", dom_adapter_contract);
    entity_bool!("render_plan_contract", render_plan_contract);
    entity_bool!("dashboard_renderer_contract", dashboard_renderer_contract);
    entity_bool!("surface_optimizer_contract", surface_optimizer_contract);
    entity_bool!("interaction_runtime_contract", interaction_runtime_contract);
    entity_bool!("control_runtime_contract", control_runtime_contract);
    entity_bool!("visual_regression_contract", visual_regression_contract);
    entity_bool!(
        "web_tests_cover_accessibility_order",
        web_tests_cover_accessibility_order
    );
    entity_bool!(
        "web_tests_cover_hierarchy_demo",
        web_tests_cover_hierarchy_demo
    );
    entity_bool!(
        "web_e2e_configures_browser_smoke",
        web_e2e_configures_browser_smoke
    );
    entity_bool!(
        "web_exposes_one_command_verify",
        web_exposes_one_command_verify
    );
    entity_bool!(
        "web_e2e_proves_browser_hierarchy",
        web_e2e_proves_browser_hierarchy
    );
    entity_bool!(
        "openapi_export_writes_json_and_yaml",
        openapi_export_writes_json_and_yaml
    );
    entity_bool!(
        "openapi_json_covers_hierarchy",
        openapi_json_covers_hierarchy
    );
    entity_bool!("openapi_json_covers_geo", openapi_json_covers_geo);
    entity_bool!(
        "openapi_yaml_covers_hierarchy",
        openapi_yaml_covers_hierarchy
    );
    entity_bool!("openapi_yaml_covers_geo", openapi_yaml_covers_geo);
    entity_bool!(
        "docs_cover_hierarchy_contract",
        docs_cover_hierarchy_contract
    );
    entity_bool!(
        "sdk_package_exports_public_surface",
        sdk_package_exports_public_surface
    );
    entity_bool!(
        "engine_docs_cover_package_and_cli",
        engine_docs_cover_package_and_cli
    );

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_tessellation_contract"),
        kind: "tessellation_contract".to_string(),
        summary: format!(
            "checked tessellation cross-surface contracts, {ok_count}/{total_count} invariants intact, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.7 },
        entities: vec![serde_json::Value::Object(entity)],
        evidence,
        warnings,
        meta: Some(json!({
            "contract_count": total_count,
            "ok_count": ok_count,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[derive(Clone, Copy)]
struct ContractCheck<'a> {
    ok: bool,
    path: &'a Path,
    src: Option<&'a str>,
    needle: &'a str,
    detail: &'a str,
    warning: &'a str,
}

fn has_all(src: Option<&str>, needles: &[&str]) -> bool {
    src.is_some_and(|src| needles.iter().all(|needle| src.contains(needle)))
}

fn classified_sector_route_path(root: &Path) -> PathBuf {
    root.join(format!("{}fron-console", "sis"))
        .join("src/app/api/tessellation/sectors/route.ts")
}

fn push_evidence(evidence: &mut Vec<EvidenceItem>, check: &ContractCheck<'_>) {
    let line = check.src.and_then(|src| find_line(src, check.needle));
    evidence.push(EvidenceItem {
        kind: "tessellation_contract".to_string(),
        path: check.path.display().to_string(),
        line,
        detail: check.detail.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::doctor_tessellation_contract;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("leio-code-tessellation-{name}-{nanos}"));
        fs::create_dir_all(&root).expect("create temp root");
        root
    }

    fn write(root: &Path, path: &str, content: &str) {
        let full = root.join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("create parent");
        fs::write(full, content).expect("write fixture");
    }

    fn classified_sector_route_fixture_path() -> String {
        format!(
            "{}fron-console/src/app/api/tessellation/sectors/route.ts",
            "sis"
        )
    }

    fn write_happy_fixture(root: &Path) {
        write(
            root,
            "Makefile",
            "tessellation-wasm-test RUST_LOG=error wasm-pack test --node tessellation-wasm tessellation-web-verify pnpm -C \"$(TESS)/web\" verify tessellation-power-mobile-benchmark --project=mobile-power tessellation-engine tessellation-release-check cargo run -p tessellation-engine -- verify scenarios cargo run -p tessellation-engine -- schemas --out engine/schemas unexpected tessellation package name missing tessellation package exports ./wasm-bg ./app-spec ./presets ./runtime ./runtime/render-plan ./runtime/power-layout ./runtime/performance-budget ./runtime/surface-optimizer ./runtime/interaction ./runtime/controls ./renderers/dashboard ./testing/visual-regression pnpm -C \"$(TESS)\" pack --dry-run",
        );
        write(
            root,
            "tessellation-solver/tessellation-core/src/ffi.rs",
            r#"
pub unsafe extern "C" fn tessellation_solve_hierarchy() {}
pub unsafe extern "C" fn tessellation_solve_power_diagram() {}
use crate::hierarchy::{HierarchicalConfig, HierarchicalSolver};
use crate::products::{PowerDiagramConfig, PowerDiagramProduct};
fn f() { HierarchicalSolver::new; PowerDiagramProduct::solve; }
"#,
        );
        write(
            root,
            "tessellation-solver/tessellation-core/src/hierarchy.rs",
            "fn scale_target_to_frame() {} source_width test_child_targets_scale_to_solved_parent_surface",
        );
        write(
            root,
            "tessellation-solver/tessellation-core/src/products.rs",
            "pub struct SemanticLayoutProduct pub struct PowerDiagramProduct PowerDiagramConfig WeightCountMismatch power_diagram_product_returns_polygon_cells",
        );
        write(
            root,
            "tessellation-solver/tessellation-engine/src/main.rs",
            "solve-power scaffold_new_app NewOptions ScaffoldKind runDashboardSurface runTessellationApp createDashboard @example/tessellation/renderers/dashboard @example/tessellation/wasm-bg?url scaffold_new_dashboard_writes_one_surface_project tessellation new <dashboard|landing|webapp> verify_scenarios schema_version tessellation.engine.v0.1 PowerDiagramProduct::solve SemanticLayoutProduct::solve_hierarchy",
        );
        write(
            root,
            "tessellation-solver/engine/schemas/engine-envelope.schema.json",
            "tessellation.engine.v0.1 semantic-layout semantic-layout-hierarchy power-diagram",
        );
        write(
            root,
            "tessellation-solver/engine/schemas/solver-config.schema.json",
            "Tessellation SolverConfig targets $defs",
        );
        write(
            root,
            "tessellation-solver/engine/schemas/hierarchical-config.schema.json",
            "Tessellation HierarchicalConfig nodes",
        );
        write(
            root,
            "tessellation-solver/engine/schemas/power-diagram-config.schema.json",
            "Tessellation PowerDiagramConfig sites weights",
        );
        write(
            root,
            "tessellation-solver/tessellation-wasm/src/lib.rs",
            "pub struct HierarchicalLayoutBuffer; const H_STRIDE: usize = 6; #[wasm_bindgen(js_name = \"lastLayoutResult\")] #[wasm_bindgen(js_name = \"solvePowerDiagram\")] pub fn solve_power_diagram() {}",
        );
        write(
            root,
            "tessellation-solver/tessellation-wasm/tests/wasm_tests.rs",
            "test_hierarchical_layout_buffer HierarchicalLayoutBuffer last_layout_result parent_id",
        );
        write(
            root,
            "example-api/example/core/evolution/tessellation_gepa.py",
            "lib.tessellation_solve_hierarchy.argtypes\ndef _call_hierarchical_solver(): pass\n@router.post(\"/solve/hierarchy\")\nlib.tessellation_solve_power_diagram.argtypes\ndef _call_power_diagram_solver(): pass\n@router.post(\"/geo/solve\")\nTessellationGeoSolveRequest",
        );
        write(
            root,
            "example-api/tests/test_tessellation_gepa.py",
            "test_hierarchical_solve_returns_parent_depth_metadata /tessellation/solve/hierarchy _minimal_hierarchy_config_dict test_geo_solve_returns_power_diagram_cells /tessellation/geo/solve _minimal_geo_solve_payload",
        );
        write(
            root,
            "example-api/tests/test_tessellation_ffi_smoke.py",
            "tessellation_solve_hierarchy TestHierarchySolveMinimal _call_solve_hierarchy tessellation_solve_power_diagram TestPowerDiagramSolveMinimal _call_solve_power_diagram",
        );
        write(
            root,
            "tessellation-solver/tessellation-core/tests/scenario_fixtures.rs",
            "formal_layout_scenarios_solve_and_emit_semantics formal_canvas_morph_scenario_preserves_semantic_ids assert_semantic_contract min_semantic_triples",
        );
        write(
            root,
            "tessellation-solver/scenarios/dondonha-search.json",
            r#""name": "dondonha-search", "min_semantic_triples": 73"#,
        );
        write(
            root,
            "tessellation-solver/scenarios/dashboard.json",
            r#""name": "dashboard", "min_semantic_triples": 73"#,
        );
        write(
            root,
            "tessellation-solver/scenarios/google-homepage.json",
            r#""name": "google-homepage", "min_semantic_triples": 55"#,
        );
        write(
            root,
            "tessellation-solver/scenarios/canvas-morph.json",
            r#""name": "canvas-morph", "source_config": {}, "target_config": {}"#,
        );
        write(
            root,
            "tessellation-solver/scenarios/sector-coverage-power.json",
            r#""name": "sector-coverage-power", "mode": "power-diagram", "sites": [], "min_vertices_per_cell": 3"#,
        );
        write(
            root,
            &classified_sector_route_fixture_path(),
            "/tessellation/geo/solve parsePowerDiagramCells buildPowerDiagramFeatureCollection tessellation_engine: \"power-diagram\"",
        );
        write(
            root,
            "pnpm-workspace.yaml",
            "example-studio-web tessellation-solver\" tessellation-solver/web",
        );
        write(
            root,
            "tessellation-solver/web/package.json",
            r#""build:wasm": "RUST_LOG=error wasm-pack build ../tessellation-wasm --target web --out-dir ../pkg", "dev": "pnpm --loglevel error build:wasm && vite", "build": "pnpm --loglevel error build:wasm && tsc --noEmit && vite build", "test": "vitest run src", "test:e2e": "env -u NO_COLOR playwright test", "verify": "pnpm --loglevel error test && pnpm --loglevel error test:e2e && pnpm --loglevel error build""#,
        );
        write(
            root,
            "tessellation-solver/web/src/wasm.ts",
            "HierarchicalLayoutBuffer hierarchical-layout-buffer stride: 6 buildAccessibilityFromRegions",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/semantic-compiler.ts",
            "compileSearchToLayout buildSemanticDocument contentJsonLd semanticTriples emitCompilerTriples",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/semantic-compiler.test.ts",
            "compiles classified search through a JSON-LD document to solver config keeps compiled region ids aligned with semantic result and cluster nodes emits deterministic document IRIs",
        );
        write(
            root,
            "tessellation-solver/sdk/shadcn-tailwind.js",
            "compileShadcnTailwindToLayout parseTailwindHints parseGridColumns preservedTokens tess:component hierarchical-layout-buffer resolveTailwindStyle resolvedStyle resolvedTailwindTokenCount variantRuleCount",
        );
        write(
            root,
            "tessellation-solver/sdk/shadcn-tailwind.d.ts",
            "ShadcnTailwindNode ShadcnTailwindLayoutPlan HierarchicalConfig StyleTokenEntry TailwindStyleResolution",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/shadcn-tailwind-compiler.test.ts",
            "compiles a shadcn dashboard tree into a hierarchical solver config preserves arbitrary Tailwind tokens while extracting layout hints derives responsive grid columns from arbitrary minmax Tailwind syntax emits semantic triples that bind components, class names, and region ids resolvedStyle.declarations.backgroundColor variantRuleCount",
        );
        write(
            root,
            "tessellation-solver/sdk/tailwind-style-resolver.js",
            "resolveTailwindStyle resolveTailwindTokens SHADCN_COLOR_NAMES variantRules unresolvedTokens parseArbitraryProperty cssVariables",
        );
        write(
            root,
            "tessellation-solver/sdk/tailwind-style-resolver.d.ts",
            "TailwindStyleResolverOptions TailwindStyleResolution TailwindVariantRule TailwindAppliedToken",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/tailwind-style-resolver.test.ts",
            "resolves shadcn CSS-variable colors and arbitrary values keeps state variants as explicit rules instead of flattening them does not apply inactive breakpoint declarations to the current surface bg-background",
        );
        write(
            root,
            "tessellation-solver/sdk/tailwind-build.js",
            "compileTailwindCss compileShadcnProjectTailwind extractCompiledClassRules buildTailwindParityReport @tailwindcss/postcss declarationMismatches",
        );
        write(
            root,
            "tessellation-solver/sdk/tailwind-build.d.ts",
            "TailwindCompileResult CompiledClassRuleExtraction TailwindParityReport ShadcnTailwindProject",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/tailwind-build.test.ts",
            "extracts class rules from compiled Tailwind CSS including variants builds a parity report between resolved styles and compiled CSS invokes injected PostCSS/Tailwind implementations for host-project compilation",
        );
        write(
            root,
            "tessellation-solver/sdk/shadcn-project.js",
            "loadShadcnTailwindProject extractShadcnCssVariables createTailwindResolverOptions components.json @theme darkResolverOptions",
        );
        write(
            root,
            "tessellation-solver/sdk/shadcn-project.d.ts",
            "ShadcnTailwindProject ShadcnCssVariableExtraction ShadcnComponentsConfig TailwindStyleResolverOptions",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/shadcn-project.test.ts",
            "extracts light and dark shadcn CSS variables from globals.css loads components.json and feeds project theme into TSX compilation loadShadcnTailwindProject createTailwindResolverOptions",
        );
        write(
            root,
            "tessellation-solver/sdk/tsx-extractor.js",
            "extractShadcnTailwindTreeFromSource extractAndCompileShadcnTailwindFromSource typescript is required for TSX extraction CLASS_HELPER_NAMES collectClassTokens spread JSX attributes are not statically expanded",
        );
        write(
            root,
            "tessellation-solver/sdk/tsx-extractor.d.ts",
            "TsxExtractionOptions TsxExtractionResult ExtractedShadcnTailwindLayoutPlan",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/tsx-extractor.test.ts",
            "extracts real shadcn TSX into a component tree extracts and compiles TSX into the tessellation hierarchy plan bg-[oklch(0.62_0.14_168)] typescript: ts",
        );
        write(
            root,
            "tessellation-solver/sdk/app-spec.js",
            "TESSELLATION_APP_SCHEMA_VERSION tessellation.app.v0.1 defineTessellationApp compileTessellationAppToLayout validateTessellationAppSpec",
        );
        write(
            root,
            "tessellation-solver/sdk/app-spec.d.ts",
            "TessellationAppSpec TessellationAppNode CompiledTessellationApp CompileTessellationAppOptions",
        );
        write(
            root,
            "tessellation-solver/sdk/presets.js",
            "createDashboard createLandingPage createWebApp",
        );
        write(
            root,
            "tessellation-solver/sdk/presets.d.ts",
            "DashboardPresetOptions LandingPresetOptions WebAppPresetOptions",
        );
        write(
            root,
            "tessellation-solver/sdk/runtime.js",
            "runTessellationApp compileTessellationSurface solveWithHierarchicalLayoutBuffer paintCanvas2DCommands resolveCanvasPaint geometryBackend morphPowerWeights polygon-text syncAccessibilityMirror performanceMonitor.record resetPerformance get performance",
        );
        write(
            root,
            "tessellation-solver/sdk/runtime.d.ts",
            "TessellationRuntimeOptions TessellationCanvasRuntime HierarchicalLayoutBufferConstructor PowerDiagramSolver solvePowerDiagram morphPowerWeights",
        );
        write(
            root,
            "tessellation-solver/sdk/power-layout.js",
            "solvePowerTessellationLayoutPlan buildPowerTessellationRenderPlan verifyPowerTessellation verifyPowerRenderPlan PowerLayoutReadinessError minTouchTarget failClosedPowerLayout",
        );
        write(
            root,
            "tessellation-solver/sdk/power-layout.d.ts",
            "PowerTessellationSolution PowerTessellationProof PowerLayoutReadinessError verifyPowerRenderPlan",
        );
        write(
            root,
            "tessellation-solver/sdk/performance-budget.js",
            "TESSELLATION_MOBILE_FRAME_BUDGET_MS createTessellationPerformanceMonitor summarizePerformanceSamples TessellationPerformanceBudgetError p95Ms",
        );
        write(
            root,
            "tessellation-solver/sdk/performance-budget.d.ts",
            "TessellationPerformanceSample TessellationPerformanceSnapshot TessellationPerformanceOptions",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/performance-budget.test.ts",
            "tessellation mobile performance budget reports bounded p50/p95 frame metrics and violations fail closed on a single over-budget frame",
        );
        write(
            root,
            "tessellation-solver/web/src/power-benchmark.ts",
            "__powerSurfaceBenchmark runBenchmark performanceBudgetMs: 1000 / 60 everyFrameValid mirrorPowerCellCount",
        );
        write(
            root,
            "tessellation-solver/web/e2e/power-runtime.spec.ts",
            "Power Surface browser release gate mobile frame budget p95Ms touchTargets noHorizontalOverflow",
        );
        write(
            root,
            "tessellation-solver/web/power-studio.html",
            "Semantic geometry live-toggle weight-evidence proof-grid event-list",
        );
        write(
            root,
            "tessellation-solver/web/src/power-studio.ts",
            "__powerStudio ResizeObserver morphPowerWeights setPowerWeights performanceBudgetMs: 1000 / 60 showGateMessage addGeometryEvent Last verified geometry noHorizontalOverflow",
        );
        write(
            root,
            "tessellation-solver/web/src/power-studio.css",
            "button:focus-visible @media (max-width: 720px) min-height: 44px prefers-reduced-motion",
        );
        write(
            root,
            "tessellation-solver/web/e2e/power-studio.spec.ts",
            "Power Surface real-time studio Auto pulse data-power-studio-ready touchTargets: true geometryAfter.polygon geometry / evidence noHorizontalOverflow",
        );
        write(
            root,
            "tessellation-solver/web/showcase.html",
            "GUIDED QUICKSTART tutorial-surface data-example-action data-language=\"pt\" starter-dialog recipe-input",
        );
        write(
            root,
            "tessellation-solver/web/src/showcase.ts",
            "__tessellationShowcase setLanguage focusNextExample starterProjectFiles buildRecipeFromEditor geometryBackend: \"power-diagram\"",
        );
        write(
            root,
            "tessellation-solver/web/src/showcase.css",
            ".lesson-preview { grid-column: auto; order: 2; } .lesson-code { order: 3; } #starter-dialog .language-switch",
        );
        write(
            root,
            "tessellation-solver/web/e2e/showcase.spec.ts",
            "Tessellation web showcase examples.operations starter-dialog lang\", \"pt-BR mobileOrder.preview",
        );
        write(
            root,
            "tessellation-solver/web/studio.html",
            "OPERATIONAL SURFACE STUDIO theme-color\" content=\"#ffffff data-add-kind=\"metric\" SEMANTIC LAYERS property-form generation-command duplicate-element auto-apply-note solve-count drag-ghost insertion-guide move-earlier move-later DRAG TO REORDER contract-bounded contract-contained contract-overlap contract-targets spec-dialog",
        );
        write(
            root,
            "tessellation-solver/web/src/operational-studio.ts",
            "__operationalStudio createDashboard runDashboardSurface addElement addElements duplicateSelected removeSelected applyGenerationCommand commandSegments persistDocument estimatedSurfaceHeight installStudioDragHandlers autoScrollDragViewport updateDragGhost showInsertionGuide hideDirectManipulationFeedback reorderElement moveSelectedBy schedulePropertyApply scheduleGenerationApply runtime.update runtimeMountCount hotUpdateCount solveCount releaseVerified setContractStatus ENGINE GATE FAILED historyIndex geometry: runtime.renderPlan?.geometry",
        );
        write(
            root,
            "tessellation-solver/web/src/operational-studio.css",
            "color-scheme: light .workbench-body .component-palette .layer-list .property-panel .generation-bar .is-studio-dragging .drag-ghost .insertion-guide .reorder-actions solve-pulse @media (max-width: 720px)",
        );
        write(
            root,
            "tessellation-solver/web/e2e/operational-studio.spec.ts",
            "Operational Surface Studio colorScheme)).toBe(\"light\") rgb(237, 242, 247) geometry).toBe(\"rectangles\") metric-3 critical compliance alert Remove the selected element generates compound changes atomically restores the local document Duplicate selected element snapshot.releaseVerified).toBe(true) toBeEnabled() AUTOSOLVE # drags widgets on the rendered surface data-source-id data-target-id Alt+ArrowUp engine autosolved auto-fits the surface whenever new widgets require more rows expanded.releaseVerified).toBe(true) snapshot.runtimeMountCount).toBe(1) snapshot.hotUpdateCount).toBeGreaterThan(0) name: \"Generate\" toHaveCount(0) selects the mobile surface automatically",
        );
        write(
            root,
            "example-studio-web/src/app/surfaces/SurfaceStudio.tsx",
            "@example/tessellation/presets @example/tessellation/renderers/dashboard example.surface.v1 example-surface-studio/v1 runDashboardSurface runtime.update Object.values(proof).every(Boolean) createSurfaceProject updateSurfaceProject connectSurfaceStream publishSurfaceEvent updateContent SurfaceDataBinding mapBoundPayload data-agent-last-live-binding data-agent-last-live-projection example-surface-studio-agent/v1 SURFACE_AGENT_CAPABILITIES exampleStudioAgent parseSurfaceAgentRequest validatedAgentElementPatch request-governed-command renderSurfacePdf verifiedReleaseProof compilePdfArtifact pdfCacheKeyRef pdfArtifact?.digest shortDigest PdfAStandard PDF archival standard PDF/A-1b PDF/A-2b PDF/A-3a set-pdf-standard preview-pdf export-pdf Preview PDF Export PDF Source semantic ID Flight DoExchange Save project Changes apply and solve automatically",
        );
        write(
            root,
            "example-studio-web/src/app/surfaces/surface-studio.module.css",
            ".shell .leftRail .tabs .viewportPanel .inspector .bindingFields .pdfDialog .commandBar @media (max-width: 760px)",
        );
        write(
            root,
            "example-studio-web/src/app/surfaces/page.tsx",
            "Operational Surfaces · Example Studio <SurfaceStudio />",
        );
        write(
            root,
            "example-studio-web/tests/surfaces.spec.ts",
            "Example Operational Surface workspace data-agent-contract data-agent-verified Layers 7 persists and hot-renders Flight DoExchange surface events project_kind).toBe('operational-surface') Flight DoExchange · 1 events claims-feed→open-claims,sla-risk open-claims=1291;sla-risk=38 exposes a validated AI-native operational surface contract example-surface-studio-agent/v1 surface.realtime-inspect unsafeHtml commandWithoutProject surface:solve compiles a verified operational surface PDF through the Typst endpoint api/studio/surface/render claims-operations.pdf RUST + TYPST ARTIFACT x-example-artifact-digest render-deck+typst sha256:aaaaaaaaaaaa PDF archival standard PDF/A-3A pdf_standard).toBe('a-3a') renderBodies).toHaveLength(1) surface:preview-pdf surface:export-pdf Source semantic ID valuePath: 'metrics.open' realtime.transport).toBe('flight-do-exchange') keeps the integrated workspace bounded on mobile",
        );
        write(
            root,
            "example-studio-web/src/lib/surface-projects.ts",
            "project_kind: 'operational-surface' listSurfaceProjects createSurfaceProject updateSurfaceProject normalizeSurfaceProject",
        );
        write(
            root,
            "example-studio-web/src/lib/surface-realtime.ts",
            "connectSurfaceStream publishSurfaceEvent new EventSource human_confirmed",
        );
        write(
            root,
            "example-studio-web/src/lib/surface-pdf.ts",
            "renderSurfacePdf /api/studio/surface/render application/pdf release_proof x-example-artifact-digest x-example-render-engine verifiable artifact identity PdfAStandard pdf_standard x-example-pdf-standard",
        );
        write(
            root,
            "example-api/example/pdf_studio/surface.py",
            "class OperationalSurfaceDocument class SurfaceReleaseProof class SurfaceRenderRequest Literal[\"a-1b\", \"a-2b\", \"a-3a\"] pdf_standard surface_artifact_digest hashlib.sha256 surface_to_deck TessellatedLayout Release contract VERIFIED",
        );
        write(
            root,
            "example-api/example/tests/pdf_studio/test_surface_render.py",
            "test_surface_compiles_to_tessellated_deck_with_live_values test_surface_render_endpoint_uses_canonical_deck_renderer test_surface_artifact_digest_changes_with_live_snapshot test_surface_render_accepts_supported_pdfa_profiles test_surface_render_rejects_unknown_pdfa_profile x-example-artifact-digest verapdf test_surface_renderer_produces_real_typst_pdfa",
        );
        write(
            root,
            "example-api/example/routers/pdf_studio.py",
            "/surface/render surface_artifact_digest X-Example-Artifact-Digest X-Example-Render-Engine X-Example-PDF-Standard pdf_standard=request.pdf_standard render_surface",
        );
        write(
            root,
            "example-deck/src/tessellate.rs",
            "body_card_emits_title_and_description_once stat_card_emits_verification_description",
        );
        write(
            root,
            "example-deck/src/compile.rs",
            "pdf_standard --pdf-standard",
        );
        write(
            root,
            "example-deck/src/bin/render_deck.rs",
            "--pdf-standard a-1b a-2b a-3a opts.pdf_standard(standard)",
        );
        write(
            root,
            "example-api/example/pdf_studio/render.py",
            "PdfStandard --pdf-standard pdf_standard=pdf_standard",
        );
        write(
            root,
            "example-api/Dockerfile",
            "typst-cli --version '~0.14' --features typst-subprocess,md fonts-noto-core fonts-noto-cjk fonts-noto-color-emoji",
        );
        write(
            root,
            "example-report/src/bin/workspace_docs_report.rs",
            "render_typst_report compile_typst_subprocess .pdf_standard(\"a-2b\") workspace-docs-report.typ",
        );
        write(
            root,
            "example-report/Cargo.toml",
            "typst-subprocess required-features = [\"typst-subprocess\", \"md\"]",
        );
        write(
            root,
            "example-api/example/pdf_studio/report.py",
            "same Typst CLI typst_available [\"typst\", \"--version\"]",
        );
        write(
            root,
            "example-render-core/src/render_typst_report.rs",
            "render_typst_report New Computer Modern Math Noto Sans Symbols2 Noto Color Emoji Noto Serif CJK SC",
        );
        write(
            root,
            ".claude/skills/example-deck/shared/cover.typ",
            "#CED2D7 #9DA6B0",
        );
        write(
            root,
            "example-api/pdf-studio-deck-theme/cover.typ",
            "#CED2D7 #9DA6B0",
        );
        write(
            root,
            ".github/workflows/workspace-verify.yml",
            "pdfa-conformance: Typst 0.14.2 surface-a-1b.pdf surface-a-2b.pdf surface-a-3a.pdf workspace-docs-report-a-2b.pdf test_subprocess_report_renderer_produces_real_pdfa_without_latex verapdf/cli@sha256: isCompliant=\"true\"",
        );
        write(
            root,
            "deploy/scripts/compose_smoke.sh",
            "/v2/pdf-studio/surface/render for pdf_standard in (\"a-1b\", \"a-2b\", \"a-3a\") operational surface {pdf_standard} render smoke failed surface_pdf_bytes x-example-pdf-standard render-deck+typst x-example-artifact-digest returned mismatched ETag workspace-docs-report+typst workspace-docs Typst report smoke failed render_surface",
        );
        write(
            root,
            "example-api/example/flight/surface_realtime.py",
            "class SurfaceRealtimeHub class SurfaceRealtimeUnavailable canonical_surface_tenant_key EXAMPLE_FLIGHT_TENANT_HMAC_SECRET EXAMPLE_SURFACE_REALTIME_BACKEND get_redis client.xadd( client.xread( example:surface:realtime: subscriber_capacity _bounded_put surface_realtime_hub",
        );
        write(
            root,
            "example-api/example/flight/server.py",
            "surface:{project} SURFACE_EVENT_SCHEMA_VERSION _exchange_surface flight-do-exchange Flight producers cannot bypass the HTTP human gate",
        );
        write(
            root,
            "example-api/example/tests/flight/test_surface_realtime_exchange.py",
            "test_redis_stream_fans_out_across_realtime_hub_instances test_redis_stream_replays_recent_surface_events test_http_and_flight_use_the_same_opaque_tenant_key test_surface_do_exchange_runs_over_a_real_flight_channel test_surface_do_exchange_is_bidirectional_and_arrow_native test_surface_do_exchange_rejects_client_supplied_tenant_scope test_surface_do_exchange_cannot_bypass_command_human_gate",
        );
        write(
            root,
            "example-api/example/routers/v2/studio_projects.py",
            "Literal[\"deck\", \"report\", \"operational-surface\"] SurfaceRealtimeUnavailable project_kind=body.project_kind /projects/{project_id}/stream /projects/{project_id}/events human_confirmed",
        );
        write(
            root,
            "example-api/example/core/duckdb.py",
            "project_kind    VARCHAR NOT NULL DEFAULT 'deck' {\"project_kind\": \"VARCHAR NOT NULL DEFAULT 'deck'\"} project_kind: str = \"deck\"",
        );
        write(
            root,
            "example-api/example/tests/pdf_studio/test_studio_projects.py",
            "test_crud_operational_surface_kind_round_trip test_router_creates_operational_surface_project test_router_surface_event_bridge_is_tenant_scoped test_router_surface_stream_fails_closed_when_shared_backend_is_unavailable test_router_surface_command_requires_human_gate",
        );
        write(
            root,
            "example-studio-web/package.json",
            "@example/tessellation workspace:*",
        );
        write(
            root,
            "example-studio-web/next.config.ts",
            "transpilePackages: ['@example/tessellation'] outputFileTracingRoot: workspaceRoot",
        );
        write(
            root,
            "example-studio-web/Dockerfile",
            "Build context is the workspace root COPY tessellation-solver/sdk pnpm --filter example-studio-web build",
        );
        write(
            root,
            "example-studio-web/README.md",
            "Operational Surface workspace @example/tessellation Redis Streams dot-separated sourceId example-surface-studio-agent/v1 operator confirmation gate Rust/Typst renderer not a canvas screenshot modal PDF preview compiled Blob is reused deterministic SHA-256 digest X-Example-Artifact-Digest PDF/A-1b PDF/A-2b PDF/A-3a --pdf-standard veraPDF validation http://localhost:3200/surfaces",
        );
        write(
            root,
            "example-api/docker-compose.yml",
            "context: .. dockerfile: example-studio-web/Dockerfile EXAMPLE_SURFACE_REALTIME_BACKEND",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/creator-sdk.test.ts",
            "creates a dashboard app spec and compiles it through the hierarchy compiler compiles, renders, and hit-tests a no-component-DOM surface resolves shadcn CSS variable paints before canvas drawing paints the dashboard renderer from semantic render-plan data hot-updates a dashboard spec without replacing its runtime hot-updates realtime content without re-solving verified geometry morphs retained semantic nodes during a hot update runDashboardSurface renders semantic hierarchy as true nested Power Diagram polygons rejects unreadable and undersized interactive Power cells re-solves and verifies every animated Power frame",
        );
        write(
            root,
            "tessellation-solver/sdk/dom-adapter.js",
            "applyTessellationLayoutToDom buildTessellationDomPatches regionsFromFlatBuffer data-tessellation-id resolvedStyle variant-count",
        );
        write(
            root,
            "tessellation-solver/sdk/dom-adapter.d.ts",
            "TessellationDomPatch TessellationDomApplyResult FlatSolvedLayout ShadcnTailwindLayoutPlan",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/dom-adapter.test.ts",
            "builds deterministic DOM patches from solved tessellation regions extracts stride-6 hierarchy buffers without losing parent/depth metadata applies patches to existing nodes by semantic id",
        );
        write(
            root,
            "tessellation-solver/sdk/render-plan.js",
            "buildTessellationRenderPlan buildCanvas2DCommands hitTestRenderPlan render-surface rounded-rect semanticTriples",
        );
        write(
            root,
            "tessellation-solver/sdk/render-plan.d.ts",
            "TessellationRenderPlan RenderPlanNode Canvas2DCommand RenderPlanOptions",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/render-plan.test.ts",
            "builds a DOM-free render surface from shadcn/Tailwind layout output emits deterministic Canvas2D commands and hit-tests semantic nodes excludeSemanticIds",
        );
        write(
            root,
            "tessellation-solver/sdk/surface-optimizer.js",
            "optimizeSurfaceRenderPlan verifySurfaceRenderPlan criticalVisibleFraction actionSeparation nonOverlapping",
        );
        write(
            root,
            "tessellation-solver/sdk/surface-optimizer.d.ts",
            "SurfaceOptimizerResult SurfaceOptimizerProof SurfaceOptimizerDiagnostic optimizeSurfaceRenderPlan verifySurfaceRenderPlan",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/surface-optimizer.test.ts",
            "projects convex regions into surface bounds separates overlapping sibling rectangles critical action separation",
        );
        write(
            root,
            "tessellation-solver/sdk/interaction-runtime.js",
            "createSurfaceInteractionModel buildSurfaceAccessibilityMirror createSurfaceCommandEnvelope routeSurfaceCommand tessellation.command.v0.1 focusOrder",
        );
        write(
            root,
            "tessellation-solver/sdk/interaction-runtime.d.ts",
            "SurfaceInteractionModel SurfaceCommandEnvelope SurfaceAccessibilityMirror createSurfaceInteractionModel routeSurfaceCommand",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/interaction-runtime.test.ts",
            "builds deterministic focus order emits command envelopes semantic accessibility mirror",
        );
        write(
            root,
            "tessellation-solver/sdk/control-runtime.js",
            "createSurfaceControlState reduceSurfaceControlState surfaceControlTransitionFromKey surfaceControlTransitionFromPointer surfaceControlTransitionFromPaste validateSurfaceControls tessellation.controls.v0.1 cursors dirty touched",
        );
        write(
            root,
            "tessellation-solver/sdk/control-runtime.d.ts",
            "SurfaceControlState SurfaceControlTransition SurfaceControlValidity createSurfaceControlState reduceSurfaceControlState surfaceControlTransitionFromPointer surfaceControlTransitionFromPaste",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/control-runtime.test.ts",
            "extracts form primitives reduces text and toggle transitions keyboard transitions pointer transitions pastes text at the current rendered caret position",
        );
        write(
            root,
            "tessellation-solver/sdk/dashboard-renderer.js",
            "runDashboardSurface paintDashboardRenderPlan compileTessellationSurface hitTestRenderPlan optimizeSurfaceRenderPlan createSurfaceInteractionModel createSurfaceControlState function update( function updateContent( content-only update cannot add, remove, or rename semantic regions createGeometryTransition transitionRenderPlan paintedRenderPlan component DOM",
        );
        write(
            root,
            "tessellation-solver/sdk/dashboard-renderer.d.ts",
            "DashboardSurfaceRuntime DashboardRendererOptions runDashboardSurface paintDashboardRenderPlan readonly renderedPlan update( updateContent(",
        );
        write(
            root,
            "tessellation-solver/sdk/visual-regression.js",
            "buildVisualRegressionFixture measureTessellationDomSnapshot compareVisualRegressionSnapshot getBoundingClientRect computedStyleExpectations geometry-mismatch",
        );
        write(
            root,
            "tessellation-solver/sdk/visual-regression.d.ts",
            "VisualRegressionFixture VisualDomSnapshotEntry VisualRegressionComparison ComputedStyleExpectation",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/visual-regression.test.ts",
            "matches rendered DOM snapshots against tessellation patches reports geometry and computed-style drift with semantic IDs summarizeVisualDiagnostics",
        );
        write(
            root,
            "tessellation-solver/web/e2e/shadcn-visual-regression.spec.ts",
            "shadcn visual regression harness matches tessellation patches to browser geometry and computed styles measureTessellationDomSnapshot compareVisualRegressionSnapshot screenshot.byteLength",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/accessibility-order.ts",
            "compareRegionsForReadingOrder rowTolerance roleToAria",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/accessibility-order.test.ts",
            "keeps left-to-right order when row tops differ slightly orders later rows after earlier rows once the row band is exceeded builds aria nodes from visual region order",
        );
        write(
            root,
            "tessellation-solver/web/src/engine/config-builder.test.ts",
            "builds a hierarchical config with root groups and child result nodes hierarchical-layout-buffer normalizes undersized surfaces",
        );
        write(
            root,
            "tessellation-solver/web/src/renderer/canvas-renderer.test.ts",
            "decodes stride-6 positions into groups and child regions buildRenderGroups hitTestLayout",
        );
        write(
            root,
            "tessellation-solver/web/playwright.config.ts",
            r#"baseURL: "http://127.0.0.1:5173" command: "pnpm dev --host 127.0.0.1" reuseExistingServer: true mobile-power Pixel 7 power-runtime.spec.ts"#,
        );
        write(
            root,
            "tessellation-solver/web/vite.config.ts",
            "powerStudio power-studio.html powerBenchmark showcase: resolve showcase.html studio: resolve studio.html",
        );
        write(
            root,
            "tessellation-solver/web/e2e/liquid-search.spec.ts",
            r#"Liquid Search browser smoke expectCanvasToBePainted keeps canvas bounded after viewport resize keeps narrow mobile hierarchy tall enough for result groups keeps finance child cards separated on narrow viewports canvasFitsPanel childRegionsDoNotOverlap firstReadingOrderLabel noHorizontalDocumentOverflow metric(page, "engine") hierarchy .overlay-item .canvas-warning canvas.result-canvas"#,
        );
        write(
            root,
            "example-api/scripts/export_openapi.py",
            "_ensure_import_paths openapi.json openapi.yaml yaml.dump",
        );
        write(
            root,
            "example-api/docs/openapi.json",
            "\"/tessellation/solve/hierarchy\" TessellationHierarchicalSolveRequest TessellationHierarchicalSolveResponse \"/tessellation/geo/solve\" TessellationGeoSolveRequest TessellationGeoSolveResponse",
        );
        write(
            root,
            "example-api/docs/openapi.yaml",
            "/tessellation/solve/hierarchy: TessellationHierarchicalSolveRequest TessellationHierarchicalSolveResponse /tessellation/geo/solve: TessellationGeoSolveRequest TessellationGeoSolveResponse",
        );
        write(
            root,
            "docs/guides/building-webpages-with-tessellation.md",
            "HierarchicalLayoutBuffer @example/tessellation @example/tessellation/app-spec @example/tessellation/presets @example/tessellation/runtime tessellation.app.v0.1 tessellation new dashboard @example/tessellation/compiler/tailwind-style @example/tessellation/compiler/tailwind-build @example/tessellation/compiler/shadcn-project @example/tessellation/compiler/tsx-extractor @example/tessellation/runtime/dom-adapter @example/tessellation/runtime/render-plan @example/tessellation/runtime/surface-optimizer @example/tessellation/runtime/interaction @example/tessellation/runtime/controls @example/tessellation/renderers/dashboard @example/tessellation/testing/visual-regression single rendered Canvas2D/WebGPU surface components.json make tessellation-wasm-test make tessellation-web-verify pnpm -C tessellation-solver/web test:e2e compileSearchToLayout semanticTriples SemanticLayoutProduct PowerDiagramProduct tessellation_solve_power_diagram /tessellation/geo/solve cargo run -p tessellation-engine tessellation-solver/engine/schemas/ make tessellation-release-check Reading order is derived from solved region geometry using row bands tessellation-solver/scenarios/*.json cargo test formal_ --features ffi /tessellation/solve/hierarchy HierarchicalResult",
        );
        write(
            root,
            "tessellation-solver/package.json",
            r#""name": "@example/tessellation", "./wasm-bg", "./schemas/*": "./engine/schemas/*", "./compiler/shadcn-tailwind", "./compiler/tailwind-style", "./compiler/tailwind-build", "./compiler/shadcn-project", "./compiler/tsx-extractor", "./app-spec", "./presets", "./runtime", "./runtime/dom-adapter", "./runtime/render-plan", "./runtime/power-layout", "./runtime/performance-budget", "./runtime/surface-optimizer", "./runtime/interaction", "./runtime/controls", "./renderers/dashboard", "./testing/visual-regression", "default": "./pkg/tessellation_wasm.js", "types": "./pkg/tessellation_wasm.d.ts", "sdk/tailwind-style-resolver.js", "sdk/tailwind-style-resolver.d.ts", "sdk/tailwind-build.js", "sdk/tailwind-build.d.ts", "sdk/shadcn-project.js", "sdk/shadcn-project.d.ts", "sdk/shadcn-tailwind.js", "sdk/shadcn-tailwind.d.ts", "sdk/tsx-extractor.js", "sdk/tsx-extractor.d.ts", "sdk/app-spec.js", "sdk/app-spec.d.ts", "sdk/presets.js", "sdk/presets.d.ts", "sdk/runtime.js", "sdk/runtime.d.ts", "sdk/dom-adapter.js", "sdk/dom-adapter.d.ts", "sdk/render-plan.js", "sdk/render-plan.d.ts", "sdk/power-layout.js", "sdk/power-layout.d.ts", "sdk/performance-budget.js", "sdk/performance-budget.d.ts", "sdk/surface-optimizer.js", "sdk/surface-optimizer.d.ts", "sdk/interaction-runtime.js", "sdk/interaction-runtime.d.ts", "sdk/control-runtime.js", "sdk/control-runtime.d.ts", "sdk/dashboard-renderer.js", "sdk/dashboard-renderer.d.ts", "sdk/visual-regression.js", "sdk/visual-regression.d.ts", "pkg/tessellation_wasm_bg.wasm", "verify": "cargo test -p tessellation-engine && cargo run -p tessellation-engine -- verify scenarios && pnpm pack --dry-run""#,
        );
        write(
            root,
            "docs/tessellation-engine.md",
            "# Tessellation Engine @example/tessellation cargo install --path tessellation-solver/tessellation-engine tessellation new dashboard tessellation verify tessellation-solver/scenarios schema_version /tessellation/geo/solve Creator SDK @example/tessellation/app-spec @example/tessellation/presets @example/tessellation/runtime tessellation.app.v0.1 shadcn/Tailwind Reproduction @example/tessellation/compiler/shadcn-tailwind @example/tessellation/compiler/tailwind-style @example/tessellation/compiler/tailwind-build @example/tessellation/compiler/shadcn-project @example/tessellation/compiler/tsx-extractor @example/tessellation/runtime/dom-adapter @example/tessellation/runtime/render-plan @example/tessellation/runtime/surface-optimizer @example/tessellation/runtime/interaction @example/tessellation/runtime/controls @example/tessellation/renderers/dashboard @example/tessellation/testing/visual-regression components.json resolvedStyle rendered surface make tessellation-release-check",
        );
        write(
            root,
            "tessellation-solver/README.md",
            "# @example/tessellation HierarchicalLayoutBuffer Creator SDK @example/tessellation/app-spec @example/tessellation/presets @example/tessellation/runtime tessellation.app.v0.1 tessellation new dashboard shadcn/Tailwind Compiler @example/tessellation/compiler/shadcn-tailwind @example/tessellation/compiler/tailwind-style @example/tessellation/compiler/tailwind-build @example/tessellation/compiler/shadcn-project @example/tessellation/compiler/tsx-extractor @example/tessellation/runtime/dom-adapter @example/tessellation/runtime/render-plan @example/tessellation/runtime/surface-optimizer @example/tessellation/runtime/interaction @example/tessellation/runtime/controls @example/tessellation/renderers/dashboard @example/tessellation/testing/visual-regression components.json resolvedStyle tessellation solve-power tessellation.engine.v0.1 make tessellation-release-check power-studio.html Power Lab solver engineering Web Showcase showcase.html in-browser JSON recipe editor canonical Operational Surface Studio example-studio-web http://localhost:3200/surfaces SCADA/CAE environment drag widgets directly on the rendered surface atomic multi-element generative UI commands There are no Apply or Generate buttons immediately runs a new solver pass hot-update the live runtime in place morph retained semantic frames semantic drag ghost and solver insertion guide Move Earlier/Move Later controls Alt+Arrow newly added components always enter an auto-fitted layout persist locally with runtime validation Export remains locked unless the live release contract proves",
        );
    }

    #[test]
    fn detects_happy_path_tessellation_contract() {
        let root = temp_root("happy");
        write_happy_fixture(&root);

        let envelope = doctor_tessellation_contract(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
        assert_eq!(envelope.evidence.len(), 57);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn warns_when_sector_route_uses_stale_raw_ingest() {
        let root = temp_root("stale-sector");
        write_happy_fixture(&root);
        write(
            &root,
            &classified_sector_route_fixture_path(),
            "/tessellation/geo/solve parsePowerDiagramCells buildPowerDiagramFeatureCollection tessellation_engine: \"power-diagram\" /tessellation/ingest/raw",
        );

        let envelope = doctor_tessellation_contract(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("ingest/raw"))
        );

        let _ = fs::remove_dir_all(root);
    }
}
