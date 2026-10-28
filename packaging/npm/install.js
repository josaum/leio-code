import { chmodSync, mkdirSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { applyRegistration } from "../register-mcp.js";

const root = path.dirname(fileURLToPath(import.meta.url));
const version = "2.6.7";
const tag = `leio-code-plugin-v${version}`;
const repo = "josaum/leio-code";

const assets = {
  "darwin-arm64": ["leio-code-darwin-arm64", "leio-code-darwin-arm64-harness"],
  "darwin-x64": ["leio-code-darwin-amd64", "leio-code-darwin-amd64-harness"],
  "linux-x64": ["leio-code-linux-amd64", "leio-code-linux-amd64-harness"],
  "win32-x64": ["leio-code-windows-amd64.exe", "leio-code-windows-amd64-harness.exe"],
};

const key = `${process.platform}-${process.arch}`;
const pair = assets[key];
if (!pair) {
  console.error(`leio-code has no prebuilt binary for ${key}`);
  process.exit(1);
}

const vendor = path.join(root, "vendor");
mkdirSync(vendor, { recursive: true });
const ext = process.platform === "win32" ? ".exe" : "";

for (const [asset, name] of [
  [pair[0], `leio-code${ext}`],
  [pair[1], `leio-harness${ext}`],
]) {
  const url = `https://github.com/${repo}/releases/download/${tag}/${asset}`;
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) {
    console.error(`leio-code download failed for ${asset}: HTTP ${response.status}`);
    process.exit(1);
  }
  const dest = path.join(vendor, name);
  writeFileSync(dest, Buffer.from(await response.arrayBuffer()));
  if (process.platform !== "win32") {
    chmodSync(dest, 0o755);
  }
}

if (process.env.LEIO_MCP_SKIP_REGISTER !== "1") {
  const launcher = path.join(root, "bin", "leio-mcp.js");
  applyRegistration(process.env.LEIO_MCP_HOME || os.homedir(), {
    command: process.execPath,
    args: [launcher],
  });
}
