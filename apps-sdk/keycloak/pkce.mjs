import crypto from "node:crypto";

export function base64UrlEncode(input) {
  const buffer = Buffer.isBuffer(input) ? input : Buffer.from(input);
  return buffer
    .toString("base64")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/g, "");
}

export function createPkcePair({ verifierBytes = 32 } = {}) {
  if (!Number.isInteger(verifierBytes) || verifierBytes < 32) {
    throw new Error("verifierBytes must be an integer >= 32");
  }

  const verifier = base64UrlEncode(crypto.randomBytes(verifierBytes));
  const challenge = base64UrlEncode(
    crypto.createHash("sha256").update(verifier).digest(),
  );

  return {
    code_verifier: verifier,
    code_challenge: challenge,
    code_challenge_method: "S256",
  };
}

export function buildAuthorizationEndpoint(issuerUrl) {
  const issuer = new URL(issuerUrl);
  const pathname = issuer.pathname.replace(/\/$/, "");
  return new URL(
    `${pathname}/protocol/openid-connect/auth`,
    issuer.origin,
  ).href;
}

export function buildTokenEndpoint(issuerUrl) {
  const issuer = new URL(issuerUrl);
  const pathname = issuer.pathname.replace(/\/$/, "");
  return new URL(
    `${pathname}/protocol/openid-connect/token`,
    issuer.origin,
  ).href;
}

export function buildPkceAuthorizationUrl({
  issuerUrl,
  clientId,
  redirectUri,
  scopes = ["openid"],
  resource,
  state,
  nonce,
  codeChallenge,
  codeChallengeMethod = "S256",
  prompt = "login",
}) {
  const { code_verifier, code_challenge, code_challenge_method } =
    codeChallenge ? {
      code_verifier: null,
      code_challenge: codeChallenge,
      code_challenge_method: codeChallengeMethod,
    } : createPkcePair();

  const url = new URL(buildAuthorizationEndpoint(issuerUrl));
  url.searchParams.set("response_type", "code");
  url.searchParams.set("client_id", clientId);
  url.searchParams.set("redirect_uri", redirectUri);
  url.searchParams.set("scope", Array.from(new Set(["openid", ...scopes])).join(" "));
  url.searchParams.set("code_challenge", code_challenge);
  url.searchParams.set("code_challenge_method", code_challenge_method);
  if (state) {
    url.searchParams.set("state", state);
  }
  if (nonce) {
    url.searchParams.set("nonce", nonce);
  }
  if (prompt) {
    url.searchParams.set("prompt", prompt);
  }
  if (resource) {
    url.searchParams.set("resource", resource);
  }

  return {
    authorization_url: url.href,
    code_verifier,
    code_challenge,
    code_challenge_method,
  };
}

export async function exchangeAuthorizationCode({
  issuerUrl,
  clientId,
  clientSecret,
  redirectUri,
  code,
  codeVerifier,
}) {
  const tokenEndpoint = buildTokenEndpoint(issuerUrl);
  const body = new URLSearchParams({
    grant_type: "authorization_code",
    client_id: clientId,
    redirect_uri: redirectUri,
    code,
    code_verifier: codeVerifier,
  });

  if (clientSecret) {
    body.set("client_secret", clientSecret);
  }

  const response = await fetch(tokenEndpoint, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body,
  });

  const text = await response.text();
  if (!response.ok) {
    throw new Error(
      `Token exchange failed: ${response.status} ${response.statusText} ${text}`,
    );
  }

  return JSON.parse(text);
}
