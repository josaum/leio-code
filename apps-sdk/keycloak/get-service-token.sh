#!/usr/bin/env bash
set -euo pipefail

KEYCLOAK_BASE_URL="${KEYCLOAK_BASE_URL:-http://localhost:8080}"
KEYCLOAK_REALM="${KEYCLOAK_REALM:-leio-code}"
KEYCLOAK_CLIENT_ID="${KEYCLOAK_CLIENT_ID:-leio-code-apps-sdk}"
KEYCLOAK_CLIENT_SECRET="${KEYCLOAK_CLIENT_SECRET:-${LEIO_KEYCLOAK_APPS_CLIENT_SECRET:-change-me-before-production}}"
KEYCLOAK_SCOPE="${KEYCLOAK_SCOPE:-repo.read}"

TOKEN_URL="${KEYCLOAK_BASE_URL%/}/realms/${KEYCLOAK_REALM}/protocol/openid-connect/token"

curl -fsS \
  -X POST "$TOKEN_URL" \
  -H "content-type: application/x-www-form-urlencoded" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=${KEYCLOAK_CLIENT_ID}" \
  --data-urlencode "client_secret=${KEYCLOAK_CLIENT_SECRET}" \
  --data-urlencode "scope=${KEYCLOAK_SCOPE}"
