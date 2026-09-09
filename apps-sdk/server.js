#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { randomUUID, createHash } from "node:crypto";
import { execFileSync, spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { createMcpExpressApp } from "@modelcontextprotocol/sdk/server/express.js";
import { z } from "zod";
import { SignJWT, importPKCS8 } from "jose";

import { createAuthRuntime } from "./auth.js";
import {
  isGitHubRepoUrl,
  normalizeRepoUrl,
  parseGitHubOwnerRepo,
  requireRemoteRepoUrl,
} from "./repo-url.js";
import {
  buildActionPalette,
  buildUiHints,
  buildWorkspaceCapabilityHints,
  formatTextResult,
  summarizeEnvelope,
  summarizeEnvelopeMeta,
} from "../mcp/envelope.js";
import { buildEvidenceContract } from "../mcp/evidence-contract.js";
import {
  GUIDE_TOPICS,
  buildGuideStructuredContent,
  guideActionPalette,
  guideNextTools,
} from "../mcp/guide.js";
import {
  LeioGuideToolOutputSchema,
  LeioSessionTargetOutputSchema,
  LeioSpecialistOutputSchema,
  LeioToolOutputSchema,
} from "./output-schemas.js";
import {
  readOnlyAnnotations,
  sessionWriteAnnotations,
  specialistAnnotations,
} from "./tool-annotations.js";
import {
  IMPLEMENTATION_APPS_SDK,
  PROTOCOL_VERSION,
  SUPPORTED_PROTOCOL_VERSIONS,
  TOOL_EXECUTION_FORBIDDEN,
  appsSdkServerOptions,
  executionErrorResult,
  finalizeCallToolResult,
  installModernProtocol,
  protocolMeta,
  wrapToolHandler,
} from "../mcp/mcp-spec-2025-11-25.js";
import { buildTenantScopeEnv } from "./tenant-scope.mjs";
import { resolveBinaryPath as resolveTrustedBinary } from "../mcp/resolve-binary.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const leioCodeRoot = path.resolve(__dirname, "..");
const widgetPath = path.join(__dirname, "public", "leio-code.html");
const privacyPagePath = path.join(__dirname, "public", "privacy.html");
const supportPagePath = path.join(__dirname, "public", "support.html");
const termsPagePath = path.join(__dirname, "public", "terms.html");
const widgetUri = "ui://widget/leio-code.html";
const defaultTimeoutMs = Number.parseInt(
  process.env.LEIO_CODE_TIMEOUT_MS ?? "180000",
  10,
);
const defaultIndexTimeoutMs = Number.parseInt(
  process.env.LEIO_CODE_INDEX_TIMEOUT_MS ?? "300000",
  10,
);
const host = process.env.LEIO_APPS_SDK_HOST ?? "127.0.0.1";
const port = Number.parseInt(process.env.LEIO_APPS_SDK_PORT ?? "3333", 10);

/** Browser origins allowed to call /mcp (ChatGPT Apps + local smoke). */
const DEFAULT_MCP_CORS_ORIGINS = [
  "https://chatgpt.com",
  "https://chat.openai.com",
  "https://platform.openai.com",
];

function parseCsvList(value) {
  return String(value ?? "")
    .split(/[,\s]+/)
    .map((item) => item.trim())
    .filter(Boolean);
}

function parseBoolEnv(name, fallback) {
  const raw = process.env[name];
  if (raw == null || String(raw).trim() === "") {
    return fallback;
  }
  return ["1", "true", "yes", "on"].includes(String(raw).trim().toLowerCase());
}

function isLoopbackHost(value) {
  const normalized = String(value ?? "").trim().toLowerCase();
  return normalized === "127.0.0.1" || normalized === "localhost" || normalized === "::1";
}

function buildMcpCorsOrigins() {
  const configured = parseCsvList(process.env.LEIO_APPS_SDK_CORS_ORIGINS);
  return Array.from(new Set([...DEFAULT_MCP_CORS_ORIGINS, ...configured]));
}

function buildAllowedHosts() {
  const hosts = new Set([
    "127.0.0.1",
    "localhost",
    "::1",
    `127.0.0.1:${port}`,
    `localhost:${port}`,
  ]);
  for (const candidate of [
    process.env.LEIO_APPS_SDK_PUBLIC_URL,
    ...parseCsvList(process.env.LEIO_APPS_SDK_ALLOWED_HOSTS),
  ]) {
    if (!candidate) continue;
    try {
      const url = candidate.includes("://")
        ? new URL(candidate)
        : new URL(`https://${candidate}`);
      hosts.add(url.hostname.toLowerCase());
      hosts.add(url.host.toLowerCase());
    } catch {
      hosts.add(candidate.toLowerCase());
    }
  }
  return Array.from(hosts);
}

function applyMcpCorsHeaders(req, res) {
  const origin = typeof req.headers.origin === "string" ? req.headers.origin : "";
  const allowed = buildMcpCorsOrigins();
  if (origin && allowed.includes(origin)) {
    res.setHeader("Access-Control-Allow-Origin", origin);
    res.setHeader("Vary", "Origin");
    res.setHeader("Access-Control-Allow-Credentials", "true");
  }
  res.setHeader(
    "Access-Control-Allow-Methods",
    "GET, POST, DELETE, OPTIONS",
  );
  res.setHeader(
    "Access-Control-Allow-Headers",
    [
      "Content-Type",
      "Authorization",
      "Mcp-Session-Id",
      "Last-Event-ID",
      "mcp-protocol-version",
    ].join(", "),
  );
  res.setHeader("Access-Control-Expose-Headers", "Mcp-Session-Id");
  res.setHeader("Access-Control-Max-Age", "86400");
}
const checkoutRoot = path.resolve(
  process.env.LEIO_CODE_CHECKOUTS_ROOT ?? "/tmp/leio-code-checkouts",
);
const allowServerRepoRoot = parseBoolEnv(
  "LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT",
  isLoopbackHost(host),
);
const auth = createAuthRuntime({ host, port });
const checkoutLocks = new Map();
const indexLocks = new Map();
const sessionRepoTargets = new Map();
const githubConnectStates = new Map();
const githubSessions = new Map();

/// Kind families come from the binary itself (`capabilities --catalog`),
/// not from mirror arrays: the CLI value enums and doctor registry are the
/// single source of truth, so a new kind cannot drift between surfaces.
function loadKindCatalog() {
  const binaryName =
    process.platform === "win32" ? "leio-code.exe" : "leio-code";
  const binaryPath = resolveTrustedBinary({
    binaryName,
    startDir: path.resolve(__dirname, ".."),
  });
  if (!binaryPath) {
    throw new Error(
      "leio-code binary not found; set LEIO_CODE_BIN or install to ~/.cargo/bin",
    );
  }
  const stdout = execFileSync(
    binaryPath,
    ["capabilities", "--catalog", "--json"],
    {
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    },
  );
  const envelope = extractTrailingJson(stdout);
  const catalog = envelope?.entities?.[0];
  if (!catalog || !Array.isArray(catalog.find_kinds)) {
    throw new Error("leio-code capabilities --catalog returned no catalog");
  }
  return catalog;
}

const KIND_CATALOG = loadKindCatalog();
const findKinds = KIND_CATALOG.find_kinds;
const graphKinds = KIND_CATALOG.graph_kinds;
const explainKinds = KIND_CATALOG.explain_kinds;
const vigorosDepths = ["quick", "deep"];
const doctorKinds = KIND_CATALOG.doctor_kinds;

function hasRepoMarker(candidate) {
  return [
    ".git",
    path.join(".leio-code", "config.toml"),
    "pyproject.toml",
    "package.json",
    "Cargo.toml",
  ].some((marker) => fs.existsSync(path.join(candidate, marker)));
}

function findNearestRepoRoot(start) {
  let current = path.resolve(start);
  while (true) {
    if (hasRepoMarker(current)) {
      return current;
    }
    const parent = path.dirname(current);
    if (parent === current) {
      return null;
    }
    current = parent;
  }
}

const cargoWorkspaceRoot = findNearestRepoRoot(leioCodeRoot) ?? leioCodeRoot;

function resolveConfiguredRepoRoot() {
  const configured = process.env.LEIO_CODE_REPO_ROOT;
  if (typeof configured !== "string" || !configured.trim()) {
    return null;
  }
  const resolved = path.resolve(configured.trim());
  return hasRepoMarker(resolved) ? resolved : null;
}

const defaultRepoRoot =
  resolveConfiguredRepoRoot() ??
  findNearestRepoRoot(process.cwd()) ??
  cargoWorkspaceRoot;

function resolveRepoRoot(override) {
  return path.resolve(override ?? defaultRepoRoot);
}

function normalizeUrl(value) {
  if (!value) {
    return null;
  }

  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }

  return trimmed.replace(/\/+$/, "");
}

function firstConfigured(...values) {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) {
      return value.trim();
    }
  }
  return null;
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function isPlaceholderEmail(value) {
  return value.endsWith("@replace-me.invalid");
}

function buildLegalConfig() {
  const publicBaseUrl = normalizeUrl(process.env.LEIO_APPS_SDK_PUBLIC_URL)
    ?? `http://${host}:${port}`;
  const appName = firstConfigured(
    process.env.LEIO_APPS_SDK_RESOURCE_NAME,
    "LEIO Code",
  );
  const publisherName = firstConfigured(
    process.env.LEIO_APPS_SDK_PUBLISHER_NAME,
    appName,
  );
  const companyName = firstConfigured(
    process.env.LEIO_APPS_SDK_COMPANY_NAME,
    publisherName,
    appName,
  );
  const companyUrl = normalizeUrl(process.env.LEIO_APPS_SDK_COMPANY_URL) ?? publicBaseUrl;
  const supportUrl = normalizeUrl(process.env.LEIO_APPS_SDK_SUPPORT_URL)
    ?? `${publicBaseUrl}/support`;
  const privacyUrl = normalizeUrl(process.env.LEIO_APPS_SDK_PRIVACY_URL)
    ?? `${publicBaseUrl}/privacy`;
  const termsUrl = normalizeUrl(process.env.LEIO_APPS_SDK_TERMS_URL)
    ?? `${publicBaseUrl}/terms`;
  const supportEmail = firstConfigured(
    process.env.LEIO_APPS_SDK_SUPPORT_EMAIL,
    "support@replace-me.invalid",
  );
  const privacyEmail = firstConfigured(
    process.env.LEIO_APPS_SDK_PRIVACY_EMAIL,
    supportEmail,
  );
  const securityEmail = firstConfigured(
    process.env.LEIO_APPS_SDK_SECURITY_EMAIL,
    supportEmail,
  );
  const supportHours = firstConfigured(
    process.env.LEIO_APPS_SDK_SUPPORT_HOURS,
    "[configure LEIO_APPS_SDK_SUPPORT_HOURS before launch]",
  );
  const legalLastUpdated = firstConfigured(
    process.env.LEIO_APPS_SDK_LEGAL_LAST_UPDATED,
    new Date().toISOString().slice(0, 10),
  );

  const missingItems = [];
  if (!process.env.LEIO_APPS_SDK_PUBLISHER_NAME?.trim()) {
    missingItems.push("publisher name");
  }
  if (!process.env.LEIO_APPS_SDK_COMPANY_URL?.trim()) {
    missingItems.push("company URL");
  }
  if (isPlaceholderEmail(supportEmail)) {
    missingItems.push("support email");
  }
  if (isPlaceholderEmail(privacyEmail)) {
    missingItems.push("privacy email");
  }
  if (isPlaceholderEmail(securityEmail)) {
    missingItems.push("security email");
  }
  if ((supportHours ?? "").startsWith("[configure")) {
    missingItems.push("support hours");
  }

  return {
    appName,
    publisherName,
    companyName,
    companyUrl,
    supportUrl,
    privacyUrl,
    termsUrl,
    supportEmail,
    privacyEmail,
    securityEmail,
    supportHours,
    legalLastUpdated,
    publicBaseUrl,
    mcpUrl: `${publicBaseUrl}/mcp`,
    missingItems,
  };
}

const legal = buildLegalConfig();

function buildVigorosConfig() {
  const publicUrl = normalizeUrl(process.env.LEIO_VIGOROS_PUBLIC_URL)
    ?? "https://carlos-motta-apps-sdk.fly.dev";
  const mcpUrl = normalizeUrl(process.env.LEIO_VIGOROS_MCP_URL);
  const tokenUrl = normalizeUrl(process.env.LEIO_VIGOROS_TOKEN_URL);
  const clientId = process.env.LEIO_VIGOROS_CLIENT_ID?.trim() ?? "";
  const clientSecret = process.env.LEIO_VIGOROS_CLIENT_SECRET?.trim() ?? "";
  const scopes = process.env.LEIO_VIGOROS_SCOPES?.trim() ?? "";
  const jwtIssuer = normalizeUrl(process.env.LEIO_VIGOROS_JWT_ISSUER);

  return {
    publicUrl,
    mcpUrl,
    tokenUrl,
    clientId,
    clientSecret,
    scopes,
    jwtIssuer,
  };
}

const vigorosBridge = buildVigorosConfig();

function buildVigorosBridgeInfo() {
  const authConfigured = Boolean(
    vigorosBridge.tokenUrl
      && vigorosBridge.clientId
      && vigorosBridge.clientSecret,
  );
  const missing = [];
  if (!vigorosBridge.mcpUrl) {
    missing.push("LEIO_VIGOROS_MCP_URL");
  }
  if (vigorosBridge.tokenUrl && !vigorosBridge.clientId) {
    missing.push("LEIO_VIGOROS_CLIENT_ID");
  }
  if (vigorosBridge.tokenUrl && !vigorosBridge.clientSecret) {
    missing.push("LEIO_VIGOROS_CLIENT_SECRET");
  }

  return {
    enabled: Boolean(vigorosBridge.mcpUrl),
    configured: Boolean(vigorosBridge.mcpUrl),
    public_url: vigorosBridge.publicUrl,
    auth_configured: authConfigured,
    jwt_issuer_configured: Boolean(vigorosBridge.jwtIssuer),
    depths: vigorosDepths,
    missing,
  };
}

async function fetchVigorosAccessToken() {
  if (!vigorosBridge.tokenUrl) {
    return null;
  }
  if (!vigorosBridge.clientId || !vigorosBridge.clientSecret) {
    throw new Error(
      "VIGOROS token auth requires LEIO_VIGOROS_CLIENT_ID and LEIO_VIGOROS_CLIENT_SECRET",
    );
  }

  const body = new URLSearchParams({
    grant_type: "client_credentials",
    client_id: vigorosBridge.clientId,
    client_secret: vigorosBridge.clientSecret,
  });
  if (vigorosBridge.scopes) {
    body.set("scope", vigorosBridge.scopes);
  }

  const response = await fetch(vigorosBridge.tokenUrl, {
    method: "POST",
    headers: {
      "content-type": "application/x-www-form-urlencoded",
      accept: "application/json",
    },
    body,
  });
  const text = await response.text();
  const payload = text ? JSON.parse(text) : {};
  if (!response.ok) {
    throw new Error(
      `VIGOROS token request failed: ${response.status} ${response.statusText} ${text}`,
    );
  }
  if (!payload.access_token) {
    throw new Error("VIGOROS token response did not include access_token");
  }
  return payload.access_token;
}

async function callVigorosTool(name, args) {
  if (!vigorosBridge.mcpUrl) {
    throw new Error("Carlos Motta bridge is not configured. Set LEIO_VIGOROS_MCP_URL.");
  }

  const accessToken = await fetchVigorosAccessToken();
  const transport = new StreamableHTTPClientTransport(new URL(vigorosBridge.mcpUrl), {
    requestInit: accessToken
      ? {
          headers: {
            authorization: `Bearer ${accessToken}`,
          },
        }
      : undefined,
  });
  const client = new Client({ name: "leio-code-vigoros-bridge", version: "2.0.0" });

  try {
    await client.connect(transport);
    return await client.callTool({ name, arguments: args });
  } finally {
    await client.close().catch(() => {});
  }
}

function renderTemplate(templatePath) {
  const warningHtml = legal.missingItems.length
    ? `<div class="notice"><strong>Draft notice:</strong> replace these placeholders before public submission: ${escapeHtml(legal.missingItems.join(", "))}.</div>`
    : "";
  let html = fs.readFileSync(templatePath, "utf8");
  html = html.replaceAll("{{LEGAL_NOTICE}}", warningHtml);
  const replacements = {
    APP_NAME: legal.appName,
    PUBLISHER_NAME: legal.publisherName,
    COMPANY_NAME: legal.companyName,
    COMPANY_URL: legal.companyUrl,
    SUPPORT_URL: legal.supportUrl,
    PRIVACY_URL: legal.privacyUrl,
    TERMS_URL: legal.termsUrl,
    SUPPORT_EMAIL: legal.supportEmail,
    PRIVACY_EMAIL: legal.privacyEmail,
    SECURITY_EMAIL: legal.securityEmail,
    SUPPORT_HOURS: legal.supportHours,
    LEGAL_LAST_UPDATED: legal.legalLastUpdated,
    PUBLIC_BASE_URL: legal.publicBaseUrl,
    MCP_URL: legal.mcpUrl,
  };

  for (const [key, value] of Object.entries(replacements)) {
    html = html.replaceAll(`{{${key}}}`, escapeHtml(value));
  }
  return html;
}

function buildWidgetMetadata() {
  let widgetOrigin = null;
  try {
    widgetOrigin = new URL(legal.publicBaseUrl).origin;
  } catch {
    widgetOrigin = null;
  }

  return {
    ...(widgetOrigin ? { "openai/widgetDomain": widgetOrigin } : {}),
    "openai/widgetCSP": {
      connect_domains: widgetOrigin ? [widgetOrigin] : [],
      redirect_domains: widgetOrigin ? [widgetOrigin] : [],
    },
  };
}

function resolveBinaryPath() {
  const binaryName = process.platform === "win32" ? "leio-code.exe" : "leio-code";
  return resolveTrustedBinary({
    startDir: cargoWorkspaceRoot,
    binaryName,
  });
}

function buildInvocation(cliArgs) {
  const binaryPath = resolveBinaryPath();
  if (binaryPath) {
    return {
      command: binaryPath,
      args: cliArgs,
      cwd: cargoWorkspaceRoot,
    };
  }

  return {
    command: "cargo",
    args: [
      "run",
      "--quiet",
      "--manifest-path",
      path.join(leioCodeRoot, "Cargo.toml"),
      "--",
      ...cliArgs,
    ],
    cwd: cargoWorkspaceRoot,
  };
}

function runProcess(command, args, cwd, timeoutMs, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
      env: {
        ...process.env,
        ...(options.env ?? {}),
      },
    });

    let stdout = "";
    let stderr = "";
    let settled = false;
    let timer = null;

    if (timeoutMs > 0) {
      timer = setTimeout(() => {
        if (settled) {
          return;
        }
        settled = true;
        child.kill("SIGTERM");
        reject(new Error(`leio-code timed out after ${timeoutMs}ms`));
      }, timeoutMs);
    }

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });

    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });

    child.on("error", (error) => {
      if (settled) {
        return;
      }
      settled = true;
      if (timer) {
        clearTimeout(timer);
      }
      reject(error);
    });

    child.on("close", (code, signal) => {
      if (settled) {
        return;
      }
      settled = true;
      if (timer) {
        clearTimeout(timer);
      }
      resolve({
        code: code ?? 1,
        signal: signal ?? null,
        stdout,
        stderr,
      });
    });
  });
}

function normalizeGitRef(value) {
  if (!value || typeof value !== "string") {
    return null;
  }
  const trimmed = value.trim();
  return trimmed || null;
}

function buildRepoCacheKey(repoUrl, gitRef, authKey = "") {
  return createHash("sha256")
    .update(`${repoUrl}#${gitRef ?? ""}#${authKey}`)
    .digest("hex")
    .slice(0, 24);
}

function indexPathForRepo(repoRoot) {
  return path.join(repoRoot, ".leio-code", "index.json");
}

function indexedRevisionPathForRepo(repoRoot) {
  return path.join(repoRoot, ".leio-code", "indexed-revision");
}

function rememberSessionRepoTarget(sessionId, repoUrl, gitRef) {
  if (!sessionId || !repoUrl) {
    return;
  }

  sessionRepoTargets.set(sessionId, {
    repoUrl: normalizeRepoUrl(repoUrl),
    gitRef: normalizeGitRef(gitRef),
  });
}

function clearSessionRuntimeState(sessionId) {
  if (!sessionId) {
    return;
  }
  sessionRepoTargets.delete(sessionId);
  githubSessions.delete(sessionId);
}

function renderSimpleHtmlPage(title, body) {
  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>${escapeHtml(title)}</title>
    <style>
      body {
        margin: 0;
        min-height: 100vh;
        display: grid;
        place-items: center;
        background: #0f172a;
        color: #e2e8f0;
        font: 16px/1.5 ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      }
      main {
        width: min(640px, calc(100vw - 32px));
        padding: 28px;
        border-radius: 18px;
        background: rgba(15, 23, 42, 0.92);
        border: 1px solid rgba(148, 163, 184, 0.25);
        box-shadow: 0 24px 80px rgba(15, 23, 42, 0.35);
      }
      h1 {
        margin: 0 0 12px;
        font-size: 24px;
      }
      p {
        margin: 0 0 12px;
      }
      code {
        font-family: ui-monospace, "SFMono-Regular", Menlo, Consolas, monospace;
        background: rgba(148, 163, 184, 0.18);
        padding: 2px 6px;
        border-radius: 8px;
      }
      a {
        color: #5eead4;
      }
    </style>
  </head>
  <body>
    <main>
      <h1>${escapeHtml(title)}</h1>
      ${body}
    </main>
  </body>
</html>`;
}

function withQueryParams(baseUrl, params) {
  const url = new URL(baseUrl);
  for (const [key, value] of Object.entries(params)) {
    if (value == null || value === "") {
      continue;
    }
    url.searchParams.set(key, value);
  }
  return url.toString();
}

function parsePrivateKey(value) {
  if (!value || typeof value !== "string") {
    return null;
  }

  return value.includes("\\n") ? value.replaceAll("\\n", "\n") : value;
}

function buildGitHubAppConfig() {
  const slug = process.env.LEIO_CODE_GITHUB_APP_SLUG?.trim() ?? null;
  const appId = process.env.LEIO_CODE_GITHUB_APP_ID?.trim() ?? null;
  const clientId = process.env.LEIO_CODE_GITHUB_APP_CLIENT_ID?.trim() ?? null;
  const clientSecret = process.env.LEIO_CODE_GITHUB_APP_CLIENT_SECRET?.trim() ?? null;
  const privateKey = parsePrivateKey(process.env.LEIO_CODE_GITHUB_APP_PRIVATE_KEY?.trim() ?? null);
  const callbackUrl = normalizeUrl(
    process.env.LEIO_CODE_GITHUB_APP_CALLBACK_URL
      ?? (legal.publicBaseUrl ? `${legal.publicBaseUrl}/github/app/callback` : null),
  );

  const configured = Boolean(slug && appId && clientId && clientSecret && privateKey && callbackUrl);

  return {
    slug,
    appId,
    clientId,
    clientSecret,
    privateKey,
    callbackUrl,
    configured,
  };
}

const githubApp = buildGitHubAppConfig();

async function githubFetchJson(url, init = {}) {
  const response = await fetch(url, init);
  const text = await response.text();
  const payload = text ? JSON.parse(text) : null;
  if (!response.ok) {
    throw new Error(
      `GitHub request failed: ${response.status} ${response.statusText} ${text}`,
    );
  }
  return payload;
}

async function createGitHubAppJwt() {
  if (!githubApp.configured) {
    throw new Error("GitHub App is not configured");
  }

  const key = await importPKCS8(githubApp.privateKey, "RS256");
  const now = Math.floor(Date.now() / 1000);
  return new SignJWT({})
    .setProtectedHeader({ alg: "RS256" })
    .setIssuedAt(now - 60)
    .setExpirationTime(now + 9 * 60)
    .setIssuer(githubApp.appId)
    .sign(key);
}

async function exchangeGitHubCodeForUserToken({ code, state }) {
  if (!githubApp.configured) {
    throw new Error("GitHub App is not configured");
  }

  const body = new URLSearchParams({
    client_id: githubApp.clientId,
    client_secret: githubApp.clientSecret,
    code,
    redirect_uri: githubApp.callbackUrl,
    state,
  });

  return githubFetchJson("https://github.com/login/oauth/access_token", {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-www-form-urlencoded",
    },
    body,
  });
}

async function listGitHubUserInstallations(accessToken) {
  const payload = await githubFetchJson("https://api.github.com/user/installations", {
    headers: {
      accept: "application/vnd.github+json",
      authorization: `Bearer ${accessToken}`,
      "x-github-api-version": "2026-03-10",
    },
  });

  return Array.isArray(payload?.installations) ? payload.installations : [];
}

async function listGitHubInstallationRepositories(accessToken, installationId) {
  const payload = await githubFetchJson(
    `https://api.github.com/user/installations/${installationId}/repositories`,
    {
      headers: {
        accept: "application/vnd.github+json",
        authorization: `Bearer ${accessToken}`,
        "x-github-api-version": "2026-03-10",
      },
    },
  );

  return Array.isArray(payload?.repositories) ? payload.repositories : [];
}

async function createGitHubInstallationAccessToken({ installationId, repositoryId }) {
  const appJwt = await createGitHubAppJwt();
  const body = repositoryId
    ? JSON.stringify({ repository_ids: [Number(repositoryId)] })
    : undefined;

  return githubFetchJson(
    `https://api.github.com/app/installations/${installationId}/access_tokens`,
    {
      method: "POST",
      headers: {
        accept: "application/vnd.github+json",
        authorization: `Bearer ${appJwt}`,
        "x-github-api-version": "2026-03-10",
        ...(body ? { "content-type": "application/json" } : {}),
      },
      body,
    },
  );
}

function getGitHubSession(githubSessionId) {
  if (!githubSessionId) {
    return null;
  }

  const session = githubSessions.get(githubSessionId);
  if (!session) {
    return null;
  }

  if (session.expiresAt && session.expiresAt <= Date.now()) {
    githubSessions.delete(githubSessionId);
    return null;
  }

  return session;
}

function pruneGitHubConnectStates() {
  const cutoff = Date.now() - 15 * 60 * 1000;
  for (const [state, pending] of githubConnectStates.entries()) {
    if ((pending?.createdAt ?? 0) < cutoff) {
      githubConnectStates.delete(state);
    }
  }
}

async function fetchGitHubUserProfile(accessToken) {
  return githubFetchJson("https://api.github.com/user", {
    headers: {
      accept: "application/vnd.github+json",
      authorization: `Bearer ${accessToken}`,
      "x-github-api-version": "2026-03-10",
    },
  });
}

async function hydrateGitHubSession(accessToken) {
  const [profile, installations] = await Promise.all([
    fetchGitHubUserProfile(accessToken),
    listGitHubUserInstallations(accessToken),
  ]);

  const repositories = [];
  const normalizedInstallations = [];

  for (const installation of installations) {
    const installationRepositories = await listGitHubInstallationRepositories(
      accessToken,
      installation.id,
    );

    normalizedInstallations.push({
      id: installation.id,
      account_login: installation.account?.login ?? null,
      account_type: installation.account?.type ?? null,
      repository_selection: installation.repository_selection ?? null,
      repository_count: installationRepositories.length,
    });

    for (const repo of installationRepositories) {
      repositories.push({
        id: repo.id,
        name: repo.name,
        full_name: repo.full_name,
        private: Boolean(repo.private),
        default_branch: repo.default_branch ?? null,
        html_url: repo.html_url ?? null,
        clone_url: repo.clone_url ?? null,
        installation_id: installation.id,
        installation_account_login: installation.account?.login ?? null,
      });
    }
  }

  return {
    accessToken,
    login: profile?.login ?? null,
    github_user_id: profile?.id ?? null,
    avatar_url: profile?.avatar_url ?? null,
    installations: normalizedInstallations,
    repositories,
  };
}

function findGitHubRepoAccess(sessionId, repoUrlInput) {
  if (!sessionId) {
    return null;
  }

  const githubSession = getGitHubSession(sessionId);
  if (!githubSession) {
    return null;
  }

  const normalizedRepoUrl = normalizeRepoUrl(repoUrlInput);
  const parsedRepo = parseGitHubOwnerRepo(repoUrlInput);
  const repoFullName = parsedRepo
    ? `${parsedRepo.owner}/${parsedRepo.repo}`.toLowerCase()
    : null;

  return (
    githubSession.repositories.find((repo) => {
      const repoCloneUrl = normalizeRepoUrl(repo.clone_url);
      const repoHtmlUrl = normalizeRepoUrl(repo.html_url);
      const repoName = typeof repo.full_name === "string"
        ? repo.full_name.toLowerCase()
        : null;

      return (
        (normalizedRepoUrl && (repoCloneUrl === normalizedRepoUrl || repoHtmlUrl === normalizedRepoUrl))
        || (repoFullName && repoName === repoFullName)
      );
    }) ?? null
  );
}

function buildRepoAuthContext(sessionId, repoUrl, explicitAuthContext = null) {
  if (explicitAuthContext) {
    return explicitAuthContext;
  }

  const githubRepoAccess = findGitHubRepoAccess(sessionId, repoUrl);
  if (!githubRepoAccess) {
    return null;
  }

  return {
    sessionId,
    githubInstallationId: githubRepoAccess.installation_id,
    githubRepositoryId: githubRepoAccess.id,
  };
}

async function buildGitAuth(repoUrl, authContext = {}) {
  const context = authContext ?? {};
  const gitHttpUsername = process.env.LEIO_CODE_GIT_HTTP_USERNAME?.trim();
  const gitHttpPassword = process.env.LEIO_CODE_GIT_HTTP_PASSWORD?.trim();
  const githubToken =
    process.env.LEIO_CODE_GITHUB_TOKEN?.trim()
    ?? process.env.GITHUB_TOKEN?.trim()
    ?? process.env.LEIO_CODE_GIT_AUTH_TOKEN?.trim();
  const gitlabToken =
    process.env.LEIO_CODE_GITLAB_TOKEN?.trim()
    ?? process.env.LEIO_CODE_GIT_AUTH_TOKEN?.trim();

  const env = {
    GIT_TERMINAL_PROMPT: "0",
  };
  const args = [];

  const repo = normalizeRepoUrl(repoUrl) ?? "";
  const lowerRepo = repo.toLowerCase();

  if (
    context.sessionId &&
    context.githubInstallationId &&
    isGitHubRepoUrl(repoUrl)
  ) {
    const githubSession = getGitHubSession(context.sessionId);
    if (!githubSession) {
      throw new Error("GitHub connection expired or missing. Reconnect GitHub and select the repository again.");
    }

    const tokenPayload = await createGitHubInstallationAccessToken({
      installationId: context.githubInstallationId,
      repositoryId: context.githubRepositoryId,
    });
    const basic = Buffer.from(`x-access-token:${tokenPayload.token}`).toString("base64");
    args.push("-c", `http.extraHeader=AUTHORIZATION: Basic ${basic}`);
    return { env, args };
  }

  if (gitHttpUsername && gitHttpPassword) {
    const basic = Buffer.from(`${gitHttpUsername}:${gitHttpPassword}`).toString("base64");
    args.push("-c", `http.extraHeader=AUTHORIZATION: Basic ${basic}`);
    return { env, args };
  }

  if (githubToken && lowerRepo.startsWith("https://github.com/")) {
    const basic = Buffer.from(`x-access-token:${githubToken}`).toString("base64");
    args.push("-c", `http.extraHeader=AUTHORIZATION: Basic ${basic}`);
    return { env, args };
  }

  if (gitlabToken && lowerRepo.includes("gitlab")) {
    const basic = Buffer.from(`oauth2:${gitlabToken}`).toString("base64");
    args.push("-c", `http.extraHeader=AUTHORIZATION: Basic ${basic}`);
    return { env, args };
  }

  return { env, args };
}

async function runGit(args, cwd, repoUrl, authContext) {
  const authOptions = await buildGitAuth(repoUrl, authContext);
  const execution = await runProcess(
    "git",
    [...authOptions.args, ...args],
    cwd,
    defaultTimeoutMs,
    { env: authOptions.env },
  );

  if (execution.code !== 0) {
    const detail = execution.stderr.trim() || execution.stdout.trim();
    throw new Error(detail || `git failed with exit code ${execution.code}`);
  }

  return execution;
}

async function ensureRepoCheckout(repoUrlInput, gitRefInput, authContext = {}) {
  const context = authContext ?? {};
  const repoUrl = requireRemoteRepoUrl(repoUrlInput);

  const gitRef = normalizeGitRef(gitRefInput);
  const authKey = context.sessionId
    ? `github-session:${context.sessionId}:${context.githubInstallationId ?? ""}:${context.githubRepositoryId ?? ""}`
    : "";
  const cacheKey = buildRepoCacheKey(repoUrl, gitRef, authKey);
  const checkoutPath = path.join(checkoutRoot, cacheKey);

  if (!checkoutLocks.has(cacheKey)) {
    checkoutLocks.set(
      cacheKey,
      (async () => {
        fs.mkdirSync(checkoutRoot, { recursive: true });

        if (!fs.existsSync(path.join(checkoutPath, ".git"))) {
          fs.rmSync(checkoutPath, { recursive: true, force: true });
          await runGit(
            ["clone", "--depth", "1", repoUrl, checkoutPath],
            checkoutRoot,
            repoUrl,
            context,
          );
        }

        await runGit(["remote", "set-url", "origin", repoUrl], checkoutPath, repoUrl, context);

        if (gitRef) {
          await runGit(["fetch", "--depth", "1", "origin", gitRef], checkoutPath, repoUrl, context);
        } else {
          await runGit(["fetch", "--depth", "1", "origin"], checkoutPath, repoUrl, context);
        }

        await runGit(["checkout", "--force", "FETCH_HEAD"], checkoutPath, repoUrl, context);
        const revision = (await runGit(
          ["rev-parse", "HEAD"],
          checkoutPath,
          repoUrl,
          context,
        )).stdout.trim();
        return {
          repoRoot: checkoutPath,
          revision: revision || null,
        };
      })().finally(() => {
        checkoutLocks.delete(cacheKey);
      }),
    );
  }

  return checkoutLocks.get(cacheKey);
}

async function resolveRepoRevision(repoRoot, repoUrl = null, authContext = null) {
  try {
    if (repoUrl) {
      return (await runGit(
        ["rev-parse", "HEAD"],
        repoRoot,
        repoUrl,
        authContext,
      )).stdout.trim() || null;
    }

    const execution = await runProcess(
      "git",
      ["rev-parse", "HEAD"],
      repoRoot,
      defaultTimeoutMs,
    );
    if (execution.code !== 0) {
      return null;
    }
    return execution.stdout.trim() || null;
  } catch {
    return null;
  }
}

async function ensureRepoIndexed(repoRoot, revision = null) {
  const indexPath = indexPathForRepo(repoRoot);
  const revisionPath = indexedRevisionPathForRepo(repoRoot);
  const currentRevision = revision?.trim() || null;

  const hasCurrentIndex = (() => {
    if (!fs.existsSync(indexPath)) {
      return false;
    }
    if (!currentRevision) {
      return true;
    }
    try {
      return fs.readFileSync(revisionPath, "utf8").trim() === currentRevision;
    } catch {
      return false;
    }
  })();

  if (hasCurrentIndex) {
    return;
  }

  if (!indexLocks.has(repoRoot)) {
    indexLocks.set(
      repoRoot,
      (async () => {
        fs.mkdirSync(path.dirname(indexPath), { recursive: true });
        const invocation = buildInvocation(["index", "--repo", repoRoot]);
        const execution = await runProcess(
          invocation.command,
          invocation.args,
          invocation.cwd,
          defaultIndexTimeoutMs,
        );
        if (execution.code !== 0) {
          const detail = execution.stderr.trim() || execution.stdout.trim();
          throw new Error(detail || `leio-code index failed for ${repoRoot}`);
        }

        if (currentRevision) {
          fs.writeFileSync(revisionPath, `${currentRevision}\n`, "utf8");
        }
      })().finally(() => {
        indexLocks.delete(repoRoot);
      }),
    );
  }

  return indexLocks.get(repoRoot);
}

async function resolveRuntimeRepoTarget(options = {}) {
  const sessionTarget = options.sessionId ? sessionRepoTargets.get(options.sessionId) ?? null : null;
  if (options.repoRoot) {
    const repoRoot = resolveRepoRoot(options.repoRoot);
    if (
      !options.trustedRepoRoot
      && !allowServerRepoRoot
      && repoRoot !== defaultRepoRoot
    ) {
      throw new Error(
        "repo_root is disabled on the hosted LEIO service; use repo_url or the active repository target.",
      );
    }
    const revision = await resolveRepoRevision(repoRoot);
    await ensureRepoIndexed(repoRoot, revision);
    return {
      repoRoot,
      repoRootMode: "repo_root",
      repoUrl: null,
      gitRef: null,
      revision,
    };
  }

  if (options.repoUrl) {
    const authContext = buildRepoAuthContext(
      options.sessionId,
      options.repoUrl,
      options.authContext,
    );
    const checkout = await ensureRepoCheckout(options.repoUrl, options.gitRef, authContext);
    await ensureRepoIndexed(checkout.repoRoot, checkout.revision);
    rememberSessionRepoTarget(options.sessionId, options.repoUrl, options.gitRef);
    return {
      repoRoot: checkout.repoRoot,
      repoRootMode: "repo_url",
      repoUrl: normalizeRepoUrl(options.repoUrl),
      gitRef: normalizeGitRef(options.gitRef),
      revision: checkout.revision,
    };
  }

  if (sessionTarget?.repoUrl) {
    const authContext = buildRepoAuthContext(
      options.sessionId,
      sessionTarget.repoUrl,
      options.authContext,
    );
    const checkout = await ensureRepoCheckout(
      sessionTarget.repoUrl,
      sessionTarget.gitRef,
      authContext,
    );
    await ensureRepoIndexed(checkout.repoRoot, checkout.revision);
    return {
      repoRoot: checkout.repoRoot,
      repoRootMode: "session_repo_url",
      repoUrl: sessionTarget.repoUrl,
      gitRef: sessionTarget.gitRef,
      revision: checkout.revision,
    };
  }

  const repoRoot = resolveRepoRoot();
  const revision = await resolveRepoRevision(repoRoot);
  await ensureRepoIndexed(repoRoot, revision);
  return {
    repoRoot,
    repoRootMode: "default",
    repoUrl: null,
    gitRef: null,
    revision,
  };
}

function extractTrailingJson(stdout) {
  const trimmed = stdout.trim();
  if (!trimmed) {
    return null;
  }

  try {
    return JSON.parse(trimmed);
  } catch {
    // Progress lines may precede the final JSON envelope.
  }

  const lines = trimmed.split(/\r?\n/);
  for (let i = lines.length - 1; i >= 0; i -= 1) {
    if (!lines[i].trim().startsWith("{")) {
      continue;
    }

    const candidate = lines.slice(i).join("\n");
    try {
      return JSON.parse(candidate);
    } catch {
      // Keep scanning upward.
    }
  }

  return null;
}

function buildToolMeta(invoking, invoked, securitySchemes) {
  return {
    ...protocolMeta(),
    securitySchemes,
    ui: {
      resourceUri: widgetUri,
      visibility: ["model", "app"],
    },
    "openai/outputTemplate": widgetUri,
    "openai/widgetAccessible": true,
    "openai/toolInvocation/invoking": invoking,
    "openai/toolInvocation/invoked": invoked,
  };
}

const appToolNameMap = {
  leio_code_guide: "guide_repository_tools",
  leio_code_capabilities: "inspect_repository_capabilities",
  leio_code_status: "inspect_repository_status",
  leio_code_context: "prepare_repository_context",
  leio_code_find: "search_repository",
  leio_code_explain: "explain_repository",
  leio_code_doctor: "audit_repository_contracts",
  leio_code_audit: "audit_repository_rollup",
  leio_code_graph: "graph_repository",
  leio_code_export: "inspect_repository_status",
};

function mapToolNameToAppSurface(toolName) {
  return appToolNameMap[toolName] ?? toolName;
}

function buildAppsSdkActionPalette(actionPalette) {
  if (!actionPalette || typeof actionPalette !== "object") {
    return null;
  }

  const showTools = Array.isArray(actionPalette.show_tools)
    ? actionPalette.show_tools.map(mapToolNameToAppSurface)
    : [];
  const hideTools = Array.isArray(actionPalette.hide_tools)
    ? actionPalette.hide_tools.map(mapToolNameToAppSurface)
    : [];
  const recommendedNextTools = Array.isArray(actionPalette.recommended_next_tools)
    ? actionPalette.recommended_next_tools.map(mapToolNameToAppSurface)
    : [];

  return {
    ...actionPalette,
    source_contract: "apps-sdk",
    show_tools: [...new Set(showTools)],
    hide_tools: [...new Set(hideTools)],
    recommended_start_tool: mapToolNameToAppSurface(
      actionPalette.recommended_start_tool,
    ),
    recommended_next_tools: [...new Set(recommendedNextTools)],
    tool_aliases: appToolNameMap,
  };
}

function buildSourceControlInfo(sessionId) {
  const githubSession = getGitHubSession(sessionId);
  const installUrl = githubApp.configured && githubApp.slug
    ? `https://github.com/apps/${githubApp.slug}/installations/new`
    : null;
  const connectUrl = githubApp.configured && sessionId && legal.publicBaseUrl
    ? withQueryParams(`${legal.publicBaseUrl}/github/app/connect`, {
        mcp_session_id: sessionId,
      })
    : null;

  return {
    supports_repo_url: true,
    supports_server_repo_root: allowServerRepoRoot,
    github: {
      configured: githubApp.configured,
      connected: Boolean(githubSession),
      app_slug: githubApp.slug,
      connect_url: connectUrl,
      install_url: installUrl,
      login: githubSession?.login ?? null,
      installation_count: githubSession?.installations?.length ?? 0,
      repository_count: githubSession?.repositories?.length ?? 0,
      repositories: githubSession?.repositories ?? [],
    },
    selected_repository: sessionId
      ? sessionRepoTargets.get(sessionId) ?? null
      : null,
  };
}

function augmentStructuredContent(structuredContent, options = {}) {
  const capabilityHints = buildWorkspaceCapabilityHints(
    structuredContent?.workspace_capabilities ?? null,
  );
  const baseActionPalette = buildActionPalette(capabilityHints);
  const augmented = {
    ...structuredContent,
    workspace_capability_hints: capabilityHints,
    action_palette: buildAppsSdkActionPalette(baseActionPalette),
    raw_action_palette: baseActionPalette,
    auth: auth.summary(),
    mcp_session_id: options.sessionId ?? null,
    source_control: buildSourceControlInfo(options.sessionId),
    specialist_bridge: buildVigorosBridgeInfo(),
  };
  if (options.evidenceContract !== true) {
    return augmented;
  }
  return {
    ...augmented,
    evidence_contract: buildEvidenceContract(augmented, {
      producerVersion: IMPLEMENTATION_APPS_SDK.version,
      transport: "streamable-http",
    }),
  };
}

const VigorosBridgeOutputSchema = z.object({
  answer: z.string(),
  answer_id: z.string(),
  summary: z.string().optional(),
  synthesis_mode: z.string().nullable().optional(),
  research_dossier: z.record(z.unknown()).nullable().optional(),
  research_brief: z.record(z.unknown()).nullable().optional(),
  citations: z.array(z.unknown()).optional(),
  timings_ms: z.record(z.unknown()).optional(),
  ui_hints: z.record(z.unknown()).optional(),
  action_palette: z.record(z.unknown()).optional(),
}).passthrough();

function summarizeAuditForSpecialist(auditStructuredContent) {
  if (!auditStructuredContent || typeof auditStructuredContent !== "object") {
    return [];
  }

  const sections = Array.isArray(auditStructuredContent.sections)
    ? auditStructuredContent.sections.slice(0, 12)
    : [];
  const summary = typeof auditStructuredContent.envelope_summary?.summary === "string"
    ? auditStructuredContent.envelope_summary.summary
    : null;
  const badges = Array.isArray(auditStructuredContent.ui_hints?.badges)
    ? auditStructuredContent.ui_hints.badges
        .map((badge) => {
          if (!badge || typeof badge !== "object") {
            return null;
          }
          const label = typeof badge.label === "string" ? badge.label : null;
          const value = typeof badge.value === "string" ? badge.value : null;
          return label && value ? `${label}: ${value}` : value ?? label;
        })
        .filter(Boolean)
        .slice(0, 8)
    : [];

  return [
    ...(summary ? [`Audit summary: ${summary}`] : []),
    ...sections.map((section) => `Audit finding: ${section}`),
    ...badges.map((badge) => `Audit badge: ${badge}`),
  ];
}

function buildCarlosMottaQuestion({
  question,
  repoTarget,
  statusStructuredContent,
  auditStructuredContent,
}) {
  const statusSections = Array.isArray(statusStructuredContent?.sections)
    ? statusStructuredContent.sections.slice(0, 12)
    : [];
  const capabilityNotes = Array.isArray(statusStructuredContent?.workspace_capability_hints?.notes)
    ? statusStructuredContent.workspace_capability_hints.notes.slice(0, 6)
    : [];
  const auditSections = summarizeAuditForSpecialist(auditStructuredContent);

  return [
    "Você está recebendo contexto do LEIO Code sobre um repositório de software.",
    "Responda como Carlos Motta, com foco em governança, disclosure, compliance, controles, trilha de auditoria, superfícies regulatórias e risco operacional. Se a evidência do repositório for insuficiente, diga exatamente o que falta.",
    repoTarget?.repoUrl ? `Repository URL: ${repoTarget.repoUrl}` : null,
    repoTarget?.gitRef ? `Git ref: ${repoTarget.gitRef}` : null,
    repoTarget?.revision ? `Revision: ${repoTarget.revision}` : null,
    statusStructuredContent?.workspace_profile
      ? `Workspace profile: ${statusStructuredContent.workspace_profile}`
      : null,
    ...statusSections.map((section) => `Repository signal: ${section}`),
    ...capabilityNotes.map((note) => `Capability note: ${note}`),
    ...auditSections,
    "",
    `Pergunta do usuário: ${question}`,
  ]
    .filter(Boolean)
    .join("\n");
}

function currentRepoTargetFromStructuredContent(structuredContent) {
  return {
    repoRoot: structuredContent?.repo_root ?? null,
    repoUrl: structuredContent?.repo_url ?? null,
    gitRef: structuredContent?.git_ref ?? null,
    revision: structuredContent?.revision ?? null,
  };
}

async function invokeLeioTool(subcommandArgs, options = {}) {
  const repoTarget = await resolveRuntimeRepoTarget(options);
  const repoRoot = repoTarget.repoRoot;
  const timeoutMs = options.timeoutMs ?? defaultTimeoutMs;
  const cliArgs = ["--json", "--repo", repoRoot];

  cliArgs.push(...subcommandArgs);

  const invocation = buildInvocation(cliArgs);
  const tenantScopeEnv = buildTenantScopeEnv({
    authInfo: options.authInfo ?? null,
    repoTarget,
  });
  let execution;
  try {
    execution = await runProcess(
      invocation.command,
      invocation.args,
      invocation.cwd,
      timeoutMs,
      { env: tenantScopeEnv },
    );
  } catch (error) {
    return executionErrorResult(error, {
      repo_root: repoRoot,
      repo_url: repoTarget.repoUrl ?? null,
      git_ref: repoTarget.gitRef ?? null,
    });
  }
  const envelope = extractTrailingJson(execution.stdout);

  if (execution.code !== 0 && !envelope) {
    const detail = execution.stderr.trim() || execution.stdout.trim();
    return executionErrorResult(
      detail || `leio-code failed with exit code ${execution.code}`,
      {
        exit_code: execution.code,
        repo_root: repoRoot,
        repo_url: repoTarget.repoUrl ?? null,
        git_ref: repoTarget.gitRef ?? null,
        stdout: execution.stdout,
        stderr: execution.stderr,
      },
    );
  }

  const envelopeMetaSummary = summarizeEnvelopeMeta(envelope?.meta);
  const structuredContent = augmentStructuredContent({
    ok: execution.code === 0,
    repo_root: repoRoot,
    repo_root_mode: repoTarget.repoRootMode,
    repo_url: repoTarget.repoUrl,
    git_ref: repoTarget.gitRef,
    revision: repoTarget.revision,
    envelope,
    envelope_summary: summarizeEnvelope(envelope),
    envelope_meta: envelope?.meta ?? null,
    envelope_meta_summary: envelopeMetaSummary,
    workspace_profile: envelopeMetaSummary?.workspace_profile ?? null,
    workspace_capabilities:
      envelopeMetaSummary?.workspace_capabilities ?? null,
    ui_hints: buildUiHints(envelope),
    backend: envelopeMetaSummary?.backend ?? null,
    collection_selection: envelopeMetaSummary?.collection_selection ?? null,
    fca_relation_contract:
      envelopeMetaSummary?.fca_relation_contract ?? null,
    search_context: envelopeMetaSummary?.search ?? null,
    stdout: execution.stdout,
    stderr: execution.stderr,
  }, {
    sessionId: options.sessionId,
    evidenceContract: true,
  });

  return finalizeCallToolResult({
    metadata: buildWidgetMetadata(),
    structuredContent,
    content: [
      {
        type: "text",
        text: formatTextResult(invocation, execution, envelope),
      },
    ],
  });
}

async function buildStatusToolResult(targetOverride) {
  const authInfo = typeof targetOverride === "string"
    ? null
    : targetOverride?.authInfo ?? null;
  const repoTarget = await resolveRuntimeRepoTarget(
    typeof targetOverride === "string"
      ? { repoRoot: targetOverride }
      : targetOverride,
  );
  const repoRoot = repoTarget.repoRoot;
  const indexPath = path.join(repoRoot, ".leio-code", "index.json");
  const sections = [];
  let workspaceFacets = null;
  let workspaceCapabilities = null;

  try {
    const stat = fs.statSync(indexPath);
    const ageMs = Date.now() - stat.mtimeMs;
    const ageMin = Math.floor(ageMs / 60000);
    let ageDisplay;
    if (ageMin > 1440) ageDisplay = `${Math.floor(ageMin / 1440)}d ago (STALE)`;
    else if (ageMin > 60) ageDisplay = `${Math.floor(ageMin / 60)}h ago`;
    else ageDisplay = `${ageMin}m ago`;
    const sizeMb = (stat.size / 1048576).toFixed(1);
    sections.push(`Index: ${ageDisplay}, ${sizeMb} MB`);
  } catch {
    sections.push("Index: not found — run indexing first");
  }

  try {
    // Keep the status surface bounded. Full doctor coverage is available
    // through audit_repository_rollup; status uses the fast baseline preset.
    const doctor = await invokeLeioTool(["doctor", "baseline"], {
      repoRoot,
      trustedRepoRoot: true,
      authInfo,
      timeoutMs: 30000,
    });
    const envelope = doctor.structuredContent.envelope;
    const entities = Array.isArray(envelope?.entities) ? envelope.entities : [];
    const doctorEntities = entities.filter((item) => item?.doctor);
    const profile = doctor.structuredContent.workspace_profile;
    if (doctorEntities.length > 0) {
      const withWarnings = doctorEntities.filter(
        (item) => Number(item.warning_count ?? 0) > 0,
      );
      if (withWarnings.length === 0) {
        sections.push(
          `Doctors: all ${doctorEntities.length} green${profile ? ` (${profile})` : ""}`,
        );
      } else {
        const warningCount = withWarnings.reduce(
          (sum, item) => sum + Number(item.warning_count ?? 0),
          0,
        );
        sections.push(
          `Doctors: ${withWarnings.length}/${doctorEntities.length} with warnings (${warningCount} total)${profile ? ` (${profile})` : ""}`,
        );
      }
    } else if (typeof envelope?.summary === "string") {
      sections.push(`Doctors: ${envelope.summary}`);
    }
  } catch {
    sections.push("Doctors: unavailable");
  }

  try {
    const capabilitiesResult = await invokeLeioTool(["capabilities"], {
      repoRoot,
      trustedRepoRoot: true,
      authInfo,
      timeoutMs: 20000,
    });
    workspaceCapabilities = capabilitiesResult.structuredContent.workspace_capabilities;
    workspaceFacets =
      workspaceCapabilities?.workspace_facets ??
      JSON.parse(fs.readFileSync(indexPath, "utf-8"))?.workspace_facets ??
      null;
    if (workspaceCapabilities?.workspace_profile) {
      sections.push(`Profile: ${workspaceCapabilities.workspace_profile}`);
    }
    if (Array.isArray(workspaceCapabilities?.find_kinds)) {
      sections.push(`Find kinds: ${workspaceCapabilities.find_kinds.join(", ")}`);
    }
    if (Array.isArray(workspaceCapabilities?.doctor_kinds)) {
      sections.push(
        `Doctor suites: ${
          workspaceCapabilities.doctor_kinds.length > 0
            ? workspaceCapabilities.doctor_kinds.join(", ")
            : "none configured"
        }`,
      );
    }
    if (Array.isArray(workspaceCapabilities?.notes) && workspaceCapabilities.notes.length > 0) {
      sections.push(`Notes: ${workspaceCapabilities.notes.join(" | ")}`);
    }
  } catch {
    // Keep status usable without capabilities.
  }

  try {
    const idx = JSON.parse(fs.readFileSync(indexPath, "utf-8"));
    workspaceFacets = workspaceFacets ?? idx?.meta?.workspace_facets ?? null;
    if (workspaceFacets?.has_deploy_topology) {
      sections.push(`Deploy targets: ${workspaceFacets.deploy_targets}`);
    }
    if (workspaceFacets?.has_cartridges) {
      const cartridges = new Set();
      for (const target of idx.deploy_targets ?? []) {
        if (Array.isArray(target?.cartridges)) {
          target.cartridges.forEach((value) => cartridges.add(value));
        }
      }
      sections.push(`Active cartridges: ${[...cartridges].sort().join(", ")}`);
    }
    if (Array.isArray(idx.files)) {
      sections.push(`Indexed files: ${idx.files.length}`);
    }
  } catch {
    // Already covered above.
  }

  const structuredContent = augmentStructuredContent({
    ok: true,
    repo_root: repoRoot,
    repo_root_mode: repoTarget.repoRootMode,
    repo_url: repoTarget.repoUrl,
    git_ref: repoTarget.gitRef,
    revision: repoTarget.revision,
    sections,
    workspace_facets: workspaceFacets,
    workspace_capabilities: workspaceCapabilities,
  }, {
    sessionId: typeof targetOverride === "string" ? null : targetOverride?.sessionId ?? null,
    evidenceContract: true,
  });

  return {
    metadata: buildWidgetMetadata(),
    structuredContent,
    content: [{ type: "text", text: sections.join("\n") }],
  };
}

async function buildCarlosMottaSpecialistResult({
  question,
  depth = "quick",
  include_audit = false,
  audit_kind = "all",
  repo_root,
  repo_url,
  git_ref,
  sessionId = null,
  authInfo = null,
}) {
  const statusResult = await buildStatusToolResult({
    repoRoot: repo_root,
    repoUrl: repo_url,
    gitRef: git_ref,
    sessionId,
    authInfo,
  });
  const statusStructuredContent = statusResult.structuredContent;
  const repoTarget = currentRepoTargetFromStructuredContent(statusStructuredContent);

  let auditStructuredContent = null;
  if (include_audit) {
    const auditResult = await invokeLeioTool(["doctor", audit_kind], {
      repoRoot: statusStructuredContent.repo_root,
      repoUrl: statusStructuredContent.repo_url,
      gitRef: statusStructuredContent.git_ref,
      sessionId,
      authInfo,
      timeoutMs: 30000,
    });
    auditStructuredContent = auditResult.structuredContent;
  }

  const remoteResult = await callVigorosTool("research_with_twin", {
    question: buildCarlosMottaQuestion({
      question,
      repoTarget,
      statusStructuredContent,
      auditStructuredContent,
    }),
    depth,
  });

  const specialist = VigorosBridgeOutputSchema.parse(remoteResult.structuredContent ?? {});
  const structuredContent = augmentStructuredContent({
    ok: true,
    summary:
      specialist.summary
      ?? "Carlos Motta specialist review over current repository context.",
    repo_root: statusStructuredContent.repo_root,
    repo_root_mode: statusStructuredContent.repo_root_mode,
    repo_url: statusStructuredContent.repo_url,
    git_ref: statusStructuredContent.git_ref,
    revision: statusStructuredContent.revision,
    sections: statusStructuredContent.sections,
    workspace_facets: statusStructuredContent.workspace_facets,
    workspace_capabilities: statusStructuredContent.workspace_capabilities,
    specialist_review: {
      bridge: "vigoros-mcp",
      question,
      depth,
      answer_id: specialist.answer_id,
      answer: specialist.answer,
      synthesis_mode: specialist.synthesis_mode ?? "single_pass",
      timings_ms: specialist.timings_ms ?? null,
      citations: specialist.citations ?? [],
      research_dossier: specialist.research_dossier ?? null,
      research_brief: specialist.research_brief ?? null,
      source_summary: {
        repo_url: statusStructuredContent.repo_url,
        git_ref: statusStructuredContent.git_ref,
        revision: statusStructuredContent.revision,
      },
      audit: include_audit
        ? {
            kind: audit_kind,
            summary: auditStructuredContent?.envelope_summary?.summary ?? null,
            sections: auditStructuredContent?.sections ?? [],
          }
        : null,
    },
  }, {
    sessionId,
  });

  return {
    metadata: buildWidgetMetadata(),
    structuredContent,
    content: [
      {
        type: "text",
        text: [
          specialist.answer,
          "",
          `Answer ID: ${specialist.answer_id}`,
          `Depth: ${depth}`,
          `Synthesis mode: ${specialist.synthesis_mode ?? "single_pass"}`,
        ].join("\n"),
      },
    ],
  };
}

function getServer() {
  const server = new McpServer(
    IMPLEMENTATION_APPS_SDK,
    appsSdkServerOptions(),
  );
  const registerTool = server.registerTool.bind(server);
  server.registerTool = (name, config, handler) =>
    registerTool(
      name,
      {
        ...config,
        execution: config.execution ?? TOOL_EXECUTION_FORBIDDEN,
        _meta: {
          ...protocolMeta({ tool: name }),
          ...(config._meta ?? {}),
        },
      },
      wrapToolHandler(handler),
    );

  server.registerResource("leio-code-widget", widgetUri, {}, async () => ({
    contents: [
      {
        uri: widgetUri,
        mimeType: "text/html;profile=mcp-app",
        text: fs.readFileSync(widgetPath, "utf8"),
        _meta: {
          ui: { prefersBorder: true },
          "openai/widgetDescription":
            "Capability-aware LEIO Code results for repository status, lookup, explainability, and doctors.",
          ...buildWidgetMetadata(),
        },
      },
    ],
  }));

  server.registerTool(
    "inspect_repository_capabilities",
    {
      title: "Inspect repository capabilities",
      description:
        "Return the current workspace profile, optional workspace facets, and the find/explain/doctor families that are meaningful for this repository. Provide repo_url to analyze an allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand.",
      inputSchema: {
        repo_root: z
          .string()
          .describe("Absolute repository root on the server to inspect.")
          .optional(),
        repo_url: z
          .string()
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to inspect.")
          .optional(),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
          .optional(),
      },
      securitySchemes: auth.getSecuritySchemes("inspect_repository_capabilities"),
      annotations: readOnlyAnnotations,
      outputSchema: LeioToolOutputSchema,
      _meta: buildToolMeta(
        "Inspecting…",
        "Capabilities ready",
        auth.getSecuritySchemes("inspect_repository_capabilities"),
      ),
    },
    async ({ repo_root, repo_url, git_ref }, extra) => {
      const authError = auth.ensureToolAccess(
        "inspect_repository_capabilities",
        extra,
      );
      if (authError) {
        return authError;
      }
      return invokeLeioTool(["capabilities"], {
        repoRoot: repo_root,
        repoUrl: repo_url,
        gitRef: git_ref,
        sessionId: extra?.sessionId ?? null,
        authInfo: extra?.authInfo ?? null,
      });
    },
  );

  server.registerTool(
    "inspect_repository_status",
    {
      title: "Inspect repository status",
      description:
        "Return an instant LEIO Code snapshot: index freshness, optional workspace facets, and current capabilities. Provide repo_url to analyze an allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand.",
      inputSchema: {
        repo_root: z
          .string()
          .describe("Absolute repository root on the server to inspect.")
          .optional(),
        repo_url: z
          .string()
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to inspect.")
          .optional(),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
          .optional(),
      },
      securitySchemes: auth.getSecuritySchemes("inspect_repository_status"),
      annotations: readOnlyAnnotations,
      outputSchema: LeioToolOutputSchema,
      _meta: buildToolMeta(
        "Inspecting…",
        "Status ready",
        auth.getSecuritySchemes("inspect_repository_status"),
      ),
    },
    async ({ repo_root, repo_url, git_ref }, extra) => {
      const authError = auth.ensureToolAccess("inspect_repository_status", extra);
      if (authError) {
        return authError;
      }
      return buildStatusToolResult({
        repoRoot: repo_root,
        repoUrl: repo_url,
        gitRef: git_ref,
        sessionId: extra?.sessionId ?? null,
        authInfo: extra?.authInfo ?? null,
      });
    },
  );

  server.registerTool(
    "prepare_repository_context",
    {
      title: "Prepare repository context",
      description:
        "Build an agent-ready context bundle for a task, including ordered zones, instruction sources, memory sources, verification anchors, ranked files, symbols, graph follow-ups, tests, risk notes, and doctor suggestions. Provide repo_url to analyze an allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand.",
      inputSchema: {
        repo_root: z
          .string()
          .describe("Absolute repository root on the server to inspect.")
          .optional(),
        repo_url: z
          .string()
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to inspect.")
          .optional(),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
          .optional(),
        task: z
          .string()
          .min(1)
          .describe("Natural-language task, symbol, path, env var, Redis key, or feature area to prepare context for."),
        limit: z
          .number()
          .int()
          .min(1)
          .max(40)
          .default(8)
          .describe("Maximum number of ranked files/entities to include."),
      },
      securitySchemes: auth.getSecuritySchemes("prepare_repository_context"),
      annotations: readOnlyAnnotations,
      outputSchema: LeioToolOutputSchema,
      _meta: buildToolMeta(
        "Preparing context…",
        "Context ready",
        auth.getSecuritySchemes("prepare_repository_context"),
      ),
    },
    async ({ repo_root, repo_url, git_ref, task, limit }, extra) => {
      const authError = auth.ensureToolAccess("prepare_repository_context", extra);
      if (authError) {
        return authError;
      }
      return invokeLeioTool(["context", task, "--limit", String(limit ?? 8)], {
        repoRoot: repo_root,
        repoUrl: repo_url,
        gitRef: git_ref,
        sessionId: extra?.sessionId ?? null,
        authInfo: extra?.authInfo ?? null,
      });
    },
  );

  if (vigorosBridge.mcpUrl) {
    server.registerTool(
      "consult_carlos_motta_specialist",
      {
        title: "Consult Carlos Motta specialist",
        description:
          "Send the active repository context to the Carlos Motta digital twin for specialist reasoning on governance, compliance, controls, disclosure, and regulatory implications grounded in LEIO Code evidence.",
        inputSchema: {
          question: z
            .string()
            .min(1)
            .describe("Specialist question to ask Carlos Motta about the selected repository."),
          depth: z
            .enum(vigorosDepths)
            .default("quick")
            .describe("How deep Carlos Motta should investigate before answering."),
          include_audit: z
            .boolean()
            .default(false)
            .describe("Include a LEIO doctor summary in the evidence packet before asking Carlos Motta."),
          audit_kind: z
            .enum(doctorKinds)
            .default("all")
            .describe("Doctor suite to include when include_audit=true."),
          repo_root: z
            .string()
            .describe("Absolute repository root on the server to inspect.")
            .optional(),
          repo_url: z
            .string()
            .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to analyze.")
            .optional(),
          git_ref: z
            .string()
            .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
            .optional(),
        },
        securitySchemes: auth.getSecuritySchemes("consult_carlos_motta_specialist"),
        annotations: specialistAnnotations,
        outputSchema: LeioSpecialistOutputSchema,
        _meta: buildToolMeta(
          "Consulting Carlos Motta…",
          "Carlos Motta review ready",
          auth.getSecuritySchemes("consult_carlos_motta_specialist"),
        ),
      },
      async ({ question, depth, include_audit, audit_kind, repo_root, repo_url, git_ref }, extra) => {
        const authError = auth.ensureToolAccess("consult_carlos_motta_specialist", extra);
        if (authError) {
          return authError;
        }
        return buildCarlosMottaSpecialistResult({
          question,
          depth,
          include_audit,
          audit_kind,
          repo_root,
          repo_url,
          git_ref,
          sessionId: extra?.sessionId ?? null,
          authInfo: extra?.authInfo ?? null,
        });
      },
    );
  }

  server.registerTool(
    "search_repository",
    {
      title: "Search repository",
      description:
        "Search indexed repository entities such as symbols, env vars, Redis keys, routes, services, and optional deploy topology when present. Provide repo_url to analyze an allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand.",
      inputSchema: {
        repo_root: z
          .string()
          .describe("Absolute repository root on the server to inspect.")
          .optional(),
        repo_url: z
          .string()
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to inspect.")
          .optional(),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
          .optional(),
        kind: z.enum(findKinds),
        needle: z.string().min(1),
      },
      securitySchemes: auth.getSecuritySchemes("search_repository"),
      annotations: readOnlyAnnotations,
      outputSchema: LeioToolOutputSchema,
      _meta: buildToolMeta(
        "Searching…",
        "Results ready",
        auth.getSecuritySchemes("search_repository"),
      ),
    },
    async ({ repo_root, repo_url, git_ref, kind, needle }, extra) => {
      const authError = auth.ensureToolAccess("search_repository", extra);
      if (authError) {
        return authError;
      }
      return invokeLeioTool(["find", kind, needle], {
        repoRoot: repo_root,
        repoUrl: repo_url,
        gitRef: git_ref,
        sessionId: extra?.sessionId ?? null,
        authInfo: extra?.authInfo ?? null,
      });
    },
  );

  server.registerTool(
    "search_repository_memory",
    {
      title: "Search repository memory",
      description:
        "Use this when exact entity lookup is insufficient and semantic or FCA-backed retrieval is needed across the selected repository. Runs read-only `leio-code find symbol` over the local Arrow node store (`.leio-code/exports/arrow-nodes-v1/nodes.arrow`); it never mutates the repository. Provide repo_url to analyze an allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand.",
      inputSchema: {
        repo_root: z
          .string()
          .describe("Absolute repository root on the server to inspect.")
          .optional(),
        repo_url: z
          .string()
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to inspect.")
          .optional(),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
          .optional(),
        query: z
          .string()
          .trim()
          .min(1)
          .max(500)
          .describe("Natural-language semantic query over the selected repository memory."),
        limit: z
          .number()
          .int()
          .min(1)
          .max(50)
          .default(8)
          .describe("Maximum number of ranked memory matches to return."),
      },
      securitySchemes: auth.getSecuritySchemes("search_repository_memory"),
      annotations: readOnlyAnnotations,
      outputSchema: LeioToolOutputSchema,
      _meta: buildToolMeta(
        "Searching repository memory…",
        "Repository memory ready",
        auth.getSecuritySchemes("search_repository_memory"),
      ),
    },
    async ({ repo_root, repo_url, git_ref, query, limit }, extra) => {
      const authError = auth.ensureToolAccess("search_repository_memory", extra);
      if (authError) {
        return authError;
      }
      return invokeLeioTool(
        ["find", "symbol", query, "--limit", String(limit ?? 8)],
        {
          repoRoot: repo_root,
          repoUrl: repo_url,
          gitRef: git_ref,
          sessionId: extra?.sessionId ?? null,
          authInfo: extra?.authInfo ?? null,
        },
      );
    },
  );

  server.registerTool(
    "explain_repository",
    {
      title: "Explain repository entity",
      description:
        "Explain runtime lineage or operational meaning for env vars, Redis keys, and optional deploy or cartridge topology. Provide repo_url to analyze an allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand.",
      inputSchema: {
        repo_root: z
          .string()
          .describe("Absolute repository root on the server to inspect.")
          .optional(),
        repo_url: z
          .string()
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to inspect.")
          .optional(),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
          .optional(),
        kind: z.enum(explainKinds),
        needle: z.string().optional(),
      },
      securitySchemes: auth.getSecuritySchemes("explain_repository"),
      annotations: readOnlyAnnotations,
      outputSchema: LeioToolOutputSchema,
      _meta: buildToolMeta(
        "Explaining…",
        "Explanation ready",
        auth.getSecuritySchemes("explain_repository"),
      ),
    },
    async ({ repo_root, repo_url, git_ref, kind, needle }, extra) => {
      const authError = auth.ensureToolAccess("explain_repository", extra);
      if (authError) {
        return authError;
      }
      if (!needle) {
        throw new Error(`needle is required for explain kind "${kind}"`);
      }
      const args = ["explain", kind];
      if (needle) {
        args.push(needle);
      }
      return invokeLeioTool(args, {
        repoRoot: repo_root,
        repoUrl: repo_url,
        gitRef: git_ref,
        sessionId: extra?.sessionId ?? null,
        authInfo: extra?.authInfo ?? null,
      });
    },
  );

  server.registerTool(
    "audit_repository_contracts",
    {
      title: "Audit repository contracts",
      description:
        "Run LEIO Code doctors (CLI `leio-code doctor`) when the repository profile exposes them. Use `all` for a full sweep or a targeted doctor kind. This is NOT the composite CLI `leio-code audit --strict` — use `audit_repository_rollup` for that. Provide repo_url to analyze an allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand.",
      inputSchema: {
        repo_root: z
          .string()
          .describe("Absolute repository root on the server to inspect.")
          .optional(),
        repo_url: z
          .string()
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to inspect.")
          .optional(),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
          .optional(),
        kind: z.enum(doctorKinds).default("all"),
      },
      securitySchemes: auth.getSecuritySchemes("audit_repository_contracts"),
      annotations: readOnlyAnnotations,
      outputSchema: LeioToolOutputSchema,
      _meta: buildToolMeta(
        "Auditing…",
        "Audit ready",
        auth.getSecuritySchemes("audit_repository_contracts"),
      ),
    },
    async ({ repo_root, repo_url, git_ref, kind }, extra) => {
      const authError = auth.ensureToolAccess(
        "audit_repository_contracts",
        extra,
      );
      if (authError) {
        return authError;
      }
      return invokeLeioTool(["doctor", kind], {
        repoRoot: repo_root,
        repoUrl: repo_url,
        gitRef: git_ref,
        sessionId: extra?.sessionId ?? null,
        authInfo: extra?.authInfo ?? null,
        timeoutMs: 30000,
      });
    },
  );


  server.registerTool(
    "graph_repository",
    {
      title: "Graph repository topology",
      description:
        "Structural code topology: who calls what, who imports what, what symbols live in a file. Prefer this over search_repository for call-chain and dependency questions. Maps to CLI `leio-code graph`. Provide repo_url to analyze an allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand.",
      inputSchema: {
        repo_root: z
          .string()
          .describe("Absolute repository root on the server to inspect.")
          .optional(),
        repo_url: z
          .string()
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to inspect.")
          .optional(),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
          .optional(),
        kind: z.enum(graphKinds).describe("Structural graph query kind."),
        needle: z
          .string()
          .min(1)
          .describe("Symbol name, file path, or import literal. Omit for kind=dead-code.")
          .optional(),
      },
      securitySchemes: auth.getSecuritySchemes("graph_repository"),
      annotations: readOnlyAnnotations,
      outputSchema: LeioToolOutputSchema,
      _meta: buildToolMeta(
        "Graphing…",
        "Graph ready",
        auth.getSecuritySchemes("graph_repository"),
      ),
    },
    async ({ repo_root, repo_url, git_ref, kind, needle }, extra) => {
      const authError = auth.ensureToolAccess("graph_repository", extra);
      if (authError) {
        return authError;
      }
      if (kind !== "dead-code" && !needle) {
        throw new Error(`graph kind ${kind} requires needle`);
      }
      const graphArgs = ["graph", kind];
      if (needle) {
        graphArgs.push(needle);
      }
      return invokeLeioTool(graphArgs, {
        repoRoot: repo_root,
        repoUrl: repo_url,
        gitRef: git_ref,
        sessionId: extra?.sessionId ?? null,
        authInfo: extra?.authInfo ?? null,
      });
    },
  );

  server.registerTool(
    "guide_repository_tools",
    {
      title: "Guide repository tools",
      description:
        "Explain when to use each LEIO Code Apps SDK tool family (status, capabilities, context, exact search, semantic repository memory, explain, graph, doctor, audit rollup). Prefer this when unsure which tool to call. See also skills/leio-code/SKILL.md.",
      inputSchema: {
        topic: z
          .enum(GUIDE_TOPICS)
          .default("general")
          .describe("Which LEIO Code workflow to explain."),
        repo_root: z
          .string()
          .describe("Absolute repository root on the server to inspect.")
          .optional(),
        repo_url: z
          .string()
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to inspect.")
          .optional(),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
          .optional(),
      },
      securitySchemes: auth.getSecuritySchemes("guide_repository_tools"),
      annotations: readOnlyAnnotations,
      outputSchema: LeioGuideToolOutputSchema,
      _meta: buildToolMeta(
        "Guiding…",
        "Guide ready",
        auth.getSecuritySchemes("guide_repository_tools"),
      ),
    },
    async ({ topic, repo_root, repo_url, git_ref }, extra) => {
      const authError = auth.ensureToolAccess("guide_repository_tools", extra);
      if (authError) {
        return authError;
      }
      let capabilities = null;
      let resolvedRoot = null;
      try {
        if (topic !== "conversation") {
          const capsResult = await invokeLeioTool(["capabilities"], {
            repoRoot: repo_root,
            repoUrl: repo_url,
            gitRef: git_ref,
            sessionId: extra?.sessionId ?? null,
            authInfo: extra?.authInfo ?? null,
          });
          if (!capsResult.isError) {
            capabilities =
              capsResult.structuredContent?.workspace_capabilities ??
              capsResult.structuredContent?.envelope?.meta?.workspace_capabilities ??
              null;
            if (repo_root || repo_url) {
              resolvedRoot = capsResult.structuredContent?.repo_root ?? null;
            }
          }
        }
      } catch {
        capabilities = null;
      }
      const built = buildGuideStructuredContent(topic ?? "general", {
        capabilities,
        nextTools: guideNextTools(topic, capabilities)
          .filter((tool) => tool !== "leio_code_export" && Object.hasOwn(appToolNameMap, tool))
          .map(mapToolNameToAppSurface),
        routingDoc: "skills/leio-code/SKILL.md",
        routingNote:
          "Apps SDK tool names: inspect_*/prepare_*/search_repository (exact index lookup) / search_repository_memory (local Arrow semantic retrieval) / explain_*/graph_*/audit_repository_contracts (doctor) / audit_repository_rollup (CLI audit). Full routing: skills/leio-code/SKILL.md.",
      });
      const workspaceCapabilityHints = buildWorkspaceCapabilityHints(capabilities);
      return {
        content: [{ type: "text", text: built.text }],
        structuredContent: {
          ...built.structuredContent,
          ...(resolvedRoot === null ? {} : { repo_root: resolvedRoot }),
          workspace_capability_hints: workspaceCapabilityHints,
          action_palette: guideActionPalette(
            buildAppsSdkActionPalette(buildActionPalette(workspaceCapabilityHints)),
            built.structuredContent.next_tools,
          ),
        },
      };
    },
  );

  server.registerTool(
    "audit_repository_rollup",
    {
      title: "Audit repository rollup",
      description:
        "Composite pre-deploy audit matching CLI `leio-code audit`: status snapshot + every doctor + capabilities. Set strict=true for the same non-zero exit behavior as `leio-code audit --strict`. Do not confuse with `audit_repository_contracts`, which only runs doctors.",
      inputSchema: {
        repo_root: z
          .string()
          .describe("Absolute repository root on the server to inspect.")
          .optional(),
        repo_url: z
          .string()
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to inspect.")
          .optional(),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout when repo_url is used.")
          .optional(),
        strict: z
          .boolean()
          .default(false)
          .describe("When true, fail if any doctor reports a warning (CLI --strict)."),
        format: z
          .enum(["markdown", "json"])
          .default("markdown")
          .describe("Report format."),
      },
      securitySchemes: auth.getSecuritySchemes("audit_repository_rollup"),
      annotations: readOnlyAnnotations,
      outputSchema: LeioToolOutputSchema,
      _meta: buildToolMeta(
        "Auditing rollup…",
        "Rollup ready",
        auth.getSecuritySchemes("audit_repository_rollup"),
      ),
    },
    async ({ repo_root, repo_url, git_ref, strict, format }, extra) => {
      const authError = auth.ensureToolAccess("audit_repository_rollup", extra);
      if (authError) {
        return authError;
      }
      const args = ["audit", "--format", format ?? "markdown"];
      if (strict) {
        args.push("--strict");
      }
      return invokeLeioTool(args, {
        repoRoot: repo_root,
        repoUrl: repo_url,
        gitRef: git_ref,
        sessionId: extra?.sessionId ?? null,
        authInfo: extra?.authInfo ?? null,
        timeoutMs: defaultTimeoutMs,
      });
    },
  );

  server.registerTool(
    "select_repository_target",
    {
      title: "Select repository target",
      description:
        "Persist the active repository for this ChatGPT session. Supports allowlisted HTTPS public repo URLs directly and private GitHub repositories after the user connects the GitHub App.",
      inputSchema: {
        repo_url: z
          .string()
          .min(1)
          .describe("Allowlisted HTTPS Git repository URL or GitHub owner/repo shorthand to analyze."),
        git_ref: z
          .string()
          .describe("Optional branch, tag, or commit to checkout.")
          .optional(),
      },
      securitySchemes: auth.getSecuritySchemes("inspect_repository_status"),
      annotations: sessionWriteAnnotations,
      outputSchema: LeioSessionTargetOutputSchema,
      _meta: buildToolMeta(
        "Selecting repository…",
        "Repository selected",
        auth.getSecuritySchemes("inspect_repository_status"),
      ),
    },
    async ({ repo_url, git_ref }, extra) => {
      const authError = auth.ensureToolAccess("inspect_repository_status", extra);
      if (authError) {
        return authError;
      }
      if (!extra?.sessionId) {
        throw new Error("select_repository_target requires an MCP session");
      }
      return buildStatusToolResult({
        repoUrl: repo_url,
        gitRef: git_ref,
        sessionId: extra.sessionId,
        authInfo: extra?.authInfo ?? null,
      });
    },
  );

  server.registerTool(
    "clear_repository_target",
    {
      title: "Clear repository target",
      description:
        "Clear the active repository selection for this ChatGPT session and fall back to the server default repository.",
      inputSchema: {},
      securitySchemes: auth.getSecuritySchemes("inspect_repository_status"),
      annotations: sessionWriteAnnotations,
      outputSchema: LeioSessionTargetOutputSchema,
      _meta: buildToolMeta(
        "Clearing repository…",
        "Repository cleared",
        auth.getSecuritySchemes("inspect_repository_status"),
      ),
    },
    async (_args, extra) => {
      const authError = auth.ensureToolAccess("inspect_repository_status", extra);
      if (authError) {
        return authError;
      }
      if (extra?.sessionId) {
        sessionRepoTargets.delete(extra.sessionId);
      }
      return buildStatusToolResult({
        sessionId: extra?.sessionId ?? null,
        authInfo: extra?.authInfo ?? null,
      });
    },
  );

  installModernProtocol(server, {
    implementation: IMPLEMENTATION_APPS_SDK,
    capabilities: appsSdkServerOptions().capabilities,
    instructions: appsSdkServerOptions().instructions,
  });
  return server;
}

const allowedHosts = buildAllowedHosts();
const app = createMcpExpressApp({
  host,
  // When binding beyond loopback (Docker --network host / 0.0.0.0), restrict Host.
  ...(host === "0.0.0.0" || host === "::" ? { allowedHosts } : {}),
});
const transports = {};

app.use((req, res, next) => {
  if (req.path === "/mcp" || req.path.startsWith("/mcp/")) {
    applyMcpCorsHeaders(req, res);
    if (req.method === "OPTIONS") {
      res.status(204).end();
      return;
    }
  }
  next();
});

app.get("/", (_req, res) => {
  res.json({
    name: IMPLEMENTATION_APPS_SDK.name,
    title: IMPLEMENTATION_APPS_SDK.title,
    version: IMPLEMENTATION_APPS_SDK.version,
    protocolVersion: PROTOCOL_VERSION,
    supportedProtocolVersions: SUPPORTED_PROTOCOL_VERSIONS,
    status: "ok",
    mcp_endpoint: "/mcp",
    widget: widgetUri,
    auth: auth.summary(),
    source_control: {
      supports_repo_url: true,
      supports_server_repo_root: allowServerRepoRoot,
      allow_server_repo_root: allowServerRepoRoot,
      ...(allowServerRepoRoot ? { default_repo_root: defaultRepoRoot } : {}),
      github_app_configured: githubApp.configured,
      github_app_slug: githubApp.slug,
    },
    specialist_bridge: buildVigorosBridgeInfo(),
    legal: {
      privacy_policy_url: legal.privacyUrl,
      support_url: legal.supportUrl,
      terms_url: legal.termsUrl,
      configured: legal.missingItems.length === 0,
      missing: legal.missingItems,
    },
  });
});

app.get("/health", (_req, res) => {
  res.json({
    ok: true,
    name: IMPLEMENTATION_APPS_SDK.name,
    version: IMPLEMENTATION_APPS_SDK.version,
    protocolVersion: PROTOCOL_VERSION,
    supportedProtocolVersions: SUPPORTED_PROTOCOL_VERSIONS,
    auth: auth.summary(),
    source_control: {
      supports_repo_url: true,
      supports_server_repo_root: allowServerRepoRoot,
      allow_server_repo_root: allowServerRepoRoot,
      ...(allowServerRepoRoot ? { default_repo_root: defaultRepoRoot } : {}),
      github_app_configured: githubApp.configured,
      github_app_slug: githubApp.slug,
    },
    specialist_bridge: buildVigorosBridgeInfo(),
    legal: {
      privacy_policy_url: legal.privacyUrl,
      support_url: legal.supportUrl,
      terms_url: legal.termsUrl,
      configured: legal.missingItems.length === 0,
      missing: legal.missingItems,
    },
  });
});

app.get("/widget", (_req, res) => {
  res.type("html").send(fs.readFileSync(widgetPath, "utf8"));
});

app.get("/privacy", (_req, res) => {
  res.type("html").send(renderTemplate(privacyPagePath));
});

app.get("/support", (_req, res) => {
  res.type("html").send(renderTemplate(supportPagePath));
});

app.get("/terms", (_req, res) => {
  res.type("html").send(renderTemplate(termsPagePath));
});

app.get("/github/app/status", (req, res) => {
  const sessionId = typeof req.query.mcp_session_id === "string"
    ? req.query.mcp_session_id.trim()
    : null;
  res.json({
    ok: true,
    mcp_session_id: sessionId,
    source_control: buildSourceControlInfo(sessionId),
  });
});

app.get("/github/app/connect", (req, res) => {
  if (!githubApp.configured) {
    res
      .status(503)
      .type("html")
      .send(
        renderSimpleHtmlPage(
          "GitHub App unavailable",
          "<p>The GitHub App source connector is not configured on this deployment.</p>",
        ),
      );
    return;
  }

  const mcpSessionId = typeof req.query.mcp_session_id === "string"
    ? req.query.mcp_session_id.trim()
    : null;
  if (!mcpSessionId) {
    res
      .status(400)
      .type("html")
      .send(
        renderSimpleHtmlPage(
          "Missing MCP session",
          "<p>This GitHub connection flow must start from an active ChatGPT session.</p>",
        ),
      );
    return;
  }

  pruneGitHubConnectStates();
  const state = randomUUID();
  const redirectUrl = typeof req.query.redirectUrl === "string"
    ? req.query.redirectUrl
    : null;

  githubConnectStates.set(state, {
    mcpSessionId,
    redirectUrl,
    createdAt: Date.now(),
  });

  const authorizeUrl = withQueryParams("https://github.com/login/oauth/authorize", {
    client_id: githubApp.clientId,
    redirect_uri: githubApp.callbackUrl,
    state,
    allow_signup: "false",
  });

  res.redirect(authorizeUrl);
});

app.get("/github/app/callback", async (req, res) => {
  pruneGitHubConnectStates();

  const state = typeof req.query.state === "string" ? req.query.state : null;
  const pending = state ? githubConnectStates.get(state) ?? null : null;
  if (state) {
    githubConnectStates.delete(state);
  }

  const complete = ({ ok, title, message }) => {
    const redirectUrl = pending?.redirectUrl ?? null;
    if (redirectUrl) {
      const nextUrl = new URL(redirectUrl);
      nextUrl.searchParams.set("leio_github", ok ? "connected" : "error");
      if (!ok) {
        nextUrl.searchParams.set("leio_github_message", message);
      }
      res.redirect(nextUrl.toString());
      return;
    }

    res
      .status(ok ? 200 : 400)
      .type("html")
      .send(
        renderSimpleHtmlPage(
          title,
          `<p>${escapeHtml(message)}</p><p>Return to ChatGPT and refresh repository status.</p>`,
        ),
      );
  };

  if (!pending?.mcpSessionId) {
    complete({
      ok: false,
      title: "GitHub connection expired",
      message: "The connection state is missing or expired. Start the GitHub connection again from ChatGPT.",
    });
    return;
  }

  if (typeof req.query.error === "string") {
    complete({
      ok: false,
      title: "GitHub authorization failed",
      message: req.query.error_description || req.query.error,
    });
    return;
  }

  const code = typeof req.query.code === "string" ? req.query.code : null;
  if (!code || !state) {
    complete({
      ok: false,
      title: "Missing authorization code",
      message: "GitHub did not return an authorization code.",
    });
    return;
  }

  try {
    const tokenPayload = await exchangeGitHubCodeForUserToken({ code, state });
    const accessToken = tokenPayload?.access_token ?? null;
    if (!accessToken) {
      throw new Error("GitHub did not return an access token.");
    }

    const hydratedSession = await hydrateGitHubSession(accessToken);
    githubSessions.set(pending.mcpSessionId, {
      ...hydratedSession,
      scope: tokenPayload?.scope ?? null,
      tokenType: tokenPayload?.token_type ?? null,
      expiresAt: tokenPayload?.expires_in
        ? Date.now() + Number(tokenPayload.expires_in) * 1000
        : null,
      connectedAt: Date.now(),
    });

    complete({
      ok: true,
      title: "GitHub connected",
      message: `Connected ${hydratedSession.login ?? "GitHub"} with ${hydratedSession.repositories.length} accessible repositories.`,
    });
  } catch (error) {
    console.error("[LEIO Apps SDK] GitHub callback failed", error);
    complete({
      ok: false,
      title: "GitHub connection failed",
      message: error instanceof Error ? error.message : "Unexpected GitHub callback error.",
    });
  }
});

if (auth.oauthUiEnabled) {
  app.get(auth.resourceMetadataPath, (_req, res) => {
    res.json(auth.metadataDocument());
  });
}

app.post("/mcp", async (req, res) => {
  const sessionId = req.headers["mcp-session-id"];

  try {
    const ok = await auth.maybeAttachAuthInfo(req, res);
    if (!ok) {
      return;
    }
    let transport;
    if (sessionId && transports[sessionId]) {
      transport = transports[sessionId];
    } else {
      transport = new StreamableHTTPServerTransport({
        sessionIdGenerator: () => randomUUID(),
        onsessioninitialized: (nextSessionId) => {
          transports[nextSessionId] = transport;
        },
      });

      transport.onclose = () => {
        const sid = transport.sessionId;
        if (sid && transports[sid]) {
          delete transports[sid];
        }
        clearSessionRuntimeState(sid);
      };

      const server = getServer();
      await server.connect(transport);
    }

    await transport.handleRequest(req, res, req.body);
  } catch (error) {
    console.error("[LEIO Apps SDK] POST /mcp failed", error);
    if (!res.headersSent) {
      res.status(500).json({
        jsonrpc: "2.0",
        error: {
          code: -32603,
          message: "Internal server error",
        },
        id: null,
      });
    }
  }
});

app.get("/mcp", async (req, res) => {
  const sessionId = req.headers["mcp-session-id"];
  const ok = await auth.maybeAttachAuthInfo(req, res);
  if (!ok) {
    return;
  }
  if (!sessionId || !transports[sessionId]) {
    res.status(400).send("Invalid or missing session ID");
    return;
  }

  try {
    await transports[sessionId].handleRequest(req, res);
  } catch (error) {
    console.error("[LEIO Apps SDK] GET /mcp failed", error);
    if (!res.headersSent) {
      res.status(500).send("Internal server error");
    }
  }
});

app.delete("/mcp", async (req, res) => {
  const sessionId = req.headers["mcp-session-id"];
  const ok = await auth.maybeAttachAuthInfo(req, res);
  if (!ok) {
    return;
  }
  if (!sessionId || !transports[sessionId]) {
    res.status(400).send("Invalid or missing session ID");
    return;
  }

  try {
    await transports[sessionId].handleRequest(req, res);
  } catch (error) {
    console.error("[LEIO Apps SDK] DELETE /mcp failed", error);
    if (!res.headersSent) {
      res.status(500).send("Internal server error");
    }
  }
});

app.listen(port, host, () => {
  console.error(
    `[LEIO Apps SDK] listening on http://${host}:${port} (MCP endpoint: /mcp)`,
  );
});
