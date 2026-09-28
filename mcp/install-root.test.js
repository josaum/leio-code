import assert from "node:assert/strict";
import test from "node:test";
import {
  isPluginMetadataDir,
  leioCheckoutCandidates,
  pluginRootFromEnv,
  resolveLeioCheckout,
} from "./install-root.js";

test("any dot-plugin directory is metadata, not a checkout", () => {
  assert.equal(isPluginMetadataDir(".claude-plugin"), true);
  assert.equal(isPluginMetadataDir(".codex-plugin"), true);
  assert.equal(isPluginMetadataDir(".cursor-plugin"), true);
  assert.equal(isPluginMetadataDir(".grok-plugin"), true);
  assert.equal(isPluginMetadataDir("leio-code"), false);
  assert.equal(isPluginMetadataDir("plugin"), false);
});

test("LEIO_PLUGIN_ROOT selects the directory for every host", () => {
  assert.equal(
    pluginRootFromEnv({ LEIO_PLUGIN_ROOT: "/opt/host/plugin", CLAUDE_PLUGIN_ROOT: "/claude" }, "/checkout"),
    "/opt/host/plugin",
  );
  assert.equal(
    pluginRootFromEnv({ CODEX_PLUGIN_ROOT: "/codex" }, "/checkout"),
    "/codex",
  );
  assert.equal(pluginRootFromEnv({}, "/checkout"), "/checkout");
});

test("a plugin metadata directory searches the parent checkout", () => {
  const candidates = leioCheckoutCandidates("/work/.cursor-plugin");
  assert.deepEqual(candidates, [
    "/work/.cursor-plugin",
    "/work",
    "/work/leio-code",
  ]);
  const found = resolveLeioCheckout("/work/.cursor-plugin", (candidate) => candidate === "/work");
  assert.equal(found, "/work");
});

test("a direct checkout does not search sibling directories", () => {
  assert.deepEqual(leioCheckoutCandidates("/src/leio-code"), ["/src/leio-code"]);
});
