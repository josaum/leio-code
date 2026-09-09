#!/usr/bin/env bash
#
# start-local.sh — idempotent local launcher for the leio-code Apps SDK
# (MCP-over-HTTP server). Suitable for connecting Codex (or any other remote
# MCP client) to a warm leio-code index without paying the CLI cold-start
# cost on every query.
#
# Behavior:
#   1. Picks an open port if LEIO_APPS_SDK_PORT is not set.
#   2. Detects whether a server is already listening on /health. If yes, no-op.
#   3. Bootstraps the leio-code index if `.leio-code/index.json` is missing
#      or older than the most recent source file under `src/`.
#   4. Installs apps-sdk node_modules if missing.
#   5. Starts the server in the background, captures its PID into a pidfile,
#      and waits for /health to return 200 (timeout 30s).
#   6. Prints the MCP endpoint URL and a Codex config snippet ready to paste
#      into ~/.codex/config.toml.
#
# Idempotent — running it twice is a no-op the second time. Stop the running
# server with `kill $(cat .leio-code/apps-sdk.pid)`.
#
# Usage:
#   bash leio-code/apps-sdk/scripts/start-local.sh
#   bash leio-code/apps-sdk/scripts/start-local.sh --foreground    # don't background
#   bash leio-code/apps-sdk/scripts/start-local.sh --stop          # kill running instance

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APPS_SDK_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
LEIO_CODE_ROOT="$(cd "${APPS_SDK_DIR}/.." && pwd)"
# Index this checkout. `apps-sdk/../..` is the parent of `leio-code/` (every
# sibling repo under ~/projects) — that is not a git/Cargo root and the
# indexer dies on binary files. Override with LEIO_CODE_REPO_ROOT only when
# the intended default target really is a different tree.
REPO_ROOT="${LEIO_CODE_REPO_ROOT:-${LEIO_CODE_ROOT}}"

PID_DIR="${REPO_ROOT}/.leio-code"
PID_FILE="${PID_DIR}/apps-sdk.pid"
LOG_FILE="${PID_DIR}/apps-sdk.log"
LAUNCH_LABEL="${LEIO_APPS_SDK_LAUNCH_LABEL:-com.josaum.leio-code.apps-sdk}"

mkdir -p "${PID_DIR}"

# ---------------------------------------------------------------------------
# Flags
# ---------------------------------------------------------------------------

MODE="background"
if [[ "${1:-}" == "--foreground" ]]; then
  MODE="foreground"
elif [[ "${1:-}" == "--stop" ]]; then
  if [[ -f "${PID_FILE}" ]]; then
    pid="$(cat "${PID_FILE}")"
    if kill -0 "${pid}" 2>/dev/null; then
      kill "${pid}"
      echo "stopped leio-code apps-sdk (pid ${pid})"
    fi
    rm -f "${PID_FILE}"
  else
    echo "no pidfile at ${PID_FILE}; nothing to stop"
  fi
  if command -v launchctl >/dev/null 2>&1; then
    launchctl remove "${LAUNCH_LABEL}" >/dev/null 2>&1 || true
  fi
  exit 0
fi

LEIO_APPS_SDK_HOST="${LEIO_APPS_SDK_HOST:-127.0.0.1}"

BINARY_NAME="leio-code"
if [[ "${OS:-}" == "Windows_NT" ]]; then
  BINARY_NAME="leio-code.exe"
fi

# Same rule as mcp/resolve-binary.js isCargoTargetBinary.
is_cargo_target_bin() {
  case "$1" in
    */target/release/*|*/target/debug/*|*/target/*/release/*|*/target/*/debug/*)
      return 0
      ;;
    */release/"${BINARY_NAME}"|*/debug/"${BINARY_NAME}")
      return 0
      ;;
  esac
  return 1
}

resolve_leio_code_bin() {
  if [[ -n "${LEIO_CODE_BIN:-}" && -f "${LEIO_CODE_BIN}" && -x "${LEIO_CODE_BIN}" ]]; then
    printf '%s\n' "${LEIO_CODE_BIN}"
    return 0
  fi

  local candidate
  candidate="${CARGO_HOME:-$HOME/.cargo}/bin/${BINARY_NAME}"
  if [[ -f "${candidate}" && -x "${candidate}" ]]; then
    printf '%s\n' "${candidate}"
    return 0
  fi

  local dir
  local old_ifs="${IFS}"
  IFS=':'
  # shellcheck disable=SC2086
  for dir in ${PATH}; do
    IFS="${old_ifs}"
    candidate="${dir}/${BINARY_NAME}"
    if [[ -f "${candidate}" && -x "${candidate}" ]] && ! is_cargo_target_bin "${candidate}"; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  IFS="${old_ifs}"

  for candidate in \
    "${REPO_ROOT}/target/release/${BINARY_NAME}" \
    "${REPO_ROOT}/target/debug/${BINARY_NAME}" \
    "${LEIO_CODE_ROOT}/target/release/${BINARY_NAME}" \
    "${LEIO_CODE_ROOT}/target/debug/${BINARY_NAME}"
  do
    if [[ -n "${candidate}" && -f "${candidate}" && -x "${candidate}" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
}

LEIO_CODE_BIN_RESOLVED="$(resolve_leio_code_bin)"
if [[ -n "${LEIO_CODE_BIN_RESOLVED}" ]]; then
  export LEIO_CODE_BIN="${LEIO_CODE_BIN_RESOLVED}"
fi

# ---------------------------------------------------------------------------
# Already running? (Check pidfile FIRST so re-runs are truly idempotent.)
# ---------------------------------------------------------------------------

if [[ -f "${PID_FILE}" ]]; then
  existing_pid="$(cat "${PID_FILE}")"
  if kill -0 "${existing_pid}" 2>/dev/null; then
    # Recover the running port from `lsof` — the pidfile doesn't store it
    # because LEIO_APPS_SDK_PORT may have varied across invocations.
    existing_port="$(lsof -aPi -p "${existing_pid}" -sTCP:LISTEN 2>/dev/null \
                       | awk 'NR>1 {split($9, a, ":"); print a[length(a)]; exit}')"
    if [[ -n "${existing_port}" ]]; then
      existing_health="http://${LEIO_APPS_SDK_HOST}:${existing_port}/health"
      existing_mcp="http://${LEIO_APPS_SDK_HOST}:${existing_port}/mcp"
      if curl -fsS --max-time 1 "${existing_health}" >/dev/null 2>&1; then
        echo "leio-code apps-sdk already running (pid ${existing_pid}) on ${existing_health}"
        echo
        echo "Codex config snippet (paste into ~/.codex/config.toml):"
        cat <<TOML
[mcp_servers.leio_code]
url = "${existing_mcp}"
transport = "streamable_http"
TOML
        exit 0
      fi
    fi
  fi
  # Stale pidfile — clean up.
  rm -f "${PID_FILE}"
fi

# ---------------------------------------------------------------------------
# Port selection (only reached when no running instance is detected)
# ---------------------------------------------------------------------------

if [[ -z "${LEIO_APPS_SDK_PORT:-}" ]]; then
  # Pick a free port on 8181-8190; default 8181 if available.
  for candidate in 8181 8182 8183 8184 8185 8186 8187 8188 8189 8190; do
    if ! lsof -iTCP:"${candidate}" -sTCP:LISTEN -n -P >/dev/null 2>&1; then
      LEIO_APPS_SDK_PORT="${candidate}"
      break
    fi
  done
  if [[ -z "${LEIO_APPS_SDK_PORT:-}" ]]; then
    echo "no free port in 8181-8190; set LEIO_APPS_SDK_PORT explicitly" >&2
    exit 1
  fi
fi

HEALTH_URL="http://${LEIO_APPS_SDK_HOST}:${LEIO_APPS_SDK_PORT}/health"
MCP_URL="http://${LEIO_APPS_SDK_HOST}:${LEIO_APPS_SDK_PORT}/mcp"

# ---------------------------------------------------------------------------
# Index freshness
# ---------------------------------------------------------------------------

INDEX_FILE="${REPO_ROOT}/.leio-code/index.json"
needs_index=0
if [[ ! -f "${INDEX_FILE}" ]]; then
  needs_index=1
elif find "${REPO_ROOT}" -type f \( -name "*.rs" -o -name "*.py" -o -name "*.ts" -o -name "*.tsx" -o -name "*.js" \) \
        -not -path "*/target/*" -not -path "*/node_modules/*" -not -path "*/.git/*" \
        -newer "${INDEX_FILE}" -print -quit 2>/dev/null | grep -q .; then
  needs_index=1
fi

if [[ "${needs_index}" -eq 1 ]]; then
  echo "[start-local.sh] bootstrapping leio-code index (cold start)..."
  if [[ -n "${LEIO_CODE_BIN_RESOLVED}" ]]; then
    (cd "${REPO_ROOT}" && "${LEIO_CODE_BIN_RESOLVED}" bootstrap-kb >/dev/null 2>&1) || true
  else
    echo "[start-local.sh] warning: leio-code binary not found; server will index on first query (slow first call)" >&2
  fi
fi

# ---------------------------------------------------------------------------
# Node modules
# ---------------------------------------------------------------------------

if [[ ! -d "${APPS_SDK_DIR}/node_modules" ]]; then
  echo "[start-local.sh] installing apps-sdk dependencies..."
  (cd "${APPS_SDK_DIR}" && npm ci --omit=dev >/dev/null 2>&1)
fi

# ---------------------------------------------------------------------------
# Launch
# ---------------------------------------------------------------------------

export LEIO_APPS_SDK_HOST
export LEIO_APPS_SDK_PORT
export LEIO_CODE_REPO_ROOT="${REPO_ROOT}"
export LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT="${LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT:-true}"

if [[ "${MODE}" == "foreground" ]]; then
  exec node "${APPS_SDK_DIR}/server.js"
fi

# Background mode. On macOS, launch through launchd so the server survives
# tool-runner process-group cleanup after this script exits.
if [[ "$(uname -s)" == "Darwin" ]] \
  && command -v launchctl >/dev/null 2>&1 \
  && [[ "${LEIO_APPS_SDK_DISABLE_LAUNCHCTL:-0}" != "1" ]]; then
  NODE_BIN="${NODE_BIN:-$(command -v node || true)}"
  if [[ -z "${NODE_BIN}" ]]; then
    echo "[start-local.sh] node not found on PATH" >&2
    exit 1
  fi
  launchctl remove "${LAUNCH_LABEL}" >/dev/null 2>&1 || true
  rm -f "${PID_FILE}"
  launchctl submit \
    -l "${LAUNCH_LABEL}" \
    -o "${LOG_FILE}" \
    -e "${LOG_FILE}" \
    -- /usr/bin/env \
      "LEIO_APPS_SDK_HOST=${LEIO_APPS_SDK_HOST}" \
      "LEIO_APPS_SDK_PORT=${LEIO_APPS_SDK_PORT}" \
      "LEIO_CODE_REPO_ROOT=${REPO_ROOT}" \
      "LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT=${LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT:-true}" \
      "LEIO_CODE_BIN=${LEIO_CODE_BIN:-}" \
      "${NODE_BIN}" "${APPS_SDK_DIR}/server.js"
else
  nohup node "${APPS_SDK_DIR}/server.js" </dev/null >>"${LOG_FILE}" 2>&1 &
  server_pid="$!"
  echo "${server_pid}" >"${PID_FILE}"
  disown "${server_pid}" 2>/dev/null || true
fi

# ---------------------------------------------------------------------------
# Health wait
# ---------------------------------------------------------------------------

for _ in $(seq 1 30); do
  if curl -fsS --max-time 1 "${HEALTH_URL}" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

if ! curl -fsS --max-time 1 "${HEALTH_URL}" >/dev/null 2>&1; then
  echo "[start-local.sh] server failed to come up; tail of ${LOG_FILE}:" >&2
  tail -20 "${LOG_FILE}" >&2 || true
  exit 1
fi

if [[ ! -s "${PID_FILE}" ]]; then
  pgrep -f "${APPS_SDK_DIR}/server.js" | head -n 1 >"${PID_FILE}" || true
fi

# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

echo "leio-code apps-sdk: running on ${HEALTH_URL} (pid $(cat "${PID_FILE}"))"
echo "    MCP endpoint:  ${MCP_URL}"
echo "    logs:          ${LOG_FILE}"
echo "    stop:          bash $0 --stop"
echo
echo "Codex config snippet (paste into ~/.codex/config.toml):"
cat <<TOML
[mcp_servers.leio_code]
url = "${MCP_URL}"
transport = "streamable_http"
TOML
