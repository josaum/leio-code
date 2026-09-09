// The committed service descriptor must match a regeneration from the live
// server. Drift means a tool/schema change forgot the artifact.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const artifact = path.join(here, "..", "schema", "leio-code-mcp-service-descriptor.json");

test("service descriptor matches a live tools/list regeneration", () => {
  const before = readFileSync(artifact, "utf8");
  execFileSync(process.execPath, [path.join(here, "generate-descriptor.mjs")], {
    cwd: path.join(here, ".."),
    stdio: "pipe",
  });
  const after = readFileSync(artifact, "utf8");
  assert.equal(after, before, "schema/leio-code-mcp-service-descriptor.json is stale — regenerate with `node mcp/generate-descriptor.mjs`");

  const descriptor = JSON.parse(after);
  const names = Object.keys(descriptor.methods);
  assert.ok(names.length >= 16, `expected the full tool catalog, got ${names.length}`);
  for (const name of names) {
    const method = descriptor.methods[name];
    assert.equal(method.type, "method", name);
    assert.ok(Array.isArray(method.params), `${name}.params must be an array`);
    assert.ok(method.description, `${name} must carry a description`);
  }
});
