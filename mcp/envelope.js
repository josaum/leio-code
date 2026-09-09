export function summarizeEnvelopeMeta(meta) {
  if (!meta || typeof meta !== "object" || Array.isArray(meta)) {
    return null;
  }

  const search =
    meta.search && typeof meta.search === "object"
      ? {
          strategy:
            typeof meta.search.strategy === "string"
              ? meta.search.strategy
              : null,
          needle:
            typeof meta.search.needle === "string" ? meta.search.needle : null,
        }
      : null;

  const workspaceFacets =
    meta.workspace_facets && typeof meta.workspace_facets === "object"
      ? {
          deploy_targets:
            typeof meta.workspace_facets.deploy_targets === "number"
              ? meta.workspace_facets.deploy_targets
              : null,
          profiles:
            typeof meta.workspace_facets.profiles === "number"
              ? meta.workspace_facets.profiles
              : null,
          secret_sets:
            typeof meta.workspace_facets.secret_sets === "number"
              ? meta.workspace_facets.secret_sets
              : null,
          cartridges:
            typeof meta.workspace_facets.cartridges === "number"
              ? meta.workspace_facets.cartridges
              : null,
          has_deploy_topology:
            typeof meta.workspace_facets.has_deploy_topology === "boolean"
              ? meta.workspace_facets.has_deploy_topology
              : null,
          has_profile_envs:
            typeof meta.workspace_facets.has_profile_envs === "boolean"
              ? meta.workspace_facets.has_profile_envs
              : null,
          has_secret_sets:
            typeof meta.workspace_facets.has_secret_sets === "boolean"
              ? meta.workspace_facets.has_secret_sets
              : null,
          has_cartridges:
            typeof meta.workspace_facets.has_cartridges === "boolean"
              ? meta.workspace_facets.has_cartridges
              : null,
        }
      : null;

  const workspaceCapabilities =
    meta.workspace_capabilities && typeof meta.workspace_capabilities === "object"
      ? {
          workspace_profile:
            typeof meta.workspace_capabilities.workspace_profile === "string"
              ? meta.workspace_capabilities.workspace_profile
              : null,
          workspace_facets:
            meta.workspace_capabilities.workspace_facets &&
            typeof meta.workspace_capabilities.workspace_facets === "object"
              ? {
                  deploy_targets:
                    typeof meta.workspace_capabilities.workspace_facets
                      .deploy_targets === "number"
                      ? meta.workspace_capabilities.workspace_facets.deploy_targets
                      : null,
                  profiles:
                    typeof meta.workspace_capabilities.workspace_facets
                      .profiles === "number"
                      ? meta.workspace_capabilities.workspace_facets.profiles
                      : null,
                  secret_sets:
                    typeof meta.workspace_capabilities.workspace_facets
                      .secret_sets === "number"
                      ? meta.workspace_capabilities.workspace_facets.secret_sets
                      : null,
                  cartridges:
                    typeof meta.workspace_capabilities.workspace_facets
                      .cartridges === "number"
                      ? meta.workspace_capabilities.workspace_facets.cartridges
                      : null,
                  has_deploy_topology:
                    typeof meta.workspace_capabilities.workspace_facets
                      .has_deploy_topology === "boolean"
                      ? meta.workspace_capabilities.workspace_facets
                          .has_deploy_topology
                      : null,
                  has_profile_envs:
                    typeof meta.workspace_capabilities.workspace_facets
                      .has_profile_envs === "boolean"
                      ? meta.workspace_capabilities.workspace_facets
                          .has_profile_envs
                      : null,
                  has_secret_sets:
                    typeof meta.workspace_capabilities.workspace_facets
                      .has_secret_sets === "boolean"
                      ? meta.workspace_capabilities.workspace_facets
                          .has_secret_sets
                      : null,
                  has_cartridges:
                    typeof meta.workspace_capabilities.workspace_facets
                      .has_cartridges === "boolean"
                      ? meta.workspace_capabilities.workspace_facets.has_cartridges
                      : null,
                }
              : null,
          find_kinds: Array.isArray(meta.workspace_capabilities.find_kinds)
            ? meta.workspace_capabilities.find_kinds.filter(
                (value) => typeof value === "string",
              )
            : [],
          explain_kinds: Array.isArray(meta.workspace_capabilities.explain_kinds)
            ? meta.workspace_capabilities.explain_kinds.filter(
                (value) => typeof value === "string",
              )
            : [],
          doctor_kinds: Array.isArray(meta.workspace_capabilities.doctor_kinds)
            ? meta.workspace_capabilities.doctor_kinds.filter(
                (value) => typeof value === "string",
              )
            : [],
          graph_kinds: Array.isArray(meta.workspace_capabilities.graph_kinds)
            ? meta.workspace_capabilities.graph_kinds.filter(
                (value) => typeof value === "string",
              )
            : [],
          export_kinds: Array.isArray(meta.workspace_capabilities.export_kinds)
            ? meta.workspace_capabilities.export_kinds.filter(
                (value) => typeof value === "string",
              )
            : [],
          notes: Array.isArray(meta.workspace_capabilities.notes)
            ? meta.workspace_capabilities.notes.filter(
                (value) => typeof value === "string",
              )
            : [],
        }
      : null;

  const summary = {
    backend: typeof meta.backend === "string" ? meta.backend : null,
    search,
  };

  if (workspaceFacets) {
    summary.workspace_facets = workspaceFacets;
  }
  const workspaceProfile =
    typeof meta.workspace_profile === "string"
      ? meta.workspace_profile
      : workspaceCapabilities?.workspace_profile ?? null;
  if (workspaceProfile) {
    summary.workspace_profile = workspaceProfile;
  }
  if (workspaceCapabilities) {
    summary.workspace_capabilities = workspaceCapabilities;
  }

  return summary;
}

export function summarizeEnvelope(envelope) {
  if (!envelope) {
    return null;
  }

  const metaSummary = summarizeEnvelopeMeta(envelope?.meta);

  return {
    query_id: envelope.query_id ?? null,
    kind: envelope.kind ?? null,
    summary: envelope.summary ?? null,
    confidence:
      typeof envelope.confidence === "number" ? envelope.confidence : null,
    timing_ms:
      typeof envelope.timing_ms === "number" ? envelope.timing_ms : null,
    entity_count: Array.isArray(envelope.entities) ? envelope.entities.length : 0,
    evidence_count: Array.isArray(envelope.evidence)
      ? envelope.evidence.length
      : 0,
    warning_count: Array.isArray(envelope.warnings) ? envelope.warnings.length : 0,
    backend: metaSummary?.backend ?? null,
    search_strategy: metaSummary?.search?.strategy ?? null,
    workspace_profile: metaSummary?.workspace_profile ?? null,
  };
}

export function buildUiHints(envelope) {
  const envelopeSummary = summarizeEnvelope(envelope);
  const metaSummary = summarizeEnvelopeMeta(envelope?.meta);
  const badges = [];

  if (metaSummary?.backend) {
    badges.push({
      key: "backend",
      label: "Backend",
      value: metaSummary.backend,
      tone: "neutral",
    });
  }

  if (metaSummary?.workspace_profile) {
    badges.push({
      key: "profile",
      label: "Profile",
      value: metaSummary.workspace_profile,
      tone: metaSummary.workspace_profile === "generic" ? "neutral" : "positive",
    });
  }

  if (envelopeSummary?.search_strategy) {
    badges.push({
      key: "search",
      label: "Search",
      value: envelopeSummary.search_strategy,
      tone:
        envelopeSummary.search_strategy === "concept_relation_match"
          ? "positive"
          : "neutral",
    });
  }

  return {
    badges,
    states: {
      backend: metaSummary?.backend ?? null,
      search_strategy: envelopeSummary?.search_strategy ?? null,
      workspace_profile: metaSummary?.workspace_profile ?? null,
    },
  };
}

export function buildWorkspaceCapabilityHints(capabilities) {
  if (!capabilities || typeof capabilities !== "object") {
    return null;
  }

  const facets =
    capabilities.workspace_facets &&
    typeof capabilities.workspace_facets === "object"
      ? capabilities.workspace_facets
      : null;
  const findKinds = Array.isArray(capabilities.find_kinds)
    ? capabilities.find_kinds.filter((value) => typeof value === "string")
    : [];
  const explainKinds = Array.isArray(capabilities.explain_kinds)
    ? capabilities.explain_kinds.filter((value) => typeof value === "string")
    : [];
  const doctorKinds = Array.isArray(capabilities.doctor_kinds)
    ? capabilities.doctor_kinds.filter((value) => typeof value === "string")
    : [];
  const graphKinds = Array.isArray(capabilities.graph_kinds)
    ? capabilities.graph_kinds.filter((value) => typeof value === "string")
    : [];
  const exportKinds = Array.isArray(capabilities.export_kinds)
    ? capabilities.export_kinds.filter((value) => typeof value === "string")
    : [];
  const notes = Array.isArray(capabilities.notes)
    ? capabilities.notes.filter((value) => typeof value === "string")
    : [];

  const unavailableOptionalFacets = [];
  if (facets?.has_deploy_topology === false) {
    unavailableOptionalFacets.push("deploy-target");
  }
  if (facets?.has_cartridges === false) {
    unavailableOptionalFacets.push("cartridge");
  }
  if (facets?.has_profile_envs === false) {
    unavailableOptionalFacets.push("profile-env");
  }
  if (facets?.has_secret_sets === false) {
    unavailableOptionalFacets.push("secret-set");
  }

  return {
    workspace_profile:
      typeof capabilities.workspace_profile === "string"
        ? capabilities.workspace_profile
        : null,
    supports_optional_topology:
      facets?.has_deploy_topology === true || facets?.has_cartridges === true,
    supports_doctors: doctorKinds.length > 0,
    available: {
      find_kinds: findKinds,
      explain_kinds: explainKinds,
      doctor_kinds: doctorKinds,
      graph_kinds: graphKinds,
      export_kinds: exportKinds,
    },
    unavailable_optional_facets: unavailableOptionalFacets,
    notes,
  };
}

export function buildActionPalette(capabilityHints) {
  if (!capabilityHints || typeof capabilityHints !== "object") {
    return null;
  }

  const showTools = [
    "leio_code_capabilities",
    "leio_code_status",
    "leio_code_context",
    "leio_code_find",
    "leio_code_graph",
    "leio_code_export",
  ];
  if (
    Array.isArray(capabilityHints.available?.explain_kinds) &&
    capabilityHints.available.explain_kinds.length > 0
  ) {
    showTools.push("leio_code_explain");
  }
  if (capabilityHints.supports_doctors) {
    showTools.push("leio_code_doctor");
  }

  const hideTools = [];
  if (!capabilityHints.supports_doctors) {
    hideTools.push("leio_code_doctor");
  }

  const recommendedNext = ["leio_code_context", "leio_code_find", "leio_code_graph"];
  if (
    Array.isArray(capabilityHints.available?.explain_kinds) &&
    capabilityHints.available.explain_kinds.length > 0
  ) {
    recommendedNext.splice(1, 0, "leio_code_explain");
  }
  if (capabilityHints.supports_doctors) {
    recommendedNext.push("leio_code_doctor");
  }

  const queryStarters = [
    "Check workspace capabilities",
    "Build context for <task>",
    "Find symbol <name>",
    "Graph callers-of <symbol>",
  ];

  if (
    Array.isArray(capabilityHints.available?.find_kinds) &&
    capabilityHints.available.find_kinds.includes("deploy-target")
  ) {
    queryStarters.push("Find deploy-target <name>");
  }
  if (
    Array.isArray(capabilityHints.available?.find_kinds) &&
    capabilityHints.available.find_kinds.includes("cartridge")
  ) {
    queryStarters.push("Find cartridge <name>");
  }
  if (capabilityHints.supports_doctors) {
    queryStarters.push("Run doctor all");
  }

  return {
    show_tools: showTools,
    hide_tools: hideTools,
    recommended_start_tool: "leio_code_context",
    recommended_next_tools: recommendedNext,
    query_starters: queryStarters,
  };
}

export function formatTextResult(invocation, execution, envelope) {
  const audit = envelope?.summary;
  const summary =
    (audit && typeof audit === "object"
      ? typeof audit.passed === "boolean" && Number.isFinite(audit.doctor_count)
        ? `Audit: ${audit.passed ? "passed" : "failed"}; ${audit.doctor_count} doctors, ${audit.warning_count ?? 0} warnings (${audit.workspace_profile ?? "unknown profile"})`
        : JSON.stringify(audit)
      : audit) ||
    execution.stdout.trim() ||
    execution.stderr.trim() ||
    "leio-code command completed";

  const confidence =
    typeof envelope?.confidence === "number"
      ? `\nconfidence: ${envelope.confidence.toFixed(2)}`
      : "";
  const timing =
    typeof envelope?.timing_ms === "number"
      ? `\ntiming_ms: ${envelope.timing_ms}`
      : "";
  const warnings =
    Array.isArray(envelope?.warnings) && envelope.warnings.length > 0
      ? `\nwarnings: ${envelope.warnings.map((warning) =>
        typeof warning === "string" ? warning
          : Array.isArray(warning?.warning_excerpts)
            ? `${warning.name ?? "doctor"}: ${warning.warning_excerpts.join("; ")}`
            : JSON.stringify(warning),
      ).join(" | ")}`
      : "";
  const metaSummary = summarizeEnvelopeMeta(envelope?.meta);
  const search = metaSummary?.search?.strategy
    ? `\nsearch: ${metaSummary.search.strategy}`
    : "";
  const workspaceProfile = metaSummary?.workspace_profile
    ? `\nprofile: ${metaSummary.workspace_profile}`
    : "";

  const exitSuffix =
    execution.code === 0 ? "" : `\nexit_code: ${execution.code}`;

  return `${summary}${confidence}${timing}${search}${workspaceProfile}${warnings}${exitSuffix}\ncommand: ${invocation.command} ${invocation.args.join(" ")}`.trim();
}
