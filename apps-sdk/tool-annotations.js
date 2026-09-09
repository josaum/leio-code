/**
 * ToolAnnotations from MCP 2025-11-25 (schema.ts).
 * All properties are untrusted hints. Display-name precedence is
 * Tool.title, then annotations.title, then name.
 *
 * Hosted inspect tools may clone/fetch a private checkout and refresh
 * the server-side index, so they are not readOnlyHint: true.
 */
export {
  openWorldWriteAnnotations,
  readOnlyAnnotations,
  sessionWriteAnnotations,
  sidecarWriteAnnotations,
  specialistAnnotations,
} from "../mcp/mcp-spec-2025-11-25.js";
