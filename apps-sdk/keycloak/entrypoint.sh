#!/usr/bin/env bash
set -euo pipefail

IMPORT_DIR="/opt/keycloak/data/import"
SOURCE_REALM="/opt/keycloak/local-realm/leio-code-realm.json"
TARGET_REALM="${IMPORT_DIR}/leio-code-realm.json"
APPS_CLIENT_SECRET="${LEIO_KEYCLOAK_APPS_CLIENT_SECRET:-change-me-before-production}"
HOSTNAME_URL="${KEYCLOAK_HOSTNAME_URL:-https://${FLY_APP_NAME:-localhost}.fly.dev}"
HTTP_PORT="${PORT:-8080}"
ENABLE_POSTGRES_RUNTIME="${LEIO_KEYCLOAK_ENABLE_POSTGRES_RUNTIME:-false}"

mkdir -p "$IMPORT_DIR"
cp "$SOURCE_REALM" "$TARGET_REALM"

escaped_secret="$(printf '%s' "$APPS_CLIENT_SECRET" | sed 's/[\/&]/\\&/g')"
sed -i "s/change-me-before-production/${escaped_secret}/g" "$TARGET_REALM"

if [[ "$ENABLE_POSTGRES_RUNTIME" == "true" && -n "${DATABASE_URL:-}" && -z "${KC_DB_URL:-}" ]]; then
  db_uri="${DATABASE_URL#postgres://}"
  db_uri="${db_uri#postgresql://}"
  creds="${db_uri%%@*}"
  host_and_rest="${db_uri#*@}"
  host_port="${host_and_rest%%/*}"
  db_and_query="${host_and_rest#*/}"
  db_name="${db_and_query%%\?*}"
  db_user="${creds%%:*}"
  db_pass="${creds#*:}"
  db_host="${host_port%%:*}"
  db_port="${host_port##*:}"

  export KC_DB=postgres
  export KC_DB_USERNAME="${KC_DB_USERNAME:-$db_user}"
  export KC_DB_PASSWORD="${KC_DB_PASSWORD:-$db_pass}"
  export KC_DB_URL="${KC_DB_URL:-jdbc:postgresql://${db_host}:${db_port}/${db_name}}"
fi

export KC_DB="${KC_DB:-dev-file}"
export KC_CACHE="${KC_CACHE:-local}"
export KC_HTTP_ENABLED=true
export KC_PROXY_HEADERS="${KC_PROXY_HEADERS:-xforwarded}"
export KC_HEALTH_ENABLED="${KC_HEALTH_ENABLED:-true}"
export KC_METRICS_ENABLED="${KC_METRICS_ENABLED:-true}"

exec /opt/keycloak/bin/kc.sh start \
  --optimized \
  --http-port="${HTTP_PORT}" \
  --http-host=0.0.0.0 \
  --hostname="${HOSTNAME_URL}" \
  --proxy-headers=xforwarded \
  --http-enabled=true \
  --import-realm
