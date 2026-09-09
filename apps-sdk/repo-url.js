const DEFAULT_ALLOWED_REPO_HOSTS = Object.freeze(["github.com", "gitlab.com"]);
const LOCALHOST_NAMES = new Set(["localhost", "127.0.0.1", "::1"]);
const GITHUB_OWNER_PATTERN = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$/;
const REPO_NAME_PATTERN = /^[A-Za-z0-9._-]+$/;

function parseBooleanEnv(value) {
  return ["1", "true", "yes", "on"].includes(String(value ?? "").trim().toLowerCase());
}

function parseAllowedHosts(value) {
  if (value === undefined || value === null || String(value).trim() === "") {
    return new Set(DEFAULT_ALLOWED_REPO_HOSTS);
  }

  return new Set(
    String(value)
      .split(",")
      .map((host) => host.trim().toLowerCase())
      .filter(Boolean),
  );
}

function buildRepoUrlOptions(options = {}) {
  const env = options.env ?? process.env;
  const allowedHosts = options.allowedHosts
    ? new Set(options.allowedHosts.map((host) => String(host).trim().toLowerCase()).filter(Boolean))
    : parseAllowedHosts(env.LEIO_CODE_ALLOWED_REPO_HOSTS);

  return {
    allowedHosts,
    allowInsecureLocalhost: options.allowInsecureLocalhost
      ?? parseBooleanEnv(env.LEIO_CODE_ALLOW_INSECURE_LOCAL_REPO_URLS),
  };
}

function normalizeGitHubShorthand(value) {
  const parts = value.split("/");
  if (parts.length !== 2) {
    return null;
  }

  const [owner, repoSegment] = parts;
  const repo = repoSegment.replace(/\.git$/i, "");
  if (!GITHUB_OWNER_PATTERN.test(owner) || !REPO_NAME_PATTERN.test(repo)) {
    return null;
  }
  if (repo === "." || repo === "..") {
    return null;
  }

  return `https://github.com/${owner}/${repo}.git`;
}

function hasUnsafePathSegment(pathname) {
  return pathname
    .split("/")
    .filter(Boolean)
    .some((segment) => {
      try {
        const decoded = decodeURIComponent(segment);
        return decoded === "." || decoded === ".." || decoded.includes("/") || decoded.includes("\\");
      } catch {
        return true;
      }
    });
}

function normalizeParsedRepoUrl(url, options) {
  const hostname = url.hostname.toLowerCase();
  const isLocalhost = LOCALHOST_NAMES.has(hostname);
  const isHttps = url.protocol === "https:";
  const isAllowedLocalHttp = url.protocol === "http:" && isLocalhost && options.allowInsecureLocalhost;

  if (!isHttps && !isAllowedLocalHttp) {
    return { ok: false, error: "repo_url must use https" };
  }

  if (url.username || url.password) {
    return { ok: false, error: "repo_url must not include embedded credentials" };
  }

  if (url.search || url.hash) {
    return { ok: false, error: "repo_url must not include query strings or fragments" };
  }

  if (!options.allowedHosts.has("*") && !options.allowedHosts.has(hostname) && !isAllowedLocalHttp) {
    return {
      ok: false,
      error: `repo_url host is not allowed: ${hostname}`,
    };
  }

  const pathParts = url.pathname.split("/").filter(Boolean);
  if (pathParts.length < 2) {
    return { ok: false, error: "repo_url must include an owner/group and repository path" };
  }

  if (hasUnsafePathSegment(url.pathname)) {
    return { ok: false, error: "repo_url contains an unsafe path segment" };
  }

  url.hostname = hostname;
  url.pathname = `/${pathParts.join("/")}`;
  url.search = "";
  url.hash = "";

  return {
    ok: true,
    url: url.toString().replace(/\/$/, ""),
  };
}

function parseRemoteRepoUrl(value, options = {}) {
  if (!value || typeof value !== "string") {
    return { ok: false, error: "repo_url is required" };
  }

  const trimmed = value.trim();
  if (!trimmed) {
    return { ok: false, error: "repo_url is required" };
  }
  if (/\s/.test(trimmed)) {
    return { ok: false, error: "repo_url must not contain whitespace" };
  }

  const shorthand = normalizeGitHubShorthand(trimmed);
  if (shorthand) {
    return { ok: true, url: shorthand };
  }

  let url;
  try {
    url = new URL(trimmed);
  } catch {
    return { ok: false, error: "repo_url must be a GitHub owner/repo shorthand or an https URL" };
  }

  return normalizeParsedRepoUrl(url, buildRepoUrlOptions(options));
}

export function normalizeRepoUrl(value, options = {}) {
  const parsed = parseRemoteRepoUrl(value, options);
  return parsed.ok ? parsed.url : null;
}

export function requireRemoteRepoUrl(value, options = {}) {
  const parsed = parseRemoteRepoUrl(value, options);
  if (!parsed.ok) {
    throw new Error(parsed.error);
  }
  return parsed.url;
}

export function isGitHubRepoUrl(value, options = {}) {
  const repoUrl = normalizeRepoUrl(value, options);
  return Boolean(repoUrl && repoUrl.toLowerCase().startsWith("https://github.com/"));
}

export function parseGitHubOwnerRepo(repoUrlInput, options = {}) {
  const repoUrl = normalizeRepoUrl(repoUrlInput, options);
  if (!repoUrl || !isGitHubRepoUrl(repoUrl, options)) {
    return null;
  }

  const url = new URL(repoUrl);
  const parts = url.pathname.replace(/^\/+|\/+$/g, "").replace(/\.git$/i, "").split("/");
  if (parts.length < 2) {
    return null;
  }

  return {
    owner: parts[0],
    repo: parts[1],
  };
}
