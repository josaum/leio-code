#!/usr/bin/env node

import {
  buildDesiredAppsClientConfig,
  parseList,
  updateEnvFileContent,
} from "./chatgpt-client-config.mjs";
import { readFile, writeFile } from "node:fs/promises";

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    const value = argv[i];
    if (value === "--base-url") args.baseUrl = argv[++i];
    else if (value === "--realm") args.realm = argv[++i];
    else if (value === "--admin-username") args.adminUsername = argv[++i];
    else if (value === "--admin-password") args.adminPassword = argv[++i];
    else if (value === "--client-id") args.clientId = argv[++i];
    else if (value === "--public-url") args.publicUrl = argv[++i];
    else if (value === "--chatgpt-redirect-uri") args.chatgptRedirectUri = argv[++i];
    else if (value === "--extra-redirect-uri") {
      args.extraRedirectUris ??= [];
      args.extraRedirectUris.push(argv[++i]);
    } else if (value === "--extra-web-origin") {
      args.extraWebOrigins ??= [];
      args.extraWebOrigins.push(argv[++i]);
    } else if (value === "--disable-bootstrap-wildcard") args.disableBootstrapWildcard = true;
    else if (value === "--disable-legacy-redirect") args.disableLegacyRedirect = true;
    else if (value === "--disable-review-redirect") args.disableReviewRedirect = true;
    else if (value === "--sync-env-file") args.syncEnvFile = argv[++i];
    else if (value === "--rotate-secret") args.rotateSecret = true;
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
  if (!response.ok) {
    throw new Error(
      `Keycloak request failed: ${response.status} ${response.statusText} ${text}`,
    );
  }
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
  const { payload } = await fetchJson(tokenUrl, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body,
  });
  if (!payload?.access_token) {
    throw new Error("Keycloak admin token response did not contain access_token");
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

async function syncEnvFile({
  envFile,
  baseUrl,
  realm,
  clientId,
  clientSecret,
  publicUrl,
  audience,
  exactRedirectUri,
}) {
  const issuer = new URL(`/realms/${realm}`, baseUrl).toString();
  let original = "";
  try {
    original = await readFile(envFile, "utf8");
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw error;
    }
  }

  const nextContent = updateEnvFileContent(original, {
    KEYCLOAK_BASE_URL: baseUrl,
    KEYCLOAK_ISSUER: issuer,
    KEYCLOAK_REALM: realm,
    LEIO_APPS_SDK_PUBLIC_URL: publicUrl ?? "",
    LEIO_APPS_SDK_AUTH_MODE: "oauth-jwt",
    LEIO_APPS_SDK_JWT_ISSUER: issuer,
    LEIO_APPS_SDK_AUTHORIZATION_SERVERS: issuer,
    LEIO_APPS_SDK_JWT_AUDIENCE: audience,
    LEIO_KEYCLOAK_APPS_CLIENT_ID: clientId,
    KEYCLOAK_CLIENT_ID: clientId,
    KEYCLOAK_CLIENT_SECRET: clientSecret,
    LEIO_KEYCLOAK_APPS_CLIENT_SECRET: clientSecret,
    ...(exactRedirectUri
      ? { LEIO_OPENAI_CHATGPT_REDIRECT_URI: exactRedirectUri }
      : {}),
  });

  await writeFile(envFile, nextContent, { mode: 0o600 });
}

async function resolveClient({ baseUrl, realm, token, clientId }) {
  const searchUrl = new URL(`/admin/realms/${realm}/clients`, baseUrl);
  searchUrl.searchParams.set("clientId", clientId);
  const { payload } = await fetchJson(searchUrl, {
    headers: adminHeaders(token),
  });
  const client = Array.isArray(payload) ? payload[0] ?? null : null;
  if (!client?.id) {
    throw new Error(`Keycloak client not found: ${clientId}`);
  }
  return client;
}

async function updateClient({ baseUrl, realm, token, client }) {
  const clientUrl = new URL(`/admin/realms/${realm}/clients/${client.id}`, baseUrl);
  await fetchText(clientUrl, {
    method: "PUT",
    headers: adminHeaders(token),
    body: JSON.stringify(client),
  });
}

async function rotateClientSecret({ baseUrl, realm, token, clientUuid }) {
  const secretUrl = new URL(
    `/admin/realms/${realm}/clients/${clientUuid}/client-secret`,
    baseUrl,
  );
  const { payload } = await fetchJson(secretUrl, {
    method: "POST",
    headers: adminHeaders(token),
  });
  return payload?.value ?? null;
}

async function getClientSecret({ baseUrl, realm, token, clientUuid }) {
  const secretUrl = new URL(
    `/admin/realms/${realm}/clients/${clientUuid}/client-secret`,
    baseUrl,
  );
  const { payload } = await fetchJson(secretUrl, {
    headers: adminHeaders(token),
  });
  return payload?.value ?? null;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const baseUrl = envOr(
    args.baseUrl,
    process.env.KEYCLOAK_BASE_URL,
    "http://localhost:8080",
  );
  const realm = envOr(args.realm, process.env.KEYCLOAK_REALM, "leio-code");
  const adminUsername = envOr(
    args.adminUsername,
    process.env.KEYCLOAK_BOOTSTRAP_ADMIN_USERNAME,
    "admin",
  );
  const adminPassword = envOr(
    args.adminPassword,
    process.env.KEYCLOAK_BOOTSTRAP_ADMIN_PASSWORD,
    "admin",
  );
  const clientId = envOr(
    args.clientId,
    process.env.LEIO_KEYCLOAK_APPS_CLIENT_ID,
    "leio-code-apps-sdk",
  );
  const publicUrl = envOr(
    args.publicUrl,
    process.env.LEIO_APPS_SDK_PUBLIC_URL,
    null,
  );
  const exactRedirectUri = envOr(
    args.chatgptRedirectUri,
    process.env.LEIO_OPENAI_CHATGPT_REDIRECT_URI,
    null,
  );
  const extraRedirectUris = [
    ...parseList(process.env.LEIO_OPENAI_EXTRA_REDIRECT_URIS),
    ...(args.extraRedirectUris ?? []),
  ];
  const extraWebOrigins = [
    ...parseList(process.env.LEIO_KEYCLOAK_APPS_WEB_ORIGINS),
    ...(args.extraWebOrigins ?? []),
  ];
  const rotateSecret = Boolean(args.rotateSecret);
  const syncEnvFilePath = envOr(
    args.syncEnvFile,
    process.env.LEIO_KEYCLOAK_ENV_SYNC_FILE,
    null,
  );
  const audience = envOr(
    process.env.LEIO_APPS_SDK_JWT_AUDIENCE,
    "leio-code-apps-sdk",
  );

  const desiredConfig = buildDesiredAppsClientConfig({
    publicUrl,
    exactRedirectUri,
    includeBootstrapWildcard: !args.disableBootstrapWildcard,
    includeLegacyRedirect: !args.disableLegacyRedirect,
    includeReviewRedirect: !args.disableReviewRedirect,
    extraRedirectUris,
    extraWebOrigins,
  });

  const token = await getAdminToken({
    baseUrl,
    adminUsername,
    adminPassword,
  });
  const client = await resolveClient({ baseUrl, realm, token, clientId });

  client.standardFlowEnabled = true;
  client.publicClient = false;
  client.serviceAccountsEnabled = true;
  client.implicitFlowEnabled = false;
  client.directAccessGrantsEnabled = false;
  client.redirectUris = desiredConfig.redirectUris;
  client.webOrigins = desiredConfig.webOrigins;
  client.rootUrl = desiredConfig.rootUrl ?? client.rootUrl;
  client.baseUrl = desiredConfig.baseUrl ?? client.baseUrl;
  client.attributes = {
    ...(client.attributes ?? {}),
    "pkce.code.challenge.method": "S256",
  };

  await updateClient({ baseUrl, realm, token, client });

  const clientSecret = rotateSecret
    ? await rotateClientSecret({ baseUrl, realm, token, clientUuid: client.id })
    : await getClientSecret({ baseUrl, realm, token, clientUuid: client.id });

  if (syncEnvFilePath) {
    await syncEnvFile({
      envFile: syncEnvFilePath,
      baseUrl,
      realm,
      clientId,
      clientSecret,
      publicUrl,
      audience,
      exactRedirectUri,
    });
  }

  const payload = {
    ok: true,
    realm,
    client_id: clientId,
    client_uuid: client.id,
    public_url: publicUrl,
    redirect_uris: desiredConfig.redirectUris,
    web_origins: desiredConfig.webOrigins,
    review_redirect_included: desiredConfig.redirectUris.includes(
      "https://platform.openai.com/apps-manage/oauth",
    ),
    legacy_redirect_included: desiredConfig.redirectUris.includes(
      "https://chatgpt.com/connector_platform_oauth_redirect",
    ),
    bootstrap_wildcard_included: desiredConfig.redirectUris.includes(
      "https://chatgpt.com/connector/oauth/*",
    ),
    exact_chatgpt_redirect_uri: exactRedirectUri,
    client_secret: clientSecret,
    client_secret_rotated: rotateSecret,
    env_file_synced: syncEnvFilePath,
  };

  if (args.json) {
    process.stdout.write(`${JSON.stringify(payload, null, 2)}\n`);
    return;
  }

  process.stdout.write(
    `configured ${clientId} in realm ${realm} with ${desiredConfig.redirectUris.length} redirect URIs\n`,
  );
  if (exactRedirectUri) {
    process.stdout.write(`exact_chatgpt_redirect_uri=${exactRedirectUri}\n`);
  }
  if (clientSecret) {
    process.stdout.write(`client_secret=${clientSecret}\n`);
  }
}

await main();
