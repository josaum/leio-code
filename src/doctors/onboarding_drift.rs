use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct OnboardingDriftDoctor;

impl Doctor for OnboardingDriftDoctor {
    fn name(&self) -> &'static str {
        "onboarding-drift"
    }

    fn description(&self) -> &'static str {
        "Checks that tenant onboarding keeps using the shared Autopilot engine, shared onboarding UI, and vertical preset packs instead of drifting into app-local wizard scaffolds."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_onboarding_drift(root)
    }
}

struct OnboardingTarget {
    app: &'static str,
    wizard_path: &'static str,
    globals_path: &'static str,
}

const TARGETS: &[OnboardingTarget] = &[OnboardingTarget {
    app: "example-ops",
    wizard_path: "example-ops/src/components/tenants/onboarding/TenantOnboardingWizard.tsx",
    globals_path: "example-ops/src/app/globals.css",
}];

pub fn doctor_onboarding_drift(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let shared_index_path = root.join("packages/ops-autopilot/src/index.ts");
    let shared_ui_path = root.join("packages/ops-autopilot/src/ui/onboarding.tsx");
    let fitness_presets_path = root.join("packages/ops-fitness/src/onboarding.ts");
    let bpo_presets_path = root.join("packages/ops-bpo/src/onboarding.ts");

    let shared_index_src = read_text(&shared_index_path, &mut warnings);
    let shared_ui_src = read_text(&shared_ui_path, &mut warnings);
    let fitness_presets_src = read_text(&fitness_presets_path, &mut warnings);
    let bpo_presets_src = read_text(&bpo_presets_path, &mut warnings);

    let shared_index_exports_engine = shared_index_src
        .as_deref()
        .is_some_and(|src| src.contains("export * from \"./onboarding\";"));
    let shared_index_exports_ui = shared_index_src
        .as_deref()
        .is_some_and(|src| src.contains("export * from \"./ui/onboarding\";"));
    let shared_ui_has_common_surfaces = shared_ui_src.as_deref().is_some_and(|src| {
        [
            "export function OnboardingChoiceGrid",
            "export function OnboardingMetricGrid",
            "export function OnboardingSectionCard",
            "export function OnboardingPresetGrid",
            "export function OnboardingSelectableCardGrid",
            "export function OnboardingScaffold",
            "export function OnboardingLoadingState",
            "export function OnboardingErrorState",
        ]
        .iter()
        .all(|needle| src.contains(needle))
    });
    let fitness_presets_are_shared = fitness_presets_src.as_deref().is_some_and(|src| {
        src.contains("import type { OnboardingPreset } from \"@example/ops-autopilot\";")
            && src.contains("export const fitnessTenantOnboardingPresets")
    });
    let bpo_presets_are_shared = bpo_presets_src.as_deref().is_some_and(|src| {
        src.contains("import type { OnboardingPreset } from \"@example/ops-autopilot\";")
            && src.contains("export const bpoTenantOnboardingPresets")
    });

    if !shared_index_exports_engine {
        warnings.push(
            "ops-autopilot index no longer clearly exports the shared onboarding engine surface"
                .to_string(),
        );
    }
    if !shared_index_exports_ui {
        warnings.push(
            "ops-autopilot index no longer clearly exports the shared onboarding UI surface"
                .to_string(),
        );
    }
    if !shared_ui_has_common_surfaces {
        warnings.push(
            "ops-autopilot shared onboarding UI no longer clearly exposes scaffold, preset, selectable-card, and state surfaces"
                .to_string(),
        );
    }
    if !fitness_presets_are_shared {
        warnings.push(
            "ops-fitness no longer clearly publishes its onboarding presets through the shared Autopilot preset contract"
                .to_string(),
        );
    }
    if !bpo_presets_are_shared {
        warnings.push(
            "ops-bpo no longer clearly publishes its onboarding presets through the shared Autopilot preset contract"
                .to_string(),
        );
    }

    for (path, src, needle, detail, kind) in [
        (
            &shared_index_path,
            shared_index_src.as_ref(),
            "export * from \"./onboarding\";",
            "ops-autopilot index exports the shared onboarding engine",
            "shared_onboarding",
        ),
        (
            &shared_index_path,
            shared_index_src.as_ref(),
            "export * from \"./ui/onboarding\";",
            "ops-autopilot index exports the shared onboarding UI",
            "shared_onboarding",
        ),
        (
            &shared_ui_path,
            shared_ui_src.as_ref(),
            "export function OnboardingScaffold",
            "ops-autopilot publishes the shared onboarding scaffold",
            "shared_ui",
        ),
        (
            &fitness_presets_path,
            fitness_presets_src.as_ref(),
            "export const fitnessTenantOnboardingPresets",
            "fitness vertical publishes shared onboarding presets",
            "vertical_presets",
        ),
        (
            &bpo_presets_path,
            bpo_presets_src.as_ref(),
            "export const bpoTenantOnboardingPresets",
            "BPO vertical publishes shared onboarding presets",
            "vertical_presets",
        ),
    ] {
        push_evidence(&mut evidence, path, src, needle, detail, kind);
    }

    for target in TARGETS {
        let wizard_path = root.join(target.wizard_path);
        let globals_path = root.join(target.globals_path);

        let wizard_src = read_text(&wizard_path, &mut warnings);
        let globals_src = read_text(&globals_path, &mut warnings);

        let wizard_imports_shared_autopilot = wizard_src.as_deref().is_some_and(|src| {
            src.contains("from '@example/ops-autopilot';")
                && src.contains("OnboardingScaffold,")
                && src.contains("OnboardingPresetGrid,")
                && src.contains("OnboardingSelectableCardGrid,")
                && src.contains("buildOnboardingProgressSnapshot,")
                && src.contains("readPersistedOnboardingDraft,")
        });
        let wizard_uses_shared_scaffold = wizard_src
            .as_deref()
            .is_some_and(|src| src.contains("<OnboardingScaffold"));
        let wizard_uses_shared_presets = wizard_src
            .as_deref()
            .is_some_and(|src| src.contains("<OnboardingPresetGrid"));
        let wizard_uses_shared_selectables = wizard_src
            .as_deref()
            .is_some_and(|src| src.contains("<OnboardingSelectableCardGrid"));
        let globals_scan_shared_autopilot = globals_src
            .as_deref()
            .is_some_and(|src| src.contains("@source \"../../../packages/ops-autopilot/src\";"));

        if !wizard_imports_shared_autopilot {
            warnings.push(format!(
                "{} tenant onboarding wizard no longer clearly imports the shared Autopilot onboarding surface",
                target.app
            ));
        }
        if !wizard_uses_shared_scaffold {
            warnings.push(format!(
                "{} tenant onboarding wizard no longer clearly renders the shared OnboardingScaffold",
                target.app
            ));
        }
        if !wizard_uses_shared_presets {
            warnings.push(format!(
                "{} tenant onboarding wizard no longer clearly renders the shared OnboardingPresetGrid",
                target.app
            ));
        }
        if !wizard_uses_shared_selectables {
            warnings.push(format!(
                "{} tenant onboarding wizard no longer clearly renders the shared OnboardingSelectableCardGrid",
                target.app
            ));
        }
        if !globals_scan_shared_autopilot {
            warnings.push(format!(
                "{} globals.css no longer clearly scans the shared ops-autopilot source tree for Tailwind classes",
                target.app
            ));
        }

        for (path, src, needle, detail, kind) in [
            (
                &wizard_path,
                wizard_src.as_ref(),
                "from '@example/ops-autopilot';",
                "tenant onboarding wizard imports the shared Autopilot surface",
                "wizard",
            ),
            (
                &wizard_path,
                wizard_src.as_ref(),
                "<OnboardingScaffold",
                "tenant onboarding wizard renders the shared scaffold",
                "wizard",
            ),
            (
                &wizard_path,
                wizard_src.as_ref(),
                "<OnboardingPresetGrid",
                "tenant onboarding wizard renders the shared preset grid",
                "wizard",
            ),
            (
                &wizard_path,
                wizard_src.as_ref(),
                "<OnboardingSelectableCardGrid",
                "tenant onboarding wizard renders the shared selectable-card grid",
                "wizard",
            ),
            (
                &globals_path,
                globals_src.as_ref(),
                "@source \"../../../packages/ops-autopilot/src\";",
                "frontend Tailwind sources include the shared onboarding package",
                "style_source",
            ),
        ] {
            push_evidence(&mut evidence, path, src, needle, detail, kind);
        }

        entities.push(json!({
            "app": target.app,
            "wizard_imports_shared_autopilot": wizard_imports_shared_autopilot,
            "wizard_uses_shared_scaffold": wizard_uses_shared_scaffold,
            "wizard_uses_shared_presets": wizard_uses_shared_presets,
            "wizard_uses_shared_selectables": wizard_uses_shared_selectables,
            "globals_scan_shared_autopilot": globals_scan_shared_autopilot,
        }));
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_onboarding_drift"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "shared Autopilot onboarding engine, UI, presets, and frontend wiring are intact for the single Example Ops UI".to_string()
        } else {
            format!(
                "onboarding drift checks found {} warning(s) across shared onboarding engine, UI, presets, or frontend wiring",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.97 } else { 0.72 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn push_evidence(
    evidence: &mut Vec<EvidenceItem>,
    path: &Path,
    src: Option<&String>,
    needle: &str,
    detail: &str,
    kind: &str,
) {
    if let Some(src) = src
        && let Some(line) = find_line(src, needle)
    {
        evidence.push(EvidenceItem {
            kind: kind.to_string(),
            path: path.display().to_string(),
            line: Some(line),
            detail: detail.to_string(),
        });
    }
}
