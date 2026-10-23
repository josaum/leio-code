import test from "node:test";
import assert from "node:assert/strict";

import { createAuthRuntime } from "./auth.js";

const snapshot = { ...process.env };

function restoreEnv() {
  for (const key of Object.keys(process.env)) {
    if (!(key in snapshot)) {
      delete process.env[key];
    }
  }
  for (const [key, value] of Object.entries(snapshot)) {
    process.env[key] = value;
  }
}

function withEnv(patch, fn) {
  restoreEnv();
  for (const [key, value] of Object.entries(patch)) {
    if (value == null) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
  let result;
  try {
    result = fn();
  } catch (error) {
    restoreEnv();
    throw error;
  }
  if (result && typeof result.then === "function") {
    return result.finally(() => {
      restoreEnv();
    });
  }
  restoreEnv();
  return result;
}

function makeResponseRecorder() {
  return {
    headers: new Map(),
    statusCode: null,
    body: null,
    set(name, value) {
      this.headers.set(name, value);
      return this;
    },
    status(code) {
      this.statusCode = code;
      return this;
    },
    json(payload) {
      this.body = payload;
      return this;
    },
  };
}

test("none mode keeps all tools noauth", () => {
  withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "none",
      LEIO_APPS_SDK_PUBLIC_URL: "https://leio-code.example.com",
    },
    () => {
      const auth = createAuthRuntime({ host: "127.0.0.1", port: 3333 });
      assert.equal(auth.enabled, false);
      assert.deepEqual(auth.getSecuritySchemes("search_repository"), [
        { type: "noauth" },
      ]);
    },
  );
});

test("repository memory fails closed when auth is disabled", () => {
  withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "none",
      LEIO_APPS_SDK_PUBLIC_URL: "https://leio-code.example.com",
    },
    () => {
      const auth = createAuthRuntime({ host: "127.0.0.1", port: 3333 });
      assert.equal(
        auth.ensureToolAccess("search_repository_memory", {}).isError,
        true,
      );
      assert.equal(auth.ensureToolAccess("search_repository", {}), null);
    },
  );
});

test("repository memory remains protected when a legacy tool list omits it", () => {
  withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "oauth-jwt",
      LEIO_APPS_SDK_PUBLIC_URL: "https://leio-code.example.com",
      LEIO_APPS_SDK_AUTHORIZATION_SERVERS: "https://auth.example.com",
      LEIO_APPS_SDK_JWT_ISSUER: "https://auth.example.com",
      LEIO_APPS_SDK_JWT_AUDIENCE: "leio-code-apps-sdk",
      LEIO_APPS_SDK_JWKS_URL: "https://auth.example.com/.well-known/jwks.json",
      LEIO_APPS_SDK_PROTECTED_TOOLS: "audit_repository_contracts",
    },
    () => {
      const auth = createAuthRuntime({ host: "127.0.0.1", port: 3333 });
      assert.ok(
        auth.summary().protected_tools.includes("search_repository_memory"),
      );
      assert.deepEqual(auth.getSecuritySchemes("search_repository_memory"), [
        { type: "oauth2", scopes: ["repo.read"] },
      ]);
      assert.equal(
        auth.ensureToolAccess("search_repository_memory", {}).isError,
        true,
      );
    },
  );
});

test("oauth-jwt mode advertises oauth for public and protected tools", () => {
  withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "oauth-jwt",
      LEIO_APPS_SDK_PUBLIC_URL: "https://leio-code.example.com",
      LEIO_APPS_SDK_AUTHORIZATION_SERVERS: "https://auth.example.com",
      LEIO_APPS_SDK_JWT_ISSUER: "https://auth.example.com",
      LEIO_APPS_SDK_JWT_AUDIENCE: "leio-code-apps-sdk",
      LEIO_APPS_SDK_TENANT_CLAIM: "tenant_id",
      LEIO_APPS_SDK_JWKS_URL: "https://auth.example.com/.well-known/jwks.json",
      LEIO_APPS_SDK_AUTH_SCOPES: "repo.read",
    },
    () => {
      const auth = createAuthRuntime({ host: "127.0.0.1", port: 3333 });
      assert.equal(auth.oauthUiEnabled, true);
      assert.deepEqual(auth.getSecuritySchemes("search_repository"), [
        { type: "noauth" },
        { type: "oauth2", scopes: ["repo.read"] },
      ]);
      assert.deepEqual(auth.getSecuritySchemes("audit_repository_contracts"), [
        { type: "oauth2", scopes: ["repo.read"] },
      ]);
      assert.deepEqual(auth.getSecuritySchemes("audit_repository_rollup"), [
        { type: "oauth2", scopes: ["repo.read"] },
      ]);
      assert.deepEqual(auth.getSecuritySchemes("search_repository_memory"), [
        { type: "oauth2", scopes: ["repo.read"] },
      ]);
      assert.ok(
        auth.summary().protected_tools.includes("search_repository_memory"),
      );
      assert.equal(
        auth.resourceMetadataPath,
        "/.well-known/oauth-protected-resource/mcp",
      );
    },
  );
});

test("static-bearer mode does not advertise oauth UI metadata", () => {
  withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "static-bearer",
      LEIO_APPS_SDK_STATIC_BEARER_TOKENS: "secret-token",
      LEIO_APPS_SDK_PUBLIC_URL: "https://leio-code.example.com",
    },
    () => {
      const auth = createAuthRuntime({ host: "127.0.0.1", port: 3333 });
      assert.equal(auth.enabled, true);
      assert.equal(auth.oauthUiEnabled, false);
      assert.deepEqual(auth.getSecuritySchemes("audit_repository_contracts"), [
        { type: "noauth" },
      ]);
      const toolError = auth.ensureToolAccess("audit_repository_contracts", {});
      assert.equal(toolError.isError, true);
      assert.equal(toolError._meta, undefined);
    },
  );
});

test("static-bearer mode attaches a stable tenant scope", async () => {
  await withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "static-bearer",
      LEIO_APPS_SDK_STATIC_BEARER_TOKENS: "secret-token",
      LEIO_APPS_SDK_STATIC_TENANT_ID: "ops-tenant",
      LEIO_APPS_SDK_PUBLIC_URL: "https://leio-code.example.com",
    },
    async () => {
      const auth = createAuthRuntime({ host: "127.0.0.1", port: 3333 });
      const request = {
        headers: { authorization: "Bearer secret-token" },
        auth: null,
      };
      const response = makeResponseRecorder();
      assert.equal(await auth.maybeAttachAuthInfo(request, response), true);
      assert.equal(request.auth.extra.tenantId, "ops-tenant");
      assert.equal(request.auth.extra.iss, "leio:static-bearer");
    },
  );
});

test("oauth-jwt mode accepts localhost issuer/public URL for local dev", () => {
  withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "oauth-jwt",
      LEIO_APPS_SDK_PUBLIC_URL: "http://localhost:3333",
      LEIO_APPS_SDK_AUTHORIZATION_SERVERS: "http://localhost:8080/realms/leio-code",
      LEIO_APPS_SDK_JWT_ISSUER: "http://localhost:8080/realms/leio-code",
      LEIO_APPS_SDK_JWT_AUDIENCE: "leio-code-apps-sdk",
    },
    () => {
      const auth = createAuthRuntime({ host: "127.0.0.1", port: 3333 });
      assert.equal(auth.oauthUiEnabled, true);
      assert.equal(
        auth.summary().oidc_discovery_url,
        "http://localhost:8080/realms/leio-code/.well-known/openid-configuration",
      );
    },
  );
});

test("oauth-jwt mode rejects insecure non-local issuer/public URL", () => {
  withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "oauth-jwt",
      LEIO_APPS_SDK_PUBLIC_URL: "http://leio-code.example.com",
      LEIO_APPS_SDK_AUTHORIZATION_SERVERS: "http://auth.example.com",
      LEIO_APPS_SDK_JWT_ISSUER: "http://auth.example.com",
      LEIO_APPS_SDK_JWT_AUDIENCE: "leio-code-apps-sdk",
    },
    () => {
      assert.throws(
        () => createAuthRuntime({ host: "127.0.0.1", port: 3333 }),
        /must use HTTPS unless it targets localhost/,
      );
    },
  );
});

test("oauth-jwt mode retries discovery after a transient failure instead of poisoning the session", async () => {
  await withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "oauth-jwt",
      LEIO_APPS_SDK_PUBLIC_URL: "https://leio-code.example.com",
      LEIO_APPS_SDK_AUTHORIZATION_SERVERS: "https://auth.example.com",
      LEIO_APPS_SDK_JWT_ISSUER: "https://auth.example.com",
      LEIO_APPS_SDK_JWT_AUDIENCE: "leio-code-apps-sdk",
    },
    async () => {
      let discoveryCalls = 0;
      const auth = createAuthRuntime(
        { host: "127.0.0.1", port: 3333 },
        {
          fetchImpl: async () => {
            discoveryCalls += 1;
            if (discoveryCalls === 1) {
              throw new Error("fetch failed");
            }
            return new Response(
              JSON.stringify({
                jwks_uri: "https://auth.example.com/.well-known/jwks.json",
              }),
              {
                status: 200,
                headers: { "content-type": "application/json" },
              },
            );
          },
          createRemoteJWKSetImpl: () => Symbol("jwks"),
          jwtVerifyImpl: async () => ({
            payload: {
              sub: "user-123",
              scope: "repo.read",
              aud: "leio-code-apps-sdk",
              iss: "https://auth.example.com",
              tenant_id: "acme",
            },
          }),
        },
      );

      const req = { headers: { authorization: "Bearer test-token" } };
      const firstRes = makeResponseRecorder();
      const firstResult = await auth.maybeAttachAuthInfo(req, firstRes);
      assert.equal(firstResult, false);
      assert.equal(firstRes.statusCode, 503);
      assert.equal(firstRes.body.error, "temporarily_unavailable");

      const secondRes = makeResponseRecorder();
      const secondResult = await auth.maybeAttachAuthInfo(req, secondRes);
      assert.equal(secondResult, true);
      assert.equal(secondRes.statusCode, null);
      assert.equal(discoveryCalls, 2);
      assert.equal(req.auth?.clientId, "user-123");
      assert.equal(req.auth?.extra?.tenantId, "acme");
    },
  );
});

test("oauth-jwt mode reports transient verifier failures as temporarily unavailable", async () => {
  await withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "oauth-jwt",
      LEIO_APPS_SDK_PUBLIC_URL: "https://leio-code.example.com",
      LEIO_APPS_SDK_AUTHORIZATION_SERVERS: "https://auth.example.com",
      LEIO_APPS_SDK_JWT_ISSUER: "https://auth.example.com",
      LEIO_APPS_SDK_JWT_AUDIENCE: "leio-code-apps-sdk",
      LEIO_APPS_SDK_JWKS_URL: "https://auth.example.com/.well-known/jwks.json",
    },
    async () => {
      const auth = createAuthRuntime(
        { host: "127.0.0.1", port: 3333 },
        {
          createRemoteJWKSetImpl: () => Symbol("jwks"),
          jwtVerifyImpl: async () => {
            const error = new Error("failed to fetch remote JWK Set");
            error.code = "ETIMEDOUT";
            throw error;
          },
        },
      );

      const req = { headers: { authorization: "Bearer test-token" } };
      const res = makeResponseRecorder();
      const allowed = await auth.maybeAttachAuthInfo(req, res);

      assert.equal(allowed, false);
      assert.equal(res.statusCode, 503);
      assert.equal(res.body.error, "temporarily_unavailable");
      assert.match(
        res.headers.get("WWW-Authenticate"),
        /temporarily_unavailable/,
      );
    },
  );
});

test("auth runtime covers validation errors, scopes, metadata, and header edge cases", async () => {
  assert.throws(
    () => createAuthRuntime({ host: "127.0.0.1", port: 3333 }, { fetchImpl: null }),
    /global fetch is required/,
  );

  withEnv({ LEIO_APPS_SDK_AUTH_MODE: "static-bearer" }, () => {
    assert.throws(
      () => createAuthRuntime({ host: "127.0.0.1", port: 3333 }),
      /LEIO_APPS_SDK_STATIC_BEARER_TOKENS is required/,
    );
  });

  withEnv({ LEIO_APPS_SDK_AUTH_MODE: "oauth-jwt" }, () => {
    assert.throws(
      () => createAuthRuntime({ host: "127.0.0.1", port: 3333 }),
      /LEIO_APPS_SDK_JWT_ISSUER and LEIO_APPS_SDK_JWT_AUDIENCE are required/,
    );
  });

  await withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "static-bearer",
      LEIO_APPS_SDK_STATIC_BEARER_TOKENS: "secret-token",
      LEIO_APPS_SDK_REQUIRE_AUTH_ON_CONNECT: "true",
      LEIO_APPS_SDK_RESOURCE_DOCUMENTATION_URL: "not-a-valid-url",
    },
    async () => {
      const auth = createAuthRuntime({ host: "127.0.0.1", port: 3333 });
      assert.equal(auth.requireAuthOnConnect, true);
      assert.equal(auth.metadataDocument().resource_documentation, undefined);

      const resMissing = makeResponseRecorder();
      assert.equal(await auth.maybeAttachAuthInfo({ headers: {} }, resMissing), false);
      assert.equal(resMissing.statusCode, 401);

      const resMalformed = makeResponseRecorder();
      assert.equal(
        await auth.maybeAttachAuthInfo({ headers: { authorization: "Basic abc" } }, resMalformed),
        false,
      );
      assert.equal(resMalformed.statusCode, 401);

      const resBadToken = makeResponseRecorder();
      assert.equal(
        await auth.maybeAttachAuthInfo(
          { headers: { authorization: "Bearer wrong-token" } },
          resBadToken,
        ),
        false,
      );
      assert.equal(resBadToken.statusCode, 401);
    },
  );

  await withEnv(
    {
      LEIO_APPS_SDK_AUTH_MODE: "oauth-jwt",
      LEIO_APPS_SDK_PUBLIC_URL: "https://leio-code.example.com",
      LEIO_APPS_SDK_JWT_ISSUER: "https://auth.example.com",
      LEIO_APPS_SDK_JWT_AUDIENCE: "leio-code-apps-sdk",
      LEIO_APPS_SDK_OIDC_DISCOVERY_URL: "https://auth.example.com/.well-known/openid-configuration",
      LEIO_APPS_SDK_REQUIRE_AUTH_ON_CONNECT: "false",
      LEIO_APPS_SDK_AUDIT_SCOPES: "repo.read repo.audit",
    },
    async () => {
      let mode = "http-500";
      const auth = createAuthRuntime(
        { host: "127.0.0.1", port: 3333 },
        {
          fetchImpl: async () => {
            if (mode === "http-500") {
              return new Response("down", { status: 502, statusText: "Bad Gateway" });
            }
            if (mode === "no-jwks-uri") {
              return new Response(JSON.stringify({}), { status: 200 });
            }
            return new Response(
              JSON.stringify({ jwks_uri: "https://auth.example.com/jwks.json" }),
              { status: 200 },
            );
          },
          createRemoteJWKSetImpl: () => Symbol("jwks"),
          jwtVerifyImpl: async (token) => {
            if (token === "expired") {
              throw new Error("jwt expired");
            }
            if (token === "wrong-resource") {
              return {
                payload: {
                  azp: "client-azp",
                  scp: ["repo.read"],
                  resource: ["https://other.example.com/mcp"],
                },
              };
            }
            return {
              payload: {
                client_id: "client-id-1",
                scp: ["repo.read"],
                exp: 9999999999,
                resource: "https://leio-code.example.com/mcp",
              },
            };
          },
        },
      );

      // No auth header when requireAuthOnConnect=false succeeds
      assert.equal(await auth.maybeAttachAuthInfo({ headers: {} }, makeResponseRecorder()), true);

      // 502 from discovery
      const res502 = makeResponseRecorder();
      assert.equal(
        await auth.maybeAttachAuthInfo({ headers: { authorization: "Bearer ok" } }, res502),
        false,
      );
      assert.equal(res502.statusCode, 503);

      // Missing jwks_uri from discovery
      mode = "no-jwks-uri";
      const resNoJwks = makeResponseRecorder();
      assert.equal(
        await auth.maybeAttachAuthInfo({ headers: { authorization: "Bearer ok" } }, resNoJwks),
        false,
      );
      assert.equal(resNoJwks.statusCode, 503);

      // Valid discovery, expired token -> 401
      mode = "ok";
      const resExpired = makeResponseRecorder();
      assert.equal(
        await auth.maybeAttachAuthInfo({ headers: { authorization: "Bearer expired" } }, resExpired),
        false,
      );
      assert.equal(resExpired.statusCode, 401);

      // Resource claim mismatch -> 401
      const resWrongRes = makeResponseRecorder();
      assert.equal(
        await auth.maybeAttachAuthInfo(
          { headers: { authorization: "Bearer wrong-resource" } },
          resWrongRes,
        ),
        false,
      );
      assert.equal(resWrongRes.statusCode, 401);

      // Valid token with matching resource claim
      const reqOk = { headers: { authorization: "Bearer valid" } };
      assert.equal(await auth.maybeAttachAuthInfo(reqOk, makeResponseRecorder()), true);
      assert.equal(reqOk.auth.clientId, "client-id-1");

      // Scope check on audit_repository_contracts (needs repo.audit)
      const insufficient = auth.ensureToolAccess("audit_repository_contracts", {
        authInfo: reqOk.auth,
      });
      assert.equal(insufficient.isError, true);
      assert.match(insufficient._meta["mcp/www_authenticate"][0], /insufficient_scope/);

      const sufficient = auth.ensureToolAccess("audit_repository_contracts", {
        authInfo: { scopes: ["repo.read", "repo.audit"] },
      });
      assert.equal(sufficient, null);
    },
  );
});
