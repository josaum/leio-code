/**
 * Shared LEIO Code tool-routing guide content for stdio MCP and Apps SDK.
 * Keep topic lists and examples capability-aware; do not hard-code doctor counts.
 */

export const GUIDE_TOPICS = [
  "general",
  "conversation",
  "context",
  "lookup",
  "explain",
  "doctor",
  "graph",
  "navigation",
  "export",
  "knowledge",
];

export const GUIDE_CONTENT = {
  conversation: {
    when: "Use for WhatsApp or normalized conversation files and evidence-based semantic, temporal or authorship investigation.",
    tools: [
      "Local stdio: leio_code_conversation(repo_root=<absolute folder>, sources=[<selected TXT/ZIP/JSON>], date_order=dmy|mdy, account=<exact label>, target=<optional ID>). CLI: conversation --source <file>. Select files explicitly; no directory indexing or network/model calls.",
      "Read the packet inventory and source hashes, then evidence_records and contexts. prediction_input excludes the target and later messages; later_retrospective_only must remain separately labeled. Account labels are not physical authors.",
      "Perform semantic contextual analysis with source IDs: speech acts, referents, premises, reported speech, topic and relationship history, alternatives and disconfirming evidence. Similarity retrieval is not copying or causal evidence.",
      "Follow method_plan prerequisites for conditional token likelihood, session-block style scans, Zipf held-out likelihood and Pareto/EVT tail diagnostics. proposed-not-run is not an executed result; no author probabilities without a validated attribution design.",
      "Reference Provider: local LeioBridge.conversation validates the typed packet and preserves unsigned evidence. Governed verify_claims and ontology promotion are separate; a valid digest or category law never establishes authorship. Conversation file access is not exposed through the hosted Apps SDK.",
      "Keep raw audio/ASR and user corrections as separate evidence. An unanswered export tail is an observation boundary; separately reported no-reply times are right-censored, not intent evidence.",
    ],
    examples: [
      "Prepare a local conversation review from chat.zip with the exact account label and explicit dmy dates.",
      "Inspect an earlier message by ID, keeping prediction context separate from later interpretation.",
    ],
  },
  general: {
    when: "Use when the agent needs to choose the right LEIO Code tool family quickly.",
    tools: [
      "For conversational files use the conversation guide topic; local stdio/CLI prepares a source-backed evidence packet and method plan without indexing the folder.",
      "status / inspect_repository_status for an instant repository health snapshot (index age, doctors, optional workspace facets, and current workspace capabilities).",
      "capabilities / inspect_repository_capabilities for the exact query kinds and verification suites that are meaningful for the current repository profile.",
      "context / prepare_repository_context for building a ranked agent-ready edit/review bundle from a natural-language task.",
      "find / search_repository for direct lookups of symbols, env vars, Redis keys, API routes, and Docker services.",
      "find / search_repository kind=deploy-target for target manifests and deployment topology.",
      "find / search_repository kind=cartridge for cartridge ownership and source files.",
      "find / search_repository_memory for local Arrow semantic and FCA-backed retrieval (`.leio-code/exports/arrow-nodes-v1/nodes.arrow`) when exact index lookup is insufficient.",
      "explain / explain_repository for evidence-backed operational meaning and lineage of env vars and Redis keys.",
      "explain / explain_repository kind=deploy-target for deployment readiness and lineage.",
      "explain / explain_repository kind=cartridge for cartridge integrations and deployment topology.",
      "doctor / audit_repository_contracts for contract drift when a workspace profile provides doctors (this is doctor, not CLI audit --strict).",
      "graph / graph_repository for callers, callees, callsites, file symbol inventories, and import topology.",
      "index for refreshing the workspace index after large code changes (CLI / stdio MCP).",
      "init / verify / watch for onboarding, profile verification, and detached reindex-on-change (stdio MCP / CLI).",
      "export for formal-context (plus lattice.json / induced.ttl) and code-graph artifacts, including streamed json/arrow formats (CLI / stdio MCP).",
      "For stateful graph and FCA exploration, use guide topic=navigation. Local nav follows call edges or shared indexed attributes; parent/child/peer are lattice cover relations, not runtime dependencies. Keep one explicit session per agent.",
      "knowledge for the compiled markdown/wiki knowledge base (fully local Arrow). kind=explain is SPARQL-gated (or refuse); kind=sparql is raw; compile writes formal.nq.",
      "status lists workspace_members and session isolation. Concurrent agents set LEIO_SESSION. Events land in .leio-code/events/events.ndjson (JSON-LD PROV).",
    ],
    examples: [
      "Check repository status before starting work.",
      "Build context for fixing Apps SDK Docker smoke.",
      "Find where validate_bearer is defined, then graph its callers.",
      "Explain deploy-target backend_api when the repo models deployment topology.",
      "Explain cartridge customer_success when the repo models cartridges.",
      "Check workspace capabilities first on an arbitrary repository before choosing optional query kinds or verification suites.",
      "Run doctor deploy before pushing to production.",
      "Find which optional cartridges are loaded by a target in a profile-aware workspace.",
    ],
  },
  context: {
    when: "Use before implementation or review when the agent needs a compact, ranked working set for a task rather than one exact lookup.",
    tools: [
      "context / prepare_repository_context(task=...) returns ordered context zones plus ranked files, symbols, env vars, Redis keys, graph follow-ups, tests, anchors, memory sources, instruction sources, and risk notes.",
      "Context bundles also include deploy-target matches when the repository models deployment topology.",
      "Use find/search or graph/graph_repository on the returned follow-ups when more precision is needed before editing.",
      "Use doctor / audit_repository_contracts when the context bundle suggests a profile-specific contract check.",
    ],
    examples: [
      "Build context for fixing Apps SDK Docker smoke.",
      "Build context for reviewing auth token propagation.",
      "Build context for migrating a Redis session key.",
    ],
  },
  lookup: {
    when: "Use for precise entity lookup before any broader reasoning. PREFERRED over Grep/Glob.",
    tools: [
      "find/search kind=symbol for symbol ownership and file/line evidence.",
      "find/search kind=env-var for environment variable usage and declaration sites.",
      "find/search kind=redis-key for runtime key ownership and hot-path state usage.",
      "find/search kind=deploy-target for target manifests and deployment topology.",
      "find/search kind=cartridge for cartridge directories, routers, types, and which deploy targets load them.",
      "find/search kind=api-route for API endpoint ownership across Python routers and Rust handlers.",
      "find/search kind=docker-service for Docker service definitions, health checks, and port mappings.",
    ],
    examples: [
      "Find symbol AuthClaim.",
      "Find env-var JWT_SECRET.",
      "Find deploy-target backend_api.",
      "Find cartridge customer_success.",
      "Find api-route /v2/whatsapp/webhook.",
      "Find docker-service example-api.",
    ],
  },
  explain: {
    when: "Use when the agent needs provenance, runtime meaning, deploy lineage, or cartridge topology.",
    tools: [
      "explain kind=deploy-target for readiness, health, smoke, rollback, and secret-set lineage.",
      "explain kind=env-var for where a variable is declared, read, and operationally important.",
      "explain kind=redis-key for ownership and state semantics.",
      "explain kind=cartridge for where a cartridge is deployed, its integrations, health checks, source files, and key symbols.",
    ],
    examples: [
      "Explain deploy-target backend_api.",
      "Explain cartridge customer_success.",
      "Explain env-var DATABASE_URL.",
    ],
  },
  doctor: {
    when: "Use for contract drift, runtime mismatches, or CI-grade architectural verification. Essential before any deploy or cross-cutting refactor.",
    tools: [
      "doctor / audit_repository_contracts kind=all for the full doctor suite registered for the active workspace profile.",
      "doctor / audit_repository_contracts kind=baseline for the fast health gate; kind=ci for the PR-shaped gate.",
      "Targeted doctor kinds from capabilities / doctor --help (do not hard-code the suite list).",
      "For the composite pre-deploy rollup (status + every doctor + capabilities) with --strict exit codes, use CLI: leio-code audit --strict (or Apps audit_repository_rollup when exposed).",
    ],
    examples: [
      "Run doctor all.",
      "Run doctor baseline.",
      "Run doctor ci.",
      "Run doctor deploy.",
      "Run doctor auth-brokering.",
      "Shell: leio-code audit --strict --format markdown --out /tmp/audit.md",
    ],
  },
  graph: {
    when: "Use for structural questions that require call or import topology rather than fuzzy search.",
    tools: [
      "callers-of and callees-of for symbol adjacency.",
      "callsites-of for concrete call evidence with file and line.",
      "symbols-in for per-file symbol inventory.",
      "For ambiguous names, use symbols-in on the known file and copy the chosen row's stable symbol URN into the next graph needle or local nav goto. Do not reconstruct the URN or silently choose a same-named symbol.",
      "imports-in for per-file import statements.",
      "importers-of for reverse lookup by module specifier, imported name, file path, or raw statement — not exact-raw-only. Python `from pkg.mod import Name` is found by pkg.mod, Name, or pkg/mod.py.",
      "resolved-imports-in and resolved-importers-of for canonical workspace file resolution.",
      "dead-code for unused-symbol candidates when the profile exposes it.",
      "Graph results are indexed call/import evidence with resolution limits; inspect callsites and unresolved candidates before claiming impact. FCA shared attributes and cover relations are not proof of runtime coupling.",
      "Local nav callers/callees/neighbors keeps a cursor for multi-step exploration; guide topic=navigation explains session and select semantics. Ordinary graph queries are stateless; formal-context export is not required.",
      "When next_calls are returned, inspect tool, arguments and reason, then run only the relevant bounded follow-up. Keep repo_root and any index pinned.",
    ],
    examples: [
      "Graph callers-of validate_bearer.",
      "Graph symbols-in example-platform/flight-contracts/src/auth.rs.",
      "Graph importers-of cartridges.c4gym.seed_cobranca.",
      "Graph resolved-importers-of packages/trpc/src/client/example.ts.",
    ],
  },
  navigation: {
    when: "Use for a stateful agent walk through code edges, FCA concepts, or wiki headings after resolving the starting entity.",
    tools: [
      "Local stdio leio_code_nav and CLI nav expose navigation; the hosted Apps SDK has no nav tool. Use its graph_repository for stateless graph evidence or switch to local stdio/CLI for a cursor.",
      "Resolve a code symbol with graph symbols-in and use the selected row.symbol stable URN as nav goto needle. This preserves identity when several files define the same name. goto also accepts concept IDs, files, and section:path#line headings.",
      "Pin absolute repo_root and any index on every call. Pass the same explicit session on every nav call, with a distinct value per concurrent agent. CLI uses --session; LEIO_SESSION is the environment fallback. Read here to check the current node and session before resuming.",
      "callers/callees/neighbors enumerate indexed call edges. FCA parent is more general, child is more specific, and peer has a shared parent; these cover relations group shared indexed attributes. They are not proof of runtime coupling, execution, or semantic equivalence.",
      "Walks return role=result rows without moving the cursor. Inspect each result's path, symbol and zero-based index; select that index before the next walk. kind=select with index=0 means the first result row, not the first envelope entity. back/forward move through cursor history.",
      "Exact indexed file paths resolve before fuzzy lookup, including ./src/file.rs, file:src/file.rs and in-repository absolute paths. File and symbol results carry source lines when known. Concepts expose bounded concept_details with labels, defining intent attributes and representative members; samples are not the complete extent, and a concept family is not a source path.",
      "Read envelope.meta.result_page for returned count, known total (or null), has_more and next_offset. Continue using the same query, session and limit (1–100), with the returned offset; select indices start at zero on each page. here preserves the page; selecting or moving through history clears it. Paged Arrow fallback uses deterministic lexical and FCA ranking without remote embeddings; use context/adaptive for semantic discovery first.",
      "Read envelope.meta.lattice.state: current compares the selected index and live wiki inputs, stale differs, unverified is an older artifact without usable provenance, missing/invalid require preparation, and unchecked applies to the lightweight IRI path. Reindex code changes first. Follow explicit rebuild suggestions when rebuild_required; ordinary graph navigation remains available.",
      "If FCA lattice artifacts are missing, deliberately prepare them with export kind=formal-context before a lattice walk. Ordinary graph queries and call navigation do not need formal-context export. related may include heading/lattice relations or retrieval candidates; it does not establish call edges.",
      "Use align for structural alignment diagnostics. A coherent functor or concept membership does not prove a runtime claim; verify behavior through graph callsites, source evidence and focused execution as appropriate.",
      "The examples use /abs/repo and a returned symbol URN as placeholders. Replace them with your repository and selected graph row.symbol. Run kind=select with index=0 only after parent returned a result with index=0, then back returns to the previous node. next_calls are bounded suggestions with tool, arguments and reason, not automatic execution.",
    ],
    examples: [
      'leio_code_graph({"repo_root":"/abs/repo","kind":"symbols-in","needle":"src/auth.rs"})',
      'leio_code_nav({"repo_root":"/abs/repo","session":"agent-auth-review","kind":"goto","needle":"<returned row.symbol URN>"})',
      'leio_code_nav({"repo_root":"/abs/repo","session":"agent-auth-review","kind":"parent","limit":5})',
      'leio_code_nav({"repo_root":"/abs/repo","session":"agent-auth-review","kind":"select","index":0})',
      'leio_code_nav({"repo_root":"/abs/repo","session":"agent-auth-review","kind":"back"})',
    ],
  },
  export: {
    when: "Use when the task needs machine-consumable graph or formal-context artifacts.",
    tools: [
      "formal-context export for FCA and downstream embedding/training pipelines.",
      "code-graph export for N-Quads, revision manifests, and the structural query cache.",
    ],
    examples: ["Export formal-context.", "Export code-graph."],
  },
  knowledge: {
    when: "Use for article-centric querying over the local compiled wiki (Arrow, fully offline). Use kind=explain when the answer must be SPARQL-grounded with a lattice proof. This is for KB retrieval, not code-graph lookup.",
    tools: [
      "knowledge kind=explain for SPARQL-gated answers (IRI + heading + functor trail; refuses when unbound).",
      "knowledge kind=sparql for a raw SPARQL query against the formal graph.",
      "knowledge kind=adaptive for title -> article MAX_SIM -> topic -> lexical -> hybrid chunk fallback retrieval.",
      "knowledge kind=text for fast lexical ranking across titles, topics, and article bodies.",
      "knowledge kind=status for wiki section counts plus meta.formal (triples, prefixes, freshness).",
      "knowledge kind=compile to rebuild `.leio-code/exports/knowledge-v1/` including formal.nq.",
    ],
    examples: [
      "Explain hours for a unit and show the SPARQL + lattice trail.",
      "Run SPARQL SELECT against the formal knowledge graph.",
      "Query the compiled wiki for MTU troubleshooting patterns.",
      "Check KB status for a repo-scoped wiki and inspect article coverage.",
    ],
  },
};

export function filterGuideByCapabilities(guide, capabilities) {
  if (!capabilities || typeof capabilities !== "object") {
    return guide;
  }

  const findKinds = new Set(
    Array.isArray(capabilities.find_kinds) ? capabilities.find_kinds : [],
  );
  const explainKinds = new Set(
    Array.isArray(capabilities.explain_kinds) ? capabilities.explain_kinds : [],
  );
  const doctorKinds = new Set(
    Array.isArray(capabilities.doctor_kinds) ? capabilities.doctor_kinds : [],
  );

  const supportsDeploy =
    findKinds.has("deploy-target") || explainKinds.has("deploy-target");
  const supportsCartridge =
    findKinds.has("cartridge") || explainKinds.has("cartridge");
  const supportsDoctors = doctorKinds.size > 0;
  const doctorPresets = new Set(["all", "baseline", "ci"]);

  // Keep core routes and optional advice on separate lines in GUIDE_CONTENT.
  const shouldKeep = (line) => {
    if (!supportsDeploy && line.includes("deploy-target")) {
      return false;
    }
    if (!supportsCartridge && line.includes("cartridge")) {
      return false;
    }
    if (!supportsDoctors && /\bdoctor\b/i.test(line)) {
      return false;
    }
    // Match command syntax, not prose such as "doctor suggestions" or "doctor kinds".
    const doctorCommands = line.matchAll(
      /(?:\bdoctor(?:\s*\/\s*audit_repository_contracts)?\s+kind=|(?:^|\brun\s+|\bleio-code\s+)doctor\s+(?:kind=)?)([a-z][a-z0-9-]*)\b/gi,
    );
    for (const [, kind] of doctorCommands) {
      if (!doctorPresets.has(kind) && !doctorKinds.has(kind)) {
        return false;
      }
    }
    return true;
  };

  return {
    ...guide,
    tools: guide.tools.filter(shouldKeep),
    examples: guide.examples.filter(shouldKeep),
  };
}

export function formatCapabilitiesSummary(capabilities) {
  if (!capabilities || typeof capabilities !== "object") {
    return [];
  }

  const sections = [];
  if (capabilities.workspace_profile) {
    sections.push(`Profile: ${capabilities.workspace_profile}`);
  }
  if (Array.isArray(capabilities.find_kinds)) {
    sections.push(`Find kinds: ${capabilities.find_kinds.join(", ")}`);
  }
  if (Array.isArray(capabilities.explain_kinds)) {
    sections.push(`Explain kinds: ${capabilities.explain_kinds.join(", ")}`);
  }
  if (Array.isArray(capabilities.graph_kinds)) {
    sections.push(`Graph kinds: ${capabilities.graph_kinds.join(", ")}`);
  }
  if (Array.isArray(capabilities.doctor_kinds)) {
    sections.push(
      `Doctor suites: ${
        capabilities.doctor_kinds.length > 0
          ? capabilities.doctor_kinds.join(", ")
          : "none configured"
      }`,
    );
  }
  if (Array.isArray(capabilities.notes) && capabilities.notes.length > 0) {
    sections.push(`Notes: ${capabilities.notes.join(" | ")}`);
  }
  return sections;
}

const TOPIC_NEXT_TOOLS = {
  general: ["leio_code_capabilities", "leio_code_context", "leio_code_find"],
  context: ["leio_code_context", "leio_code_find", "leio_code_graph"],
  lookup: ["leio_code_find", "leio_code_explain", "leio_code_graph"],
  explain: ["leio_code_explain", "leio_code_find", "leio_code_graph"],
  doctor: ["leio_code_doctor", "leio_code_audit", "leio_code_capabilities"],
  graph: ["leio_code_graph", "leio_code_find", "leio_code_nav"],
  navigation: ["leio_code_nav", "leio_code_graph", "leio_code_export"],
  export: ["leio_code_export", "leio_code_graph"],
  knowledge: ["leio_code_knowledge"],
  conversation: ["leio_code_conversation"],
};

/** Canonical stdio names; hosted callers map only tools their surface exposes. */
export function guideNextTools(topic, capabilities) {
  const candidates = TOPIC_NEXT_TOOLS[GUIDE_TOPICS.includes(topic) ? topic : "general"];
  const kindFields = {
    leio_code_find: "find_kinds", leio_code_explain: "explain_kinds",
    leio_code_doctor: "doctor_kinds", leio_code_audit: "doctor_kinds",
    leio_code_graph: "graph_kinds", leio_code_export: "export_kinds",
  };
  return candidates.filter((tool) => {
    const kinds = capabilities?.[kindFields[tool]];
    return !Array.isArray(kinds) || kinds.length > 0;
  });
}

/** Keep topic recommendations consistent for models and the optional UI. */
export function guideActionPalette(palette, nextTools) {
  if (!palette) return null;
  const guided = {
    ...palette,
    recommended_next_tools: [...nextTools],
    show_tools: [...new Set([...(palette.show_tools ?? []), ...nextTools])],
  };
  if (nextTools.length > 0) guided.recommended_start_tool = nextTools[0];
  else delete guided.recommended_start_tool;
  return guided;
}

export function buildGuideStructuredContent(topic, options = {}) {
  const selectedTopic = GUIDE_TOPICS.includes(topic) ? topic : "general";
  const guide = GUIDE_CONTENT[selectedTopic];
  const filtered = filterGuideByCapabilities(guide, options.capabilities);
  const capabilityLines = formatCapabilitiesSummary(options.capabilities);
  const nextTools = options.nextTools ?? guideNextTools(selectedTopic, options.capabilities);
  const textParts = [
    `LEIO Code guide: ${selectedTopic}`,
    `when: ${filtered.when}`,
    "",
    "tools:",
    ...filtered.tools.map((item) => `- ${item}`),
    "",
    "examples:",
    ...filtered.examples.map((item) => `- ${item}`),
  ];
  if (capabilityLines.length > 0) {
    textParts.push(
      "",
      "workspace capabilities:",
      ...capabilityLines.map((line) => `- ${line}`),
    );
  }
  if (options.routingNote) {
    textParts.push("", options.routingNote);
  }
  textParts.push("", nextTools.length > 0
    ? `Next tools: ${nextTools.join(", ")}`
    : "Next tools: none for this topic on this transport.");

  return {
    text: textParts.join("\n"),
    structuredContent: {
      topic: selectedTopic,
      when: filtered.when,
      tools: filtered.tools,
      examples: filtered.examples,
      workspace_capabilities: options.capabilities ?? null,
      next_tools: nextTools,
      routing_doc: options.routingDoc ?? "skills/leio-code/SKILL.md",
    },
  };
}
