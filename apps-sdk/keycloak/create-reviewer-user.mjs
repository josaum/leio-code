#!/usr/bin/env node

import { randomBytes } from "node:crypto";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const createDevUserScript = path.join(__dirname, "create-dev-user.mjs");

function envOr(...values) {
  return values.find((value) => typeof value === "string" && value.trim())?.trim() ?? null;
}

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    const value = argv[i];
    if (value === "--password") args.password = argv[++i];
    else if (value === "--username") args.username = argv[++i];
    else if (value === "--email") args.email = argv[++i];
    else if (value === "--json") args.json = true;
  }
  return args;
}

function generatePassword() {
  const raw = randomBytes(18).toString("base64url");
  return `LeioReview-${raw}`;
}

async function runCreateDevUser(argv) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [createDevUserScript, ...argv], {
      stdio: ["ignore", "pipe", "pipe"],
      env: process.env,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code !== 0) {
        reject(new Error(stderr.trim() || stdout.trim() || `exit ${code}`));
        return;
      }
      resolve(stdout.trim());
    });
  });
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const username = envOr(
    args.username,
    process.env.LEIO_KEYCLOAK_REVIEWER_USERNAME,
    "reviewer@getjai.com",
  );
  const email = envOr(
    args.email,
    process.env.LEIO_KEYCLOAK_REVIEWER_EMAIL,
    username,
  );
  const password = envOr(
    args.password,
    process.env.LEIO_KEYCLOAK_REVIEWER_PASSWORD,
    generatePassword(),
  );

  const forwarded = [
    "--username",
    username,
    "--email",
    email,
    "--password",
    password,
    "--first-name",
    "OpenAI",
    "--last-name",
    "Reviewer",
    "--tenant-id",
    envOr(process.env.LEIO_KEYCLOAK_REVIEWER_TENANT_ID, "public-demo"),
    "--json",
  ];

  const output = await runCreateDevUser(forwarded);
  const payload = JSON.parse(output);
  const result = {
    ...payload,
    login: username,
    password,
    issuer: envOr(
      process.env.KEYCLOAK_ISSUER,
      process.env.LEIO_APPS_SDK_JWT_ISSUER,
      "https://auth.getjai.com/realms/leio-code",
    ),
    mcp_url: envOr(
      process.env.LEIO_APPS_SDK_PUBLIC_URL
        ? `${process.env.LEIO_APPS_SDK_PUBLIC_URL.replace(/\/+$/, "")}/mcp`
        : null,
      "https://leio-code.getjai.com/mcp",
    ),
    submission_notes:
      "Plain password, email verified, no required actions, OTP/WebAuthn cleared. Paste login + password into the plugin submission portal only — do not commit the password.",
  };

  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

await main();
