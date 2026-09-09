import test from "node:test";
import assert from "node:assert/strict";

import {
  buildActionPalette,
  buildWorkspaceCapabilityHints,
  buildUiHints,
  formatTextResult,
  summarizeEnvelope,
  summarizeEnvelopeMeta,
} from "./envelope.js";

test("summarizeEnvelopeMeta normalizes backend and search fields", () => {
  const summary = summarizeEnvelopeMeta({
    backend: "arrow_ipc_local",
    collection_selection: { resolved_collection: "dropped" },
    fca_relation_contract: { ready: true },
    search: {
      strategy: "concept_relation_match",
      needle: "customer ops deploy target",
      noisy: "ignored",
    },
  });

  assert.deepEqual(summary, {
    backend: "arrow_ipc_local",
    search: {
      strategy: "concept_relation_match",
      needle: "customer ops deploy target",
    },
  });
});

test("summarizeEnvelope promotes search strategy and warning counts", () => {
  const summary = summarizeEnvelope({
    query_id: "q1",
    kind: "explain",
    summary: "demo",
    confidence: 0.97,
    timing_ms: 123,
    entities: [{}],
    evidence: [{ kind: "arrow_node" }],
    warnings: ["w1", "w2"],
    meta: {
      backend: "arrow_ipc_local",
      search: {
        strategy: "concept_relation_match",
      },
    },
  });

  assert.equal(summary.backend, "arrow_ipc_local");
  assert.equal(summary.search_strategy, "concept_relation_match");
  assert.equal(summary.warning_count, 2);
});

test("formatTextResult includes search strategy hints", () => {
  const text = formatTextResult(
    { command: "cargo", args: ["run", "--json", "find", "symbol", "demo"] },
    { code: 0, stdout: "", stderr: "" },
    {
      summary: "LEIO Code find: 3 hits",
      confidence: 0.97,
      timing_ms: 313,
      warnings: [],
      meta: {
        search: {
          strategy: "concept_relation_match",
        },
      },
    },
  );

  assert.match(text, /search: concept_relation_match/);
});

test("text summaries describe audit objects and preserve actionable diagnostics", () => {
  const invocation = { command: "leio-code", args: ["audit"] };
  const execution = { code: 1, stdout: "", stderr: "" };
  const text = formatTextResult(invocation, execution, {
    summary: { passed: false, doctor_count: 7, failing_doctor_count: 1, warning_count: 2, workspace_profile: "leio-code" },
    warnings: [{ name: "self-contract", warning_count: 2, warning_excerpts: ["rebuild the installed binary"] }],
  });
  assert.match(text, /Audit: failed/);
  assert.match(text, /7 doctors/);
  assert.match(text, /2 warnings/);
  assert.match(text, /self-contract/);
  assert.match(text, /rebuild the installed binary/);
  assert.doesNotMatch(text, /\[object Object\]/);
  assert.match(formatTextResult(invocation, { ...execution, stderr: "binary unavailable" }, null), /binary unavailable/);
});

test("buildUiHints emits backend, profile, and search badges", () => {
  const uiHints = buildUiHints({
    meta: {
      backend: "arrow_ipc_local",
      workspace_profile: "generic",
      search: {
        strategy: "concept_relation_match",
      },
    },
  });

  assert.deepEqual(uiHints.states, {
    backend: "arrow_ipc_local",
    search_strategy: "concept_relation_match",
    workspace_profile: "generic",
  });
  assert.deepEqual(
    uiHints.badges.map((badge) => [badge.key, badge.value, badge.tone]),
    [
      ["backend", "arrow_ipc_local", "neutral"],
      ["profile", "generic", "neutral"],
      ["search", "concept_relation_match", "positive"],
    ],
  );
});

test("summarizeEnvelopeMeta preserves workspace capabilities when present", () => {
  const summary = summarizeEnvelopeMeta({
    workspace_profile: "generic",
    workspace_capabilities: {
      workspace_profile: "generic",
      workspace_facets: {
        deploy_targets: 0,
        profiles: 0,
        secret_sets: 0,
        cartridges: 0,
        has_deploy_topology: false,
        has_profile_envs: false,
        has_secret_sets: false,
        has_cartridges: false,
      },
      find_kinds: ["symbol", "env-var"],
      explain_kinds: ["env-var", "redis-key"],
      doctor_kinds: [],
      graph_kinds: ["callers-of"],
      export_kinds: ["formal-context"],
      notes: ["no workspace-specific doctors are configured"],
    },
  });

  assert.equal(summary.workspace_profile, "generic");
  assert.deepEqual(summary.workspace_capabilities, {
    workspace_profile: "generic",
    workspace_facets: {
      deploy_targets: 0,
      profiles: 0,
      secret_sets: 0,
      cartridges: 0,
      has_deploy_topology: false,
      has_profile_envs: false,
      has_secret_sets: false,
      has_cartridges: false,
    },
    find_kinds: ["symbol", "env-var"],
    explain_kinds: ["env-var", "redis-key"],
    doctor_kinds: [],
    graph_kinds: ["callers-of"],
    export_kinds: ["formal-context"],
    notes: ["no workspace-specific doctors are configured"],
  });
});

test("buildWorkspaceCapabilityHints marks optional topology as unavailable on generic repos", () => {
  const hints = buildWorkspaceCapabilityHints({
    workspace_profile: "generic",
    workspace_facets: {
      deploy_targets: 0,
      profiles: 0,
      secret_sets: 0,
      cartridges: 0,
      has_deploy_topology: false,
      has_profile_envs: false,
      has_secret_sets: false,
      has_cartridges: false,
    },
    find_kinds: ["symbol", "env-var", "redis-key", "api-route", "docker-service"],
    explain_kinds: ["env-var", "redis-key"],
    doctor_kinds: [],
    graph_kinds: ["callers-of"],
    export_kinds: ["formal-context"],
    notes: ["no workspace-specific doctors are configured"],
  });

  assert.deepEqual(hints, {
    workspace_profile: "generic",
    supports_optional_topology: false,
    supports_doctors: false,
    available: {
      find_kinds: ["symbol", "env-var", "redis-key", "api-route", "docker-service"],
      explain_kinds: ["env-var", "redis-key"],
      doctor_kinds: [],
      graph_kinds: ["callers-of"],
      export_kinds: ["formal-context"],
    },
    unavailable_optional_facets: [
      "deploy-target",
      "cartridge",
      "profile-env",
      "secret-set",
    ],
    notes: ["no workspace-specific doctors are configured"],
  });
});

test("buildActionPalette hides doctors for generic repositories", () => {
  const palette = buildActionPalette(
    buildWorkspaceCapabilityHints({
      workspace_profile: "generic",
      workspace_facets: {
        deploy_targets: 0,
        profiles: 0,
        secret_sets: 0,
        cartridges: 0,
        has_deploy_topology: false,
        has_profile_envs: false,
        has_secret_sets: false,
        has_cartridges: false,
      },
      find_kinds: ["symbol", "env-var", "redis-key", "api-route", "docker-service"],
      explain_kinds: ["env-var", "redis-key"],
      doctor_kinds: [],
      graph_kinds: ["callers-of"],
      export_kinds: ["formal-context"],
      notes: [],
    }),
  );

  assert.equal(palette.recommended_start_tool, "leio_code_context");
  assert.deepEqual(palette.hide_tools, ["leio_code_doctor"]);
  assert.ok(palette.show_tools.includes("leio_code_context"));
  assert.ok(palette.show_tools.includes("leio_code_find"));
  assert.ok(!palette.show_tools.includes("leio_code_doctor"));
  assert.ok(palette.recommended_next_tools.includes("leio_code_context"));
  assert.ok(palette.query_starters.includes("Build context for <task>"));
  assert.ok(palette.query_starters.includes("Find deploy-target <name>") === false);
  assert.ok(!palette.query_starters.includes("Run doctor all"));
});
