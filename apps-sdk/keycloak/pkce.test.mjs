import test from "node:test";
import assert from "node:assert/strict";

import {
  base64UrlEncode,
  buildAuthorizationEndpoint,
  buildPkceAuthorizationUrl,
  buildTokenEndpoint,
  createPkcePair,
  exchangeAuthorizationCode,
} from "./pkce.mjs";

test("base64url encoding matches RFC 7636 example inputs", () => {
  const pair = createPkcePair({ verifierBytes: 32 });
  assert.equal(typeof pair.code_verifier, "string");
  assert.equal(typeof pair.code_challenge, "string");
  assert.equal(pair.code_challenge_method, "S256");
});

test("base64UrlEncode strips padding and unsafe characters", () => {
  assert.equal(base64UrlEncode(Buffer.from([0xfb, 0xef, 0xff])), "--__");
});

test("buildAuthorizationEndpoint and buildTokenEndpoint derive Keycloak paths", () => {
  assert.equal(
    buildAuthorizationEndpoint("http://localhost:8080/realms/leio-code"),
    "http://localhost:8080/realms/leio-code/protocol/openid-connect/auth",
  );
  assert.equal(
    buildTokenEndpoint("http://localhost:8080/realms/leio-code"),
    "http://localhost:8080/realms/leio-code/protocol/openid-connect/token",
  );
});

test("buildPkceAuthorizationUrl includes scopes, resource, and PKCE params", async () => {
  const result = buildPkceAuthorizationUrl({
    issuerUrl: "http://localhost:8080/realms/leio-code",
    clientId: "leio-code-apps-sdk",
    redirectUri: "http://127.0.0.1:3333/callback",
    scopes: ["openid", "repo.read"],
    resource: "http://127.0.0.1:3333/mcp",
    state: "state-123",
    nonce: "nonce-123",
  });

  const url = new URL(result.authorization_url);
  assert.equal(url.origin, "http://localhost:8080");
  assert.equal(url.pathname, "/realms/leio-code/protocol/openid-connect/auth");
  assert.equal(url.searchParams.get("response_type"), "code");
  assert.equal(url.searchParams.get("client_id"), "leio-code-apps-sdk");
  assert.equal(url.searchParams.get("redirect_uri"), "http://127.0.0.1:3333/callback");
  assert.equal(url.searchParams.get("scope"), "openid repo.read");
  assert.equal(url.searchParams.get("resource"), "http://127.0.0.1:3333/mcp");
  assert.equal(url.searchParams.get("state"), "state-123");
  assert.equal(url.searchParams.get("nonce"), "nonce-123");
  assert.equal(url.searchParams.get("code_challenge_method"), "S256");
  assert.equal(typeof result.code_verifier, "string");

  assert.throws(() => createPkcePair({ verifierBytes: 16 }), /verifierBytes/);
  assert.equal(base64UrlEncode("hello"), "aGVsbG8");

  const preChallenge = buildPkceAuthorizationUrl({
    issuerUrl: "http://localhost:8080/realms/leio-code",
    clientId: "leio-code-apps-sdk",
    redirectUri: "http://127.0.0.1:3333/callback",
    codeChallenge: "custom-challenge",
    prompt: "",
  });
  assert.equal(preChallenge.code_verifier, null);
  assert.equal(preChallenge.code_challenge, "custom-challenge");

  const origFetch = globalThis.fetch;
  try {
    globalThis.fetch = async () =>
      new Response(JSON.stringify({ access_token: "tok-1" }), { status: 200 });
    const okRes = await exchangeAuthorizationCode({
      issuerUrl: "http://localhost:8080/realms/leio-code",
      clientId: "client-1",
      clientSecret: "sec-1",
      redirectUri: "http://127.0.0.1:3333/callback",
      code: "code-1",
      codeVerifier: "ver-1",
    });
    assert.equal(okRes.access_token, "tok-1");

    globalThis.fetch = async () =>
      new Response("bad code", { status: 400, statusText: "Bad Request" });
    await assert.rejects(
      () =>
        exchangeAuthorizationCode({
          issuerUrl: "http://localhost:8080/realms/leio-code",
          clientId: "client-1",
          redirectUri: "http://127.0.0.1:3333/callback",
          code: "code-1",
          codeVerifier: "ver-1",
        }),
      /Token exchange failed/,
    );
  } finally {
    globalThis.fetch = origFetch;
  }
});
