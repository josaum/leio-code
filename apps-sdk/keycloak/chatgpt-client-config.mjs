export function parseList(value) {
  if (!value || typeof value !== "string") {
    return [];
  }
  return value
    .split(/[,\n]+/)
    .map((item) => item.trim())
    .filter(Boolean);
}

export function unique(values) {
  return [...new Set(values.filter(Boolean))];
}

export function normalizeRedirectUris({
  exactRedirectUri,
  includeBootstrapWildcard = true,
  includeLegacyRedirect = true,
  includeReviewRedirect = true,
  extraRedirectUris = [],
} = {}) {
  const redirects = [];

  if (includeBootstrapWildcard) {
    redirects.push("https://chatgpt.com/connector/oauth/*");
  }
  if (exactRedirectUri) {
    redirects.push(exactRedirectUri);
  }
  if (includeLegacyRedirect) {
    redirects.push("https://chatgpt.com/connector_platform_oauth_redirect");
  }
  if (includeReviewRedirect) {
    redirects.push("https://platform.openai.com/apps-manage/oauth");
  }

  for (const value of extraRedirectUris) {
    redirects.push(value);
  }

  return unique(redirects);
}

export function deriveWebOrigins({
  redirectUris = [],
  extraWebOrigins = [],
} = {}) {
  const origins = [];
  for (const value of redirectUris) {
    try {
      const url = new URL(value.replace("*", "bootstrap"));
      origins.push(url.origin);
    } catch {
      // ignore invalid values; validation belongs to the caller
    }
  }
  for (const value of extraWebOrigins) {
    origins.push(value);
  }
  return unique(origins);
}

export function buildDesiredAppsClientConfig({
  publicUrl,
  exactRedirectUri,
  includeBootstrapWildcard = true,
  includeLegacyRedirect = true,
  includeReviewRedirect = true,
  extraRedirectUris = [],
  extraWebOrigins = [],
} = {}) {
  const redirectUris = normalizeRedirectUris({
    exactRedirectUri,
    includeBootstrapWildcard,
    includeLegacyRedirect,
    includeReviewRedirect,
    extraRedirectUris,
  });

  const webOrigins = deriveWebOrigins({
    redirectUris,
    extraWebOrigins,
  });

  return {
    redirectUris,
    webOrigins,
    rootUrl: publicUrl ?? undefined,
    baseUrl: publicUrl ?? undefined,
  };
}

export function updateEnvFileContent(content, assignments) {
  const normalized = content.replace(/\r\n/g, "\n");
  const endsWithNewline = normalized.endsWith("\n");
  const lines = normalized.length ? normalized.split("\n") : [];
  const keys = Object.keys(assignments);
  const seen = new Set();

  const updatedLines = lines.map((line) => {
    const match = line.match(/^\s*([A-Z0-9_]+)=/);
    if (!match) {
      return line;
    }
    const key = match[1];
    if (!Object.prototype.hasOwnProperty.call(assignments, key)) {
      return line;
    }
    seen.add(key);
    return `${key}=${assignments[key]}`;
  });

  for (const key of keys) {
    if (!seen.has(key)) {
      updatedLines.push(`${key}=${assignments[key]}`);
    }
  }

  const rendered = updatedLines.join("\n");
  return rendered.length === 0
    ? ""
    : endsWithNewline || normalized.length === 0
      ? `${rendered}\n`
      : rendered;
}
