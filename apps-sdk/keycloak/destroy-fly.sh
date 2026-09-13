#!/usr/bin/env bash
# Decommission Fly LEIO surfaces after GCP cutover (auth.getjai.com + example-leio).
# Safe to re-run: ignores missing apps/volumes.
set -euo pipefail

APPS=(leio-code-apps-sdk leio-code-keycloak)
VOLUME_ID="${LEIO_KEYCLOAK_FLY_VOLUME_ID:-vol_vly56m8jq280m9g4}"

command -v flyctl >/dev/null || command -v fly >/dev/null || {
  echo "flyctl not found" >&2
  exit 1
}
FLY="${FLY:-$(command -v flyctl || command -v fly)}"

for app in "${APPS[@]}"; do
  if "$FLY" apps list 2>/dev/null | rg -q "[[:space:]]${app}[[:space:]]"; then
    echo "destroying app ${app}"
    "$FLY" apps destroy "$app" --yes
  else
    echo "app ${app} already gone"
  fi
done

if "$FLY" volumes list 2>/dev/null | rg -q "$VOLUME_ID"; then
  echo "destroying volume ${VOLUME_ID}"
  "$FLY" volumes destroy "$VOLUME_ID" --yes
else
  echo "volume ${VOLUME_ID} already gone"
fi

echo "Fly LEIO decommission complete. Canonical: https://leio-code.getjai.com + https://auth.getjai.com/realms/leio-code"
