#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KEYCLOAK_DIR="$SCRIPT_DIR"
APPS_SDK_DIR="$(cd "$KEYCLOAK_DIR/.." && pwd)"

APP_NAME="${FLY_KEYCLOAK_APP_NAME:-leio-code-keycloak}"
PRIMARY_REGION="${FLY_PRIMARY_REGION:-gru}"
FLY_ORG="${FLY_ORG:-personal}"
POSTGRES_APP_NAME="${FLY_KEYCLOAK_POSTGRES_APP_NAME:-leio-code-keycloak-db}"
POSTGRES_DATABASE_NAME="${FLY_KEYCLOAK_POSTGRES_DATABASE:-keycloak}"
POSTGRES_USERNAME="${FLY_KEYCLOAK_POSTGRES_USERNAME:-keycloak}"
POSTGRES_WAIT_TIMEOUT_SECONDS="${FLY_KEYCLOAK_POSTGRES_WAIT_TIMEOUT_SECONDS:-1200}"
POSTGRES_WAIT_INTERVAL_SECONDS="${FLY_KEYCLOAK_POSTGRES_WAIT_INTERVAL_SECONDS:-10}"
VOLUME_NAME="${FLY_KEYCLOAK_VOLUME_NAME:-keycloak_data}"
VOLUME_SIZE_GB="${FLY_KEYCLOAK_VOLUME_SIZE_GB:-10}"
ADMIN_USERNAME="${KEYCLOAK_BOOTSTRAP_ADMIN_USERNAME:-admin}"
ADMIN_PASSWORD="${KEYCLOAK_BOOTSTRAP_ADMIN_PASSWORD:-$(openssl rand -base64 24 | tr -d '\n')}"
APPS_CLIENT_SECRET="${LEIO_KEYCLOAK_APPS_CLIENT_SECRET:-$(openssl rand -hex 24)}"
PUBLIC_URL="${KEYCLOAK_HOSTNAME_URL:-https://${APP_NAME}.fly.dev}"
ENV_FILE="${KEYCLOAK_FLY_ENV_FILE:-$KEYCLOAK_DIR/.env.fly.local}"
ENABLE_POSTGRES="${LEIO_KEYCLOAK_ENABLE_POSTGRES:-0}"
ENABLE_POSTGRES_RUNTIME="${LEIO_KEYCLOAK_ENABLE_POSTGRES_RUNTIME:-$ENABLE_POSTGRES}"
POSTGRES_PLAN="${FLY_KEYCLOAK_POSTGRES_PLAN:-development}"

fly_json_output() {
  "$@" | sed -n '/^[[:space:]]*[{[]/,$p'
}

get_postgres_cluster_id() {
  fly_json_output fly mpg list -o "$FLY_ORG" --json | python3 - "$POSTGRES_APP_NAME" <<'PY'
import json
import sys

target = sys.argv[1]
clusters = json.load(sys.stdin)
for cluster in clusters:
    if cluster.get("name") == target:
        print(cluster["id"])
        sys.exit(0)
sys.exit(1)
PY
}

wait_for_postgres_ready() {
  local cluster_id="$1"
  local started_at
  started_at="$(date +%s)"

  while true; do
    local current_state
    current_state="$(
      fly_json_output fly mpg status "$cluster_id" --json | python3 - <<'PY'
import json
import sys

payload = json.load(sys.stdin)
print(payload["data"]["status"])
PY
    )"

    if [[ "$current_state" == "ready" ]]; then
      return 0
    fi

    local now
    now="$(date +%s)"
    if (( now - started_at >= POSTGRES_WAIT_TIMEOUT_SECONDS )); then
      echo "❌ Managed Postgres cluster $cluster_id did not become ready within ${POSTGRES_WAIT_TIMEOUT_SECONDS}s (last state: $current_state)"
      return 1
    fi

    echo "   Postgres cluster state: $current_state; waiting ${POSTGRES_WAIT_INTERVAL_SECONDS}s..."
    sleep "$POSTGRES_WAIT_INTERVAL_SECONDS"
  done
}

postgres_user_exists() {
  local cluster_id="$1"
  fly mpg users list "$cluster_id" 2>/dev/null | awk 'NR>1 {print $1}' | grep -qx "$POSTGRES_USERNAME"
}

postgres_database_exists() {
  local cluster_id="$1"
  fly mpg databases list "$cluster_id" 2>/dev/null | awk 'NR>1 {print $1}' | grep -qx "$POSTGRES_DATABASE_NAME"
}

volume_exists() {
  fly volumes list -a "$APP_NAME" 2>/dev/null | awk 'NR>1 {print $3}' | grep -qx "$VOLUME_NAME"
}

echo "═══════════════════════════════════════════"
echo "  LEIO CODE KEYCLOAK DEPLOY"
echo "═══════════════════════════════════════════"

if ! fly apps list 2>/dev/null | awk '{print $1}' | grep -qx "$APP_NAME"; then
  echo ""
  echo "▶ Creating Fly app $APP_NAME..."
  fly apps create "$APP_NAME"
fi

if [[ "$ENABLE_POSTGRES" == "1" ]]; then
  if ! fly_json_output fly mpg list -o "$FLY_ORG" --json | python3 - "$POSTGRES_APP_NAME" <<'PY'
import json
import sys

target = sys.argv[1]
clusters = json.load(sys.stdin)
sys.exit(0 if any(cluster.get("name") == target for cluster in clusters) else 1)
PY
  then
    echo ""
    echo "▶ Creating Fly Managed Postgres cluster $POSTGRES_APP_NAME..."
    fly mpg create \
      --name "$POSTGRES_APP_NAME" \
      --org "$FLY_ORG" \
      --region "$PRIMARY_REGION" \
      --plan "$POSTGRES_PLAN" \
      --volume-size 10
  fi
fi

POSTGRES_CLUSTER_ID=""
if [[ "$ENABLE_POSTGRES" == "1" ]]; then
  echo ""
  echo "▶ Resolving managed Postgres cluster..."
  POSTGRES_CLUSTER_ID="$(get_postgres_cluster_id)"
  echo "   Cluster ID: $POSTGRES_CLUSTER_ID"

  echo ""
  echo "▶ Waiting for managed Postgres readiness..."
  wait_for_postgres_ready "$POSTGRES_CLUSTER_ID"

  if ! postgres_user_exists "$POSTGRES_CLUSTER_ID"; then
    echo ""
    echo "▶ Creating Postgres user $POSTGRES_USERNAME..."
    if ! fly mpg users create "$POSTGRES_CLUSTER_ID" --username "$POSTGRES_USERNAME" --role schema_admin; then
      echo "   schema_admin role unavailable; falling back to writer"
      fly mpg users create "$POSTGRES_CLUSTER_ID" --username "$POSTGRES_USERNAME" --role writer
    fi
  fi

  if ! postgres_database_exists "$POSTGRES_CLUSTER_ID"; then
    echo ""
    echo "▶ Creating Postgres database $POSTGRES_DATABASE_NAME..."
    fly mpg databases create "$POSTGRES_CLUSTER_ID" --name "$POSTGRES_DATABASE_NAME"
  fi
fi

if [[ "$ENABLE_POSTGRES" != "1" ]] && ! volume_exists; then
  echo ""
  echo "▶ Creating persistent Fly volume $VOLUME_NAME..."
  fly volumes create "$VOLUME_NAME" \
    --app "$APP_NAME" \
    --region "$PRIMARY_REGION" \
    --size "$VOLUME_SIZE_GB" \
    --yes
fi

echo ""
echo "▶ Staging Fly secrets..."
fly secrets set \
  KC_BOOTSTRAP_ADMIN_USERNAME="$ADMIN_USERNAME" \
  KC_BOOTSTRAP_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
  LEIO_KEYCLOAK_APPS_CLIENT_SECRET="$APPS_CLIENT_SECRET" \
  LEIO_KEYCLOAK_ENABLE_POSTGRES_RUNTIME="$ENABLE_POSTGRES_RUNTIME" \
  KEYCLOAK_HOSTNAME_URL="$PUBLIC_URL" \
  --app "$APP_NAME" \
  --stage

if [[ "$ENABLE_POSTGRES" == "1" ]]; then
  echo ""
  echo "▶ Attaching Postgres cluster to Keycloak app..."
  fly mpg attach "$POSTGRES_CLUSTER_ID" \
    --app "$APP_NAME" \
    --database "$POSTGRES_DATABASE_NAME" \
    --username "$POSTGRES_USERNAME" \
    --variable-name DATABASE_URL
fi

echo ""
echo "▶ Deploying Keycloak to Fly..."
cd "$KEYCLOAK_DIR"
fly deploy --yes --strategy immediate --app "$APP_NAME" --config "$KEYCLOAK_DIR/fly.toml"

mkdir -p "$(dirname "$ENV_FILE")"
cat >"$ENV_FILE" <<EOF
KEYCLOAK_BASE_URL=${PUBLIC_URL}
KEYCLOAK_ISSUER=${PUBLIC_URL}/realms/leio-code
KEYCLOAK_REALM=leio-code
KEYCLOAK_BOOTSTRAP_ADMIN_USERNAME=${ADMIN_USERNAME}
KEYCLOAK_BOOTSTRAP_ADMIN_PASSWORD=${ADMIN_PASSWORD}
LEIO_KEYCLOAK_APPS_CLIENT_ID=leio-code-apps-sdk
KEYCLOAK_CLIENT_ID=leio-code-apps-sdk
KEYCLOAK_CLIENT_SECRET=${APPS_CLIENT_SECRET}
LEIO_KEYCLOAK_APPS_CLIENT_SECRET=${APPS_CLIENT_SECRET}
LEIO_APPS_SDK_JWT_ISSUER=${PUBLIC_URL}/realms/leio-code
LEIO_APPS_SDK_AUTHORIZATION_SERVERS=${PUBLIC_URL}/realms/leio-code
LEIO_APPS_SDK_JWT_AUDIENCE=leio-code-apps-sdk
LEIO_APPS_SDK_AUTH_SCOPES=repo.read
FLY_KEYCLOAK_POSTGRES_APP_NAME=${POSTGRES_APP_NAME}
FLY_KEYCLOAK_POSTGRES_CLUSTER_ID=${POSTGRES_CLUSTER_ID}
FLY_KEYCLOAK_POSTGRES_DATABASE=${POSTGRES_DATABASE_NAME}
FLY_KEYCLOAK_POSTGRES_USERNAME=${POSTGRES_USERNAME}
FLY_KEYCLOAK_VOLUME_NAME=${VOLUME_NAME}
FLY_KEYCLOAK_VOLUME_SIZE_GB=${VOLUME_SIZE_GB}
LEIO_KEYCLOAK_ENABLE_POSTGRES=${ENABLE_POSTGRES}
LEIO_KEYCLOAK_ENABLE_POSTGRES_RUNTIME=${ENABLE_POSTGRES_RUNTIME}
EOF
chmod 600 "$ENV_FILE"

echo ""
echo "▶ Smoke checking Keycloak discovery..."
curl -fsS -m 120 "${PUBLIC_URL}/realms/leio-code/.well-known/openid-configuration" | sed -n '1,80p'

echo ""
echo "═══════════════════════════════════════════"
echo "  ✅ KEYCLOAK DEPLOY COMPLETE"
echo "═══════════════════════════════════════════"
echo ""
echo "  App:                 $APP_NAME"
echo "  Public URL:          $PUBLIC_URL"
echo "  Issuer:              ${PUBLIC_URL}/realms/leio-code"
echo "  Env file:            $ENV_FILE"
