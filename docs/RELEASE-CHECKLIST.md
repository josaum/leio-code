# LEIO Code release / install checklist

Honest distribution path. **crates.io is not claimed** — the crate tree still
depends on workspace path deps that block a clean public publish.

## Local / CI install (preferred)

```bash
export CARGO_TARGET_DIR=/path/to/example-workspace/target   # monorepo shared target
export PATH="/Users/josaum/.nvm/versions/node/v22.19.0/bin:$PATH"  # if Apps SDK / MCP Node needed

# Build + install into ~/.cargo/bin without cargo-install cache rebuilds:
make leio-code-install-global

# Or explicit:
CARGO_TARGET_DIR=./target cargo build --release -p leio-code
install -m 0755 target/release/leio-code ~/.cargo/bin/leio-code
leio-code --version
```

## GitHub Release binaries

`leio-code-binary-release.yml` builds `leio-code-linux-amd64`,
`leio-code-darwin-arm64`, and `leio-code-darwin-amd64` on tag
`leio-code-plugin-v*`.

## Homebrew

Source-build formula lives at `leio-code/dist/homebrew/leio-code.rb`:

```bash
brew install --formula leio-code/dist/homebrew/leio-code.rb
```

To publish a tap, copy the formula and pin `url`/`tag` to the release.

## Docker ops image

```bash
docker pull jquant/leio-code:latest   # multi-arch when published that way
# VM wrapper mounts a synced repo snapshot; see docs/contributing/leio-code.md
```

## Versioned release checklist (human)

1. Bump the first-party LEIO release identity together: Cargo package/lock,
   MCP and Apps SDK root packages/locks, Codex/Claude/ChatGPT plugin manifests,
   Gemini/desktop extension manifests, and `CHANGELOG.md` `[Unreleased]` →
   tagged section. Run `leio-code doctor leio-release-coherence --repo ..`.
2. `cargo test -p leio-code` (or focused suites) green.
3. `cargo build --release -p leio-code` + `make leio-code-install-global`.
4. Apps SDK: `cd leio-code/apps-sdk && npm test &&` start server + `npm run smoke`.
5. stdio MCP: `node --check mcp/index.js`; smoke `leio_code_guide` / `leio_code_audit` if tooling available.
6. Run `leio-code doctor codex-orchestration --repo ..`; the tracked project
   config, five routed roles, portable hooks, and ownership rules must be clean.
7. Keep wheel/parser package versions independent from the LEIO product version.
   Run `make package-version-check` and `leio-code doctor artifact-reuse --repo ..`;
   never bulk-replace dependency or wheel versions during a LEIO release.
8. Docs: no stale “bootstrap-only / no init / 1 doctor” claims; doctor counts via `capabilities` only.
9. Tag `leio-code-vX.Y.Z` (or monorepo release process) and attach release notes pointing at this checklist + Docker image digest when published.
10. Do **not** mark ChatGPT store listing ready unless OAuth issuer, legal env vars (`LEIO_APPS_SDK_PUBLISHER_NAME`, company URL, support/privacy/security emails, support hours), and hosted public URL are configured with `legal.configured=true` on `/health` (same `legal` object as `GET /`). Canonical public box is GCP: [DEPLOY-GCP.md](DEPLOY-GCP.md). Fly Apps SDK is legacy; Keycloak issuer notes: [DEPLOY-FLY.md](DEPLOY-FLY.md).
11. ChatGPT directory: complete [apps-sdk/SUBMISSION.md](../apps-sdk/SUBMISSION.md), run `npm test` in `apps-sdk` (includes submission-contract), portal **Scan Tools** against [chatgpt-app-submission.json](../apps-sdk/chatgpt-app-submission.json).
12. Public source (`josaum/leio-code`) is **generated**, never edited by hand:
    `make public-plan` reviews the delta against the published checkout and
    `make public-publish` regenerates and pushes it; the `leio-code-plugin-v*` tag
    does the same through `.github/workflows/publish-public.yml`, which needs the
    `PUBLIC_REPO_TOKEN` secret. Publishing refuses a dirty tree, and a change made
    directly in the public repository is deleted by the next export. `make hooks`
    installs the pre-push contract gate (private names and credentials).

## Arbitrary-repo smoke

```bash
leio-code init --repo /path/to/other-repo
leio-code status --repo /path/to/other-repo
leio-code find symbol main --repo /path/to/other-repo
leio-code graph callers-of main --repo /path/to/other-repo
leio-code doctor all --repo /path/to/other-repo
```
