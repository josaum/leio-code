import { compactEditingResult } from "../mcp/compact.js";

/**
 * Hosted graph matches stdio: the default packet is compact, and `full`
 * returns the complete tool result. Hosted fields stay on the compact packet.
 */
export function presentHostedGraphResult(result, { full = false, scope = {} } = {}) {
  if (full === true || !result?.structuredContent?.ok) {
    return result;
  }
  const compacted = compactEditingResult(
    {
      ...result,
      structuredContent: {
        ...result.structuredContent,
        tool_family: "graph",
      },
    },
    { full: false, scope },
  );
  return {
    ...result,
    content: compacted.content ?? result.content,
    structuredContent: {
      ...result.structuredContent,
      tool_family: "graph",
      envelope:
        compacted.structuredContent?.envelope ??
        result.structuredContent.envelope,
      next_calls: compacted.structuredContent?.next_calls,
      limitation: compacted.structuredContent?.limitation,
      diagnostics: compacted.structuredContent?.diagnostics,
    },
  };
}
