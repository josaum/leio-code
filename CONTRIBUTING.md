# Contributing to LEIO Code

**This repository is generated** (see [PUBLIC-SOURCE.md](docs/PUBLIC-SOURCE.md)):
it is not edited by hand, and a commit applied here is deleted by the next export.
Start with a focused **issue** instead: the problem, the expected behavior, and a
small reproduction. For feature proposals, describe an agent workflow the change
would improve. Maintainers apply accepted changes in the source tree, and they
arrive here on the next export.

Never include credentials, private repository contents, or personal conversation
transcripts in issues or fixtures.

## Local development

Use stable Rust, Node.js 22+ and npm. Build the locked Rust workspace and install
MCP dependencies with `npm ci --prefix mcp`. Follow the
[canonical agent skill](skills/leio-code/SKILL.md) when working with an agent.

Run checks appropriate to your change:

```bash
cargo test --locked --workspace
npm test --prefix mcp
python3 -m unittest discover -s tests
```

For stdio changes, also build the release CLI and run
`scripts/verify-stdio.mjs` with `LEIO_CODE_BIN` set to its absolute path.
For documentation, check relative links and keep claims tied to reproducible
results. Label illustrations clearly. Include a motion-free alternative for
animated documentation.

## Before opening a PR

Explain the concrete behavior change and the checks you ran. Keep unrelated
formatting and generated runtime state out of the diff. Add regression coverage
when behavior changes, and preserve existing user configuration and data.

Contributions must be compatible with the project's MIT OR Apache-2.0 license.
Preserve applicable third-party notices. See [LICENSE](LICENSE).
