import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(new URL("./configure-dcr.mjs", import.meta.url));

function runConfigureDcr(env) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [scriptPath, "--json"], {
      env: { ...process.env, ...env },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.once("error", reject);
    child.once("close", (code) => resolve({ code, stdout, stderr }));
  });
}

test("configure-dcr adds audience and tenant mappers to repo.read", async (t) => {
  const updates = [];
  const repoReadScope = {
    id: "repo-read-scope",
    name: "repo.read",
    protocol: "openid-connect",
    attributes: {
      "include.in.token.scope": "true",
      "display.on.consent.screen": "true",
    },
  };
  const openidScope = {
    id: "openid-scope",
    name: "openid",
    protocol: "openid-connect",
  };

  const server = createServer(async (request, response) => {
    const url = new URL(request.url, "http://127.0.0.1");
    const sendJson = (status, payload) => {
      response.writeHead(status, { "content-type": "application/json" });
      response.end(JSON.stringify(payload));
    };

    if (
      request.method === "POST" &&
      url.pathname === "/realms/master/protocol/openid-connect/token"
    ) {
      sendJson(200, { access_token: "test-admin-token" });
      return;
    }
    if (request.method === "GET" && url.pathname === "/admin/realms/leio/components") {
      sendJson(200, []);
      return;
    }
    if (request.method === "GET" && url.pathname === "/admin/realms/leio/client-scopes") {
      sendJson(200, [repoReadScope, openidScope]);
      return;
    }
    if (
      request.method === "GET" &&
      url.pathname === "/admin/realms/leio/default-optional-client-scopes"
    ) {
      sendJson(200, [repoReadScope, openidScope]);
      return;
    }
    if (
      request.method === "POST" &&
      url.pathname ===
        "/admin/realms/leio/client-scopes/repo-read-scope/protocol-mappers/models"
    ) {
      let body = "";
      for await (const chunk of request) body += chunk;
      updates.push(JSON.parse(body));
      response.writeHead(204);
      response.end();
      return;
    }

    sendJson(404, { error: `unexpected ${request.method} ${url.pathname}` });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => server.close());

  const address = server.address();
  const result = await runConfigureDcr({
    KEYCLOAK_BASE_URL: `http://127.0.0.1:${address.port}`,
    KEYCLOAK_REALM: "leio",
    KEYCLOAK_BOOTSTRAP_ADMIN_USERNAME: "admin",
    KEYCLOAK_BOOTSTRAP_ADMIN_PASSWORD: "test-password",
  });

  assert.equal(result.code, 0, result.stderr);
  assert.equal(updates.length, 2);
  const audienceMapper = updates.find(
    (mapper) => mapper.protocolMapper === "oidc-audience-mapper",
  );
  const tenantMapper = updates.find(
    (mapper) => mapper.protocolMapper === "oidc-usermodel-attribute-mapper",
  );
  assert.ok(audienceMapper);
  assert.equal(
    audienceMapper.config?.["included.client.audience"],
    "leio-code-apps-sdk",
  );
  assert.equal(audienceMapper.config?.["access.token.claim"], "true");
  assert.ok(tenantMapper);
  assert.equal(tenantMapper.config?.["user.attribute"], "tenant_id");
  assert.equal(tenantMapper.config?.["claim.name"], "tenant_id");
  assert.equal(tenantMapper.config?.["access.token.claim"], "true");
});
