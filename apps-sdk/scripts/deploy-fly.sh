#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APPS_SDK_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
LEIO_CODE_ROOT="$(cd "$APPS_SDK_DIR/.." && pwd)"

APP_NAME="${FLY_APP_NAME:-leio-code-apps-sdk}"
PRIMARY_REGION="${FLY_PRIMARY_REGION:-gru}"
AUTH_MODE="${LEIO_APPS_SDK_AUTH_MODE:-none}"
TENANT_HMAC_SECRET="${LEIO_APPS_SDK_TENANT_HMAC_SECRET:-}"
TENANT_CLAIM="${LEIO_APPS_SDK_TENANT_CLAIM:-tenant_id}"
STATIC_TENANT_ID="${LEIO_APPS_SDK_STATIC_TENANT_ID:-static-bearer}"
GIT_URL="${LEIO_CODE_GIT_URL:-}"
GIT_REF="${LEIO_CODE_GIT_REF:-}"
AUTH_ENV_FILE="${LEIO_APPS_SDK_AUTH_ENV_FILE:-$APPS_SDK_DIR/.env.fly.auth.local}"

if [[ -n "$GIT_URL" ]]; then
  GIT_URL="$(node "$APPS_SDK_DIR/validate-repo-url.mjs" "$GIT_URL")"
fi

if [[ "$AUTH_MODE" != "none" ]]; then
  if [[ -z "$TENANT_HMAC_SECRET" ]]; then
    echo "❌ Missing required env for authenticated deploy: LEIO_APPS_SDK_TENANT_HMAC_SECRET"
    exit 1
  fi
  TENANT_HMAC_SECRET_BYTES="$(printf '%s' "$TENANT_HMAC_SECRET" | LC_ALL=C wc -c)"
  if [[ $TENANT_HMAC_SECRET_BYTES -lt 32 ]]; then
    echo "❌ LEIO_APPS_SDK_TENANT_HMAC_SECRET must contain at least 32 UTF-8 bytes"
    exit 1
  fi
fi

cd "$LEIO_CODE_ROOT"

echo "═══════════════════════════════════════════"
echo "  LEIO CODE APPS SDK DEPLOY"
echo "═══════════════════════════════════════════"

if ! fly apps list 2>/dev/null | awk '{print $1}' | grep -qx "$APP_NAME"; then
  echo ""
  echo "▶ Creating Fly app $APP_NAME..."
  fly apps create "$APP_NAME"
fi

FLY_HOSTNAME="${APP_NAME}.fly.dev"
PUBLIC_URL="https://${FLY_HOSTNAME}"

echo ""
echo "▶ Staging Fly secrets..."
fly secrets set \
  LEIO_APPS_SDK_PUBLIC_URL="$PUBLIC_URL" \
  LEIO_APPS_SDK_AUTH_MODE="$AUTH_MODE" \
  LEIO_APPS_SDK_HOST="0.0.0.0" \
  LEIO_APPS_SDK_PORT="3333" \
  --app "$APP_NAME" \
  --stage

if [[ "$AUTH_MODE" != "none" ]]; then
  fly secrets set LEIO_APPS_SDK_TENANT_HMAC_SECRET="$TENANT_HMAC_SECRET" \
    --app "$APP_NAME" \
    --stage
fi

optional_public_vars=(
  LEIO_APPS_SDK_RESOURCE_NAME
  LEIO_APPS_SDK_PUBLISHER_NAME
  LEIO_APPS_SDK_COMPANY_NAME
  LEIO_APPS_SDK_COMPANY_URL
  LEIO_APPS_SDK_SUPPORT_URL
  LEIO_APPS_SDK_PRIVACY_URL
  LEIO_APPS_SDK_TERMS_URL
  LEIO_APPS_SDK_SUPPORT_EMAIL
  LEIO_APPS_SDK_PRIVACY_EMAIL
  LEIO_APPS_SDK_SECURITY_EMAIL
  LEIO_APPS_SDK_SUPPORT_HOURS
  LEIO_APPS_SDK_LEGAL_LAST_UPDATED
  LEIO_CODE_TIMEOUT_MS
  LEIO_CODE_INDEX_TIMEOUT_MS
  LEIO_CODE_CHECKOUTS_ROOT
  LEIO_CODE_ALLOWED_REPO_HOSTS
  LEIO_CODE_ALLOW_INSECURE_LOCAL_REPO_URLS
)

for name in "${optional_public_vars[@]}"; do
  value="${!name:-}"
  if [[ -n "$value" ]]; then
    fly secrets set "$name=$value" --app "$APP_NAME" --stage
  fi
done

if [[ -n "$GIT_URL" ]]; then
  fly secrets set LEIO_CODE_GIT_URL="$GIT_URL" --app "$APP_NAME" --stage
fi

if [[ -n "$GIT_REF" ]]; then
  fly secrets set LEIO_CODE_GIT_REF="$GIT_REF" --app "$APP_NAME" --stage
fi

optional_source_vars=(
  LEIO_CODE_GIT_HTTP_USERNAME
  LEIO_CODE_GIT_HTTP_PASSWORD
  LEIO_CODE_GIT_AUTH_TOKEN
  LEIO_CODE_GITHUB_TOKEN
  LEIO_CODE_GITLAB_TOKEN
  LEIO_CODE_GITHUB_APP_SLUG
  LEIO_CODE_GITHUB_APP_ID
  LEIO_CODE_GITHUB_APP_CLIENT_ID
  LEIO_CODE_GITHUB_APP_CLIENT_SECRET
  LEIO_CODE_GITHUB_APP_PRIVATE_KEY
  LEIO_CODE_GITHUB_APP_CALLBACK_URL
)

for name in "${optional_source_vars[@]}"; do
  value="${!name:-}"
  if [[ -n "$value" ]]; then
    fly secrets set "$name=$value" --app "$APP_NAME" --stage
  fi
done

if [[ "$AUTH_MODE" == "oauth-jwt" ]]; then
  required_vars=(
    LEIO_APPS_SDK_JWT_ISSUER
    LEIO_APPS_SDK_AUTHORIZATION_SERVERS
    LEIO_APPS_SDK_JWT_AUDIENCE
  )
  for name in "${required_vars[@]}"; do
    value="${!name:-}"
    if [[ -z "$value" ]]; then
      echo "❌ Missing required env for oauth-jwt deploy: $name"
      exit 1
    fi
    fly secrets set "$name=$value" --app "$APP_NAME" --stage
  done

  fly secrets set LEIO_APPS_SDK_TENANT_CLAIM="$TENANT_CLAIM" \
    --app "$APP_NAME" \
    --stage

  optional_vars=(
    LEIO_APPS_SDK_AUTH_SCOPES
    LEIO_APPS_SDK_PROTECTED_TOOLS
    LEIO_APPS_SDK_OIDC_DISCOVERY_URL
    LEIO_APPS_SDK_JWKS_URL
  )
  for name in "${optional_vars[@]}"; do
    value="${!name:-}"
    if [[ -n "$value" ]]; then
      fly secrets set "$name=$value" --app "$APP_NAME" --stage
    fi
  done
fi

if [[ "$AUTH_MODE" == "static-bearer" ]]; then
  token_value="${LEIO_APPS_SDK_STATIC_BEARER_TOKENS:-}"
  if [[ -z "$token_value" ]]; then
    echo "❌ Missing required env for static-bearer deploy: LEIO_APPS_SDK_STATIC_BEARER_TOKENS"
    exit 1
  fi
  fly secrets set \
    LEIO_APPS_SDK_STATIC_BEARER_TOKENS="$token_value" \
    LEIO_APPS_SDK_STATIC_TENANT_ID="$STATIC_TENANT_ID" \
    --app "$APP_NAME" \
    --stage
fi

if [[ "$AUTH_MODE" != "none" ]]; then
  mkdir -p "$(dirname "$AUTH_ENV_FILE")"
  {
    echo "LEIO_APPS_SDK_PUBLIC_URL=$PUBLIC_URL"
    echo "LEIO_APPS_SDK_AUTH_MODE=$AUTH_MODE"
    echo "LEIO_APPS_SDK_TENANT_HMAC_SECRET=${LEIO_APPS_SDK_TENANT_HMAC_SECRET}"
    if [[ "$AUTH_MODE" == "static-bearer" ]]; then
      echo "LEIO_APPS_SDK_STATIC_BEARER_TOKENS=${LEIO_APPS_SDK_STATIC_BEARER_TOKENS}"
      echo "LEIO_APPS_SDK_STATIC_TENANT_ID=$STATIC_TENANT_ID"
    fi
    if [[ "$AUTH_MODE" == "oauth-jwt" ]]; then
      echo "LEIO_APPS_SDK_JWT_ISSUER=${LEIO_APPS_SDK_JWT_ISSUER}"
      echo "LEIO_APPS_SDK_AUTHORIZATION_SERVERS=${LEIO_APPS_SDK_AUTHORIZATION_SERVERS}"
      echo "LEIO_APPS_SDK_JWT_AUDIENCE=${LEIO_APPS_SDK_JWT_AUDIENCE}"
      echo "LEIO_APPS_SDK_TENANT_CLAIM=$TENANT_CLAIM"
      if [[ -n "${LEIO_APPS_SDK_AUTH_SCOPES:-}" ]]; then
        echo "LEIO_APPS_SDK_AUTH_SCOPES=${LEIO_APPS_SDK_AUTH_SCOPES}"
      fi
    fi
  } >"$AUTH_ENV_FILE"
  chmod 600 "$AUTH_ENV_FILE"
fi

echo ""
echo "▶ Deploying to Fly..."
fly deploy --app "$APP_NAME" --config "$APPS_SDK_DIR/fly.toml"

echo ""
echo "▶ Smoke checking deployment..."
curl -fsS "$PUBLIC_URL/health" | sed -n '1,120p'

echo ""
echo "═══════════════════════════════════════════"
echo "  ✅ DEPLOY COMPLETE"
echo "═══════════════════════════════════════════"
echo ""
echo "  App:                 $APP_NAME"
echo "  Public URL:          $PUBLIC_URL"
echo "  Health:              $PUBLIC_URL/health"
echo "  MCP endpoint:        $PUBLIC_URL/mcp"
echo "  Widget:              $PUBLIC_URL/widget"
echo "  Auth mode:           $AUTH_MODE"
if [[ "$AUTH_MODE" != "none" ]]; then
  echo "  Auth env file:       $AUTH_ENV_FILE"
fi
if [[ -n "$GIT_URL" ]]; then
  echo "  Runtime repo clone:  $GIT_URL"
  if [[ -n "$GIT_REF" ]]; then
    echo "  Runtime repo ref:    $GIT_REF"
  fi
else
  echo "  Runtime repo:        baked-in leio-code snapshot"
fi
