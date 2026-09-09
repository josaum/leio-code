#!/usr/bin/env node

import http from "node:http";
import { randomBytes } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";

import {
  buildPkceAuthorizationUrl,
  exchangeAuthorizationCode,
} from "./pkce.mjs";

function parseArgs(argv) {
  const args = { scopes: "openid repo.read" };
  for (let i = 0; i < argv.length; i += 1) {
    const value = argv[i];
    if (value === "--issuer") args.issuer = argv[++i];
    else if (value === "--client-id") args.clientId = argv[++i];
    else if (value === "--client-secret") args.clientSecret = argv[++i];
    else if (value === "--redirect-uri") args.redirectUri = argv[++i];
    else if (value === "--scopes") args.scopes = argv[++i];
    else if (value === "--resource") args.resource = argv[++i];
    else if (value === "--code") args.code = argv[++i];
    else if (value === "--code-verifier") args.codeVerifier = argv[++i];
    else if (value === "--state") args.state = argv[++i];
    else if (value === "--nonce") args.nonce = argv[++i];
    else if (value === "--print-url") args.printUrl = true;
    else if (value === "--exchange-code") args.exchangeCode = true;
    else if (value === "--json") args.json = true;
    else if (value === "--prompt") args.prompt = argv[++i];
    else if (value === "--open-browser") args.openBrowser = true;
    else if (value === "--no-open") args.openBrowser = false;
    else if (value === "--output") args.output = argv[++i];
    else if (value === "--timeout-ms") args.timeoutMs = Number(argv[++i]);
  }
  return args;
}

function envOr(...values) {
  return values.find((value) => typeof value === "string" && value.trim())?.trim() ?? null;
}

function buildIssuer({ baseUrl, realm }) {
  const normalizedBase = (baseUrl ?? "http://localhost:8080").replace(/\/$/, "");
  const normalizedRealm = (realm ?? "leio-code").replace(/^\/+|\/+$/g, "");
  return `${normalizedBase}/realms/${normalizedRealm}`;
}

function buildResourceUrl() {
  const publicUrl = process.env.LEIO_APPS_SDK_PUBLIC_URL?.trim();
  if (publicUrl) {
    return new URL("/mcp", publicUrl).href;
  }
  const host = process.env.LEIO_APPS_SDK_HOST?.trim() || "127.0.0.1";
  const port = process.env.LEIO_APPS_SDK_PORT?.trim() || "3333";
  return `http://${host}:${port}/mcp`;
}

function isLoopbackRedirect(redirectUri) {
  const url = new URL(redirectUri);
  return url.hostname === "127.0.0.1" || url.hostname === "localhost";
}

function openInBrowser(url) {
  const candidates =
    process.platform === "darwin"
      ? [["open", [url]]]
      : process.platform === "win32"
        ? [["cmd", ["/c", "start", "", url]]]
        : [["xdg-open", [url]]];

  for (const [command, args] of candidates) {
    const child = spawn(command, args, {
      stdio: "ignore",
      detached: true,
    });
    child.on("error", () => {});
    child.unref();
    return true;
  }
  return false;
}

function renderCallbackHtml({ ok, title, body }) {
  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>${title}</title>
    <style>
      body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; margin: 2rem; color: #111827; }
      .card { max-width: 44rem; padding: 1.5rem; border-radius: 1rem; background: ${ok ? "#ecfdf5" : "#fef2f2"}; border: 1px solid ${ok ? "#10b981" : "#ef4444"}; }
      code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
    </style>
  </head>
  <body>
    <div class="card">
      <h1>${title}</h1>
      <p>${body}</p>
      <p>You can close this tab.</p>
    </div>
  </body>
</html>`;
}

async function waitForAuthorizationCode({
  redirectUri,
  expectedState,
  timeoutMs,
}) {
  const redirect = new URL(redirectUri);
  if (!isLoopbackRedirect(redirectUri)) {
    throw new Error("Automatic human login only supports loopback redirect URIs.");
  }

  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      const requestUrl = new URL(req.url ?? "/", redirect);
      if (requestUrl.pathname !== redirect.pathname) {
        res.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
        res.end("Not found.");
        return;
      }

      const error = requestUrl.searchParams.get("error");
      const errorDescription = requestUrl.searchParams.get("error_description");
      const state = requestUrl.searchParams.get("state");
      const code = requestUrl.searchParams.get("code");

      if (error) {
        res.writeHead(400, { "content-type": "text/html; charset=utf-8" });
        res.end(
          renderCallbackHtml({
            ok: false,
            title: "Login failed",
            body: `${error}${errorDescription ? `: ${errorDescription}` : ""}`,
          }),
        );
        server.close();
        reject(new Error(`Authorization failed: ${errorDescription || error}`));
        return;
      }

      if (state !== expectedState) {
        res.writeHead(400, { "content-type": "text/html; charset=utf-8" });
        res.end(
          renderCallbackHtml({
            ok: false,
            title: "State mismatch",
            body: "The returned state does not match the PKCE session.",
          }),
        );
        server.close();
        reject(new Error("Authorization callback state mismatch."));
        return;
      }

      if (!code) {
        res.writeHead(400, { "content-type": "text/html; charset=utf-8" });
        res.end(
          renderCallbackHtml({
            ok: false,
            title: "Missing code",
            body: "The authorization server did not return a code.",
          }),
        );
        server.close();
        reject(new Error("Authorization callback did not include a code."));
        return;
      }

      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(
        renderCallbackHtml({
          ok: true,
          title: "Login complete",
          body: "The token exchange is running locally.",
        }),
      );
      server.close();
      resolve(code);
    });

    server.on("error", reject);
    server.listen(Number(redirect.port), redirect.hostname, () => {});

    const timeout = setTimeout(() => {
      server.close();
      reject(
        new Error(
          `Timed out waiting for the Keycloak callback after ${timeoutMs}ms.`,
        ),
      );
    }, timeoutMs);

    server.on("close", () => clearTimeout(timeout));
  });
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const keycloakBaseUrl = envOr(
    process.env.KEYCLOAK_BASE_URL,
    "http://localhost:8080",
  );
  const keycloakRealm = envOr(process.env.KEYCLOAK_REALM, "leio-code");
  const issuer = envOr(
    args.issuer,
    process.env.KEYCLOAK_ISSUER,
    process.env.LEIO_APPS_SDK_JWT_ISSUER,
    buildIssuer({ baseUrl: keycloakBaseUrl, realm: keycloakRealm }),
  );
  const clientId = envOr(
    args.clientId,
    process.env.KEYCLOAK_CLIENT_ID,
    process.env.LEIO_KEYCLOAK_LOCAL_DEV_CLIENT_ID,
    "leio-code-local-dev",
  );
  const redirectUri = envOr(
    args.redirectUri,
    process.env.KEYCLOAK_REDIRECT_URI,
    process.env.LEIO_KEYCLOAK_LOCAL_REDIRECT_URI,
    "http://127.0.0.1:8787/callback",
  );
  const resource = envOr(
    args.resource,
    process.env.KEYCLOAK_RESOURCE,
    buildResourceUrl(),
  );
  const scopes = envOr(
    args.scopes,
    process.env.KEYCLOAK_SCOPES,
    process.env.LEIO_KEYCLOAK_DEV_SCOPE,
    "openid repo.read",
  )
    .split(/[,\s]+/)
    .map((item) => item.trim())
    .filter(Boolean);
  const outputPath = envOr(
    args.output,
    process.env.KEYCLOAK_TOKEN_OUTPUT,
    process.env.LEIO_KEYCLOAK_TOKEN_OUTPUT,
    null,
  );
  const timeoutMs = Number.isFinite(args.timeoutMs)
    ? args.timeoutMs
    : Number(process.env.LEIO_KEYCLOAK_LOGIN_TIMEOUT_MS ?? 180000);
  const shouldOpenBrowser =
    args.openBrowser ??
    !["0", "false", "no", "off"].includes(
      (process.env.LEIO_KEYCLOAK_AUTO_OPEN ?? "true").toLowerCase(),
    );

  if (!issuer || !clientId || !redirectUri) {
    throw new Error(
      "issuer, clientId and redirectUri are required (via flags or KEYCLOAK_* env vars)",
    );
  }

  const state = args.state ?? base64Url(randomBytes(16));
  const nonce = args.nonce ?? base64Url(randomBytes(16));
  const { authorization_url, code_verifier, code_challenge } =
    buildPkceAuthorizationUrl({
      issuerUrl: issuer,
      clientId,
      redirectUri,
      scopes,
      resource,
      state,
      nonce,
      prompt: args.prompt ?? "login",
    });

  if (args.exchangeCode) {
    if (!args.code) {
      throw new Error("--code is required with --exchange-code");
    }
    const verifier = envOr(
      args.codeVerifier,
      process.env.KEYCLOAK_CODE_VERIFIER,
    );
    if (!verifier) {
      throw new Error(
        "--code-verifier or KEYCLOAK_CODE_VERIFIER is required with --exchange-code",
      );
    }
    const token = await exchangeAuthorizationCode({
      issuerUrl: issuer,
      clientId,
      clientSecret: envOr(args.clientSecret, process.env.KEYCLOAK_CLIENT_SECRET),
      redirectUri,
      code: args.code,
      codeVerifier: verifier,
    });
    process.stdout.write(`${JSON.stringify(token, null, 2)}\n`);
    return;
  }

  if (!args.printUrl && !args.json) {
    const codePromise = waitForAuthorizationCode({
      redirectUri,
      expectedState: state,
      timeoutMs,
    });

    process.stdout.write(
      `waiting for browser login on ${redirectUri} with client ${clientId}\n`,
    );
    process.stdout.write(`authorization_url=${authorization_url}\n`);
    if (shouldOpenBrowser) {
      openInBrowser(authorization_url);
    }

    const code = await codePromise;
    const token = await exchangeAuthorizationCode({
      issuerUrl: issuer,
      clientId,
      clientSecret: envOr(args.clientSecret, process.env.KEYCLOAK_CLIENT_SECRET),
      redirectUri,
      code,
      codeVerifier: code_verifier,
    });

    if (outputPath) {
      await writeFile(outputPath, `${JSON.stringify(token, null, 2)}\n`, "utf8");
      process.stdout.write(`token written to ${outputPath}\n`);
    }

    process.stdout.write(`${JSON.stringify(token, null, 2)}\n`);
    return;
  }

  if (args.json) {
    process.stdout.write(
      `${JSON.stringify(
        {
          issuer,
          client_id: clientId,
          redirect_uri: redirectUri,
          scopes,
          resource,
          state,
          nonce,
          code_verifier,
          code_challenge,
          authorization_url,
        },
        null,
        2,
      )}\n`,
    );
    return;
  }

  process.stdout.write(`authorization_url=${authorization_url}\n`);
  process.stdout.write(`code_verifier=${code_verifier}\n`);
  process.stdout.write(`code_challenge=${code_challenge}\n`);
  process.stdout.write(`redirect_uri=${redirectUri}\n`);
  process.stdout.write(`issuer=${issuer}\n`);
  process.stdout.write(`client_id=${clientId}\n`);
  process.stdout.write(`resource=${resource}\n`);
}

function base64Url(buffer) {
  return buffer
    .toString("base64")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/g, "");
}

await main();
