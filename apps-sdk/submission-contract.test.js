import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import net from "node:net";
import { fileURLToPath } from "node:url";

import { appToolNameMap } from "./tool-name-map.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

function getOpenPort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => resolve(address.port));
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

const submission = JSON.parse(
  fs.readFileSync(path.join(__dirname, "chatgpt-app-submission.json"), "utf8"),
);

test("submission contract tools match live MCP surface (VIGOROS unset)", async (t) => {
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
    name: "leio-code-submission-contract",
    version: "0.1.0",
  });
  await client.connect(transport);
  t.after(async () => {
    await client.close().catch(() => {});
  });

  const listed = await client.listTools();
  const liveNames = listed.tools.map((tool) => tool.name).sort();
  const contractNames = Object.keys(submission.tools).sort();

  assert.deepEqual(
    liveNames,
    contractNames,
    `tools/list drift: live=${liveNames.join(",")} contract=${contractNames.join(",")}`,
  );

  for (const tool of listed.tools) {
    const contract = submission.tools[tool.name];
    assert.ok(contract, `missing submission entry for ${tool.name}`);
    const live = tool.annotations ?? {};
    assert.equal(
      live.readOnlyHint,
      contract.annotations.readOnlyHint,
      `${tool.name} readOnlyHint`,
    );
    assert.equal(
      live.destructiveHint,
      contract.annotations.destructiveHint,
      `${tool.name} destructiveHint`,
    );
    assert.equal(
      live.openWorldHint,
      contract.annotations.openWorldHint,
      `${tool.name} openWorldHint`,
    );
    assert.ok(
      tool.outputSchema,
      `${tool.name} should expose outputSchema for Scan Tools`,
    );
  }

  // Feature parity: every remapped hosted tool must keep the stdio tool's
  // parameters (including enum values), so a capability cannot quietly
  // disappear when a transport is remapped. Only two drops are allowed, and
  // both are owned by the hosting server rather than the caller.
  const stdioDescriptor = JSON.parse(
    fs.readFileSync(
      path.join(__dirname, "..", "schema", "leio-code-mcp-service-descriptor.json"),
      "utf8",
    ),
  );
  const hostedOnlyParams = new Set(["repo_url", "git_ref"]);
  const droppedStdioParams = new Map([
    ["index_path", "the hosted server owns the checkout and index"],
    ["timeout_ms", "the hosted server owns its execution budget"],
  ]);
  const stdioByHostedName = new Map(
    Object.entries(appToolNameMap).map(([stdio, hosted]) => [hosted, stdio]),
  );

  for (const tool of listed.tools) {
    const stdioName = stdioByHostedName.get(tool.name);
    if (!stdioName) {
      continue; // hosted-only tool
    }
    const stdioProps = stdioDescriptor.methods[stdioName].inputSchema.properties ?? {};
    const hostedProps = tool.inputSchema?.properties ?? {};

    for (const name of Object.keys(stdioProps)) {
      if (droppedStdioParams.has(name)) {
        continue;
      }
      assert.ok(
        hostedProps[name],
        `${tool.name} drops the stdio parameter ${name} from ${stdioName}`,
      );
    }
    for (const name of Object.keys(hostedProps)) {
      assert.ok(
        stdioProps[name] || hostedOnlyParams.has(name),
        `${tool.name} adds parameter ${name} with no stdio counterpart`,
      );
    }
    for (const [name, prop] of Object.entries(stdioProps)) {
      if (!prop.enum || !hostedProps[name]?.enum) {
        continue;
      }
      assert.deepEqual(
        [...hostedProps[name].enum].sort(),
        [...prop.enum].sort(),
        `${tool.name}.${name} must expose the same values as ${stdioName}.${name}`,
      );
    }
  }
});

test("submission JSON includes exactly five test cases and negative cases", () => {
  assert.ok(Array.isArray(submission.test_cases));
  assert.equal(submission.test_cases.length, 5);
  assert.ok(Array.isArray(submission.negative_test_cases));
  assert.equal(submission.negative_test_cases.length, 3);
  assert.equal(submission.app_info.display_name, "LEIO Code");
  assert.equal(submission.app_info.category, "DEVELOPER_TOOLS");
});
