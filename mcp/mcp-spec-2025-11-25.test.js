import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  ANNOTATION_PRESETS,
  IMPLEMENTATION_APPS_SDK,
  IMPLEMENTATION_STDIO,
  INSTRUCTIONS_APPS_SDK,
  INSTRUCTIONS_STDIO,
  PACKAGE_VERSION,
  LEGACY_PROTOCOL_VERSION,
  PROTOCOL_VERSION,
  SUPPORTED_PROTOCOL_VERSIONS,
  SPEC_URL,
  STDIO_TOOL_CATALOG,
  TOOL_EXECUTION_FORBIDDEN,
  appsSdkServerOptions,
  buildDiscoverResult,
  decorateCacheableResult,
  assertToolName,
  executionErrorResult,
  finalizeCallToolResult,
  protocolMeta,
  stdioServerOptions,
  toolAnnotations,
  toolRegistrationConfig,
  wrapToolHandler,
} from "./mcp-spec-2025-11-25.js";
import { LeioToolOutputSchema } from "./output-schemas.js";

const here = dirname(fileURLToPath(import.meta.url));
const TOOL_NAME_RE = /^[A-Za-z0-9._-]{1,128}$/;

test("protocol version is dual-era 2026-07-28 plus legacy 2025-11-25", () => {
  assert.equal(PROTOCOL_VERSION, "2026-07-28");
  assert.equal(LEGACY_PROTOCOL_VERSION, "2025-11-25");
  assert.deepEqual(SUPPORTED_PROTOCOL_VERSIONS, ["2026-07-28", "2025-11-25"]);
  assert.match(SPEC_URL, /2026-07-28/);
  assert.equal(PACKAGE_VERSION, PACKAGE_VERSION, "PACKAGE_VERSION must stay exported");
  const { version } = JSON.parse(readFileSync(join(here, "package.json"), "utf8"));
  assert.equal(PACKAGE_VERSION, version);
});

test("serverInfo matches schema.ts Implementation", () => {
  for (const info of [IMPLEMENTATION_STDIO, IMPLEMENTATION_APPS_SDK]) {
    assert.equal(typeof info.name, "string");
    assert.equal(typeof info.title, "string");
    assert.equal(info.version, PACKAGE_VERSION);
    assert.equal(typeof info.description, "string");
    assert.match(info.websiteUrl, /^https:\/\//);
    assert.ok(Array.isArray(info.icons));
    assert.equal(info.icons[0].mimeType, "image/png");
    assert.ok(info.icons[0].src.startsWith("https://"));
  }
  assert.equal(IMPLEMENTATION_STDIO.name, "leio-code");
  assert.equal(IMPLEMENTATION_APPS_SDK.name, "leio-code-apps-sdk");
});

test("initialize advertises tools capability and instructions", () => {
  const stdio = stdioServerOptions();
  assert.deepEqual(stdio.capabilities.tools, {});
  assert.match(stdio.instructions, /2026-07-28/);
  assert.match(stdio.instructions, /isError/);
  assert.match(INSTRUCTIONS_STDIO, /leio_code_status/);

  const apps = appsSdkServerOptions();
  assert.deepEqual(apps.capabilities.tools, {});
  assert.deepEqual(apps.capabilities.resources, {});
  assert.match(apps.instructions, /repo_url/);
  assert.match(INSTRUCTIONS_APPS_SDK, /CallToolResult.isError/);
});

test("taskSupport is explicitly forbidden (schema default)", () => {
  assert.equal(TOOL_EXECUTION_FORBIDDEN.taskSupport, "forbidden");
});

test("ToolAnnotations expose all four spec hints", () => {
  for (const [name, preset] of Object.entries(ANNOTATION_PRESETS)) {
    assert.equal(typeof preset.readOnlyHint, "boolean", name);
    assert.equal(typeof preset.destructiveHint, "boolean", name);
    assert.equal(typeof preset.idempotentHint, "boolean", name);
    assert.equal(typeof preset.openWorldHint, "boolean", name);
  }
  const titled = toolAnnotations({ title: "Find", readOnlyHint: true });
  assert.equal(titled.title, "Find");
  assert.equal(titled.readOnlyHint, true);
});

test("stdio catalog names obey MCP tool-name rules", () => {
  const names = Object.keys(STDIO_TOOL_CATALOG);
  for (const required of [
    "leio_code_status",
    "leio_code_capabilities",
    "leio_code_context",
    "leio_code_find",
    "leio_code_graph",
    "leio_code_doctor",
    "leio_code_audit",
  ]) {
    assert.ok(names.includes(required), `missing required tool ${required}`);
  }
  for (const name of names) {
    assert.match(name, TOOL_NAME_RE);
    assert.equal(assertToolName(name), name);
    const entry = STDIO_TOOL_CATALOG[name];
    assert.equal(typeof entry.title, "string");
    assert.ok(entry.annotations);
  }
  assert.throws(() => assertToolName("bad name"), /1–128/);
});

test("executionErrorResult is a CallToolResult, not a protocol error", () => {
  const result = executionErrorResult(new Error("needle is required"));
  assert.equal(result.isError, true);
  assert.equal(result.content[0].type, "text");
  assert.match(result.content[0].text, /needle is required/);
  assert.equal(result.structuredContent.ok, false);
  assert.equal(result.structuredContent.error, "needle is required");
  assert.equal(result.resultType, "complete");
  assert.equal(result._meta["io.leio/mcpProtocolVersion"], PROTOCOL_VERSION);
  assert.equal(
    result._meta["io.modelcontextprotocol/protocolVersion"],
    PROTOCOL_VERSION,
  );
  assert.equal(typeof result.structuredContent, "object");
  assert.ok(!Array.isArray(result.structuredContent));
});

test("finalizeCallToolResult dual-writes structuredContent and sets isError from ok", () => {
  const failed = finalizeCallToolResult({
    structuredContent: { ok: false, error: "exit 2" },
  });
  assert.equal(failed.isError, true);
  assert.equal(failed.content[0].type, "text");
  assert.match(failed.content[0].text, /exit 2/);

  const ok = finalizeCallToolResult({
    content: [{ type: "text", text: "ready" }],
    structuredContent: { ok: true, repo_root: "/tmp/repo" },
  });
  assert.equal(ok.isError, false);
  assert.equal(ok.content[0].text, "ready");
  assert.equal(ok.structuredContent.repo_root, "/tmp/repo");
  assert.equal(ok.resultType, "complete");
  assert.equal(ok._meta["io.leio/mcpSpec"], SPEC_URL);
  assert.equal(ok._meta["io.modelcontextprotocol/protocolVersion"], PROTOCOL_VERSION);
});

test("wrapToolHandler converts thrown Errors into isError results", async () => {
  const handler = wrapToolHandler(async () => {
    throw new Error("graph kind callers-of requires needle");
  });
  const result = await handler({});
  assert.equal(result.isError, true);
  assert.match(result.content[0].text, /requires needle/);
});

test("toolRegistrationConfig declares execution.taskSupport forbidden", () => {
  const config = toolRegistrationConfig({
    title: "Find repository entity",
    description: "Find a symbol",
    inputSchema: { needle: {} },
    outputSchema: LeioToolOutputSchema,
    annotations: ANNOTATION_PRESETS.inspect,
  });
  assert.equal(config.execution.taskSupport, "forbidden");
  assert.equal(config.title, "Find repository entity");
  assert.equal(config.outputSchema, LeioToolOutputSchema);
  assert.equal(config._meta["io.leio/mcpProtocolVersion"], PROTOCOL_VERSION);
});

test("protocolMeta is a Result._meta object", () => {
  const meta = protocolMeta({ tool: "leio_code_find" });
  assert.equal(meta["io.leio/mcpProtocolVersion"], PROTOCOL_VERSION);
  assert.equal(meta["io.modelcontextprotocol/protocolVersion"], PROTOCOL_VERSION);
  assert.equal(meta["io.modelcontextprotocol/serverInfo"].name, "leio-code");
  assert.equal(meta.tool, "leio_code_find");
});

test("discover result lists both protocol eras", () => {
  const discovered = buildDiscoverResult({
    implementation: IMPLEMENTATION_STDIO,
    capabilities: { tools: {} },
    instructions: INSTRUCTIONS_STDIO,
  });
  assert.equal(discovered.resultType, "complete");
  assert.deepEqual(discovered.supportedVersions, SUPPORTED_PROTOCOL_VERSIONS);
  assert.equal(discovered.cacheScope, "public");
  assert.equal(typeof discovered.ttlMs, "number");
  assert.ok(discovered.capabilities.tools);
});

test("decorateCacheableResult fills ttlMs and resultType", () => {
  const decorated = decorateCacheableResult({ tools: [{ name: "a" }] });
  assert.equal(decorated.resultType, "complete");
  assert.equal(decorated.ttlMs, 300_000);
  assert.equal(decorated.cacheScope, "public");
});

test("stdio server source is wired to the dual-era contract", () => {
  const source = readFileSync(join(here, "index.js"), "utf8");
  assert.match(source, /from ["']\.\/mcp-spec-2025-11-25\.js["']/);
  assert.match(source, /IMPLEMENTATION_STDIO/);
  assert.match(source, /stdioServerOptions/);
  assert.match(source, /registerLeioTool/);
  assert.match(source, /installModernProtocol/);
  assert.doesNotMatch(source, /new McpServer\(\s*\{\s*name:\s*["']leio-code["'],\s*version:\s*["']1\.0\.0["']/);
});

test("stdio initialize still works for legacy clients and discover is registered", async (t) => {
  const { Client } = await import("@modelcontextprotocol/sdk/client/index.js");
  const { StdioClientTransport } = await import(
    "@modelcontextprotocol/sdk/client/stdio.js"
  );
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [join(here, "index.js")],
    env: {
      ...process.env,
      LEIO_CODE_REPO_ROOT: join(here, ".."),
    },
    stderr: "pipe",
  });
  const client = new Client({
    name: "leio-code-stdio-contract",
    version: PACKAGE_VERSION,
  });
  await client.connect(transport);
  t.after(async () => {
    await client.close().catch(() => {});
  });

  assert.equal(client.getServerCapabilities()?.tools !== undefined, true);
  const info = client.getServerVersion();
  assert.equal(info.name, "leio-code");
  assert.equal(info.version, PACKAGE_VERSION);
  assert.equal(info.title, "LEIO Code");

  const listed = await client.listTools();
  const names = listed.tools.map((tool) => tool.name).sort();
  assert.deepEqual(names, Object.keys(STDIO_TOOL_CATALOG).sort());
  const find = listed.tools.find((tool) => tool.name === "leio_code_find");
  assert.equal(find.title, "Find repository entity");
  assert.equal(find.annotations?.readOnlyHint, false);
  assert.equal(find.annotations?.destructiveHint, false);
  assert.equal(find.outputSchema?.type, "object");
  assert.equal(find.execution?.taskSupport ?? "forbidden", "forbidden");

  const unknown = await client.callTool({
    name: "not_a_real_tool",
    arguments: {},
  });
  assert.equal(unknown.isError, true);
  assert.match(unknown.content[0].text, /-32602|not found/i);

  const missingNeedle = await client.callTool({
    name: "leio_code_graph",
    arguments: { kind: "callers-of" },
  });
  assert.equal(missingNeedle.isError, true);
  assert.match(missingNeedle.content[0].text, /needle/i);
  assert.equal(missingNeedle.structuredContent?.ok, false);
  assert.equal(missingNeedle.resultType ?? "complete", "complete");

  const { z } = await import("zod");
  const discovered = await client.request(
    { method: "server/discover" },
    z
      .object({
        resultType: z.string(),
        supportedVersions: z.array(z.string()),
        capabilities: z.record(z.unknown()),
        ttlMs: z.number(),
        cacheScope: z.string(),
      })
      .passthrough(),
  );
  assert.deepEqual(discovered.supportedVersions, SUPPORTED_PROTOCOL_VERSIONS);
  assert.equal(discovered.resultType, "complete");
});

test("server/discover answers before initialize (2026-07-28 probe)", async () => {
  const { spawn } = await import("node:child_process");
  const child = spawn(process.execPath, [join(here, "index.js")], {
    env: { ...process.env, LEIO_CODE_REPO_ROOT: join(here, "..") },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const request = `${JSON.stringify({
    jsonrpc: "2.0",
    id: 1,
    method: "server/discover",
    params: {
      _meta: {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {
          name: "leio-discover-probe",
          version: "0.0.0",
        },
        "io.modelcontextprotocol/clientCapabilities": {},
      },
    },
  })}\n`;
  const line = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.kill("SIGTERM");
      reject(new Error("timed out waiting for server/discover"));
    }, 8000);
    let buf = "";
    child.stdout.on("data", (chunk) => {
      buf += chunk.toString("utf8");
      const idx = buf.indexOf("\n");
      if (idx >= 0) {
        clearTimeout(timer);
        resolve(buf.slice(0, idx));
      }
    });
    child.on("error", reject);
    child.stdin.write(request);
  });
  child.kill("SIGTERM");
  const message = JSON.parse(line);
  assert.equal(message.id, 1);
  assert.ok(message.result, JSON.stringify(message));
  assert.equal(message.result.resultType, "complete");
  assert.deepEqual(
    message.result.supportedVersions,
    SUPPORTED_PROTOCOL_VERSIONS,
  );
  assert.ok(message.result.capabilities.tools);
});
