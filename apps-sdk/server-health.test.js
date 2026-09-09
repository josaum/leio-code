import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import net from "node:net";
import { readFileSync } from "node:fs";
import path, { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

function getOpenPort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => {
        resolve(address.port);
      });
    });
  });
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function fetchHealth(baseUrl, deadlineMs, logs) {
  let lastError = null;
  while (Date.now() < deadlineMs) {
    try {
      const response = await fetch(`${baseUrl}/health`);
      const body = await response.text();
      if (response.ok) {
        return JSON.parse(body);
      }
      lastError = new Error(`health returned ${response.status}: ${body}`);
    } catch (error) {
      lastError = error;
    }
    await delay(150);
  }

  throw new Error(
    `server did not return healthy response: ${lastError?.message ?? "unknown"}\n${logs()}`,
  );
}

test("server health exposes a disabled specialist bridge when VIGOROS is not configured", async (t) => {
  const port = await getOpenPort();
  const logs = {
    stdout: "",
    stderr: "",
  };
  const child = spawn(process.execPath, ["server.js"], {
    cwd: __dirname,
    env: {
      ...process.env,
      LEIO_APPS_SDK_HOST: "127.0.0.1",
      LEIO_APPS_SDK_PORT: String(port),
      LEIO_APPS_SDK_AUTH_MODE: "none",
      LEIO_VIGOROS_MCP_URL: "",
      LEIO_VIGOROS_TOKEN_URL: "",
      LEIO_VIGOROS_CLIENT_ID: "",
      LEIO_VIGOROS_CLIENT_SECRET: "",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    logs.stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    logs.stderr += chunk;
  });

  t.after(() => {
    if (!child.killed) {
      child.kill("SIGTERM");
    }
  });

  const health = await fetchHealth(
    `http://127.0.0.1:${port}`,
    Date.now() + 10_000,
    () => `${logs.stdout}\n${logs.stderr}`,
  );

  assert.equal(health.ok, true);
  assert.equal(health.protocolVersion, "2026-07-28");
  assert.deepEqual(health.supportedProtocolVersions, [
    "2026-07-28",
    "2025-11-25",
  ]);
  const { version: pkgVersion } = JSON.parse(
    readFileSync(resolve(__dirname, "..", "mcp", "package.json"), "utf8"),
  );
  assert.equal(health.version, pkgVersion);
  assert.equal(health.auth.mode, "none");
  assert.equal(health.specialist_bridge.enabled, false);
  assert.equal(health.specialist_bridge.configured, false);
  assert.deepEqual(health.specialist_bridge.depths, ["quick", "deep"]);
  assert.match(
    health.specialist_bridge.missing.join(","),
    /LEIO_VIGOROS_MCP_URL/,
  );
  assert.equal(health.legal.configured, false);
  assert.ok(Array.isArray(health.legal.missing));
  assert.ok(health.legal.missing.includes("publisher name"));
  assert.ok(health.legal.privacy_policy_url);
  assert.ok(health.legal.support_url);
  assert.ok(health.legal.terms_url);
});

test("server health reports legal.configured when publisher env is complete", async (t) => {
  const port = await getOpenPort();
  const logs = {
    stdout: "",
    stderr: "",
  };
  const child = spawn(process.execPath, ["server.js"], {
    cwd: __dirname,
    env: {
      ...process.env,
      LEIO_APPS_SDK_HOST: "127.0.0.1",
      LEIO_APPS_SDK_PORT: String(port),
      LEIO_APPS_SDK_AUTH_MODE: "none",
      LEIO_APPS_SDK_PUBLIC_URL: `http://127.0.0.1:${port}`,
      LEIO_APPS_SDK_PUBLISHER_NAME: "JAI",
      LEIO_APPS_SDK_COMPANY_NAME: "JAI",
      LEIO_APPS_SDK_COMPANY_URL: "https://getjai.com",
      LEIO_APPS_SDK_SUPPORT_EMAIL: "contato@getjai.com",
      LEIO_APPS_SDK_PRIVACY_EMAIL: "contato@getjai.com",
      LEIO_APPS_SDK_SECURITY_EMAIL: "contato@getjai.com",
      LEIO_APPS_SDK_SUPPORT_HOURS: "Business days, 09:00–18:00 America/Sao_Paulo",
      LEIO_APPS_SDK_LEGAL_LAST_UPDATED: "2026-07-09",
      LEIO_VIGOROS_MCP_URL: "",
      LEIO_VIGOROS_TOKEN_URL: "",
      LEIO_VIGOROS_CLIENT_ID: "",
      LEIO_VIGOROS_CLIENT_SECRET: "",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    logs.stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    logs.stderr += chunk;
  });

  t.after(() => {
    if (!child.killed) {
      child.kill("SIGTERM");
    }
  });

  const health = await fetchHealth(
    `http://127.0.0.1:${port}`,
    Date.now() + 10_000,
    () => `${logs.stdout}\n${logs.stderr}`,
  );

  assert.equal(health.ok, true);
  assert.equal(health.legal.configured, true);
  assert.deepEqual(health.legal.missing, []);
  assert.equal(
    health.legal.privacy_policy_url,
    `http://127.0.0.1:${port}/privacy`,
  );
});

test("MCP tool list exposes graph_repository and hides Carlos when VIGOROS unset", async (t) => {
  const { Client } = await import("@modelcontextprotocol/sdk/client/index.js");
  const { StreamableHTTPClientTransport } = await import(
    "@modelcontextprotocol/sdk/client/streamableHttp.js"
  );

  const port = await getOpenPort();
  const logs = { stdout: "", stderr: "" };
  const child = spawn(process.execPath, ["server.js"], {
    cwd: __dirname,
    env: {
      ...process.env,
      LEIO_APPS_SDK_HOST: "127.0.0.1",
      LEIO_APPS_SDK_PORT: String(port),
      LEIO_APPS_SDK_AUTH_MODE: "none",
      LEIO_CODE_BIN: "/usr/bin/false",
      LEIO_VIGOROS_MCP_URL: "",
      LEIO_VIGOROS_TOKEN_URL: "",
      LEIO_VIGOROS_CLIENT_ID: "",
      LEIO_VIGOROS_CLIENT_SECRET: "",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    logs.stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    logs.stderr += chunk;
  });
  t.after(() => {
    if (!child.killed) {
      child.kill("SIGTERM");
    }
  });

  await fetchHealth(
    `http://127.0.0.1:${port}`,
    Date.now() + 10_000,
    () => `${logs.stdout}\n${logs.stderr}`,
  );

  const transport = new StreamableHTTPClientTransport(
    new URL(`http://127.0.0.1:${port}/mcp`),
  );
  const client = new Client({
    name: "leio-code-apps-sdk-tool-surface",
    version: "0.1.0",
  });
  await client.connect(transport);
  t.after(async () => {
    await client.close().catch(() => {});
  });

  const listed = await client.listTools();
  const names = listed.tools.map((tool) => tool.name);
  assert.ok(names.includes("graph_repository"));
  assert.ok(names.includes("guide_repository_tools"));
  assert.ok(names.includes("search_repository_memory"));
  assert.ok(names.includes("audit_repository_rollup"));
  assert.ok(names.includes("audit_repository_contracts"));
  assert.ok(!names.includes("consult_carlos_motta_specialist"));

  const graphTool = listed.tools.find((tool) => tool.name === "graph_repository");
  assert.match(graphTool.description, /leio-code graph/i);
  assert.notEqual(graphTool.name, "search_repository");

  const memoryTool = listed.tools.find(
    (tool) => tool.name === "search_repository_memory",
  );
  assert.match(memoryTool.description, /find symbol/i);
  assert.equal(memoryTool.annotations?.readOnlyHint, false);
  assert.equal(memoryTool.annotations?.idempotentHint, true);
  assert.equal(memoryTool.annotations?.openWorldHint, false);
  assert.equal(memoryTool.annotations?.destructiveHint, false);
  assert.ok(memoryTool.outputSchema);

  const anonymousMemoryCall = await client.callTool({
    name: "search_repository_memory",
    arguments: { query: "payment validation" },
  });
  assert.equal(anonymousMemoryCall.isError, true);
  assert.equal(anonymousMemoryCall.structuredContent?.auth_required, true);

  const guide = await client.callTool({
    name: "guide_repository_tools",
    arguments: {},
  });
  assert.notEqual(guide.isError, true);
  assert.equal(guide.structuredContent?.repo_root, undefined);
  for (const [topic, first] of [["graph", "graph_repository"], ["navigation", "graph_repository"], ["doctor", "audit_repository_contracts"], ["lookup", "search_repository"]]) {
    const result = await client.callTool({ name: "guide_repository_tools", arguments: { topic } });
    assert.equal(result.structuredContent.next_tools[0], first, topic);
    for (const tool of result.structuredContent.next_tools) assert.ok(names.includes(tool), tool);
  }
  for (const topic of ["knowledge", "conversation"]) {
    const result = await client.callTool({ name: "guide_repository_tools", arguments: { topic } });
    assert.deepEqual(result.structuredContent.next_tools, [], topic);
  }
  const exported = await client.callTool({ name: "guide_repository_tools", arguments: { topic: "export" } });
  assert.ok(!exported.structuredContent.next_tools.includes("inspect_repository_status"));
  for (const tool of exported.structuredContent.next_tools) assert.ok(names.includes(tool), tool);
});

test("OPTIONS /mcp returns ACAO for chatgpt.com", async (t) => {
  const port = await getOpenPort();
  const logs = { stdout: "", stderr: "" };
  const child = spawn(process.execPath, ["server.js"], {
    cwd: __dirname,
    env: {
      ...process.env,
      LEIO_APPS_SDK_HOST: "127.0.0.1",
      LEIO_APPS_SDK_PORT: String(port),
      LEIO_APPS_SDK_AUTH_MODE: "none",
      LEIO_VIGOROS_MCP_URL: "",
      LEIO_VIGOROS_TOKEN_URL: "",
      LEIO_VIGOROS_CLIENT_ID: "",
      LEIO_VIGOROS_CLIENT_SECRET: "",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    logs.stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    logs.stderr += chunk;
  });
  t.after(() => {
    if (!child.killed) {
      child.kill("SIGTERM");
    }
  });

  await fetchHealth(
    `http://127.0.0.1:${port}`,
    Date.now() + 10_000,
    () => `${logs.stdout}\n${logs.stderr}`,
  );

  const response = await fetch(`http://127.0.0.1:${port}/mcp`, {
    method: "OPTIONS",
    headers: {
      Origin: "https://chatgpt.com",
      "Access-Control-Request-Method": "POST",
      "Access-Control-Request-Headers": "content-type,authorization,mcp-session-id",
    },
  });

  assert.equal(response.status, 204);
  assert.equal(response.headers.get("access-control-allow-origin"), "https://chatgpt.com");
  assert.match(
    response.headers.get("access-control-allow-methods") ?? "",
    /POST/i,
  );
  assert.match(
    response.headers.get("access-control-allow-headers") ?? "",
    /mcp-session-id/i,
  );

  const denied = await fetch(`http://127.0.0.1:${port}/mcp`, {
    method: "OPTIONS",
    headers: {
      Origin: "https://evil.example",
      "Access-Control-Request-Method": "POST",
    },
  });
  assert.equal(denied.status, 204);
  assert.equal(denied.headers.get("access-control-allow-origin"), null);
});

test("loopback health allows repo_root and ignores an unmarked LEIO_CODE_REPO_ROOT", async (t) => {
  const port = await getOpenPort();
  const logs = { stdout: "", stderr: "" };
  const child = spawn(process.execPath, [path.join(__dirname, "server.js")], {
    cwd: "/",
    env: {
      ...process.env,
      LEIO_APPS_SDK_HOST: "127.0.0.1",
      LEIO_APPS_SDK_PORT: String(port),
      LEIO_APPS_SDK_AUTH_MODE: "none",
      LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT: "",
      LEIO_CODE_REPO_ROOT: "/tmp",
      LEIO_VIGOROS_MCP_URL: "",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    logs.stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    logs.stderr += chunk;
  });
  t.after(() => {
    if (!child.killed) {
      child.kill("SIGTERM");
    }
  });

  const health = await fetchHealth(
    `http://127.0.0.1:${port}`,
    Date.now() + 10_000,
    () => `${logs.stdout}\n${logs.stderr}`,
  );

  assert.equal(health.source_control.allow_server_repo_root, true);
  assert.equal(health.source_control.supports_server_repo_root, true);
  assert.equal(
    health.source_control.default_repo_root,
    path.resolve(__dirname, ".."),
  );
});

test("non-loopback bind keeps repo_root disabled by default", async (t) => {
  const port = await getOpenPort();
  const logs = { stdout: "", stderr: "" };
  const child = spawn(process.execPath, ["server.js"], {
    cwd: __dirname,
    env: {
      ...process.env,
      LEIO_APPS_SDK_HOST: "0.0.0.0",
      LEIO_APPS_SDK_PORT: String(port),
      LEIO_APPS_SDK_AUTH_MODE: "none",
      LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT: "",
      LEIO_APPS_SDK_ALLOWED_HOSTS: "127.0.0.1",
      LEIO_VIGOROS_MCP_URL: "",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    logs.stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    logs.stderr += chunk;
  });
  t.after(() => {
    if (!child.killed) {
      child.kill("SIGTERM");
    }
  });

  const health = await fetchHealth(
    `http://127.0.0.1:${port}`,
    Date.now() + 10_000,
    () => `${logs.stdout}\n${logs.stderr}`,
  );

  assert.equal(health.source_control.allow_server_repo_root, false);
  assert.equal(health.source_control.supports_server_repo_root, false);
  assert.equal(health.source_control.default_repo_root, undefined);
});
