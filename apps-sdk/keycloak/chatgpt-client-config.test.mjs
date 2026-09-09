import test from "node:test";
import assert from "node:assert/strict";

import {
  buildDesiredAppsClientConfig,
  deriveWebOrigins,
  normalizeRedirectUris,
  updateEnvFileContent,
} from "./chatgpt-client-config.mjs";

test("normalizeRedirectUris includes ChatGPT defaults and exact callback", () => {
  const redirects = normalizeRedirectUris({
    exactRedirectUri: "https://chatgpt.com/connector/oauth/callback-123",
    extraRedirectUris: ["https://example.com/oauth/callback"],
  });

  assert.deepEqual(redirects, [
    "https://chatgpt.com/connector/oauth/*",
    "https://chatgpt.com/connector/oauth/callback-123",
    "https://chatgpt.com/connector_platform_oauth_redirect",
    "https://platform.openai.com/apps-manage/oauth",
    "https://example.com/oauth/callback",
  ]);
});

test("deriveWebOrigins reduces redirects to unique origins", () => {
  const origins = deriveWebOrigins({
    redirectUris: [
      "https://chatgpt.com/connector/oauth/*",
      "https://chatgpt.com/connector/oauth/callback-123",
      "https://platform.openai.com/apps-manage/oauth",
    ],
    extraWebOrigins: ["https://leio-code.example.com"],
  });

  assert.deepEqual(origins, [
    "https://chatgpt.com",
    "https://platform.openai.com",
    "https://leio-code.example.com",
  ]);
});

test("buildDesiredAppsClientConfig keeps public URL separate from redirect origins", () => {
  const config = buildDesiredAppsClientConfig({
    publicUrl: "https://leio-code.example.com",
    exactRedirectUri: "https://chatgpt.com/connector/oauth/callback-123",
  });

  assert.equal(config.rootUrl, "https://leio-code.example.com");
  assert.equal(config.baseUrl, "https://leio-code.example.com");
  assert.ok(
    config.redirectUris.includes(
      "https://chatgpt.com/connector/oauth/callback-123",
    ),
  );
  assert.ok(config.webOrigins.includes("https://chatgpt.com"));
});

test("updateEnvFileContent replaces tracked keys and appends missing ones", () => {
  const original = [
    "KEYCLOAK_BASE_URL=https://old.example.com",
    "SOME_OTHER_KEY=value",
    "",
  ].join("\n");

  const updated = updateEnvFileContent(original, {
    KEYCLOAK_BASE_URL: "https://new.example.com",
    LEIO_APPS_SDK_AUTH_MODE: "oauth-jwt",
  });

  assert.equal(
    updated,
    [
      "KEYCLOAK_BASE_URL=https://new.example.com",
      "SOME_OTHER_KEY=value",
      "",
      "LEIO_APPS_SDK_AUTH_MODE=oauth-jwt",
      "",
    ].join("\n"),
  );
});
