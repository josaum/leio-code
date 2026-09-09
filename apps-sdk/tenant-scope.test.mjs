import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import test from "node:test";

import { buildTenantScopeEnv, deriveTenantKey } from "./tenant-scope.mjs";

const secret = "0123456789abcdef0123456789abcdef";

test("tenant keys are opaque, stable, and tenant-distinct", () => {
  const tenantA = deriveTenantKey("acme", secret);
  const tenantAAgain = deriveTenantKey("acme", secret);
  const tenantB = deriveTenantKey("globex", secret);

  assert.equal(tenantA, tenantAAgain);
  assert.notEqual(tenantA, tenantB);
  assert.match(tenantA, /^t_[0-9a-f]{32}$/);

});

test("request scope binds tenant, repository, revision, and tenant collections", () => {
  const env = buildTenantScopeEnv({
    authInfo: {
      extra: {
        tenantId: "acme",
        sub: "user-123",
        iss: "https://issuer.example/realms/leio",
      },
    },
    repoTarget: {
      repoRoot: "/workspace/repos/checkout-42",
      repoUrl: "https://github.com/example/repository.git",
      revision: "abc123",
    },
    hmacSecret: secret,
  });

  assert.match(env.LEIO_CODE_TENANT_ID, /^t_[0-9a-f]{32}$/);
  assert.match(env.LEIO_CODE_REPO, /^r_[0-9a-f]{32}$/);
  assert.equal(env.LEIO_CODE_REV, "abc123");
});

test("personal tenant fallback is derived from trusted issuer and subject", () => {
  const first = buildTenantScopeEnv({
    authInfo: { extra: { sub: "user-123", iss: "https://issuer.example" } },
    repoTarget: { repoRoot: "/workspace/baked-repo", revision: "local" },
    hmacSecret: secret,
  });
  const second = buildTenantScopeEnv({
    authInfo: { extra: { sub: "user-456", iss: "https://issuer.example" } },
    repoTarget: { repoRoot: "/workspace/baked-repo", revision: "local" },
    hmacSecret: secret,
  });

  assert.notEqual(first.LEIO_CODE_TENANT_ID, second.LEIO_CODE_TENANT_ID);
  assert.notEqual(first.LEIO_CODE_TENANT_ID, second.LEIO_CODE_TENANT_ID);
});

test("authenticated scopes fail closed without a tenant HMAC secret", () => {
  assert.throws(
    () => buildTenantScopeEnv({
      authInfo: { extra: { tenantId: "acme" } },
      repoTarget: { repoRoot: "/workspace/baked-repo", revision: "local" },
      hmacSecret: "short",
    }),
    /HMAC secret/i,
  );
});

test("anonymous tools do not receive a persistent tenant scope", () => {
  assert.deepEqual(buildTenantScopeEnv({
    authInfo: null,
    repoTarget: { repoRoot: "/workspace/baked-repo", revision: "local" },
    hmacSecret: secret,
  }), {});
});

test("local repositories with the same basename keep distinct memory scopes", () => {
  const authInfo = { extra: { tenantId: "acme" } };
  const first = buildTenantScopeEnv({
    authInfo,
    repoTarget: { repoRoot: "/srv/checkout-a/repository", revision: "abc123" },
    hmacSecret: secret,
  });
  const second = buildTenantScopeEnv({
    authInfo,
    repoTarget: { repoRoot: "/opt/checkout-b/repository", revision: "abc123" },
    hmacSecret: secret,
  });

  assert.notEqual(first.LEIO_CODE_REPO, second.LEIO_CODE_REPO);
});

test("git remote identity is stable across checkout paths", () => {
  const authInfo = { extra: { tenantId: "acme" } };
  const first = buildTenantScopeEnv({
    authInfo,
    repoTarget: {
      repoRoot: "/srv/checkout-a/one",
      gitRemote: "https://github.com/example/repository.git",
      revision: "abc123",
    },
    hmacSecret: secret,
  });
  const second = buildTenantScopeEnv({
    authInfo,
    repoTarget: {
      repoRoot: "/opt/checkout-b/two",
      gitRemote: "https://github.com/example/repository.git",
      revision: "abc123",
    },
    hmacSecret: secret,
  });

  assert.equal(first.LEIO_CODE_REPO, second.LEIO_CODE_REPO);
});
