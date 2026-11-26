import { chmodSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { applyRegistration } from "../register-mcp.js";

const root = path.dirname(fileURLToPath(import.meta.url));
const version = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8")).version;
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

async function readAsset(filename) {
  const local = process.env.LEIO_CODE_RELEASE_DIR;
  if (local) return readFileSync(path.join(local, filename));
  const url = `https://github.com/${repo}/releases/download/${tag}/${filename}`;
  const response = await fetch(url, { signal: AbortSignal.timeout(120_000) });
  if (!response.ok) throw new Error(`leio-code download failed for ${filename}: HTTP ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

for (const [asset, name] of [
  [pair[0], `leio-code${ext}`],
  [pair[1], `leio-harness${ext}`],
]) {
  const data = await readAsset(asset);
  const expected = (await readAsset(`${asset}.sha256`)).toString("ascii").trim().split(/\s+/, 1)[0];
  if (createHash("sha256").update(data).digest("hex") !== expected) {
    throw new Error(`leio-code downloaded asset failed SHA-256 verification: ${asset}`);
  }
  const dest = path.join(vendor, name);
  writeFileSync(dest, data);
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
