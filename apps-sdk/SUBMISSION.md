# ChatGPT App Directory — Submission Pack (LEIO Code)

Working document for the official app submission
([platform.openai.com/apps-manage](https://platform.openai.com/apps-manage)).
Machine-readable contract: [chatgpt-app-submission.json](./chatgpt-app-submission.json).

Sources: developers.openai.com/apps-sdk/deploy/submission +
app-submission-guidelines + MCP Apps compatibility guide.

## App identity

- **App name:** LEIO Code
- **One-line:** Capability-aware repository GPS — find owners, explain runtime
  wiring, graph call chains, and audit architectural drift before you ship.
- **Description (directory):** LEIO Code indexes arbitrary codebases and returns
  evidence-backed answers for agents: workspace health, symbol/env/route lookup,
  deploy lineage, structural graph traversal, profile-specific
  doctors, and ranked context bundles. Point at a public Git URL or connect
  GitHub for private repos; analysis stays read-only on the checkout.
- **Company:** JAI · getjai.com
- **Marketing site:** `https://leio-site.josaum.chatgpt.site/`
- **MCP server URL (permanent):** `https://leio-code.getjai.com/mcp`
- **OAuth:** Keycloak `https://auth.getjai.com/realms/leio-code`
  (shared `auth.getjai.com`, realm per product; DCR enabled; scope `repo.read`).
- **Countries:** Global (primary engineering audience). Tool surface English;
  repository content may be any language.
- **Data residency note:** submit from a GLOBAL-residency OpenAI project (EU
  residency cannot submit).

## Reviewer test account

Create or rotate the OpenAI reviewer login (no MFA, no required actions):

```bash
cd leio-code/apps-sdk
export KEYCLOAK_BASE_URL=https://auth.getjai.com
export KEYCLOAK_BOOTSTRAP_ADMIN_USERNAME="<from vigoros-keycloak.env KC_ADMIN_USERNAME>"
export KEYCLOAK_BOOTSTRAP_ADMIN_PASSWORD="<from vigoros-keycloak.env KC_ADMIN_PASSWORD>"
npm run keycloak:create-reviewer-user
```

Defaults: `reviewer@getjai.com` with a generated password printed once to stdout.
Pass `--password '<value>'` to pin a known credential for the submission form.
**Do not commit reviewer passwords** — paste only into the plugin submission portal.

Legacy dev user: `leio-dev` / `change-me-local-dev` (local smoke only).
- Default deployment analyzes the server checkout (`example-workspace` on
  `example-leio`). Reviewers can also pass `repo_url` for allowlisted public
  repos.

## Tool surface (standard deployment)

When `LEIO_VIGOROS_MCP_URL` is unset, `tools/list` exposes **11 tools**:

| Tool | Role |
| --- | --- |
| `guide_repository_tools` | When unsure which LEIO tool to call |
| `inspect_repository_capabilities` | Workspace profile + supported facets |
| `inspect_repository_status` | Index / doctor health snapshot |
| `prepare_repository_context` | Ranked edit/review context bundle |
| `search_repository` | find: symbols, env vars, routes, … |
| `explain_repository` | Runtime lineage for env/redis/deploy |
| `graph_repository` | callers-of, imports-in, … |
| `audit_repository_contracts` | Doctor suites only (CLI `doctor`) |
| `audit_repository_rollup` | Composite CLI `audit [--strict]` |
| `select_repository_target` | Persist repo_url for MCP session |
| `clear_repository_target` | Clear session target |

Optional 12th tool `consult_carlos_motta_specialist` appears only when
`LEIO_VIGOROS_MCP_URL` is configured (not part of the default submission
contract).

## Tool annotations — justifications

All standard tools: `openWorldHint: false` — effects stay inside the LEIO
server checkout, index, and in-memory MCP session. The optional Carlos Motta
bridge (`openWorldHint: true`) calls a configured Vigoros MCP when enabled.

Read-only (`readOnlyHint: true`, `destructiveHint: false`):

- `guide_repository_tools`
- `inspect_repository_capabilities`
- `inspect_repository_status`
- `prepare_repository_context`
- `search_repository`
- `explain_repository`
- `graph_repository`
- `audit_repository_contracts`
- `audit_repository_rollup`

Session write (`readOnlyHint: false`, `destructiveHint: false`):

- `select_repository_target` — updates MCP session pointer only.
- `clear_repository_target` — clears session pointer only.

## Widget / MCP Apps UI

- Resource: `ui://widget/leio-code.html` (`text/html;profile=mcp-app`)
- Standard bridge: `ui/notifications/tool-result` + `tools/call` over
  `postMessage`; ChatGPT compatibility via `window.openai`.
- Tool meta: `_meta.ui.resourceUri` + `openai/outputTemplate` alias.
- Evidence cards render `envelope.entities` and `envelope.evidence` from
  structured tool output.

## Deterministic test prompts

See [chatgpt-app-submission.json](./chatgpt-app-submission.json) `test_cases`
and `negative_test_cases`. Re-run on **web and mobile** before submit.

Quick smoke sequence in Developer Mode:

1. "Guide me on LEIO tools for finding symbols vs call chains."
2. "LEIO status snapshot for this repo."
3. "Find symbol McpServer."
4. "Who calls invokeLeioTool?"
5. "LEIO audit rollup strict — warnings only."

## Privacy / data-minimization audit notes

- Tool responses return indexed repository evidence only; OAuth identity
  scopes session repo selection, never echoed in payloads.
- `select_repository_target` may clone allowlisted public Git URLs into a
  server-side checkout directory; private repos require GitHub App connection.
- No PHI, payment data, or government identifiers are requested.
- Doctors may surface env-var **names** and contract drift from the repo; they
  do not fetch live production secrets.

## Keycloak redirect URIs

Run after you have the exact ChatGPT connector redirect from the UI:

```bash
cd leio-code/apps-sdk
npm run keycloak:configure-app-client -- \
  --chatgpt-redirect-uri '<exact-uri-from-chatgpt>'
```

Bootstrap wildcards already include:

- `https://chatgpt.com/connector/oauth/*`
- `https://chatgpt.com/connector_platform_oauth_redirect`
- `https://platform.openai.com/apps-manage/oauth`

## Pre-submission checklist

Verified in-system (2026-07-09):

- [x] Legal pages live on production: `/privacy`, `/support`, `/terms` → 200
      at `https://leio-code.getjai.com`.
- [x] `/health` reports `legal.configured: true` with publisher metadata.
- [x] CORS preflight for `https://chatgpt.com` on `OPTIONS /mcp`.
- [x] `chatgpt-app-submission.json` documents 11-tool contract + annotations.
- [x] Widget uses MCP Apps bridge + `window.openai` compatibility.
- [x] Tool `outputSchema` registered on Apps SDK tools.
- [x] `graph_repository` is distinct from `search_repository`.
- [x] Reviewer account `reviewer@getjai.com` in realm `leio-code` (no MFA,
      no required actions; rotate via `npm run keycloak:create-reviewer-user`).

Human-only, before hitting Submit:
- [ ] Paste marketing website: `https://leio-site.josaum.chatgpt.site/`
- [ ] Paste exact ChatGPT redirect URI into Keycloak (`configure-app-client`).
- [ ] Screenshots at required dimensions (web + mobile): status widget,
      symbol find, graph result, audit rollup.
- [ ] Submit from GLOBAL-data-residency project.
- [ ] Upload app icon/logo at required dimensions.
- [ ] Confirm base MCP URL is FINAL: `https://leio-code.getjai.com/mcp`
- [ ] Run portal **Scan Tools** after deploy; fix any annotation mismatches.
- [ ] Release notes drafted for the version.

## Deploy reference

- Production GCP: [docs/DEPLOY-GCP.md](../docs/DEPLOY-GCP.md)
- Legacy Fly notes: [docs/DEPLOY-FLY.md](../docs/DEPLOY-FLY.md)
- Agent routing: [skills/leio-code/SKILL.md](../skills/leio-code/SKILL.md)
