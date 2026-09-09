import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const cliDockerfile = readFileSync(resolve(__dirname, "../Dockerfile"), "utf8");
const gcpDockerfile = readFileSync(resolve(__dirname, "Dockerfile.gcp"), "utf8");
const serverSource = readFileSync(resolve(__dirname, "server.js"), "utf8");
const envExample = readFileSync(resolve(__dirname, ".env.example"), "utf8");
const deployFlySource = readFileSync(
  resolve(__dirname, "scripts/deploy-fly.sh"),
  "utf8",
);

function requireMatch(source, pattern, label) {
  const match = source.match(pattern);
  assert.ok(match, `${label} must declare an explicit Debian suite`);
  return match[1];
}

test("GCP runtime matches the published CLI image Debian suite", () => {
  const cliSuite = requireMatch(
    cliDockerfile,
    /^FROM debian:([a-z]+)-slim AS runtime$/m,
    "leio-code CLI runtime",
  );
  const appsSuite = requireMatch(
    gcpDockerfile,
    /^FROM node:\d+-([a-z]+)-slim AS runtime$/m,
    "Apps SDK GCP runtime",
  );

  assert.equal(
    appsSuite,
    cliSuite,
    "a CLI binary copied from jquant/leio-code must run on the same Debian ABI suite",
  );
});

test("GCP image verifies the copied CLI during the build", () => {
  assert.match(
    gcpDockerfile,
    /^RUN \/usr\/local\/bin\/leio-code --help >\/dev\/null$/m,
    "the fast image must fail its build immediately when the copied CLI is ABI-incompatible",
  );
});

test("hosted CLI invocations receive request-scoped tenant env", () => {
  assert.match(serverSource, /buildTenantScopeEnv\(\{/);
  assert.match(serverSource, /authInfo:\s*options\.authInfo/);
  assert.match(serverSource, /runProcess\([\s\S]*\{ env: tenantScopeEnv \}/);
});

test("repository memory search uses local find with request auth context", () => {
  assert.match(
    serverSource,
    /server\.registerTool\(\s*"search_repository_memory"/,
  );
  assert.match(
    serverSource,
    /invokeLeioTool\(\s*\[\s*"find",\s*"symbol",\s*query,\s*"--limit",\s*String\(limit \?\? 8\),?\s*\],\s*\{[\s\S]*?authInfo:\s*extra\?\.authInfo \?\? null/,
  );
});

test("tenant scoping is documented in the env example", () => {
  assert.match(envExample, /tenant scoping/i);
  assert.match(envExample, /never commit the value/i);
});

test("Apps SDK resolves leio-code through the shared cargo-bin helper", () => {
  assert.match(serverSource, /from ["']\.\.\/mcp\/resolve-binary\.js["']/);
  assert.doesNotMatch(serverSource, /target["'],\s*["']release["']/);
});

test("hosted callers cannot select arbitrary absolute repo_root paths", () => {
  assert.match(serverSource, /LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT/);
  assert.match(serverSource, /repo_root is disabled on the hosted LEIO service/);
  assert.match(serverSource, /function isLoopbackHost/);
  assert.match(
    serverSource,
    /parseBoolEnv\(\s*"LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT",\s*isLoopbackHost\(host\),\s*\)/,
  );
  assert.match(serverSource, /function resolveConfiguredRepoRoot/);
});

test("authenticated Fly deploys require and stage tenant scope configuration", () => {
  assert.match(
    deployFlySource,
    /AUTH_MODE" != "none"[\s\S]*Missing required env[^\n]*LEIO_APPS_SDK_TENANT_HMAC_SECRET[\s\S]*fly secrets set LEIO_APPS_SDK_TENANT_HMAC_SECRET/,
  );
  assert.match(
    deployFlySource,
    /printf '%s' "\$TENANT_HMAC_SECRET" \| LC_ALL=C wc -c/,
  );
  assert.match(deployFlySource, /TENANT_HMAC_SECRET_BYTES -lt 32/);
  assert.match(
    deployFlySource,
    /oauth-jwt[\s\S]*fly secrets set LEIO_APPS_SDK_TENANT_CLAIM=/,
  );
  assert.match(
    deployFlySource,
    /static-bearer[\s\S]*fly secrets set[\s\S]*LEIO_APPS_SDK_STATIC_TENANT_ID=/,
  );
  assert.match(
    deployFlySource,
    /echo "LEIO_APPS_SDK_TENANT_HMAC_SECRET=\$\{LEIO_APPS_SDK_TENANT_HMAC_SECRET\}"/,
  );
});
