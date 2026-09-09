import { getOAuthProtectedResourceMetadataUrl } from "@modelcontextprotocol/sdk/server/auth/router.js";
import { createRemoteJWKSet, jwtVerify } from "jose";

const defaultProtectedTools = [
  "explain_repository",
  "search_repository_memory",
  "audit_repository_contracts",
  "audit_repository_rollup",
  "consult_carlos_motta_specialist",
];
const mandatoryAuthenticatedTools = new Set(["search_repository_memory"]);

function parseList(value, { separators = /[,\s]+/ } = {}) {
  if (!value || typeof value !== "string") {
    return [];
  }
  return value
    .split(separators)
    .map((item) => item.trim())
    .filter(Boolean);
}

function unique(items) {
  return [...new Set(items)];
}

function parseBoolean(value, fallback = false) {
  if (typeof value !== "string") {
    return fallback;
  }
  if (["1", "true", "yes", "on"].includes(value.toLowerCase())) {
    return true;
  }
  if (["0", "false", "no", "off"].includes(value.toLowerCase())) {
    return false;
  }
  return fallback;
}

function resolvePublicBaseUrl({ publicUrl, host, port }) {
  if (publicUrl) {
    return new URL(publicUrl);
  }
  return new URL(`http://${host}:${port}`);
}

function normalizeScopes(raw) {
  if (Array.isArray(raw)) {
    return raw.filter((value) => typeof value === "string" && value.trim()).map((value) => value.trim());
  }
  if (typeof raw === "string") {
    return parseList(raw);
  }
  return [];
}

function normalizeResourceClaim(raw) {
  if (!raw) {
    return null;
  }
  if (Array.isArray(raw)) {
    return raw.find((value) => typeof value === "string" && value.trim()) ?? null;
  }
  if (typeof raw === "string") {
    return raw;
  }
  return null;
}

function safeUrl(value) {
  if (!value) {
    return null;
  }
  try {
    return new URL(value);
  } catch {
    return null;
  }
}

function buildAuthError({ status = 401, error = "invalid_token", message }) {
  const err = new Error(message);
  err.status = status;
  err.oauthError = error;
  return err;
}

function buildTemporaryAuthError(message) {
  return buildAuthError({
    status: 503,
    error: "temporarily_unavailable",
    message:
      message ||
      "Authentication infrastructure is temporarily unavailable. Retry shortly.",
  });
}

function isLocalOrigin(url) {
  return url.hostname === "localhost" || url.hostname === "127.0.0.1";
}

function assertHttpsOrLocal(url, label) {
  if (url.protocol === "https:" || isLocalOrigin(url)) {
    return;
  }
  throw new Error(`${label} must use HTTPS unless it targets localhost.`);
}

function buildOidcDiscoveryUrl(issuerUrl) {
  const issuer = issuerUrl instanceof URL ? issuerUrl : new URL(issuerUrl);
  const pathname =
    issuer.pathname && issuer.pathname !== "/"
      ? issuer.pathname.replace(/\/$/, "")
      : "";
  return new URL(`${pathname}/.well-known/openid-configuration`, issuer.origin);
}

export function createAuthRuntime(
  { host, port },
  {
    fetchImpl = globalThis.fetch?.bind(globalThis),
    createRemoteJWKSetImpl = createRemoteJWKSet,
    jwtVerifyImpl = jwtVerify,
  } = {},
) {
  if (typeof fetchImpl !== "function") {
    throw new Error("global fetch is required for LEIO Apps SDK auth runtime");
  }
  const mode = (process.env.LEIO_APPS_SDK_AUTH_MODE ?? "none").trim().toLowerCase();
  const authEnabled = mode !== "none";
  const publicBaseUrl = resolvePublicBaseUrl({
    publicUrl: process.env.LEIO_APPS_SDK_PUBLIC_URL,
    host,
    port,
  });
  const resourceServerUrl = new URL("/mcp", publicBaseUrl);
  const resourceMetadataUrl = getOAuthProtectedResourceMetadataUrl(resourceServerUrl);
  const resourceMetadataPath = new URL(resourceMetadataUrl).pathname;
  const serviceDocumentationUrl = safeUrl(
    process.env.LEIO_APPS_SDK_RESOURCE_DOCUMENTATION_URL,
  );
  const scopes = unique(
    normalizeScopes(process.env.LEIO_APPS_SDK_AUTH_SCOPES ?? "repo.read"),
  );
  const protectedTools = new Set(
    unique(
      [
        ...parseList(
          process.env.LEIO_APPS_SDK_PROTECTED_TOOLS
            ?? defaultProtectedTools.join(","),
        ),
        ...mandatoryAuthenticatedTools,
      ],
    ),
  );
  const requireAuthOnConnect = parseBoolean(
    process.env.LEIO_APPS_SDK_REQUIRE_AUTH_ON_CONNECT,
    false,
  );
  const staticTokens = new Set(
    parseList(process.env.LEIO_APPS_SDK_STATIC_BEARER_TOKENS),
  );
  const staticTenantId =
    process.env.LEIO_APPS_SDK_STATIC_TENANT_ID?.trim() || "static-bearer";
  const jwtIssuer = process.env.LEIO_APPS_SDK_JWT_ISSUER?.trim() || null;
  const jwtAudience = process.env.LEIO_APPS_SDK_JWT_AUDIENCE?.trim() || null;
  const jwtJwksUrl = process.env.LEIO_APPS_SDK_JWKS_URL?.trim() || null;
  const oidcDiscoveryUrl =
    process.env.LEIO_APPS_SDK_OIDC_DISCOVERY_URL?.trim() || null;
  const jwtResourceClaim =
    process.env.LEIO_APPS_SDK_JWT_RESOURCE_CLAIM?.trim() || "resource";
  const jwtTenantClaim =
    process.env.LEIO_APPS_SDK_TENANT_CLAIM?.trim() || "tenant_id";
  const authorizationServers = unique(
    parseList(process.env.LEIO_APPS_SDK_AUTHORIZATION_SERVERS ?? jwtIssuer ?? ""),
  );
  const resourceName =
    process.env.LEIO_APPS_SDK_RESOURCE_NAME?.trim() || "LEIO Code";
  const oauthUiEnabled = mode === "oauth-jwt" && authorizationServers.length > 0;

  if (mode === "static-bearer" && staticTokens.size === 0) {
    throw new Error(
      "LEIO_APPS_SDK_STATIC_BEARER_TOKENS is required when LEIO_APPS_SDK_AUTH_MODE=static-bearer",
    );
  }

  if (mode === "oauth-jwt") {
    if (!jwtIssuer || !jwtAudience) {
      throw new Error(
        "LEIO_APPS_SDK_JWT_ISSUER and LEIO_APPS_SDK_JWT_AUDIENCE are required when LEIO_APPS_SDK_AUTH_MODE=oauth-jwt",
      );
    }
    assertHttpsOrLocal(publicBaseUrl, "LEIO_APPS_SDK_PUBLIC_URL");
    assertHttpsOrLocal(new URL(jwtIssuer), "LEIO_APPS_SDK_JWT_ISSUER");
    for (const serverUrl of authorizationServers) {
      assertHttpsOrLocal(
        new URL(serverUrl),
        "LEIO_APPS_SDK_AUTHORIZATION_SERVERS entries",
      );
    }
    if (jwtJwksUrl) {
      assertHttpsOrLocal(new URL(jwtJwksUrl), "LEIO_APPS_SDK_JWKS_URL");
    }
    if (oidcDiscoveryUrl) {
      assertHttpsOrLocal(
        new URL(oidcDiscoveryUrl),
        "LEIO_APPS_SDK_OIDC_DISCOVERY_URL",
      );
    }
  }

  let jwksResolverPromise = null;

  function normalizeAuthInfrastructureError(error, messageOverride) {
    if (error?.oauthError === "temporarily_unavailable") {
      return error;
    }
    return buildTemporaryAuthError(
      messageOverride ||
        (error instanceof Error && error.message
          ? error.message
          : "Authentication infrastructure is temporarily unavailable. Retry shortly."),
    );
  }

  function errorMessageChain(error) {
    const parts = [];
    let current = error;
    while (current && !parts.includes(current)) {
      if (typeof current?.message === "string" && current.message.trim()) {
        parts.push(current.message.trim());
      }
      current = current?.cause;
    }
    return parts.join(" | ").toLowerCase();
  }

  function isTransientAuthInfrastructureError(error) {
    if (!error) {
      return false;
    }
    if (error?.oauthError === "temporarily_unavailable") {
      return true;
    }
    const code =
      typeof error?.code === "string" && error.code.trim()
        ? error.code.trim().toUpperCase()
        : null;
    if (
      code &&
      [
        "ECONNREFUSED",
        "ECONNRESET",
        "ETIMEDOUT",
        "EHOSTUNREACH",
        "ENETUNREACH",
        "ENOTFOUND",
        "UND_ERR_CONNECT_TIMEOUT",
        "UND_ERR_CONNECT_ERROR",
      ].includes(code)
    ) {
      return true;
    }
    const message = errorMessageChain(error);
    if (!message) {
      return false;
    }
    if (
      message.includes("jwt expired")
      || message.includes("exp claim timestamp check failed")
      || message.includes("signature verification failed")
      || message.includes("unexpected jwt")
      || message.includes("unexpected \"alg\"")
      || message.includes("unexpected alg")
      || message.includes("audience")
      || message.includes("issuer")
      || message.includes("claim validation failed")
      || message.includes("does not match this mcp resource")
    ) {
      return false;
    }
    return (
      message.includes("fetch failed")
      || message.includes("network")
      || message.includes("timed out")
      || message.includes("timeout")
      || message.includes("connection refused")
      || message.includes("econnrefused")
      || message.includes("econnreset")
      || message.includes("getaddrinfo")
      || message.includes("openid-configuration")
      || message.includes("oidc discovery")
      || message.includes("jwks_uri")
      || message.includes("remote jwk set")
      || message.includes("json web key set")
    );
  }

  async function resolveJwksUrl() {
    if (jwtJwksUrl) {
      return new URL(jwtJwksUrl);
    }
    const discoveryTarget = oidcDiscoveryUrl
      ? new URL(oidcDiscoveryUrl)
      : buildOidcDiscoveryUrl(jwtIssuer);
    let response;
    try {
      response = await fetchImpl(discoveryTarget, {
        headers: { accept: "application/json" },
      });
    } catch (error) {
      throw normalizeAuthInfrastructureError(
        error,
        `Failed to reach OIDC discovery document at ${discoveryTarget.href}.`,
      );
    }
    if (!response.ok) {
      throw buildTemporaryAuthError(
        `Failed to fetch OIDC discovery document from ${discoveryTarget.href}: ${response.status} ${response.statusText}`,
      );
    }
    const payload = await response.json();
    const jwksUri =
      payload && typeof payload.jwks_uri === "string" ? payload.jwks_uri : null;
    if (!jwksUri) {
      throw new Error(
        `OIDC discovery document at ${discoveryTarget.href} does not contain jwks_uri`,
      );
    }
    return new URL(jwksUri);
  }

  async function getJwksResolver() {
    if (!jwksResolverPromise) {
      jwksResolverPromise = resolveJwksUrl()
        .then((url) => createRemoteJWKSetImpl(url))
        .catch((error) => {
          jwksResolverPromise = null;
          throw normalizeAuthInfrastructureError(error);
        });
    }
    return jwksResolverPromise;
  }

  function getToolScopes(toolName) {
    if (!authEnabled) {
      return [];
    }
    if (toolName === "audit_repository_contracts") {
      return unique(
        normalizeScopes(
          process.env.LEIO_APPS_SDK_AUDIT_SCOPES ?? scopes.join(" "),
        ),
      );
    }
    return scopes;
  }

  function getSecuritySchemes(toolName) {
    const toolScopes = getToolScopes(toolName);
    if (!authEnabled) {
      return [{ type: "noauth" }];
    }
    if (!oauthUiEnabled) {
      return [{ type: "noauth" }];
    }
    if (protectedTools.has(toolName)) {
      return [{ type: "oauth2", scopes: toolScopes }];
    }
    return [
      { type: "noauth" },
      { type: "oauth2", scopes: toolScopes },
    ];
  }

  function buildWwwAuthenticate({
    error = "invalid_token",
    errorDescription = "Authentication required.",
    requiredScopes = [],
  } = {}) {
    const parts = [
      `Bearer resource_metadata="${resourceMetadataUrl}"`,
      `error="${error}"`,
      `error_description="${errorDescription}"`,
    ];
    if (!oauthUiEnabled) {
      parts.shift();
    }
    const scopeList = unique(requiredScopes).filter(Boolean);
    if (scopeList.length > 0) {
      parts.push(`scope="${scopeList.join(" ")}"`);
    }
    return parts.join(", ");
  }

  function buildToolAuthError({
    message = "Authentication required.",
    error = "invalid_token",
    errorDescription = "You need to log in to continue.",
    requiredScopes = [],
  } = {}) {
    return {
      content: [{ type: "text", text: message }],
      structuredContent: {
        ok: false,
        auth_required: true,
        auth_mode: mode,
        required_scopes: unique(requiredScopes),
        resource_metadata_url: resourceMetadataUrl,
      },
      _meta: oauthUiEnabled
        ? {
            "mcp/www_authenticate": [
              buildWwwAuthenticate({
                error,
                errorDescription,
                requiredScopes,
              }),
            ],
          }
        : undefined,
      isError: true,
    };
  }

  function metadataDocument() {
    return {
      resource: resourceServerUrl.href,
      authorization_servers: authorizationServers,
      scopes_supported: scopes,
      resource_name: resourceName,
      resource_documentation: serviceDocumentationUrl?.href,
    };
  }

  function summary() {
    return {
      mode,
      enabled: authEnabled,
      public_url: publicBaseUrl.href,
      resource_server_url: resourceServerUrl.href,
      resource_metadata_url: resourceMetadataUrl,
      authorization_servers: authorizationServers,
      scopes,
      protected_tools: [...protectedTools].sort(),
      require_auth_on_connect: requireAuthOnConnect,
      oauth_ui_enabled: oauthUiEnabled,
      oidc_discovery_url:
        mode === "oauth-jwt"
          ? (oidcDiscoveryUrl ? new URL(oidcDiscoveryUrl).href : buildOidcDiscoveryUrl(jwtIssuer).href)
          : null,
    };
  }

  async function verifyAccessToken(token) {
    if (mode === "static-bearer") {
      if (!staticTokens.has(token)) {
        throw buildAuthError({
          status: 401,
          error: "invalid_token",
          message: "Unknown bearer token.",
        });
      }
      return {
        token,
        clientId: "static-bearer",
        scopes,
        expiresAt: Math.floor(Date.now() / 1000) + 3600,
        resource: resourceServerUrl,
        extra: {
          sub: "static-bearer",
          iss: "leio:static-bearer",
          tenantId: staticTenantId,
        },
      };
    }

    if (mode === "oauth-jwt") {
      const jwks = await getJwksResolver();
      let payload;
      try {
        ({ payload } = await jwtVerifyImpl(token, jwks, {
          issuer: jwtIssuer,
          audience: jwtAudience,
          clockTolerance: 30,
        }));
      } catch (error) {
        if (isTransientAuthInfrastructureError(error)) {
          throw normalizeAuthInfrastructureError(error);
        }
        throw error;
      }

      const resourceClaim = normalizeResourceClaim(payload?.[jwtResourceClaim]);
      if (resourceClaim) {
        const resourceUrl = safeUrl(resourceClaim);
        if (!resourceUrl || resourceUrl.href !== resourceServerUrl.href) {
          throw buildAuthError({
            status: 401,
            error: "invalid_token",
            message: `JWT ${jwtResourceClaim} claim does not match this MCP resource.`,
          });
        }
      }

      return {
        token,
        clientId:
          (typeof payload.azp === "string" && payload.azp) ||
          (typeof payload.client_id === "string" && payload.client_id) ||
          (typeof payload.sub === "string" && payload.sub) ||
          "oauth-client",
        scopes: normalizeScopes(
          payload.scope ?? payload.scp ?? payload.scopes ?? [],
        ),
        expiresAt: typeof payload.exp === "number" ? payload.exp : undefined,
        resource: resourceServerUrl,
        extra: {
          sub: typeof payload.sub === "string" ? payload.sub : null,
          iss: typeof payload.iss === "string" ? payload.iss : null,
          tenantId:
            typeof payload[jwtTenantClaim] === "string" &&
            payload[jwtTenantClaim].trim()
              ? payload[jwtTenantClaim].trim()
              : null,
        },
      };
    }

    return null;
  }

  async function maybeAttachAuthInfo(req, res) {
    if (!authEnabled) {
      return true;
    }

    const authHeader = req.headers.authorization;
    if (!authHeader) {
      if (requireAuthOnConnect) {
        res.set("WWW-Authenticate", buildWwwAuthenticate({ requiredScopes: scopes }));
        res.status(401).json({
          error: "invalid_token",
          error_description: "Missing Authorization header.",
        });
        return false;
      }
      return true;
    }

    const [scheme, token] = authHeader.split(/\s+/, 2);
    if (scheme?.toLowerCase() !== "bearer" || !token) {
      res.set(
        "WWW-Authenticate",
        buildWwwAuthenticate({
          errorDescription:
            "Invalid Authorization header format, expected 'Bearer TOKEN'.",
          requiredScopes: scopes,
        }),
      );
      res.status(401).json({
        error: "invalid_token",
        error_description:
          "Invalid Authorization header format, expected 'Bearer TOKEN'.",
      });
      return false;
    }

    try {
      req.auth = await verifyAccessToken(token);
      return true;
    } catch (error) {
      const oauthError = error?.oauthError || "invalid_token";
      const errorDescription =
        error instanceof Error ? error.message : "Authentication failed.";
      res.set(
        "WWW-Authenticate",
        buildWwwAuthenticate({
          error: oauthError,
          errorDescription,
          requiredScopes: scopes,
        }),
      );
      res.status(error?.status || 401).json({
        error: oauthError,
        error_description: errorDescription,
      });
      return false;
    }
  }

  function ensureToolAccess(toolName, extra) {
    if (!protectedTools.has(toolName)) {
      return null;
    }
    if (!authEnabled && !mandatoryAuthenticatedTools.has(toolName)) {
      return null;
    }
    const requiredScopes = getToolScopes(toolName);
    const authInfo = extra?.authInfo;
    if (!authInfo) {
      return buildToolAuthError({
        message: "Authentication required: sign in to access this repository operation.",
        error: "invalid_token",
        errorDescription: "You need to log in to continue.",
        requiredScopes,
      });
    }

    const grantedScopes = Array.isArray(authInfo.scopes) ? authInfo.scopes : [];
    const missingScopes = requiredScopes.filter(
      (scope) => !grantedScopes.includes(scope),
    );
    if (missingScopes.length > 0) {
      return buildToolAuthError({
        message:
          "Authentication required: your token does not grant the scopes needed for this operation.",
        error: "insufficient_scope",
        errorDescription: `Missing required scopes: ${missingScopes.join(" ")}`,
        requiredScopes,
      });
    }

    return null;
  }

  return {
    mode,
    enabled: authEnabled,
    oauthUiEnabled,
    publicBaseUrl,
    resourceServerUrl,
    resourceMetadataUrl,
    resourceMetadataPath,
    authorizationServers,
    scopes,
    protectedTools,
    requireAuthOnConnect,
    getSecuritySchemes,
    getToolScopes,
    buildToolAuthError,
    buildWwwAuthenticate,
    ensureToolAccess,
    maybeAttachAuthInfo,
    metadataDocument,
    summary,
  };
}
