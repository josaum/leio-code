#!/usr/bin/env node

const host = process.env.LEIO_APPS_SDK_HOST ?? "127.0.0.1";
const port = process.env.LEIO_APPS_SDK_PORT ?? "3333";
const publicUrl = process.env.LEIO_APPS_SDK_PUBLIC_URL ?? `http://${host}:${port}`;
const authMode = process.env.LEIO_APPS_SDK_AUTH_MODE ?? "none";
const issuer = process.env.LEIO_APPS_SDK_JWT_ISSUER ?? null;
const authServers = (process.env.LEIO_APPS_SDK_AUTHORIZATION_SERVERS ?? issuer ?? "")
  .split(/[,\s]+/)
  .map((value) => value.trim())
  .filter(Boolean);
const scopes = (process.env.LEIO_APPS_SDK_AUTH_SCOPES ?? "repo.read")
  .split(/[,\s]+/)
  .map((value) => value.trim())
  .filter(Boolean);
const keycloakAppsClientId = process.env.LEIO_KEYCLOAK_APPS_CLIENT_ID ?? "leio-code-apps-sdk";
const exactChatgptRedirectUri =
  process.env.LEIO_OPENAI_CHATGPT_REDIRECT_URI?.trim() || null;
const extraRedirectUris = (process.env.LEIO_OPENAI_EXTRA_REDIRECT_URIS ?? "")
  .split(/[,\n]+/)
  .map((value) => value.trim())
  .filter(Boolean);

const publicBase = new URL(publicUrl);
const mcpUrl = new URL("/mcp", publicBase);
const protectedResourceMetadataUrl = new URL(
  `/.well-known/oauth-protected-resource${mcpUrl.pathname === "/" ? "" : mcpUrl.pathname}`,
  publicBase,
);

const redirectUris = [
  "https://chatgpt.com/connector/oauth/{callback_id}",
  "https://chatgpt.com/connector_platform_oauth_redirect",
  "https://platform.openai.com/apps-manage/oauth",
];

const redirectUrisForKeycloak = [
  "https://chatgpt.com/connector/oauth/*",
  ...(exactChatgptRedirectUri ? [exactChatgptRedirectUri] : []),
  "https://chatgpt.com/connector_platform_oauth_redirect",
  "https://platform.openai.com/apps-manage/oauth",
  ...extraRedirectUris,
];

const summary = {
  auth_mode: authMode,
  public_base_url: publicBase.href,
  mcp_url: mcpUrl.href,
  protected_resource_metadata_url: protectedResourceMetadataUrl.href,
  authorization_servers: authServers,
  scopes,
  redirect_uris_to_allowlist: authMode === "oauth-jwt" ? redirectUris : [],
  keycloak_apps_client: authMode === "oauth-jwt"
    ? {
        client_id: keycloakAppsClientId,
        exact_chatgpt_redirect_uri: exactChatgptRedirectUri,
        redirect_uris_to_sync: redirectUrisForKeycloak,
      }
    : null,
  notes: [
    authMode === "oauth-jwt"
      ? "Allowlist the ChatGPT production callback URL shown in app management, plus the review callback URL for submission."
      : "OAuth callback allowlisting is only relevant in oauth-jwt mode.",
    authMode === "oauth-jwt"
      ? "Expect ChatGPT to send a resource parameter on authorization and token requests; your provider should echo it into the token audience or equivalent claim."
      : "Resource echo checks apply only in oauth-jwt mode.",
    "Use a stable HTTPS public URL for ChatGPT app registration.",
  ],
};

process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
