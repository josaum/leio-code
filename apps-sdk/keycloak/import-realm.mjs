#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const defaultRealmFile = path.join(__dirname, "realm", "leio-code-realm.json");

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    const value = argv[i];
    if (value === "--base-url") args.baseUrl = argv[++i];
    else if (value === "--realm-file") args.realmFile = argv[++i];
    else if (value === "--admin-username") args.adminUsername = argv[++i];
    else if (value === "--admin-password") args.adminPassword = argv[++i];
    else if (value === "--force") args.force = true;
    else if (value === "--json") args.json = true;
  }
  return args;
}

function envOr(...values) {
  return values.find((value) => typeof value === "string" && value.trim())?.trim() ?? null;
}

async function fetchText(url, init) {
  const response = await fetch(url, init);
  const text = await response.text();
  return { response, text };
}

async function fetchJson(url, init) {
  const { response, text } = await fetchText(url, init);
  return {
    response,
    payload: text ? JSON.parse(text) : null,
  };
}

async function getAdminToken({ baseUrl, adminUsername, adminPassword }) {
  const tokenUrl = new URL("/realms/master/protocol/openid-connect/token", baseUrl);
  const body = new URLSearchParams({
    grant_type: "password",
    client_id: "admin-cli",
    username: adminUsername,
    password: adminPassword,
  });
  const { response, payload } = await fetchJson(tokenUrl, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body,
  });
  if (!response.ok || !payload?.access_token) {
    throw new Error("Keycloak admin token request failed");
  }
  return payload.access_token;
}

function adminHeaders(token) {
  return {
    authorization: `Bearer ${token}`,
    accept: "application/json",
    "content-type": "application/json",
  };
}

async function realmExists({ baseUrl, realm, token }) {
  const url = new URL(`/admin/realms/${realm}`, baseUrl);
  const { response } = await fetchText(url, { headers: adminHeaders(token) });
  return response.status === 200;
}

async function deleteRealm({ baseUrl, realm, token }) {
  const url = new URL(`/admin/realms/${realm}`, baseUrl);
  const { response, text } = await fetchText(url, {
    method: "DELETE",
    headers: adminHeaders(token),
  });
  if (!response.ok) {
    throw new Error(`Failed to delete realm ${realm}: ${response.status} ${text}`);
  }
}

async function sanitizeRealmPayload(realmPayload) {
  const payload = structuredClone(realmPayload);
  // Prod Keycloak (26.x, profile=prod) rejects registration-web-origins
  // components from dev-oriented realm exports. configure-dcr.mjs patches DCR
  // after the realm exists.
  delete payload.components;
  // offline_access may not exist yet during full-realm import; configure-dcr
  // adds repo.read/openid as optional scopes.
  delete payload.defaultOptionalClientScopes;
  return payload;
}

async function importRealm({ baseUrl, realmPayload, token }) {
  const sanitized = await sanitizeRealmPayload(realmPayload);
  const url = new URL("/admin/realms", baseUrl);
  const { response, text } = await fetchText(url, {
    method: "POST",
    headers: adminHeaders(token),
    body: JSON.stringify(sanitized),
  });
  if (!response.ok) {
    throw new Error(`Failed to import realm: ${response.status} ${text}`);
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const baseUrl = envOr(args.baseUrl, process.env.KEYCLOAK_BASE_URL, "http://localhost:8080");
  const adminUsername = envOr(
    args.adminUsername,
    process.env.KEYCLOAK_BOOTSTRAP_ADMIN_USERNAME,
    process.env.KC_ADMIN_USERNAME,
    "admin",
  );
  const adminPassword = envOr(
    args.adminPassword,
    process.env.KEYCLOAK_BOOTSTRAP_ADMIN_PASSWORD,
    process.env.KC_ADMIN_PASSWORD,
    "admin",
  );
  const realmFile = envOr(args.realmFile, process.env.LEIO_KEYCLOAK_REALM_FILE, defaultRealmFile);
  const realmPayload = JSON.parse(await readFile(realmFile, "utf8"));
  const realm = envOr(process.env.KEYCLOAK_REALM, realmPayload.realm);
  if (realm !== realmPayload.realm) {
    throw new Error("KEYCLOAK_REALM must match the realm declared in the import payload");
  }

  const token = await getAdminToken({ baseUrl, adminUsername, adminPassword });
  const exists = await realmExists({ baseUrl, realm, token });

  if (exists && !args.force) {
    const payload = { ok: true, realm, imported: false, reason: "already_exists" };
    if (args.json) {
      process.stdout.write(`${JSON.stringify(payload, null, 2)}\n`);
      return;
    }
    process.stdout.write(`realm ${realm} already exists on ${baseUrl}\n`);
    return;
  }

  if (exists && args.force) {
    await deleteRealm({ baseUrl, realm, token });
  }

  await importRealm({ baseUrl, realmPayload, token });

  const payload = { ok: true, realm, imported: true, base_url: baseUrl, realm_file: realmFile };
  if (args.json) {
    process.stdout.write(`${JSON.stringify(payload, null, 2)}\n`);
    return;
  }
  process.stdout.write(`imported realm ${realm} to ${baseUrl}\n`);
}

await main();
