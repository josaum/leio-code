# LEIO Code Apps SDK

Self-host production-capable OpenAI Apps SDK / ChatGPT MCP app for `leio-code`.
(`private: true` — not an npm publish; ChatGPT **store listing** still requires
your OAuth + legal env + hosted URL — see Production listing env vars below.)

## Contract

- Runtime entrypoint: [server.js](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/server.js)
- HTTP health probe: `GET /health`
- MCP endpoint: `POST /mcp` with streamable HTTP transport
- Public privacy page: `GET /privacy`
- Public support page: `GET /support`
- Public terms page: `GET /terms`
- Tool health/status snapshot: `inspect_repository_status`
- Capability probe: `inspect_repository_capabilities`
- Context bundle: `prepare_repository_context`
- Session target selection: `select_repository_target`
- Session target clear: `clear_repository_target`
- Search (find): `search_repository`
- Graph (callers/imports): `graph_repository` — **not** remapped to search
- Explain: `explain_repository`
- Doctor: `audit_repository_contracts` (CLI `doctor`; naming is historical)
- Composite audit: `audit_repository_rollup` (CLI `audit [--strict]`)
- Guide: `guide_repository_tools` (stdio analogue: `leio_code_guide`)
- Domain specialist `consult_carlos_motta_specialist`: registered **only** when `LEIO_VIGOROS_MCP_URL` is set
- Routing: [skills/leio-code/SKILL.md](../skills/leio-code/SKILL.md) · gaps: [docs/MCP-SURFACE-GAP.md](../docs/MCP-SURFACE-GAP.md)
- Wire contract: MCP **2025-11-25** ([docs/MCP-SPEC-2025-11-25.md](../docs/MCP-SPEC-2025-11-25.md)) — `initialize.protocolVersion`, `serverInfo` (title/version/description/websiteUrl/icons), `instructions`, `capabilities.tools`, tool titles/annotations/outputSchema, `execution.taskSupport: forbidden`, `isError` for tool execution failures
- Evidence contract: successful repository-inspection responses carry
  `urn:leio-code:unsigned-engineering-evidence:v1`, a canonical SHA-256 payload
  digest, `effect=read-only`, and `assurance=unsigned-engineering-evidence`.
  Consumers such as Reference Provider must verify this envelope before accepting it;
  it never promotes code intelligence into business authority.
- Widget resource: `ui://widget/leio-code.html`
- Widget path: [public/leio-code.html](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/public/leio-code.html)
- ChatGPT submission pack: [SUBMISSION.md](./SUBMISSION.md) · [chatgpt-app-submission.json](./chatgpt-app-submission.json)

## Production listing env vars

Set these so `/health` reports `legal.configured: true` and pages drop the draft notice:

| Env | Purpose |
| --- | --- |
| `LEIO_APPS_SDK_PUBLIC_URL` | Canonical HTTPS base URL |
| `LEIO_APPS_SDK_PUBLISHER_NAME` | Publisher display name |
| `LEIO_APPS_SDK_COMPANY_NAME` | Legal entity name (falls back to publisher) |
| `LEIO_APPS_SDK_COMPANY_URL` | Company site |
| `LEIO_APPS_SDK_SUPPORT_EMAIL` | Support contact (not `@replace-me.invalid`) |
| `LEIO_APPS_SDK_PRIVACY_EMAIL` | Privacy contact |
| `LEIO_APPS_SDK_SECURITY_EMAIL` | Security contact |
| `LEIO_APPS_SDK_SUPPORT_HOURS` | Human-readable support hours |
| `LEIO_APPS_SDK_LEGAL_LAST_UPDATED` | ISO date `YYYY-MM-DD` |
| `LEIO_APPS_SDK_AUTH_MODE` | `oauth-jwt` for ChatGPT linking (plus JWT/OIDC vars in `.env.example`) |

Optional: `LEIO_APPS_SDK_SUPPORT_URL` / `PRIVACY_URL` / `TERMS_URL` override the
default `{{PUBLIC}}/support|privacy|terms` routes.

**Not store-ready** until those are set on a hosted deployment with working OAuth.
Self-host with `AUTH_MODE=none` is fine for internal use.

## Local Run

1. Prefer `bash apps-sdk/scripts/start-local.sh` (or `bash codex/connect.sh`). That pins `LEIO_CODE_REPO_ROOT` to this checkout and listens on `127.0.0.1:8181` by default. `repo_root` is accepted on loopback; hosted binds still reject it unless `LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT` is set.
2. Or run `node server.js` / `npm start` from `apps-sdk`. The server listens on `LEIO_APPS_SDK_HOST` and `LEIO_APPS_SDK_PORT` and exposes `/`, `/health`, `/widget`, and the MCP transport at `/mcp` (`POST` plus session follow-up via `GET` and `DELETE`).
3. The same runtime also serves public-facing legal/support pages at `/privacy`, `/support`, and `/terms`.
4. Connect ChatGPT Apps SDK to the MCP endpoint and render the widget resource.
5. Start from [`.env.example`](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/.env.example) when you want a real public URL, auth mode, or publisher/support metadata.
6. Use `npm run smoke` against a running app to validate the MCP endpoint, capability probe, and protected-tool behavior.
7. Use `prepare_repository_context` before implementation/review tasks when the app needs ranked files, symbols, follow-up graph queries, test commands, risk notes, and doctor suggestions.

## Repository Selection Model

The runtime now supports three repository modes:

- `repo_root`
  - server-local absolute path
  - accepted by default on loopback binds; hosted (non-loopback) requires `LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT=true`
- `repo_url`
  - HTTPS Git repository URL or GitHub `owner/repo` shorthand
  - allowed hosts default to `github.com,gitlab.com`; set `LEIO_CODE_ALLOWED_REPO_HOSTS` for trusted self-hosted Git servers
  - the app clones the repository into `LEIO_CODE_CHECKOUTS_ROOT`
  - if the checked-out revision is not indexed yet, the runtime runs `leio-code index` automatically before the first query
- session-selected repository
  - `select_repository_target` persists the chosen `repo_url` + `git_ref` for the current ChatGPT MCP session
  - follow-up tools can omit the repository input and reuse the active target

Public GitHub and GitLab repositories work directly from `repo_url`.
The runtime rejects local paths, SSH/scp-style remotes, `file:` URLs, embedded credentials, and unlisted hosts before invoking Git.

Private repositories work when the runtime has one of these source-auth mechanisms:

- generic Git HTTP credentials via `LEIO_CODE_GIT_HTTP_USERNAME` + `LEIO_CODE_GIT_HTTP_PASSWORD`
- a direct token via `LEIO_CODE_GIT_AUTH_TOKEN`
- provider-specific tokens like `LEIO_CODE_GITHUB_TOKEN` or `LEIO_CODE_GITLAB_TOKEN`
- GitHub App installation tokens via the GitHub App connector described below

## GitHub App Source Connector

Keycloak/OIDC authenticates the ChatGPT app itself. GitHub App auth is separate and only governs access to private source repositories.

When the GitHub App env vars are configured:

- the widget shows `Connect GitHub`
- the app opens `/github/app/connect`
- GitHub OAuth returns to `/github/app/callback`
- the runtime binds the resulting GitHub account to the current MCP session
- the widget can list installed repositories and call `select_repository_target`
- subsequent LEIO tools reuse that session-selected private repository without asking for a PAT in chat

Required GitHub App env vars:

- `LEIO_CODE_GITHUB_APP_SLUG`
- `LEIO_CODE_GITHUB_APP_ID`
- `LEIO_CODE_GITHUB_APP_CLIENT_ID`
- `LEIO_CODE_GITHUB_APP_CLIENT_SECRET`
- `LEIO_CODE_GITHUB_APP_PRIVATE_KEY`
- `LEIO_CODE_GITHUB_APP_CALLBACK_URL`

The recommended product posture is:

1. user authenticates to the ChatGPT app via Keycloak/OIDC
2. user connects GitHub via the GitHub App connector
3. user installs the GitHub App on `Only select repositories`
4. user chooses the repository inside the widget
5. LEIO indexes and analyzes that repository for the current session

## Public Legal and Support Pages

The Apps SDK runtime serves public legal/support pages from env-driven templates
(source markdown remains editable):

- [PRIVACY_POLICY.md](PRIVACY_POLICY.md) → `GET /privacy`
- [SUPPORT.md](SUPPORT.md) → `GET /support`
- [TERMS_OF_USE.md](TERMS_OF_USE.md) → `GET /terms`
- public route: `/privacy`
- public route: `/support`
- public route: `/terms`

Those public pages are rendered from templates in [public/privacy.html](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/public/privacy.html), [public/support.html](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/public/support.html), and [public/terms.html](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/public/terms.html).

Set these env vars before submission so the pages stop showing placeholder warnings:

- `LEIO_APPS_SDK_PUBLISHER_NAME`
- `LEIO_APPS_SDK_COMPANY_NAME`
- `LEIO_APPS_SDK_COMPANY_URL`
- `LEIO_APPS_SDK_SUPPORT_EMAIL`
- `LEIO_APPS_SDK_PRIVACY_EMAIL`
- `LEIO_APPS_SDK_SECURITY_EMAIL`
- `LEIO_APPS_SDK_SUPPORT_HOURS`
- `LEIO_APPS_SDK_LEGAL_LAST_UPDATED`

If those variables are present in your shell at deploy time, [scripts/deploy-fly.sh](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/scripts/deploy-fly.sh) now stages them to Fly automatically.

If you do not set them, the pages still render, but they advertise that they are draft notices and are not ready for public submission.

## Container Run

Build from the `leio-code/apps-sdk` directory:

```bash
npm run docker:build
```

Smoke the built image against the baked-in repository snapshot:

```bash
npm run docker:smoke
```

Run the container against the baked-in `leio-code` snapshot:

```bash
docker run --rm -p 3333:3333 leio-code-apps-sdk
```

The image contains:

- the LEIO Code binary
- the Apps SDK runtime
- a baked-in snapshot of the `leio-code` repository itself for self-hosted smoke and demo use

To point the same image at an arbitrary fixed Git repository at runtime:

```bash
docker run --rm -p 3333:3333 \
  -e LEIO_CODE_GIT_URL=https://github.com/your-org/your-repo.git \
  -e LEIO_CODE_GIT_REF=main \
  leio-code-apps-sdk
```

When `LEIO_CODE_GIT_URL` is set, the entrypoint clones or refreshes that repo into the runtime workspace before starting the Apps SDK server.
This is useful for a deployment that should stay pinned to one repository.
The fixed `LEIO_CODE_GIT_URL` path uses the same HTTPS and host-allowlist validation as session-scoped `repo_url`.

For the generic product flow, you do not need `LEIO_CODE_GIT_URL`; the widget and tools can now accept an allowlisted `repo_url` per session.
The Docker build context is trimmed by [`.dockerignore`](/Users/josaum/projects/example-workspace/leio-code/.dockerignore).

## GCP Deploy (canonical public Apps SDK)

Canonical public path is the **dedicated GCP VM** `example-leio` at
`https://leio-code.getjai.com` — **not** Fly, and not example-platform /
example-vigoros compose. Short runbook: [docs/DEPLOY-GCP.md](../docs/DEPLOY-GCP.md).

## Fly.io (decommissioned)

Fly apps `leio-code-apps-sdk` and `leio-code-keycloak` were destroyed 2026-07-10.
OAuth now uses `https://auth.getjai.com/realms/leio-code`. See
[docs/DEPLOY-GCP.md](../docs/DEPLOY-GCP.md). Historical Fly scripts remain in-tree
only; do not redeploy.

Deploy with the baked-in `leio-code` snapshot:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
LEIO_APPS_SDK_AUTH_MODE=none npm run deploy:fly
```

Deploy against an arbitrary fixed Git repo:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
LEIO_APPS_SDK_AUTH_MODE=none \
LEIO_CODE_GIT_URL=https://github.com/your-org/your-repo.git \
LEIO_CODE_GIT_REF=main \
npm run deploy:fly
```

The deploy script stages `LEIO_APPS_SDK_PUBLIC_URL` automatically from the Fly hostname and then smokes `/health` after deploy.

If you want the public deployment to support additional repository hosts or private GitHub repos, also stage the source-connector env vars from [`.env.example`](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/.env.example), especially:

- `LEIO_CODE_CHECKOUTS_ROOT`
- `LEIO_CODE_ALLOWED_REPO_HOSTS`
- `LEIO_CODE_TIMEOUT_MS`
- `LEIO_CODE_INDEX_TIMEOUT_MS`
- GitHub App env vars when private GitHub support is required

For a public OAuth-aware deployment, the scaffold now has two Fly targets:

- Apps SDK runtime: [fly.toml](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/fly.toml)
- Keycloak issuer: [keycloak/fly.toml](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/keycloak/fly.toml)

The Keycloak Fly deploy currently defaults to a persistent volume-backed `dev-file` database because it is the most reliable zero-license path. Managed Postgres remains available, but it is opt-in through `LEIO_KEYCLOAK_ENABLE_POSTGRES=1`.

## Authentication Modes

The runtime supports three auth modes via [`.env.example`](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/.env.example):

- `LEIO_APPS_SDK_AUTH_MODE=none`
  - anonymous read-only mode
  - useful for local prototyping or fully public repository inspection
- `LEIO_APPS_SDK_AUTH_MODE=static-bearer`
  - direct clients can send a bearer token from `LEIO_APPS_SDK_STATIC_BEARER_TOKENS`
  - useful for private/internal deployments
  - this does not advertise OAuth linking UI to ChatGPT
- `LEIO_APPS_SDK_AUTH_MODE=oauth-jwt`
  - ChatGPT-compatible OAuth resource server mode
  - validates bearer JWTs against `LEIO_APPS_SDK_JWKS_URL`, `LEIO_APPS_SDK_JWT_ISSUER`, and `LEIO_APPS_SDK_JWT_AUDIENCE`
  - exposes protected resource metadata derived from `LEIO_APPS_SDK_PUBLIC_URL`
  - if `LEIO_APPS_SDK_JWKS_URL` is omitted, the runtime derives OIDC discovery from `LEIO_APPS_SDK_JWT_ISSUER` or uses `LEIO_APPS_SDK_OIDC_DISCOVERY_URL`

When `oauth-jwt` is enabled, the app exposes OAuth protected resource metadata for `/mcp` and emits `WWW-Authenticate` challenges plus tool-level `_meta["mcp/www_authenticate"]` errors for protected tools.

By default, the protected tools are:

- `explain_repository`
- `audit_repository_contracts`

Public tools can still remain anonymous while advertising an optional OAuth scheme:

- `inspect_repository_capabilities`
- `inspect_repository_status`
- `search_repository`

If you want the server to reject anonymous MCP connections entirely, set `LEIO_APPS_SDK_REQUIRE_AUTH_ON_CONNECT=true`.

## Keycloak Quickstart

For a zero-license local issuer, the scaffold now includes a Keycloak bootstrap:

- compose file: [keycloak/docker-compose.yml](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/keycloak/docker-compose.yml)
- realm import: [keycloak/realm/leio-code-realm.json](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/keycloak/realm/leio-code-realm.json)
- LEIO env template: [`.env.keycloak.example`](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/.env.keycloak.example)

Start Keycloak:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk/keycloak
docker compose up -d
```

Then load the LEIO Apps SDK envs:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
cp .env.keycloak.example .env.local
set -a
source .env.local
set +a
npm start
```

The imported realm creates a confidential OIDC client `leio-code-apps-sdk` and adds an audience mapper so the access token contains `aud=leio-code-apps-sdk`, which matches the default `LEIO_APPS_SDK_JWT_AUDIENCE`.
It also creates a separate public PKCE client `leio-code-local-dev` for local human login against the same MCP resource without reusing the ChatGPT-facing client.

## Public Keycloak Deploy

To publish a public issuer on Fly:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
set -a
source keycloak/.env.fly.local 2>/dev/null || true
set +a
LEIO_KEYCLOAK_ENABLE_POSTGRES=0 \
LEIO_KEYCLOAK_ENABLE_POSTGRES_RUNTIME=false \
npm run keycloak:deploy:fly
```

That path:

- creates the `keycloak_data` Fly volume when needed
- deploys a single-process Keycloak boot with `--import-realm`
- persists the issuer envs to `keycloak/.env.fly.local` (created by the deploy script; not committed)

If you intentionally want the managed-Postgres path later:

```bash
LEIO_KEYCLOAK_ENABLE_POSTGRES=1 \
LEIO_KEYCLOAK_ENABLE_POSTGRES_RUNTIME=true \
npm run keycloak:deploy:fly
```

Once the public issuer is live, switch the Apps SDK runtime itself to `oauth-jwt`:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
set -a
source keycloak/.env.fly.local
set +a
LEIO_APPS_SDK_AUTH_MODE=oauth-jwt npm run deploy:fly
```

The public smoke then has two expected outcomes:

- without bearer: `protected_tool: oauth_challenge`
- with a service token from [get-service-token.sh](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/keycloak/get-service-token.sh): `protected_tool: reachable_with_bearer`

For ChatGPT registration, review the redirect URIs in the imported client and replace or tighten them as needed in production. The wildcard `https://chatgpt.com/connector/oauth/*` is a bootstrap convenience for local setup, not a final compliance posture.

To sync the confidential ChatGPT-facing client in Keycloak instead of editing redirect URIs by hand:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
set -a
source .env.keycloak.example
set +a
LEIO_APPS_SDK_PUBLIC_URL=https://leio-code.example.com \
LEIO_OPENAI_CHATGPT_REDIRECT_URI='https://chatgpt.com/connector/oauth/your-real-callback-id' \
npm run keycloak:configure-app-client -- --json
```

That command updates the `leio-code-apps-sdk` client with:

- the exact ChatGPT callback when you have it
- the review callback `https://platform.openai.com/apps-manage/oauth`
- the legacy callback `https://chatgpt.com/connector_platform_oauth_redirect`
- the bootstrap wildcard `https://chatgpt.com/connector/oauth/*` unless you disable it
- `rootUrl` and `baseUrl` aligned to `LEIO_APPS_SDK_PUBLIC_URL`

If you want to rotate the confidential client secret at the same time:

```bash
npm run keycloak:configure-app-client -- --rotate-secret --json
```

If you also want the command to rewrite your local ops env file with the fresh client secret and public auth settings:

```bash
npm run keycloak:configure-app-client -- \
  --rotate-secret \
  --sync-env-file keycloak/.env.fly.local \
  --json
```

## Human Login Flow

The service-account smoke path is useful for CI, but it is not the human login path. The local human flow uses:

- the public PKCE client `leio-code-local-dev`
- the loopback redirect URI from [`.env.keycloak.example`](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/.env.keycloak.example)
- a real user in the Keycloak realm

Bootstrap the local dev user once:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
set -a
source .env.keycloak.example
set +a
npm run keycloak:create-dev-user
```

That script is idempotent. It creates or updates `LEIO_KEYCLOAK_DEV_USERNAME`, resets the password, and keeps the user enabled.

Then run the human login flow:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
set -a
source .env.keycloak.example
set +a
npm run auth:human-login
```

What that command does:

1. Generates a PKCE verifier/challenge pair.
2. Starts a loopback callback listener on `LEIO_KEYCLOAK_LOCAL_REDIRECT_URI`.
3. Opens the Keycloak authorize URL in the browser.
4. Waits for the redirect with `?code=...&state=...`.
5. Exchanges the code locally for a bearer token.
6. Prints the token JSON and optionally writes it to `LEIO_KEYCLOAK_TOKEN_OUTPUT`.

If you want the pieces separately instead of the one-shot helper:

```bash
npm run auth:pkce-url
npm run auth:pkce-exchange -- --code THE_AUTH_CODE --code-verifier THE_ORIGINAL_CODE_VERIFIER
```

The important distinction is:

- `client_credentials` via [get-service-token.sh](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/keycloak/get-service-token.sh) is the service-account smoke path
- `authorization_code` + PKCE via [human-login.mjs](/Users/josaum/projects/example-workspace/leio-code/apps-sdk/keycloak/human-login.mjs) is the human login path

The local public client intentionally keeps an audience mapper for `leio-code-apps-sdk`, so the user token is still valid for the MCP resource that validates `aud=leio-code-apps-sdk`.

## Deploy Contract

For a real ChatGPT app deployment, the important settings are:

- `LEIO_APPS_SDK_PUBLIC_URL`
  - stable public HTTPS base URL for the deployed app
- `LEIO_APPS_SDK_AUTH_MODE=oauth-jwt`
  - enables OAuth-aware tool descriptors and challenges
- `LEIO_APPS_SDK_AUTHORIZATION_SERVERS`
  - issuer URLs advertised in the protected resource metadata
- `LEIO_APPS_SDK_AUTH_SCOPES`
  - scopes required by protected tools
- `LEIO_CODE_GIT_URL` and `LEIO_CODE_GIT_REF`
  - optional runtime repo target when you want the same deployed app to inspect a codebase other than the baked-in snapshot; `LEIO_CODE_GIT_URL` must pass the repo URL allowlist
- `LEIO_CODE_ALLOWED_REPO_HOSTS`
  - comma-separated allowlist for session-scoped `repo_url` clones; defaults to `github.com,gitlab.com`

The resource metadata path is computed from the canonical `/mcp` URL, so with:

```text
LEIO_APPS_SDK_PUBLIC_URL=https://leio-code.example.com
```

the runtime will advertise:

```text
https://leio-code.example.com/.well-known/oauth-protected-resource/mcp
```

With the same base URL, the runtime also defaults the public legal pages to:

```text
https://leio-code.example.com/privacy
https://leio-code.example.com/support
https://leio-code.example.com/terms
```

For OAuth provider setup, print the current registration summary:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
npm run auth:summary
```

That summary includes:

- the canonical public `/mcp` URL
- the protected resource metadata URL
- the scopes expected by the app
- the redirect URIs you should know about during ChatGPT setup
- the Keycloak confidential client id and the redirect URIs you should sync into it

Per the official Apps SDK auth guide, ChatGPT now redirects to a production callback like `https://chatgpt.com/connector/oauth/{callback_id}`, and that exact URL is shown in the app management page. Also allowlist the review callback `https://platform.openai.com/apps-manage/oauth`. The older legacy redirect `https://chatgpt.com/connector_platform_oauth_redirect` still matters for already-published apps.

Also expect ChatGPT to send a `resource=https://your-mcp.example.com` parameter during authorization and token requests. Your authorization server should echo that value into the token audience or equivalent claim so the MCP server can verify the token was minted for this resource.

For ChatGPT connector creation to succeed, the issuer must support RFC 7591 Dynamic Client Registration. In the Keycloak setup here, that means:

- anonymous DCR must not be blocked by the `Trusted Hosts` policy
- dynamically created clients must be able to request `repo.read`

To reconcile that on a live realm:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
set -a
source keycloak/.env.fly.local
set +a
npm run keycloak:configure-dcr -- --json
```

That command removes the anonymous `Trusted Hosts` and `Allowed Client Scopes`
blockers (OIDC DCR with `openid` / `repo.read` still 403s when the scopes policy
is only allowlisted on this Keycloak build), ensures an `openid` client scope
exists, and keeps `repo.read` as a default optional client scope for newly
registered ChatGPT clients.

## Smoke Test

With the app running locally:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
npm run smoke
```

Against a deployed endpoint:

```bash
cd /Users/josaum/projects/example-workspace/leio-code/apps-sdk
LEIO_APPS_SDK_SMOKE_BASE_URL=https://leio-code.example.com \
LEIO_APPS_SDK_SMOKE_REPO_ROOT=/Users/josaum/projects/example-workspace \
npm run smoke
```

In `oauth-jwt` mode, the smoke script verifies that:

- `/health` reports the expected auth mode
- protected resource metadata resolves
- `inspect_repository_capabilities` works anonymously when allowed
- `explain_repository` returns an OAuth challenge when called without a token

## ChatGPT Connection

Use the Apps SDK flow to connect the app to the MCP endpoint exposed at `/mcp`.

If ChatGPT shows `doesn't support RFC 7591 Dynamic Client Registration`, run `npm run keycloak:configure-dcr` first against the issuer backing the app.

The first tool call should be `inspect_repository_capabilities`. The UI should treat that result as the source of truth for:

- what the repository actually models
- which `find` / `explain` / `doctor` families are meaningful
- which actions should be hidden or de-prioritized

The Apps SDK runtime remaps the repo-local `leio_code_*` palette into the Apps SDK tool names above before returning `structuredContent.action_palette`. The widget should render that server-provided palette directly instead of trying to translate tool names client-side.

The runtime tools exposed by `apps-sdk/server.js` are:

- `inspect_repository_capabilities`
- `inspect_repository_status`
- `search_repository`
- `graph_repository`
- `explain_repository`
- `audit_repository_contracts`
- `audit_repository_rollup`
- `guide_repository_tools`

These tool names are specific to the Apps SDK runtime. The repo-local CLI/MCP wrapper still uses the `leio_code_*` names documented in the main README.

## Widget Contract

The widget should read `structuredContent` and render:

- `workspace_profile`
- `workspace_capabilities`
- `workspace_capability_hints`
- `action_palette`
- `ui_hints`
- `envelope_summary`
- `sections`

`window.openai` should only be used for UI actions, not for business logic:

- request layout updates
- upload or select files when the Apps SDK surface allows it
- close the widget
- call `inspect_repository_capabilities`, `inspect_repository_status`, `search_repository`, `explain_repository`, or `audit_repository_contracts`

The widget must not reimplement repository detection or capability inference. It should render the server contract and let the server decide what is supported.

## Recommended UI Rules

- Show `inspect_repository_capabilities` first.
- Hide unsupported actions instead of showing dead buttons.
- Prefer a short status summary for generic repositories.
- Expand into details only when the capability contract says the repo models that facet.

## Official Concepts Used

- `registerTool`
- `registerResource`
- `ui://widget/*`
- `structuredContent`
- `securitySchemes`
- `annotations.readOnlyHint`
- `window.openai`
- protected resource metadata
- `WWW-Authenticate`
