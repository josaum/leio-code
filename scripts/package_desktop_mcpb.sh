#!/usr/bin/env bash
# Pack a Claude Desktop MCP Bundle (.mcpb). Does not include the Rust crate.
set -euo pipefail
ROOT="$(cd -- "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(python3 -c "import json; print(json.load(open('${ROOT}/manifest.json'))['version'])")"
OUT="${1:-${ROOT}/releases/leio-code-${VERSION}.mcpb}"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/leio-mcpb.XXXXXX")"
cleanup() { rm -rf "${STAGE}"; }
trap cleanup EXIT

mkdir -p "${STAGE}/mcp" "$(dirname "${OUT}")"
cp "${ROOT}/LICENSE" "${ROOT}/LICENSE-MIT" "${ROOT}/LICENSE-APACHE" "${ROOT}/THIRD_PARTY.md" "${STAGE}/"
cp "${ROOT}/manifest.json" "${STAGE}/manifest.json"
cp "${ROOT}/assets/icon.png" "${STAGE}/icon.png"
cp "${ROOT}/mcp/package.json" "${ROOT}/mcp/package-lock.json" "${STAGE}/mcp/"
find "${ROOT}/mcp" -maxdepth 1 -name '*.js' ! -name '*.test.js' -exec cp {} "${STAGE}/mcp/" \;
# Install a fresh flat node_modules in the stage — never copy the repo's
# pnpm-layout tree: pnpm's relative symlinks do not survive the .mcpb zip
# round-trip, and a half-dangling @modelcontextprotocol/sdk breaks the
# packaged server at import time (ERR_MODULE_NOT_FOUND).
npm ci --prefix "${STAGE}/mcp" --omit=dev --ignore-scripts >/dev/null

npx --yes @anthropic-ai/mcpb validate "${STAGE}/manifest.json"
npx --yes @anthropic-ai/mcpb pack "${STAGE}" "${OUT}"
ls -lh "${OUT}"
echo "${OUT}"
