import assert from "node:assert/strict";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

import { STDIO_TOOL_CATALOG } from "./mcp-spec-2025-11-25.js";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..");

function write(root, rel, body) {
  const path = join(root, rel);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, body);
  return path;
}

function stageFixture() {
  const root = mkdtempSync(join(tmpdir(), "leio-mcp-resilience-"));
  write(
    root,
    "Cargo.toml",
    "[package]\nname = \"mcp-resilience-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
  );
  write(
    root,
    "src/lib.rs",
    "pub fn encode_payload() {\n    let _ = std::env::var(\"MCP_SURFACE_TOKEN\");\n}\n",
  );
  return root;
}

/**
 * A trusted-looking LEIO_CODE_BIN that always fails: the server must survive
 * a broken binary at kind-catalog load and degrade to permissive schemas
 * instead of dying before it can answer a client probe.
 */
function stageBrokenBinary() {
  const dir = mkdtempSync(join(tmpdir(), "leio-mcp-broken-bin-"));
  const stub = write(dir, "leio-code", "#!/bin/sh\nexit 3\n");
  chmodSync(stub, 0o755);
  return stub;
}

async function withClient(t, envOverrides, fn) {
  const fixture = stageFixture();
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [join(here, "index.js")],
    env: {
      ...process.env,
      LEIO_CODE_REPO_ROOT: fixture,
      // Keep startup diagnostics out of the developer's real ~/.leio-code log.
      HOME: fixture,
      ...envOverrides,
    },
    stderr: "pipe",
  });
  const client = new Client({
    name: "leio-code-mcp-resilience",
    version: "2.6.0",
  });
  await client.connect(transport);
  t.after(async () => {
    await client.close().catch(() => {});
  });

  async function call(name, args = {}, timeoutMs = 60_000) {
    return client.callTool(
      {
        name,
        arguments: { repo_root: fixture, timeout_ms: timeoutMs, ...args },
      },
      undefined,
      { timeout: timeoutMs + 5_000 },
    );
  }

  return fn({ client, call, fixture });
}

async function assertFullCatalogListed(client) {
  const listed = await client.listTools();
  const names = listed.tools.map((tool) => tool.name).sort();
  assert.deepEqual(names, Object.keys(STDIO_TOOL_CATALOG).sort());
  return listed;
}

function findKindProperty(listed) {
  const find = listed.tools.find((tool) => tool.name === "leio_code_find");
  assert.ok(find, "leio_code_find missing from tools/list");
  return find.inputSchema.properties.kind;
}

test("broken leio binary: server still starts, lists tools, degrades kind schemas", async (t) => {
  const brokenBin = stageBrokenBinary();
  await withClient(t, { LEIO_CODE_BIN: brokenBin }, async ({ client, call, fixture }) => {
    const listed = await assertFullCatalogListed(client);

    const kind = findKindProperty(listed);
    assert.equal(
      kind.enum,
      undefined,
      "kind schema should degrade to free-form string when the catalog is unavailable",
    );
    assert.match(String(kind.description), /kind catalog unavailable/i);

    const find = await call("leio_code_find", {
      kind: "symbol",
      needle: "encode_payload",
    });
    assert.equal(find.isError, true);
    assert.match(
      (find.content ?? []).map((item) => item.text).join("\n"),
      /exit code 3/i,
    );

    const status = await call("leio_code_status");
    assert.equal(status.isError, true);
    assert.equal(status.structuredContent.ok, false);
    assert.equal(status.structuredContent.doctor_summary.scope, "baseline");
    assert.equal(status.structuredContent.doctor_summary.status, "failed");
    assert.equal(status.structuredContent.doctor_summary.exit_code, 3);
    assert.doesNotMatch(status.content.map((item) => item.text).join("\n"), /all .*green/);

    const startupLog = readFileSync(
      join(fixture, ".leio-code", "mcp-startup-errors.log"),
      "utf8",
    );
    assert.match(startupLog, /\[kind-catalog\]/);
  });
});

test("catalog timeout: server works with degraded schemas and a healthy binary", async (t) => {
  await withClient(
    t,
    { LEIO_CODE_CATALOG_TIMEOUT_MS: "1" },
    async ({ client, call }) => {
      const listed = await assertFullCatalogListed(client);
      assert.equal(
        findKindProperty(listed).enum,
        undefined,
        "kind schema should degrade when the catalog load times out",
      );

      const find = await call("leio_code_find", {
        kind: "symbol",
        needle: "encode_payload",
      });
      assert.equal(find.isError, false);
      assert.equal(find.structuredContent?.ok, true);
      const entityNames = (find.structuredContent?.envelope?.entities ?? []).map(
        (row) => row.name,
      );
      assert.ok(entityNames.includes("encode_payload"), JSON.stringify(entityNames));
    },
  );
});
