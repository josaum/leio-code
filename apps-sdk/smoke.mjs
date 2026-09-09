#!/usr/bin/env node

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

const baseUrl = process.env.LEIO_APPS_SDK_SMOKE_BASE_URL ?? "http://127.0.0.1:3333";
const repoRoot = process.env.LEIO_APPS_SDK_SMOKE_REPO_ROOT ?? process.cwd();
const repoUrl = process.env.LEIO_APPS_SDK_SMOKE_REPO_URL?.trim() || null;
const expectedAuthMode = process.env.LEIO_APPS_SDK_SMOKE_EXPECT_AUTH_MODE ?? null;
const bearerToken =
  process.env.LEIO_APPS_SDK_SMOKE_BEARER_TOKEN?.trim() || null;
const mcpUrl = new URL("/mcp", baseUrl);

function print(label, value) {
  process.stdout.write(`${label}: ${value}\n`);
}

async function fetchJson(url) {
  const response = await fetch(url, {
    headers: { accept: "application/json" },
  });
  const body = await response.text();
  return {
    ok: response.ok,
    status: response.status,
    json: body ? JSON.parse(body) : null,
  };
}

const health = await fetchJson(new URL("/health", baseUrl));
if (!health.ok) {
  throw new Error(`Health check failed: ${health.status}`);
}
print("health", "ok");
print("auth.mode", health.json?.auth?.mode ?? "unknown");

if (expectedAuthMode && health.json?.auth?.mode !== expectedAuthMode) {
  throw new Error(
    `Expected auth mode ${expectedAuthMode} but got ${health.json?.auth?.mode}`,
  );
}

if (health.json?.auth?.oauth_ui_enabled === true) {
  const metadataUrlRaw = health.json?.auth?.resource_metadata_url;
  if (!metadataUrlRaw) {
    throw new Error("oauth_ui_enabled=true but resource_metadata_url is missing");
  }
  const canonicalMetadataUrl = new URL(metadataUrlRaw);
  const localMetadataUrl =
    canonicalMetadataUrl.origin === new URL(baseUrl).origin
      ? canonicalMetadataUrl
      : new URL(canonicalMetadataUrl.pathname, baseUrl);
  const metadata = await fetchJson(localMetadataUrl);
  if (!metadata.ok) {
    throw new Error(
      `Protected resource metadata failed: ${metadata.status} ${localMetadataUrl.href}`,
    );
  }
  print("resource_metadata", canonicalMetadataUrl.href);
}

const transport = new StreamableHTTPClientTransport(mcpUrl, {
  requestInit: bearerToken
    ? {
        headers: {
          authorization: `Bearer ${bearerToken}`,
        },
      }
    : undefined,
});
const client = new Client({ name: "leio-code-apps-sdk-smoke", version: "0.1.0" });
await client.connect(transport);

let capabilities;
if (repoUrl) {
  const selected = await client.callTool({
    name: "select_repository_target",
    arguments: { repo_url: repoUrl },
  });
  if (selected?.isError) {
    throw new Error(
      `select_repository_target failed: ${selected?.content?.[0]?.text ?? "unknown error"}`,
    );
  }
  print("selected.repo_url", selected?.structuredContent?.repo_url ?? repoUrl);
  capabilities = await client.callTool({
    name: "inspect_repository_capabilities",
    arguments: {},
  });
} else {
  capabilities = await client.callTool({
    name: "inspect_repository_capabilities",
    arguments: { repo_root: repoRoot },
  });
}

print(
  "capabilities.profile",
  capabilities.structuredContent?.workspace_profile ?? "unknown",
);
print(
  "capabilities.start_tool",
  capabilities.structuredContent?.action_palette?.recommended_start_tool ?? "unknown",
);
print(
  "capabilities.repo_mode",
  capabilities.structuredContent?.repo_root_mode ?? "unknown",
);

const protectedResult = await client.callTool({
  name: "explain_repository",
  arguments: { repo_root: repoRoot, kind: "env-var", needle: "PATH" },
});

if (health.json?.auth?.oauth_ui_enabled === true && !bearerToken) {
  const hasChallenge = Array.isArray(
    protectedResult?._meta?.["mcp/www_authenticate"],
  );
  if (!protectedResult?.isError || !hasChallenge) {
    throw new Error(
      "Expected protected tool to return an auth challenge in oauth mode",
    );
  }
  print("protected_tool", "oauth_challenge");
} else if (health.json?.auth?.oauth_ui_enabled === true) {
  if (protectedResult?.isError) {
    throw new Error("Protected tool still failed even with a bearer token.");
  }
  print("protected_tool", "reachable_with_bearer");
} else {
  print(
    "protected_tool",
    protectedResult?.isError ? "error" : "reachable",
  );
}

await client.close();
