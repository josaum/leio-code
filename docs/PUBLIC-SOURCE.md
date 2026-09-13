# Public source distribution

This repository is **generated** from a private source tree. It is not the source
of truth and it is not edited by hand: every commit here is written by the export
pipeline in that tree, which

- replaces private workspace and product names with neutral placeholders,
- withholds operator runbooks, release archives, prebuilt wheels and internal
  benchmark reports,
- supplies curated public-only files (this document, the CLI guide, the product
  README, the benchmark narrative, the submission harness),
- scans the generated tree for private names and for credentials before
  committing anything.

Send a change that belongs to LEIO Code itself to the source tree; it reaches this
repository on the next export. A patch applied here directly is deleted by the
following export.

## What is here

- The Rust CLI and the stdio MCP server, the Apps SDK, the harness crate, the
  skills, the tests, the protocol contracts and the dual-license notices.
- The optional `example` workspace profile, which uses synthetic application paths
  and configuration names to demonstrate repository checks. Adapt those contracts
  to your repository; they are not deployment instructions for a real service.

## Install

- Local 18-tool MCP: [install with one prompt](install-stdio.md).
- Apps SDK / hosted HTTP: [apps-sdk](../apps-sdk/README.md).

Runtime dependencies resolve from the committed lockfiles. No credentials and no
sibling checkout are required.
