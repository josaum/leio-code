import { existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { applyRegistration } from "../register-mcp.js";

const entry = fileURLToPath(import.meta.url);
const root = path.resolve(path.dirname(entry), "..");
const git = path.join(root, "runtime", "git", "cmd");
const code = path.join(root, "bin", "leio-code.exe");
const harness = path.join(root, "bin", "leio-harness.exe");

if (![code, harness, path.join(git, "git.exe")].every((file) => existsSync(file))) {
  throw new Error("Extract the entire LEIO Code Windows portable ZIP before launching it.");
}

const spec = { command: process.execPath, args: [entry] };
if (process.argv.includes("--registration")) {
  console.log(JSON.stringify({ mcpServers: { "leio-code": spec } }, null, 2));
} else if (process.argv.includes("--register")) {
  const files = applyRegistration(process.env.LEIO_MCP_HOME || os.homedir(), spec);
  console.log(JSON.stringify({ registered: files }, null, 2));
} else {
  process.env.LEIO_PLUGIN_ROOT = root;
  process.env.LEIO_CODE_BIN = code;
  process.env.LEIO_HARNESS_BIN = harness;
  process.env.PATH = [git, process.env.PATH].filter(Boolean).join(path.delimiter);
  await import("./index.js");
}
