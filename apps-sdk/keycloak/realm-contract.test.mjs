import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const realmPath = new URL("./realm/leio-code-realm.json", import.meta.url);

test("keycloak realm supports human login and PKCE", async () => {
  const realm = JSON.parse(await readFile(realmPath, "utf8"));
  const appClient = realm.clients?.find(
    (entry) => entry.clientId === "leio-code-apps-sdk",
  );
  const localDevClient = realm.clients?.find(
    (entry) => entry.clientId === "leio-code-local-dev",
  );
  const repoReadScope = realm.clientScopes?.find(
    (entry) => entry.name === "repo.read",
  );

  assert.equal(realm.realm, "leio-code");
  assert.equal(realm.enabled, true);
  assert.equal(appClient.enabled, true);
  assert.equal(appClient.publicClient, false);
  assert.equal(appClient.standardFlowEnabled, true);
  assert.equal(appClient.implicitFlowEnabled, false);
  assert.equal(appClient.directAccessGrantsEnabled, false);
  assert.equal(appClient.serviceAccountsEnabled, true);
  assert.equal(appClient.attributes?.["pkce.code.challenge.method"], "S256");
  assert.ok(appClient.redirectUris.includes("https://chatgpt.com/connector/oauth/*"));
  assert.ok(
    appClient.redirectUris.includes(
      "https://chatgpt.com/connector_platform_oauth_redirect",
    ),
  );
  assert.ok(
    appClient.redirectUris.includes("https://platform.openai.com/apps-manage/oauth"),
  );
  assert.deepEqual(appClient.defaultClientScopes, ["repo.read"]);
  assert.equal(
    appClient.protocolMappers?.some(
      (mapper) =>
        mapper.protocolMapper === "oidc-audience-mapper" &&
        mapper.config?.["included.client.audience"] === "leio-code-apps-sdk",
    ),
    true,
  );

  assert.equal(localDevClient.enabled, true);
  assert.equal(localDevClient.publicClient, true);
  assert.equal(localDevClient.standardFlowEnabled, true);
  assert.equal(localDevClient.implicitFlowEnabled, false);
  assert.equal(localDevClient.directAccessGrantsEnabled, false);
  assert.equal(localDevClient.serviceAccountsEnabled, false);
  assert.equal(
    localDevClient.attributes?.["pkce.code.challenge.method"],
    "S256",
  );
  assert.ok(localDevClient.redirectUris.includes("http://127.0.0.1:8787/*"));
  assert.ok(localDevClient.redirectUris.includes("http://localhost:8787/*"));
  assert.deepEqual(localDevClient.defaultClientScopes, ["repo.read"]);
  assert.equal(
    localDevClient.protocolMappers?.some(
      (mapper) =>
        mapper.protocolMapper === "oidc-audience-mapper" &&
        mapper.config?.["included.client.audience"] === "leio-code-apps-sdk",
    ),
    true,
  );

  assert.equal(
    repoReadScope.protocolMappers?.some(
      (mapper) =>
        mapper.protocolMapper === "oidc-audience-mapper" &&
        mapper.config?.["included.client.audience"] === "leio-code-apps-sdk",
    ),
    true,
    "repo.read must add the MCP audience to tokens issued to DCR clients",
  );
  assert.equal(
    repoReadScope.protocolMappers?.some(
      (mapper) =>
        mapper.protocolMapper === "oidc-usermodel-attribute-mapper" &&
        mapper.config?.["user.attribute"] === "tenant_id" &&
        mapper.config?.["claim.name"] === "tenant_id" &&
        mapper.config?.["access.token.claim"] === "true",
    ),
    true,
    "repo.read must project the trusted tenant_id user attribute into access tokens",
  );
});
