import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

test("quick status uses the bounded baseline doctor preset on both MCP surfaces", () => {
  const stdioSource = fs.readFileSync(path.join(here, "index.js"), "utf8");
  const appsSource = fs.readFileSync(
    path.join(here, "..", "apps-sdk", "server.js"),
    "utf8",
  );

  assert.match(
    stdioSource,
    /buildInvocation\(\[\s*"--json",\s*"--repo",\s*repoRoot,\s*"doctor",\s*"baseline",/s,
  );
  assert.doesNotMatch(
    stdioSource,
    /buildInvocation\(\["--json", "--repo", repoRoot, "doctor", "all"\]\)/,
  );
  assert.match(
    appsSource,
    /invokeLeioTool\(\["doctor", "baseline"\],\s*\{/,
  );
  assert.doesNotMatch(
    appsSource,
    /invokeLeioTool\(\["doctor", "all"\],\s*\{\s*repoRoot/s,
  );
});
