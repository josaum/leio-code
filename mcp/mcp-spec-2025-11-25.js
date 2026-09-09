/**
 * Dual-era MCP contract for LEIO Code surfaces.
 *
 * Latest: 2026-07-28 (stateless core, server/discover, resultType).
 * Legacy: 2025-11-25 (initialize handshake) — still served so current
 * hosts (Grok / Claude / Cursor / SDK 1.30) keep working.
 *
 * Authoritative schema:
 *   schema/2026-07-28/schema.ts in the MCP spec tree
 * Human spec:
 *   https://modelcontextprotocol.io/specification/2026-07-28
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { z } from "zod";
import { InitializeRequestSchema } from "@modelcontextprotocol/sdk/types.js";

const here = dirname(fileURLToPath(import.meta.url));
const pkg = JSON.parse(readFileSync(join(here, "package.json"), "utf8"));

/** schema.ts `LATEST_PROTOCOL_VERSION` for 2026-07-28. */
export const PROTOCOL_VERSION = "2026-07-28";

/** Handshake-based revision still served to SDK 1.x / current hosts. */
export const LEGACY_PROTOCOL_VERSION = "2025-11-25";

export const SUPPORTED_PROTOCOL_VERSIONS = Object.freeze([
  PROTOCOL_VERSION,
  LEGACY_PROTOCOL_VERSION,
]);

export const SPEC_URL =
  "https://modelcontextprotocol.io/specification/2026-07-28";

export const SPEC_TOOLS_URL =
  "https://modelcontextprotocol.io/specification/2026-07-28/server/tools";

export const SPEC_LIFECYCLE_URL =
  "https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning";

export const SPEC_DISCOVER_URL =
  "https://modelcontextprotocol.io/specification/2026-07-28/server/discover";

export const SCHEMA_TS_URL =
  "https://github.com/modelcontextprotocol/specification/blob/main/schema/2026-07-28/schema.ts";

export const TOOLS_LIST_TTL_MS = 300_000;

export const PACKAGE_VERSION = String(pkg.version);

export const WEBSITE_URL = "https://github.com/josaum/leio-code";

/** schema.ts `Icon` — clients that render icons MUST support PNG. */
export const IMPLEMENTATION_ICONS = [
  {
    src: "https://raw.githubusercontent.com/josaum/leio-code/main/assets/icon.png",
    mimeType: "image/png",
    sizes: ["any"],
  },
];

/**
 * schema.ts `Implementation` for the stdio server (`initialize.serverInfo`).
 * `name` is the programmatic id; `title` is the UI display name.
 */
export const IMPLEMENTATION_STDIO = {
  name: "leio-code",
  title: "LEIO Code",
  version: PACKAGE_VERSION,
  description:
    "Evidence-first repository intelligence over stdio: status, capabilities, context, find, explain, graph, doctors, formal SPARQL, and lattice navigation. One absolute repo_root per call.",
  websiteUrl: WEBSITE_URL,
  icons: IMPLEMENTATION_ICONS,
};

/** schema.ts `Implementation` for the HTTP / ChatGPT Apps SDK server. */
export const IMPLEMENTATION_APPS_SDK = {
  name: "leio-code-apps-sdk",
  title: "LEIO Code Apps SDK",
  version: PACKAGE_VERSION,
  description:
    "Hosted MCP app for LEIO Code. Inspect allowlisted Git repositories over Streamable HTTP with OAuth, a ChatGPT widget, and the same evidence envelopes as the CLI.",
  websiteUrl: WEBSITE_URL,
  icons: IMPLEMENTATION_ICONS,
};

/**
 * initialize.instructions — hint for the host / model (schema.ts InitializeResult).
 */
export const INSTRUCTIONS_STDIO = [
  "LEIO Code indexes one absolute repo_root per call. Never reuse one tree's index against another.",
  "Start tasks with leio_code_context(task, repo_root); it includes provider and index diagnostics and grounded next calls. Use leio_code_status for health, leio_code_capabilities for supported kinds, and leio_code_guide if unsure. For a known exact file, graph can be called directly.",
  "Prefer those tools over Grep/Glob for symbols, env vars, routes, callers, and deploy wiring.",
  "leio_code_knowledge kind=explain answers only through SPARQL or refuses. kind=sparql is raw SPARQL. kind=compile rebuilds the wiki and formal.nq.",
  "leio_code_nav walks headings and the lattice; nav explain uses the same SPARQL proof and pins current_iri.",
  "Pin repo_root. Set LEIO_SESSION when two agents share a tree so nav cursors stay isolated.",
  "Tool execution failures (CLI non-zero, validation, timeouts) return CallToolResult.isError=true with actionable text. Retry with adjusted arguments. Unknown tools and malformed JSON-RPC stay protocol errors.",
  `Contract: MCP ${PROTOCOL_VERSION} dual-era with ${LEGACY_PROTOCOL_VERSION} (${SPEC_URL}).`,
].join(" ");

export const INSTRUCTIONS_APPS_SDK = [
  "This is the hosted LEIO Code MCP app. Prefer inspect_repository_status, inspect_repository_capabilities, then guide_repository_tools if the next tool is unclear.",
  "Pass repo_url (allowlisted HTTPS or owner/repo) or select_repository_target for the session. Do not invent absolute server repo_root paths.",
  "search_repository is find. graph_repository is the call/import graph. explain_repository is lineage. audit_repository_contracts is one doctor family. audit_repository_rollup is the composite CLI audit.",
  "search_repository_memory is local Arrow semantic retrieval, not a SPARQL proof.",
  "Auth failures and CLI failures return CallToolResult.isError=true so you can self-correct. Unknown tools stay JSON-RPC errors.",
  `Contract: MCP ${PROTOCOL_VERSION} dual-era with ${LEGACY_PROTOCOL_VERSION} (${SPEC_URL}).`,
].join(" ");

/**
 * Servers that expose tools MUST declare the tools capability.
 * We do not emit notifications/tools/list_changed, so listChanged is omitted
 * (schema default: server will not send list-changed notifications).
 */
export const TOOLS_CAPABILITY = {};

/** Apps SDK also serves the ChatGPT widget resource. No listChanged. */
export const RESOURCES_CAPABILITY = {};

/**
 * schema.ts ToolExecution.taskSupport.
 * Default when absent is "forbidden". We declare it explicitly.
 */
export const TOOL_EXECUTION_FORBIDDEN = Object.freeze({
  taskSupport: "forbidden",
});

const TOOL_NAME_RE = /^[A-Za-z0-9._-]{1,128}$/;

/**
 * schema.ts ToolAnnotations — all fields are untrusted hints.
 * Display-name precedence: Tool.title, then annotations.title, then name.
 */
export function toolAnnotations({
  title,
  readOnlyHint = false,
  destructiveHint = false,
  idempotentHint = false,
  openWorldHint = false,
} = {}) {
  const annotations = {
    readOnlyHint: Boolean(readOnlyHint),
    destructiveHint: Boolean(destructiveHint),
    idempotentHint: Boolean(idempotentHint),
    openWorldHint: Boolean(openWorldHint),
  };
  if (typeof title === "string" && title.length > 0) {
    annotations.title = title;
  }
  return annotations;
}

/**
 * Hosted inspect tools may clone/fetch a private checkout and refresh the
 * server-side index before returning. Not read-only in the spec sense.
 */
export const readOnlyAnnotations = toolAnnotations({
  readOnlyHint: false,
  destructiveHint: false,
  idempotentHint: true,
  openWorldHint: false,
});

/** Session-scoped target selection (in-memory MCP session only). */
export const sessionWriteAnnotations = toolAnnotations({
  readOnlyHint: false,
  destructiveHint: false,
  idempotentHint: true,
  openWorldHint: false,
});

/** Optional Carlos Motta / Vigoros bridge (calls a configured remote MCP). */
export const specialistAnnotations = toolAnnotations({
  readOnlyHint: false,
  destructiveHint: false,
  idempotentHint: false,
  openWorldHint: true,
});

/** Additive sidecar writes (index, export, knowledge compile). */
export const sidecarWriteAnnotations = toolAnnotations({
  readOnlyHint: false,
  destructiveHint: false,
  idempotentHint: true,
  openWorldHint: false,
});

/** May overwrite config (init --force). */
export const destructiveWriteAnnotations = toolAnnotations({
  readOnlyHint: false,
  destructiveHint: true,
  idempotentHint: false,
  openWorldHint: false,
});

/** Detached watcher process control. */
export const processControlAnnotations = toolAnnotations({
  readOnlyHint: false,
  destructiveHint: false,
  idempotentHint: false,
  openWorldHint: false,
});

/** Talks to a hosted service. */
export const openWorldWriteAnnotations = toolAnnotations({
  readOnlyHint: false,
  destructiveHint: true,
  idempotentHint: false,
  openWorldHint: true,
});

export const ANNOTATION_PRESETS = Object.freeze({
  inspect: readOnlyAnnotations,
  sessionWrite: sessionWriteAnnotations,
  specialist: specialistAnnotations,
  sidecarWrite: sidecarWriteAnnotations,
  destructiveWrite: destructiveWriteAnnotations,
  processControl: processControlAnnotations,
  openWorldWrite: openWorldWriteAnnotations,
});

/**
 * stdio tool catalog: name, title, annotations, execution.
 * `name` MUST match schema tool-name rules (1–128, [A-Za-z0-9._-]).
 */
export const STDIO_TOOL_CATALOG = Object.freeze({
  leio_code_conversation: {
    title: "Prepare local conversation review",
    annotations: readOnlyAnnotations,
  },
  leio_code_guide: {
    title: "Guide LEIO Code tools",
    annotations: readOnlyAnnotations,
  },
  leio_code_status: {
    title: "Inspect repository status",
    annotations: readOnlyAnnotations,
  },
  leio_code_capabilities: {
    title: "Inspect repository capabilities",
    annotations: readOnlyAnnotations,
  },
  leio_code_context: {
    title: "Prepare repository context",
    annotations: readOnlyAnnotations,
  },
  leio_code_index: {
    title: "Index repository",
    annotations: sidecarWriteAnnotations,
  },
  leio_code_find: {
    title: "Find repository entity",
    annotations: readOnlyAnnotations,
  },
  leio_code_explain: {
    title: "Explain repository entity",
    annotations: readOnlyAnnotations,
  },
  leio_code_doctor: {
    title: "Run architecture doctor",
    annotations: readOnlyAnnotations,
  },
  leio_code_audit: {
    title: "Audit repository rollup",
    annotations: readOnlyAnnotations,
  },
  leio_code_graph: {
    title: "Graph repository",
    annotations: readOnlyAnnotations,
  },
  leio_code_export: {
    title: "Export formal artifacts",
    annotations: sidecarWriteAnnotations,
  },
  leio_code_init: {
    title: "Initialize LEIO Code",
    annotations: destructiveWriteAnnotations,
  },
  leio_code_verify: {
    title: "Verify repository contracts",
    annotations: readOnlyAnnotations,
  },
  leio_code_watch: {
    title: "Control repository watcher",
    annotations: processControlAnnotations,
  },
  leio_code_nav: {
    title: "Navigate lattice and headings",
    annotations: sessionWriteAnnotations,
  },
  leio_code_knowledge: {
    title: "Query knowledge wiki and SPARQL",
    annotations: sidecarWriteAnnotations,
  },
  leio_code_kb: {
    title: "Pure-Arrow knowledge base",
    annotations: sidecarWriteAnnotations,
  },
});

export function assertToolName(name) {
  if (!TOOL_NAME_RE.test(name)) {
    throw new Error(
      `MCP tool name "${name}" must be 1–128 chars of [A-Za-z0-9._-]`,
    );
  }
  return name;
}

/** Result `_meta` noting the negotiated contract (schema.ts Result._meta). */
export function protocolMeta(extra = {}, implementation = IMPLEMENTATION_STDIO) {
  const extraMeta =
    extra && typeof extra === "object" && !Array.isArray(extra) ? extra : {};
  return {
    "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
    "io.modelcontextprotocol/serverInfo": {
      name: implementation.name,
      title: implementation.title,
      version: implementation.version,
    },
    "io.leio/mcpProtocolVersion": PROTOCOL_VERSION,
    "io.leio/mcpSpec": SPEC_URL,
    ...extraMeta,
  };
}

function errorMessage(error) {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  if (typeof error === "string" && error.trim()) {
    return error.trim();
  }
  return "Tool execution failed";
}

/**
 * schema.ts CallToolResult for tool execution errors.
 * Used for CLI failures, input validation, timeouts, and business-logic errors.
 * Unknown tools and malformed CallToolRequest stay JSON-RPC protocol errors.
 */
export function executionErrorResult(error, extra = {}) {
  const message = errorMessage(error);
  const structuredContent = {
    ok: false,
    error: message,
    ...extra,
  };
  return {
    resultType: "complete",
    isError: true,
    content: [
      {
        type: "text",
        text: message,
      },
    ],
    structuredContent,
    _meta: protocolMeta({ errorKind: "tool-execution" }),
  };
}

function serializeStructuredContent(structuredContent) {
  try {
    return JSON.stringify(structuredContent);
  } catch {
    return String(structuredContent);
  }
}

/**
 * Normalize a handler return into a CallToolResult:
 * - `resultType` is `complete` (2026-07-28 MUST)
 * - `content` is always a non-empty ContentBlock array
 * - `structuredContent` is an object when present
 * - structured results are also serialized into a text block when the
 *   handler did not already provide text (spec SHOULD dual-write)
 * - CLI / business failures set `isError: true` (not a JSON-RPC error)
 */
export function finalizeCallToolResult(result, extraMeta = {}) {
  if (!result || typeof result !== "object" || Array.isArray(result)) {
    return executionErrorResult("Tool handler returned a non-object result");
  }

  const structured =
    result.structuredContent &&
    typeof result.structuredContent === "object" &&
    !Array.isArray(result.structuredContent)
      ? result.structuredContent
      : undefined;

  const failed =
    result.isError === true ||
    (structured !== undefined && structured.ok === false);

  let content = Array.isArray(result.content)
    ? result.content.filter(
        (block) => block && typeof block === "object" && typeof block.type === "string",
      )
    : [];

  if (content.length === 0) {
    content = [
      {
        type: "text",
        text:
          structured !== undefined
            ? serializeStructuredContent(structured)
            : failed
              ? "Tool execution failed"
              : "Tool completed without content",
      },
    ];
  }

  return {
    ...result,
    resultType: "complete",
    content,
    ...(structured !== undefined ? { structuredContent: structured } : {}),
    isError: failed,
    _meta: {
      ...protocolMeta(extraMeta),
      ...(result._meta && typeof result._meta === "object" ? result._meta : {}),
    },
  };
}

/**
 * Wrap a tools/call handler so thrown Errors become execution errors
 * (isError:true) instead of JSON-RPC INTERNAL_ERROR.
 */
export function wrapToolHandler(handler) {
  return async (...args) => {
    try {
      const result = await handler(...args);
      return finalizeCallToolResult(result);
    } catch (error) {
      return executionErrorResult(error);
    }
  };
}

/**
 * Config object for `McpServer.registerTool`.
 */
export function toolRegistrationConfig({
  title,
  description,
  inputSchema,
  outputSchema,
  annotations,
  extraMeta = {},
} = {}) {
  return {
    title,
    description,
    inputSchema,
    ...(outputSchema ? { outputSchema } : {}),
    annotations: annotations ?? readOnlyAnnotations,
    execution: TOOL_EXECUTION_FORBIDDEN,
    _meta: protocolMeta(extraMeta),
  };
}

export function stdioServerOptions() {
  return {
    capabilities: {
      tools: TOOLS_CAPABILITY,
    },
    instructions: INSTRUCTIONS_STDIO,
  };
}

export function appsSdkServerOptions() {
  return {
    capabilities: {
      tools: TOOLS_CAPABILITY,
      resources: RESOURCES_CAPABILITY,
    },
    instructions: INSTRUCTIONS_APPS_SDK,
  };
}

export function decorateCacheableResult(result, { ttlMs = TOOLS_LIST_TTL_MS, cacheScope = "public" } = {}) {
  const base = result && typeof result === "object" ? result : {};
  return {
    ...base,
    resultType: base.resultType ?? "complete",
    ttlMs: Number.isFinite(base.ttlMs) ? base.ttlMs : ttlMs,
    cacheScope: base.cacheScope ?? cacheScope,
    _meta: {
      ...protocolMeta(),
      ...(base._meta && typeof base._meta === "object" ? base._meta : {}),
    },
  };
}

export function buildDiscoverResult({
  implementation = IMPLEMENTATION_STDIO,
  capabilities,
  instructions,
} = {}) {
  return decorateCacheableResult(
    {
      resultType: "complete",
      supportedVersions: [...SUPPORTED_PROTOCOL_VERSIONS],
      capabilities: capabilities ?? { tools: TOOLS_CAPABILITY },
      instructions,
      _meta: protocolMeta({}, implementation),
    },
    { ttlMs: 3_600_000, cacheScope: "public" },
  );
}

const DiscoverRequestSchema = z.object({
  method: z.literal("server/discover"),
  params: z
    .object({
      _meta: z.record(z.unknown()).optional(),
    })
    .passthrough()
    .optional(),
});

/**
 * Dual-era install: keep SDK initialize for 2025-11-25 hosts, and add
 * 2026-07-28 `server/discover` plus cache hints on tools/list.
 */
export function installModernProtocol(mcpServer, options = {}) {
  const proto = mcpServer.server;
  proto.setRequestHandler(DiscoverRequestSchema, async () =>
    buildDiscoverResult(options),
  );

  // Dual-era initialize echo: when a host requests the 2026-07-28 era in the
  // handshake, the SDK would downgrade the response to its own latest
  // (2025-11-25) because 1.30 predates that era. This server implements the
  // 2026 surface (server/discover, resultType, per-request _meta), so the
  // requested version is echoed instead — a strict 2026-era host otherwise
  // closes the connection right after the handshake (observed on Claude
  // Desktop Cowork pools). Clients on 2025-11-25 are unaffected: the SDK
  // already echoes their requested version.
  const originalInitialize = proto._requestHandlers?.get?.("initialize");
  if (typeof originalInitialize === "function") {
    proto.setRequestHandler(InitializeRequestSchema, async (request, extra) => {
      const result = await originalInitialize(request, extra);
      if (request.params?.protocolVersion === PROTOCOL_VERSION
          && result.protocolVersion !== PROTOCOL_VERSION) {
        result.protocolVersion = PROTOCOL_VERSION;
      }
      return result;
    });
  }

  const originalList = proto._requestHandlers?.get?.("tools/list");
  if (typeof originalList === "function") {
    proto._requestHandlers.set("tools/list", async (request, extra) => {
      const listed = await originalList(request, extra);
      return decorateCacheableResult(listed);
    });
  }

  const originalResources = proto._requestHandlers?.get?.("resources/list");
  if (typeof originalResources === "function") {
    proto._requestHandlers.set("resources/list", async (request, extra) => {
      const listed = await originalResources(request, extra);
      return decorateCacheableResult(listed);
    });
  }
}

/**
 * Gate tools/list and tools/call behind an async `prepare` step so the
 * transport connects (and probes are answered) before any slow startup work
 * runs, while no list or call response races ahead of that work.
 *
 * `prepare` must leave the registered tool set in a consistent state; the
 * tools themselves must already be registered synchronously (the SDK installs
 * its handlers and capabilities at first registration and refuses capability
 * changes once connected).
 *
 * A `prepare` failure is remembered and rethrown to every gated call; pass a
 * prepare step that swallows expected failures (e.g. an unavailable kind
 * catalog degrading to permissive schemas) so tools still serve.
 */
export function installLazyToolAccess(mcpServer, prepare) {
  const proto = mcpServer.server;
  const handlers = proto._requestHandlers;
  if (!(handlers instanceof Map)) {
    throw new Error(
      "installLazyToolAccess requires the SDK Protocol _requestHandlers map",
    );
  }

  const baseList = handlers.get("tools/list");
  const baseCall = handlers.get("tools/call");
  if (typeof baseList !== "function" || typeof baseCall !== "function") {
    throw new Error(
      "installLazyToolAccess requires tools to be registered before connecting",
    );
  }

  let readyPromise = null;
  function ensureReady() {
    if (!readyPromise) {
      readyPromise = Promise.resolve()
        .then(prepare)
        .catch((error) => {
          readyPromise = null;
          throw error;
        });
    }
    return readyPromise;
  }

  handlers.set("tools/list", async (request, extra) => {
    await ensureReady();
    return baseList(request, extra);
  });
  handlers.set("tools/call", async (request, extra) => {
    await ensureReady();
    return baseCall(request, extra);
  });
}
