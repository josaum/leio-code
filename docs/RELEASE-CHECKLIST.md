# LEIO Code release / install checklist

Honest distribution path. **crates.io is not claimed.** The workspace publishes
`leio-code` with a path dependency on `crates/leio-knowledge-core`.

## Local / CI install (preferred)

```bash
# Makefile sets CARGO_TARGET_DIR ?= target. Node must be on PATH for Apps SDK / MCP.

make install

# Or explicit:
cargo build --release -p leio-code -p leio-harness
install -m 0755 target/release/leio-code ~/.cargo/bin/leio-code
install -m 0755 target/release/leio-harness ~/.cargo/bin/leio-harness
leio-code --version
```

## GitHub Release binaries

`.github/workflows/binary-release.yml` builds `leio-code-linux-amd64`,
`leio-code-darwin-arm64`, and `leio-code-darwin-amd64`, plus the matching
harness binaries, on tag `leio-code-plugin-v*`. Current release:
[`leio-code-plugin-v2.6.5`](https://github.com/josaum/leio-code/releases/tag/leio-code-plugin-v2.6.5).

## Homebrew

This checkout has no Homebrew formula. A tap is not part of the current release.

## Docker

Apps SDK image build and smoke live in `apps-sdk` (`npm run docker:build`,
`npm run docker:smoke`). This checklist does not claim a published Docker Hub tag.

## Versioned release checklist (human)

1. Bump the first-party LEIO release identity together: Cargo package/lock,
   MCP and Apps SDK root packages/locks, Codex/Claude/ChatGPT plugin manifests,
   Gemini/desktop extension manifests, and `CHANGELOG.md` `[Unreleased]` →
   tagged section. Run `leio-code doctor leio-release-coherence --repo ..`.
2. `cargo test -p leio-code` (or focused suites) green.
3. `cargo build --release -p leio-code -p leio-harness` and `make install`.
4. Apps SDK: from the repository root, `cd apps-sdk && npm test`, then start the server and `npm run smoke`.
5. stdio MCP: `node --check mcp/index.js`; smoke `leio_code_guide` / `leio_code_audit` if tooling available.
6. Run `leio-code doctor self-contract --repo .` and
   `leio-code doctor leio-release-coherence --repo .`. Both are the native pack
   in `.leio-code/native-doctors.json`, not engine-registry doctors.
7. Keep wheel and parser package versions independent from the LEIO product version.
   Do not bulk-replace dependency or wheel versions during a LEIO release.
8. Docs: no stale “bootstrap-only / no init / 1 doctor” claims; doctor counts via `capabilities` only.
9. Tag `leio-code-plugin-vX.Y.Z` and attach release notes pointing at this checklist.
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
