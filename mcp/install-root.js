import path from "node:path";

/// Host-neutral plugin directory. LEIO_PLUGIN_ROOT wins; known host variables
/// remain as fallbacks. The MCP file's parent directory is the source checkout.
export function pluginRootFromEnv(env, fallback) {
  const selected =
    env.LEIO_PLUGIN_ROOT ||
    env.CLAUDE_PLUGIN_ROOT ||
    env.CODEX_PLUGIN_ROOT ||
    fallback;
  return path.resolve(selected);
}

/// `.claude-plugin`, `.codex-plugin`, and any other `.<host>-plugin` directory
/// sit beside or inside the checkout. The name of the host is not special.
export function isPluginMetadataDir(basename) {
  return basename.startsWith(".") && basename.endsWith("-plugin");
}

export function leioCheckoutCandidates(pluginRoot) {
  const candidates = [pluginRoot];
  if (isPluginMetadataDir(path.basename(pluginRoot))) {
    candidates.push(path.resolve(pluginRoot, ".."));
    candidates.push(path.resolve(pluginRoot, "..", "leio-code"));
  }
  return candidates;
}

/// `exists` reports whether a candidate contains this checkout's Cargo.toml
/// and mcp/index.js. The first match wins. With no match, the original root
/// is returned so the caller can report a missing checkout.
export function resolveLeioCheckout(pluginRoot, exists) {
  const candidates = leioCheckoutCandidates(pluginRoot);
  for (const candidate of candidates) {
    if (exists(candidate)) {
      return candidate;
    }
  }
  return candidates[0];
}
