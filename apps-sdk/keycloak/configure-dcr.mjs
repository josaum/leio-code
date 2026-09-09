#!/usr/bin/env node

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    const value = argv[i];
    if (value === "--base-url") args.baseUrl = argv[++i];
    else if (value === "--realm") args.realm = argv[++i];
    else if (value === "--admin-username") args.adminUsername = argv[++i];
    else if (value === "--admin-password") args.adminPassword = argv[++i];
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
    throw new Error(`Keycloak request failed: ${response.status} ${response.statusText} ${text}`);
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

async function listComponents({ baseUrl, realm, token }) {
  const componentsUrl = new URL(`/admin/realms/${realm}/components`, baseUrl);
  const { payload } = await fetchJson(componentsUrl, {
    headers: adminHeaders(token),
  });
  return Array.isArray(payload) ? payload : [];
}

async function deleteComponent({ baseUrl, realm, token, componentId }) {
  const url = new URL(`/admin/realms/${realm}/components/${componentId}`, baseUrl);
  await fetchText(url, {
    method: "DELETE",
    headers: adminHeaders(token),
  });
}

async function listDefaultOptionalScopes({ baseUrl, realm, token }) {
  const url = new URL(`/admin/realms/${realm}/default-optional-client-scopes`, baseUrl);
  const { payload } = await fetchJson(url, {
    headers: adminHeaders(token),
  });
  return Array.isArray(payload) ? payload : [];
}

async function listClientScopes({ baseUrl, realm, token }) {
  const url = new URL(`/admin/realms/${realm}/client-scopes`, baseUrl);
  const { payload } = await fetchJson(url, {
    headers: adminHeaders(token),
  });
  return Array.isArray(payload) ? payload : [];
}

async function addDefaultOptionalScope({ baseUrl, realm, token, clientScopeId }) {
  const url = new URL(
    `/admin/realms/${realm}/default-optional-client-scopes/${clientScopeId}`,
    baseUrl,
  );
  await fetchText(url, {
    method: "PUT",
    headers: adminHeaders(token),
  });
}

const repoReadAudienceMapper = {
  name: "audience-leio-code-apps-sdk",
  protocol: "openid-connect",
  protocolMapper: "oidc-audience-mapper",
  consentRequired: false,
  config: {
    "included.client.audience": "leio-code-apps-sdk",
    "id.token.claim": "false",
    "access.token.claim": "true",
  },
};

const repoReadTenantMapper = {
  name: "tenant-id",
  protocol: "openid-connect",
  protocolMapper: "oidc-usermodel-attribute-mapper",
  consentRequired: false,
  config: {
    "user.attribute": "tenant_id",
    "claim.name": "tenant_id",
    "jsonType.label": "String",
    "id.token.claim": "false",
    "access.token.claim": "true",
    "userinfo.token.claim": "false",
    multivalued: "false",
  },
};

function hasRepoReadAudienceMapper(clientScope) {
  return clientScope.protocolMappers?.some(
    (mapper) =>
      mapper.protocolMapper === "oidc-audience-mapper" &&
      mapper.config?.["included.client.audience"] === "leio-code-apps-sdk" &&
      mapper.config?.["access.token.claim"] === "true",
  );
}

async function ensureRepoReadAudienceMapper({ baseUrl, realm, token, clientScope }) {
  if (hasRepoReadAudienceMapper(clientScope)) {
    return false;
  }
  const url = new URL(
    `/admin/realms/${realm}/client-scopes/${clientScope.id}/protocol-mappers/models`,
    baseUrl,
  );
  await fetchText(url, {
    method: "POST",
    headers: adminHeaders(token),
    body: JSON.stringify(repoReadAudienceMapper),
  });
  return true;
}

function hasRepoReadTenantMapper(clientScope) {
  return clientScope.protocolMappers?.some(
    (mapper) =>
      mapper.protocolMapper === "oidc-usermodel-attribute-mapper" &&
      mapper.config?.["user.attribute"] === "tenant_id" &&
      mapper.config?.["claim.name"] === "tenant_id" &&
      mapper.config?.["access.token.claim"] === "true",
  );
}

async function ensureRepoReadTenantMapper({ baseUrl, realm, token, clientScope }) {
  if (hasRepoReadTenantMapper(clientScope)) {
    return false;
  }
  const url = new URL(
    `/admin/realms/${realm}/client-scopes/${clientScope.id}/protocol-mappers/models`,
    baseUrl,
  );
  await fetchText(url, {
    method: "POST",
    headers: adminHeaders(token),
    body: JSON.stringify(repoReadTenantMapper),
  });
  return true;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const baseUrl = envOr(args.baseUrl, process.env.KEYCLOAK_BASE_URL, "http://localhost:8080");
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

  const token = await getAdminToken({ baseUrl, adminUsername, adminPassword });
  const components = await listComponents({ baseUrl, realm, token });

  const anonymousPolicies = components.filter(
    (component) =>
      component.providerType ===
        "org.keycloak.services.clientregistration.policy.ClientRegistrationPolicy" &&
      component.subType === "anonymous",
  );

  const deletedPolicyNames = [];
  // ChatGPT anonymous DCR fails closed on Trusted Hosts and on Allowed Client
  // Scopes. Updating the allowlist via Admin API still 403s OIDC `scope`
  // requests (openid / repo.read) on this Keycloak build — delete both
  // policies so DCR can register ChatGPT connectors. Consent Required remains.
  for (const providerId of ["trusted-hosts", "allowed-client-templates"]) {
    const policy = anonymousPolicies.find(
      (component) => component.providerId === providerId,
    );
    if (policy) {
      await deleteComponent({
        baseUrl,
        realm,
        token,
        componentId: policy.id,
      });
      deletedPolicyNames.push(policy.name);
    }
  }

  const clientScopes = await listClientScopes({ baseUrl, realm, token });
  const repoReadScope = clientScopes.find((scope) => scope.name === "repo.read");
  if (!repoReadScope?.id) {
    throw new Error("Could not find Keycloak client scope 'repo.read'");
  }
  const repoReadAudienceMapperAdded = await ensureRepoReadAudienceMapper({
    baseUrl,
    realm,
    token,
    clientScope: repoReadScope,
  });
  const repoReadTenantMapperAdded = await ensureRepoReadTenantMapper({
    baseUrl,
    realm,
    token,
    clientScope: repoReadScope,
  });
  let openidScope = clientScopes.find((scope) => scope.name === "openid");
  let openidScopeCreated = false;
  if (!openidScope?.id) {
    const createUrl = new URL(`/admin/realms/${realm}/client-scopes`, baseUrl);
    await fetchText(createUrl, {
      method: "POST",
      headers: adminHeaders(token),
      body: JSON.stringify({
        name: "openid",
        description: "OpenID Connect scope for ChatGPT / OIDC DCR",
        protocol: "openid-connect",
        attributes: {
          "include.in.token.scope": "true",
          "display.on.consent.screen": "false",
        },
      }),
    });
    const refreshed = await listClientScopes({ baseUrl, realm, token });
    openidScope = refreshed.find((scope) => scope.name === "openid");
    openidScopeCreated = true;
  }

  const defaultOptionalScopes = await listDefaultOptionalScopes({ baseUrl, realm, token });
  const optionalNames = new Set(defaultOptionalScopes.map((scope) => scope.name));
  let repoReadScopeAdded = false;
  let openidOptionalAdded = false;

  if (!optionalNames.has("repo.read")) {
    await addDefaultOptionalScope({
      baseUrl,
      realm,
      token,
      clientScopeId: repoReadScope.id,
    });
    repoReadScopeAdded = true;
  }

  if (openidScope?.id && !optionalNames.has("openid")) {
    await addDefaultOptionalScope({
      baseUrl,
      realm,
      token,
      clientScopeId: openidScope.id,
    });
    openidOptionalAdded = true;
  }

  const payload = {
    ok: true,
    realm,
    base_url: baseUrl,
    deleted_policies: deletedPolicyNames,
    updated_policies: [],
    openid_scope_created: openidScopeCreated,
    openid_optional_scope_added: openidOptionalAdded,
    repo_read_optional_scope_added: repoReadScopeAdded,
    repo_read_audience_mapper_added: repoReadAudienceMapperAdded,
    repo_read_tenant_mapper_added: repoReadTenantMapperAdded,
  };

  if (args.json) {
    process.stdout.write(`${JSON.stringify(payload, null, 2)}\n`);
    return;
  }

  process.stdout.write(
    `configured DCR for realm ${realm}: deleted [${deletedPolicyNames.join(", ") || "none"}], openid scope ${openidScopeCreated ? "created" : "present"}, optional scopes repo.read=${repoReadScopeAdded ? "added" : "ok"} openid=${openidOptionalAdded ? "added" : "ok"}, repo.read audience=${repoReadAudienceMapperAdded ? "added" : "ok"}, tenant=${repoReadTenantMapperAdded ? "added" : "ok"}\n`,
  );
}

await main();
