/**
 * Name remap between the stdio MCP surface and the hosted Apps SDK surface.
 *
 * The hosted surface is a documented remap, not a copy: tools that need a
 * local checkout, a local cursor, or local artifacts stay on stdio. This
 * module is the single source of truth for that contract — server.js consumes
 * it, and surface-parity.test.js asserts both committed surfaces against it,
 * so a new tool on either transport cannot drift in silently.
 *
 * Contract doc: docs/MCP-SURFACE-GAP.md.
 */

/** stdio MCP tool name -> hosted Apps SDK tool name. */
export const appToolNameMap = {
  leio_code_guide: "guide_repository_tools",
  leio_code_capabilities: "inspect_repository_capabilities",
  leio_code_status: "inspect_repository_status",
  leio_code_context: "prepare_repository_context",
  leio_code_find: "search_repository",
  leio_code_explain: "explain_repository",
  leio_code_doctor: "audit_repository_contracts",
  leio_code_audit: "audit_repository_rollup",
  leio_code_graph: "graph_repository",
  // Deliberate many-to-one: the hosted status tool answers export questions
  // too, and no separate hosted export tool exists.
  leio_code_export: "inspect_repository_status",
};

/** Hosted Apps SDK tools with no stdio counterpart. */
export const hostedOnlyTools = [
  "search_repository_memory",
  "select_repository_target",
  "clear_repository_target",
];

/** stdio tools that stay local: files, cursors and artifacts on this machine. */
export const stdioLocalOnlyTools = [
  "leio_code_conversation",
  "leio_code_index",
  "leio_code_init",
  "leio_code_kb",
  "leio_code_knowledge",
  "leio_code_nav",
  "leio_code_verify",
  "leio_code_watch",
];

export function mapToolNameToAppSurface(toolName) {
  return appToolNameMap[toolName] ?? toolName;
}
