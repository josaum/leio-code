#!/usr/bin/env node
/**
 * Download the prebuilt `leio-code` binary for this platform from GitHub
 * Releases so the MCP package works without a Rust toolchain.
 *
 * The binary lands in `mcp/vendor/`, which `resolve-binary.js` prefers over
 * `cargo run`, so a plain `npm install` no longer requires compiling leio-code.
 *
 * Fails soft: an offline machine, an unsupported platform, or a release that
 * has not been cut yet all exit 0 with a hint, and the server falls back to
 * `LEIO_CODE_BIN` / `cargo` / `target/`.
 */
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const REPO = "josaum/leio-code";

// Release tag convention from .github/workflows/binary-release.yml.
const RELEASE_PREFIX = "leio-code-plugin-v";

// Asset names from the release matrix. Only the platforms the release workflow
// actually builds are listed; anything else degrades to the `LEIO_CODE_BIN` /
// `cargo` hint instead of guessing a URL that would 404.
const ASSETS = {
  "linux-x64": "leio-code-linux-amd64",
  "darwin-arm64": "leio-code-darwin-arm64",
  "darwin-x64": "leio-code-darwin-amd64",
};

function hint(message) {
  console.warn(`[leio-code-mcp] ${message}`);
}

function packageVersion() {
  const pkg = JSON.parse(
    readFileSync(new URL("./package.json", import.meta.url), "utf8"),
  );
  return pkg.version;
}

function platformKey() {
  return `${process.platform}-${process.arch}`;
}

async function main() {
  if (process.env.LEIO_CODE_SKIP_BINARY_DOWNLOAD === "1") {
    hint("skipping binary download (LEIO_CODE_SKIP_BINARY_DOWNLOAD=1)");
    return 0;
  }

  const key = platformKey();
  const asset = ASSETS[key];
  if (!asset) {
    hint(`no prebuilt binary for ${key}; set LEIO_CODE_BIN or install via cargo`);
    return 0;
  }

  const tag = `${RELEASE_PREFIX}${packageVersion()}`;
  const binaryName = process.platform === "win32" ? "leio-code.exe" : "leio-code";
  const vendorDir = path.join(__dirname, "vendor");
  const dest = path.join(vendorDir, binaryName);
  const url = `https://github.com/${REPO}/releases/download/${tag}/${asset}`;

  let response;
  try {
    response = await fetch(url, { redirect: "follow" });
  } catch (error) {
    hint(`binary download failed (${error.message}); set LEIO_CODE_BIN or build with cargo`);
    return 0;
  }
  if (!response.ok) {
    hint(`no release asset ${asset} for ${tag} (HTTP ${response.status}); set LEIO_CODE_BIN or build with cargo`);
    return 0;
  }

  const buffer = Buffer.from(await response.arrayBuffer());
  mkdirSync(vendorDir, { recursive: true });
  writeFileSync(dest, buffer);
  if (process.platform !== "win32") {
    chmodSync(dest, 0o755);
  }
  hint(`installed ${asset} -> ${dest}`);
  return 0;
}

main().then(
  (code) => process.exit(code),
  (error) => {
    hint(`binary install failed: ${error.message}`);
    process.exit(0);
  },
);
