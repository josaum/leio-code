#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APPS_SDK_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
IMAGE="${LEIO_CODE_APPS_SDK_IMAGE:-leio-code-apps-sdk}"
NAME="${LEIO_CODE_APPS_SDK_SMOKE_NAME:-leio-code-apps-sdk-smoke-$$}"
PORT="${LEIO_CODE_APPS_SDK_SMOKE_PORT:-}"
AUTH_MODE="${LEIO_APPS_SDK_AUTH_MODE:-none}"
HEALTH_TIMEOUT_SECONDS="${LEIO_CODE_APPS_SDK_SMOKE_TIMEOUT_SECONDS:-60}"

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}

if [[ "${LEIO_CODE_APPS_SDK_SMOKE_BUILD:-0}" == "1" ]]; then
  (cd "$APPS_SDK_DIR" && npm run docker:build)
fi

cleanup
trap cleanup EXIT

publish_arg="127.0.0.1::3333"
if [[ -n "$PORT" ]]; then
  publish_arg="127.0.0.1:${PORT}:3333"
fi

docker_env=(
  -e "LEIO_APPS_SDK_AUTH_MODE=${AUTH_MODE}"
)

for name in \
  LEIO_APPS_SDK_PUBLIC_URL \
  LEIO_APPS_SDK_STATIC_BEARER_TOKENS \
  LEIO_APPS_SDK_AUTH_SCOPES \
  LEIO_APPS_SDK_PROTECTED_TOOLS \
  LEIO_CODE_ALLOWED_REPO_HOSTS \
  LEIO_CODE_ALLOW_INSECURE_LOCAL_REPO_URLS; do
  value="${!name:-}"
  if [[ -n "$value" ]]; then
    docker_env+=(-e "$name=$value")
  fi
done

docker run \
  --detach \
  --name "$NAME" \
  --publish "$publish_arg" \
  "${docker_env[@]}" \
  "$IMAGE" >/dev/null

if [[ -z "$PORT" ]]; then
  PORT="$(docker port "$NAME" 3333/tcp | sed -E 's/.*:([0-9]+)$/\1/' | head -n 1)"
fi

BASE_URL="http://127.0.0.1:${PORT}"
deadline=$((SECONDS + HEALTH_TIMEOUT_SECONDS))

until curl -fsS "${BASE_URL}/health" >/dev/null 2>&1; do
  if ((SECONDS >= deadline)); then
    echo "container did not become healthy at ${BASE_URL}/health" >&2
    docker logs "$NAME" >&2 || true
    exit 1
  fi
  sleep 1
done

LEIO_APPS_SDK_SMOKE_BASE_URL="$BASE_URL" \
LEIO_APPS_SDK_SMOKE_REPO_ROOT="/workspace/baked-repo" \
LEIO_APPS_SDK_SMOKE_EXPECT_AUTH_MODE="$AUTH_MODE" \
node "$APPS_SDK_DIR/smoke.mjs"

echo "docker smoke passed: ${BASE_URL}"
