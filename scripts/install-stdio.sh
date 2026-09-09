#!/usr/bin/env bash
# Install the local stdio surface from an inspected checkout. No host config edits.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="${HOME}/.local/share/leio-code"
case "${1:-}" in
  --prefix) PREFIX="${2:?--prefix requires an absolute directory}"; shift 2 ;;
  --help) echo "Usage: bash scripts/install-stdio.sh [--prefix /absolute/directory]"; exit 0 ;;
esac
[[ $# -eq 0 && "${PREFIX}" = /* ]] || { echo "Expected an absolute --prefix and no other arguments" >&2; exit 2; }
for prerequisite in cargo node npm; do
  command -v "${prerequisite}" >/dev/null || { echo "Missing prerequisite: ${prerequisite}" >&2; exit 1; }
done
node -e 'if (Number(process.versions.node.split(".")[0]) < 22) { console.error("Node.js 22 or newer required"); process.exit(1); }'

# Use the checkout's own target directory, independent of a host override.
cd "${ROOT}"
CARGO_TARGET_DIR="${ROOT}/target" cargo build --locked --release -p leio-code >&2
npm ci --prefix "${ROOT}/mcp" --omit=dev --ignore-scripts >&2
mkdir -p "${PREFIX}/bin"
install -m 0755 "${ROOT}/target/release/leio-code" "${PREFIX}/bin/leio-code"

# Exercise the actual wrapper and binary before suggesting host registration.
LEIO_CODE_BIN="${PREFIX}/bin/leio-code" node "${ROOT}/scripts/verify-stdio.mjs" >&2
node --input-type=module - "${ROOT}" "${PREFIX}" <<'JS'
import path from 'node:path';
const [root, prefix] = process.argv.slice(2);
console.log(JSON.stringify({
  mcpServers: {
    'leio-code': {
      command: process.execPath,
      args: [path.join(root, 'mcp/index.js')],
      env: { LEIO_CODE_BIN: path.join(prefix, 'bin/leio-code') }
    }
  }
}, null, 2));
JS
echo "Keep this checkout: the MCP entrypoint and installed Node dependencies live here." >&2
echo "Register only leio-code stdio in your host; reconnect its MCP session after registration." >&2
