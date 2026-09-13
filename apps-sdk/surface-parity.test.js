import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  appToolNameMap,
  hostedOnlyTools,
  mapToolNameToAppSurface,
  stdioLocalOnlyTools,
} from "./tool-name-map.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

// Committed stdio surface. schema/leio-code-mcp-service-descriptor.json is
// regenerated from a live stdio tools/list by mcp/generate-descriptor.mjs, and
// mcp/service-descriptor.test.js fails when it goes stale.
const stdioTools = Object.keys(
  JSON.parse(
    readFileSync(
      resolve(__dirname, "..", "schema", "leio-code-mcp-service-descriptor.json"),
      "utf8",
    ),
  ).methods,
).sort();

// Committed hosted surface. apps-sdk/submission-contract.test.js fails when
// this file stops matching a live Apps SDK tools/list.
const hostedTools = Object.keys(
  JSON.parse(
    readFileSync(resolve(__dirname, "chatgpt-app-submission.json"), "utf8"),
  ).tools,
).sort();

test("every stdio tool is remapped onto the hosted surface or declared local-only", () => {
  const remapped = Object.keys(appToolNameMap);
  assert.deepEqual(
    stdioTools.filter((name) => !remapped.includes(name)),
    [...stdioLocalOnlyTools].sort(),
    "a new stdio tool must be remapped in tool-name-map.js or declared in stdioLocalOnlyTools",
  );
});

test("every hosted tool is a stdio remap target or declared hosted-only", () => {
  const targets = new Set(Object.values(appToolNameMap));
  assert.deepEqual(
    hostedTools.filter((name) => !targets.has(name)),
    [...hostedOnlyTools].sort(),
    "a new hosted tool must come from tool-name-map.js or be declared in hostedOnlyTools",
  );
});

test("the remap is defined only over tools the stdio surface actually exposes", () => {
  assert.deepEqual(
    Object.keys(appToolNameMap).filter((name) => !stdioTools.includes(name)),
    [],
    "tool-name-map.js names a tool that no longer exists on stdio",
  );
});

test("the two surfaces partition into the shared tools plus the documented sets", () => {
  assert.equal(
    stdioTools.length,
    Object.keys(appToolNameMap).length + stdioLocalOnlyTools.length,
    "stdio surface = remapped tools + local-only tools",
  );
  assert.equal(
    hostedTools.length,
    new Set(Object.values(appToolNameMap)).size + hostedOnlyTools.length,
    "hosted surface = remap targets + hosted-only tools",
  );
});

test("unmapped names pass through unchanged, so hosted-only tools keep their names", () => {
  for (const name of hostedOnlyTools) {
    assert.equal(mapToolNameToAppSurface(name), name);
  }
  assert.equal(mapToolNameToAppSurface("leio_code_find"), "search_repository");
  assert.equal(mapToolNameToAppSurface("leio_code_graph"), "graph_repository");
  assert.equal(mapToolNameToAppSurface("leio_code_export"), "leio_code_export");
});
