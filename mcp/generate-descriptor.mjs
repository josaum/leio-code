#!/usr/bin/env node
// Generate the LEIO Code MCP service descriptor from the live tools/list.
//
// Single source of truth: the descriptor is captured from a running server,
// never hand-edited. `mcp/service-descriptor.test.js` regenerates it and
// fails on drift, so a tool/schema change that forgets the artifact is
// caught in `make verify`.
//
// Shape: JSON Schema service descriptor — top level describes the service;
// `methods` maps every tool to a method definition (`type: "method"`,
// `params`, `returns`) per the JSON Schema Service Descriptor proposal,
// while `inputSchema`/`outputSchema` carry the full MCP contracts verbatim
// for JSON-Schema-aware consumers.
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const server = path.join(here, "index.js");
const repoRoot = path.resolve(here, "..");
const outPath = path.join(repoRoot, "schema", "leio-code-mcp-service-descriptor.json");

const child = spawn(process.execPath, [server], {
  stdio: ["pipe", "pipe", "inherit"],
  env: { ...process.env },
});

let buffer = "";
const pending = new Map();
let nextId = 1;

child.stdout.on("data", (chunk) => {
  buffer += chunk.toString();
  let index;
  while ((index = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, index).trim();
    buffer = buffer.slice(index + 1);
    if (!line) continue;
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      continue;
    }
    if (message.id !== undefined && pending.has(message.id)) {
      pending.get(message.id)(message);
      pending.delete(message.id);
    }
  }
});

function request(method, params) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, resolve);
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    setTimeout(() => {
      if (pending.has(id)) {
        pending.delete(id);
        reject(new Error(`${method} timed out`));
      }
    }, 30_000);
  });
}

const initialize = await request("initialize", {
  protocolVersion: "2025-11-25",
  capabilities: {},
  clientInfo: { name: "service-descriptor-generator", version: "1.0.0" },
});
child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");

const listing = await request("tools/list", {});
const tools = listing.result?.tools ?? [];
if (tools.length === 0) {
  throw new Error("tools/list returned no tools; refusing to write an empty descriptor");
}

const methods = {};
for (const tool of tools) {
  const params = Object.entries(tool.inputSchema?.properties ?? {}).map(([name, schema]) => ({
    name,
    ...schema,
    required: Boolean(tool.inputSchema?.required?.includes(name)),
  }));
  methods[tool.name] = {
    type: "method",
    description: tool.description,
    params,
    returns: { type: "object", description: "CallToolResult: text content plus structuredContent" },
    inputSchema: tool.inputSchema,
    ...(tool.outputSchema ? { outputSchema: tool.outputSchema } : {}),
    ...(tool.title ? { title: tool.title } : {}),
    ...(tool.annotations ? { annotations: tool.annotations } : {}),
  };
}

const descriptor = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  id: "https://raw.githubusercontent.com/josaum/leio-code/main/schema/leio-code-mcp-service-descriptor.json",
  title: "LEIO Code MCP Service Descriptor",
  description: `Service descriptor for the LEIO Code stdio MCP server (${tools.length} tools). Generated from tools/list — do not edit by hand; regenerate with \`node mcp/generate-descriptor.mjs\`.`,
  type: "object",
  protocolVersion: initialize.result?.protocolVersion,
  serverInfo: initialize.result?.serverInfo,
  capabilities: initialize.result?.capabilities,
  instructions: initialize.result?.instructions,
  tools: tools.map((tool) => ({
    name: tool.name,
    title: tool.title,
    description: tool.description,
    inputSchema: tool.inputSchema,
    ...(tool.outputSchema ? { outputSchema: tool.outputSchema } : {}),
  })),
  methods,
};

mkdirSync(path.dirname(outPath), { recursive: true });
writeFileSync(outPath, JSON.stringify(descriptor, null, 2) + "\n");
child.kill("SIGKILL");
console.log(`service descriptor: ${outPath} (${tools.length} tools, ${Object.keys(methods).length} methods)`);
process.exit(0);
