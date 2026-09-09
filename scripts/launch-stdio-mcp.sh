#!/usr/bin/env bash
# Single stdio MCP bootstrap for the Claude plugin and project .mcp.json.
set -euo pipefail

# Claude Desktop often starts MCP with a stripped PATH. Prefer the cargo-installed
# binary so a stale unpacked snapshot cannot win via walk-up `target/`.
if [[ -z "${LEIO_CODE_BIN:-}" && -x "${HOME}/.cargo/bin/leio-code" ]]; then
  export LEIO_CODE_BIN="${HOME}/.cargo/bin/leio-code"
fi
if [[ -d "${HOME}/.cargo/bin" ]]; then
  case ":${PATH}:" in
    *":${HOME}/.cargo/bin:"*) ;;
    *) export PATH="${HOME}/.cargo/bin:${PATH}" ;;
  esac
fi

if [[ -n "${CLAUDE_PLUGIN_ROOT:-}" && -f "${CLAUDE_PLUGIN_ROOT}/mcp/index.js" ]]; then
  ROOT="${CLAUDE_PLUGIN_ROOT}"
else
  ROOT="$(cd -- "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

MCP_DIR="${ROOT}/mcp"
if [[ ! -f "${MCP_DIR}/index.js" ]]; then
  echo "leio-code: mcp/index.js not found under ${ROOT}" >&2
  exit 1
fi

if [[ ! -d "${MCP_DIR}/node_modules/@modelcontextprotocol/sdk" ]] \
  || [[ ! -d "${MCP_DIR}/node_modules/zod" ]]; then
  if command -v pnpm >/dev/null 2>&1; then
    pnpm install --dir "${MCP_DIR}" --prod --ignore-scripts --ignore-workspace >/dev/null
  elif [[ -f "${MCP_DIR}/package-lock.json" ]]; then
    npm ci --prefix "${MCP_DIR}" --omit=dev --ignore-scripts >/dev/null
  else
    npm install --prefix "${MCP_DIR}" --omit=dev --ignore-scripts >/dev/null
  fi
fi

exec node "${MCP_DIR}/index.js"
