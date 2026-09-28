#!/usr/bin/env node
import { existsSync } from "node:fs";
import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "..");

function firstExisting(candidates) {
  return candidates.find((candidate) => existsSync(candidate));
}

const server = firstExisting([
  path.join(root, "server", "index.js"),
  path.join(root, "mcp", "index.js"),
  path.join(here, "index.js"),
]);
const codeBin = firstExisting([
  process.env.LEIO_CODE_BIN,
  path.join(root, "vendor", "leio-code"),
  path.join(root, "bin", "leio-code"),
].filter(Boolean));
const harnessBin = firstExisting([
  process.env.LEIO_HARNESS_BIN,
  path.join(root, "vendor", "leio-harness"),
  path.join(root, "bin", "leio-harness"),
].filter(Boolean));

if (!server || !codeBin) {
  console.error("leio-mcp is missing the server or the leio-code binary");
  process.exit(1);
}

const child = spawn(process.execPath, [server], {
  stdio: "inherit",
  env: {
    ...process.env,
    LEIO_CODE_BIN: codeBin,
    LEIO_HARNESS_BIN: harnessBin || "",
  },
});
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 1);
});
