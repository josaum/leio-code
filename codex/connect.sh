#!/usr/bin/env bash
#
# connect.sh — one-command Codex bootstrap for leio-code.
#
# Wraps `apps-sdk/scripts/start-local.sh` plus, optionally, prints the Codex
# config snippet in a form ready to append to ~/.codex/config.toml.
#
# Usage:
#   bash leio-code/codex/connect.sh             # start + print config
#   bash leio-code/codex/connect.sh --append    # also append to ~/.codex/config.toml (creates if missing)
#   bash leio-code/codex/connect.sh --stop      # stop the local server
#   bash leio-code/codex/connect.sh --status    # print current status only

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
START_LOCAL="${SCRIPT_DIR}/../apps-sdk/scripts/start-local.sh"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
CODEX_CONFIG="${HOME}/.codex/config.toml"

if [[ ! -x "${START_LOCAL}" ]]; then
  echo "missing or non-executable: ${START_LOCAL}" >&2
  exit 1
fi

case "${1:-}" in
  --stop)
    bash "${START_LOCAL}" --stop
    exit 0
    ;;
  --status)
    pidfile="${REPO_ROOT}/.leio-code/apps-sdk.pid"
    if [[ -f "${pidfile}" ]] && kill -0 "$(cat "${pidfile}")" 2>/dev/null; then
      echo "leio-code apps-sdk: RUNNING (pid $(cat "${pidfile}"))"
    else
      echo "leio-code apps-sdk: not running"
    fi
    exit 0
    ;;
  --append)
    append=1
    ;;
  "")
    append=0
    ;;
  *)
    echo "unknown flag: ${1}" >&2
    echo "usage: $0 [--append | --stop | --status]" >&2
    exit 2
    ;;
esac

# Start (or no-op if already running). Capture stdout so we can pull the
# snippet back out without re-running.
output="$(bash "${START_LOCAL}")"
echo "${output}"

# Extract the [mcp_servers.leio_code] block (between the header and EOF).
snippet="$(echo "${output}" | awk '/^\[mcp_servers\.leio_code\]/{flag=1} flag {print}')"

if [[ -z "${snippet}" ]]; then
  echo "[connect.sh] could not extract Codex snippet from start-local output" >&2
  exit 1
fi

if [[ "${append:-0}" -eq 1 ]]; then
  mkdir -p "$(dirname "${CODEX_CONFIG}")"
  if [[ -f "${CODEX_CONFIG}" ]] && grep -q "^\[mcp_servers\.leio_code\]" "${CODEX_CONFIG}"; then
    echo "[connect.sh] ${CODEX_CONFIG} already contains [mcp_servers.leio_code]; skipping append"
    echo "[connect.sh] edit it by hand to update the URL"
  else
    {
      echo
      echo "# Added by leio-code/codex/connect.sh on $(date -u +%Y-%m-%dT%H:%M:%SZ)"
      echo "${snippet}"
    } >>"${CODEX_CONFIG}"
    echo "[connect.sh] appended to ${CODEX_CONFIG}"
  fi
fi
