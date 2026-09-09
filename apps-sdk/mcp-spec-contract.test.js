import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  IMPLEMENTATION_APPS_SDK,
  PROTOCOL_VERSION,
  SUPPORTED_PROTOCOL_VERSIONS,
  TOOL_EXECUTION_FORBIDDEN,
  appsSdkServerOptions,
} from "../mcp/mcp-spec-2025-11-25.js";
import {
  readOnlyAnnotations,
  sessionWriteAnnotations,
  specialistAnnotations,
} from "./tool-annotations.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

test("Apps SDK is dual-era MCP 2026-07-28 with 2025-11-25 handshake", () => {
  const serverSource = readFileSync(resolve(__dirname, "server.js"), "utf8");
  assert.match(serverSource, /IMPLEMENTATION_APPS_SDK/);
  assert.match(serverSource, /appsSdkServerOptions/);
  assert.match(serverSource, /wrapToolHandler/);
  assert.match(serverSource, /executionErrorResult/);
  assert.match(serverSource, /TOOL_EXECUTION_FORBIDDEN/);
  assert.match(serverSource, /installModernProtocol/);
  assert.equal(PROTOCOL_VERSION, "2026-07-28");
  assert.deepEqual(SUPPORTED_PROTOCOL_VERSIONS, ["2026-07-28", "2025-11-25"]);
  const { version: pkgVersion } = JSON.parse(
    readFileSync(resolve(__dirname, "..", "mcp", "package.json"), "utf8"),
  );
  assert.equal(IMPLEMENTATION_APPS_SDK.version, pkgVersion);
  assert.equal(IMPLEMENTATION_APPS_SDK.name, "leio-code-apps-sdk");
  assert.ok(IMPLEMENTATION_APPS_SDK.title);
  assert.ok(IMPLEMENTATION_APPS_SDK.description);
  assert.ok(IMPLEMENTATION_APPS_SDK.websiteUrl);
  const options = appsSdkServerOptions();
  assert.deepEqual(options.capabilities.tools, {});
  assert.deepEqual(options.capabilities.resources, {});
  assert.match(options.instructions, /2026-07-28/);
  assert.equal(TOOL_EXECUTION_FORBIDDEN.taskSupport, "forbidden");
});

test("hosted ToolAnnotations stay on the MCP hint set", () => {
  for (const [label, annotations] of [
    ["inspect", readOnlyAnnotations],
    ["session", sessionWriteAnnotations],
    ["specialist", specialistAnnotations],
  ]) {
    assert.equal(typeof annotations.readOnlyHint, "boolean", label);
    assert.equal(typeof annotations.destructiveHint, "boolean", label);
    assert.equal(typeof annotations.idempotentHint, "boolean", label);
    assert.equal(typeof annotations.openWorldHint, "boolean", label);
  }
  assert.equal(readOnlyAnnotations.readOnlyHint, false);
  assert.equal(specialistAnnotations.openWorldHint, true);
});
