#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${CLAUDE_PLUGIN_ROOT:-}" ]]; then
  echo "CLAUDE_PLUGIN_ROOT is not set" >&2
  exit 1
fi

if [[ -z "${CLAUDE_PLUGIN_DATA:-}" ]]; then
  echo "CLAUDE_PLUGIN_DATA is not set" >&2
  exit 1
fi

PLUGIN_PARENT="$(cd "${CLAUDE_PLUGIN_ROOT}/.." && pwd)"

if [[ -d "${PLUGIN_PARENT}/mcp" && -f "${PLUGIN_PARENT}/Cargo.toml" ]]; then
  LEIO_CODE_ROOT="${PLUGIN_PARENT}"
elif [[ -d "${PLUGIN_PARENT}/leio-code/mcp" && -f "${PLUGIN_PARENT}/leio-code/Cargo.toml" ]]; then
  LEIO_CODE_ROOT="${PLUGIN_PARENT}/leio-code"
else
  echo "Unable to resolve LEIO Code root from ${CLAUDE_PLUGIN_ROOT}" >&2
  exit 1
fi

SRC_DIR="${LEIO_CODE_ROOT}/mcp"
DST_DIR="${CLAUDE_PLUGIN_DATA}/mcp"
mkdir -p "${DST_DIR}"

cp "${SRC_DIR}/index.js" "${DST_DIR}/index.js"
cp "${SRC_DIR}/envelope.js" "${DST_DIR}/envelope.js"

if [[ -f "${SRC_DIR}/package-lock.json" ]]; then
  cp "${SRC_DIR}/package-lock.json" "${DST_DIR}/package-lock.json"
fi

if ! diff -q "${SRC_DIR}/package.json" "${DST_DIR}/package.json" >/dev/null 2>&1 \
  || [[ ! -d "${DST_DIR}/node_modules/@modelcontextprotocol/sdk" ]] \
  || [[ ! -d "${DST_DIR}/node_modules/zod" ]]; then
  cp "${SRC_DIR}/package.json" "${DST_DIR}/package.json"
  rm -rf "${DST_DIR}/node_modules"
  if command -v pnpm >/dev/null 2>&1; then
    pnpm install --dir "${DST_DIR}" --prod --ignore-scripts >/dev/null
  elif [[ -f "${DST_DIR}/package-lock.json" ]]; then
    npm ci --prefix "${DST_DIR}" --omit=dev >/dev/null
  else
    npm install --prefix "${DST_DIR}" --omit=dev >/dev/null
  fi
fi
