import path from "node:path";
import { createHmac } from "node:crypto";

const MIN_HMAC_SECRET_BYTES = 32;

function requireHmacSecret(secret) {
  const normalized = typeof secret === "string" ? secret.trim() : "";
  if (Buffer.byteLength(normalized, "utf8") < MIN_HMAC_SECRET_BYTES) {
    throw new Error(
      `LEIO tenant HMAC secret must contain at least ${MIN_HMAC_SECRET_BYTES} bytes.`,
    );
  }
  return normalized;
}

function opaqueKey(prefix, domain, value, secret) {
  const key = requireHmacSecret(secret);
  const digest = createHmac("sha256", key)
    .update(domain)
    .update("\0")
    .update(String(value))
    .digest("hex")
    .slice(0, 32);
  return `${prefix}_${digest}`;
}

export function deriveTenantKey(tenantIdentity, hmacSecret) {
  const identity = typeof tenantIdentity === "string" ? tenantIdentity.trim() : "";
  if (!identity) {
    throw new Error("Authenticated LEIO requests require a tenant identity.");
  }
  return opaqueKey("t", "leio-tenant-v1", identity, hmacSecret);
}

function trustedTenantIdentity(authInfo) {
  const extra = authInfo?.extra;
  const explicitTenant = typeof extra?.tenantId === "string"
    ? extra.tenantId.trim()
    : "";
  if (explicitTenant) {
    return `claim:${explicitTenant}`;
  }

  const subject = typeof extra?.sub === "string" ? extra.sub.trim() : "";
  const issuer = typeof extra?.iss === "string" ? extra.iss.trim() : "";
  if (subject) {
    return `subject:${issuer}\0${subject}`;
  }
  return null;
}

function canonicalRepoIdentity(repoTarget) {
  const repoUrl = [
    repoTarget?.repoUrl,
    repoTarget?.gitRemote,
    process.env.LEIO_CODE_GIT_URL,
  ]
    .find((value) => typeof value === "string" && value.trim())
    ?.trim()
    .replace(/\/+$/, "")
    .replace(/\.git$/i, "") ?? "";
  if (repoUrl) {
    return `url:${repoUrl}`;
  }

  const repoRoot = typeof repoTarget?.repoRoot === "string"
    ? path.resolve(repoTarget.repoRoot)
    : "";
  if (!repoRoot) {
    throw new Error("LEIO tenant scope requires a resolved repository target.");
  }
  return `local:${repoRoot}`;
}

export function buildTenantScopeEnv({
  authInfo,
  repoTarget,
  hmacSecret = process.env.LEIO_APPS_SDK_TENANT_HMAC_SECRET,
} = {}) {
  if (!authInfo) {
    return {};
  }

  const tenantIdentity = trustedTenantIdentity(authInfo);
  if (!tenantIdentity) {
    throw new Error("Authenticated LEIO request is missing a trusted tenant claim or subject.");
  }

  const tenantKey = deriveTenantKey(tenantIdentity, hmacSecret);
  const repoKey = opaqueKey(
    "r",
    "leio-repository-v1",
    canonicalRepoIdentity(repoTarget),
    hmacSecret,
  );
  const revision = String(
    repoTarget?.revision ?? repoTarget?.gitRef ?? "local",
  ).trim() || "local";
  return {
    LEIO_CODE_TENANT_ID: tenantKey,
    LEIO_CODE_REPO: repoKey,
    LEIO_CODE_REV: revision,
  };
}
