#!/usr/bin/env node

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    const value = argv[i];
    if (value === "--base-url") args.baseUrl = argv[++i];
    else if (value === "--realm") args.realm = argv[++i];
    else if (value === "--admin-username") args.adminUsername = argv[++i];
    else if (value === "--admin-password") args.adminPassword = argv[++i];
    else if (value === "--username") args.username = argv[++i];
    else if (value === "--password") args.password = argv[++i];
    else if (value === "--email") args.email = argv[++i];
    else if (value === "--first-name") args.firstName = argv[++i];
    else if (value === "--last-name") args.lastName = argv[++i];
    else if (value === "--tenant-id") args.tenantId = argv[++i];
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

async function findUser({ baseUrl, realm, token, username }) {
  const searchUrl = new URL(`/admin/realms/${realm}/users`, baseUrl);
  searchUrl.searchParams.set("username", username);
  searchUrl.searchParams.set("exact", "true");
  const { payload } = await fetchJson(searchUrl, {
    headers: adminHeaders(token),
  });
  return Array.isArray(payload) ? payload[0] ?? null : null;
}

async function createUser({
  baseUrl,
  realm,
  token,
  username,
  email,
  tenantId,
  firstName,
  lastName,
}) {
  const createUrl = new URL(`/admin/realms/${realm}/users`, baseUrl);
  await fetchText(createUrl, {
    method: "POST",
    headers: adminHeaders(token),
    body: JSON.stringify({
      username,
      email,
      firstName,
      lastName,
      enabled: true,
      emailVerified: true,
      requiredActions: [],
      attributes: { tenant_id: [tenantId] },
    }),
  });
}

async function updateUser({
  baseUrl,
  realm,
  token,
  userId,
  username,
  email,
  tenantId,
  firstName,
  lastName,
}) {
  const userUrl = new URL(`/admin/realms/${realm}/users/${userId}`, baseUrl);
  await fetchText(userUrl, {
    method: "PUT",
    headers: adminHeaders(token),
    body: JSON.stringify({
      username,
      email,
      firstName,
      lastName,
      enabled: true,
      emailVerified: true,
      requiredActions: [],
      attributes: { tenant_id: [tenantId] },
    }),
  });
}

async function listCredentials({ baseUrl, realm, token, userId }) {
  const credUrl = new URL(
    `/admin/realms/${realm}/users/${userId}/credentials`,
    baseUrl,
  );
  const { payload } = await fetchJson(credUrl, {
    headers: adminHeaders(token),
  });
  return Array.isArray(payload) ? payload : [];
}

async function deleteCredential({ baseUrl, realm, token, userId, credentialId }) {
  const credUrl = new URL(
    `/admin/realms/${realm}/users/${userId}/credentials/${credentialId}`,
    baseUrl,
  );
  await fetchText(credUrl, {
    method: "DELETE",
    headers: adminHeaders(token),
  });
}

async function clearMfaCredentials({ baseUrl, realm, token, userId }) {
  const credentials = await listCredentials({ baseUrl, realm, token, userId });
  for (const credential of credentials) {
    if (credential.type === "otp" || credential.type === "webauthn") {
      await deleteCredential({
        baseUrl,
        realm,
        token,
        userId,
        credentialId: credential.id,
      });
    }
  }
}

async function resetPassword({ baseUrl, realm, token, userId, password }) {
  const resetUrl = new URL(
    `/admin/realms/${realm}/users/${userId}/reset-password`,
    baseUrl,
  );
  await fetchText(resetUrl, {
    method: "PUT",
    headers: adminHeaders(token),
    body: JSON.stringify({
      type: "password",
      value: password,
      temporary: false,
    }),
  });
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const baseUrl = envOr(
    args.baseUrl,
    process.env.KEYCLOAK_BASE_URL,
    "http://localhost:8080",
  );
  const realm = envOr(
    args.realm,
    process.env.KEYCLOAK_REALM,
    "leio-code",
  );
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
  const username = envOr(
    args.username,
    process.env.LEIO_KEYCLOAK_DEV_USERNAME,
    "leio-dev",
  );
  const password = envOr(
    args.password,
    process.env.LEIO_KEYCLOAK_DEV_PASSWORD,
    "change-me-local-dev",
  );
  const email = envOr(
    args.email,
    process.env.LEIO_KEYCLOAK_DEV_EMAIL,
    `${username}@example.test`,
  );
  const tenantId = envOr(
    args.tenantId,
    process.env.LEIO_KEYCLOAK_DEV_TENANT_ID,
    "public-demo",
  );
  const firstName = envOr(
    args.firstName,
    process.env.LEIO_KEYCLOAK_DEV_FIRST_NAME,
    "LEIO",
  );
  const lastName = envOr(
    args.lastName,
    process.env.LEIO_KEYCLOAK_DEV_LAST_NAME,
    "Dev",
  );

  const token = await getAdminToken({
    baseUrl,
    adminUsername,
    adminPassword,
  });

  let user = await findUser({ baseUrl, realm, token, username });
  const created = !user;
  if (!user) {
    await createUser({
      baseUrl,
      realm,
      token,
      username,
      email,
      tenantId,
      firstName,
      lastName,
    });
    user = await findUser({ baseUrl, realm, token, username });
  }

  if (!user?.id) {
    throw new Error(`Failed to resolve Keycloak user after create/update: ${username}`);
  }

  await updateUser({
    baseUrl,
    realm,
    token,
    userId: user.id,
    username,
    email,
    tenantId,
    firstName,
    lastName,
  });
  await clearMfaCredentials({ baseUrl, realm, token, userId: user.id });
  await resetPassword({
    baseUrl,
    realm,
    token,
    userId: user.id,
    password,
  });

  const payload = {
    ok: true,
    created,
    realm,
    username,
    email,
    tenant_id: tenantId,
    user_id: user.id,
    required_actions: [],
    mfa_cleared: true,
  };

  if (args.json) {
    process.stdout.write(`${JSON.stringify(payload, null, 2)}\n`);
    return;
  }

  process.stdout.write(
    `dev user ${created ? "created" : "updated"}: ${username} (${email}) in realm ${realm}\n`,
  );
}

await main();
