# Public source distribution

This repository starts with a clean source snapshot of LEIO Code 2.6.2. It does
not include historical Git objects, private operational runbooks, external
repository benchmark reports, legacy release archives, or optional binary wheels.
Original LEIO source, local tests, protocol contracts and dual-license notices
are retained. Runtime dependencies are resolved from the committed lockfiles.

Install the local 18-tool MCP using [the one-prompt guide](install-stdio.md).
No private repository credentials or sibling checkout is required. Optional
external accelerators and hosted integrations are separate installations.

The optional `example` workspace profile uses synthetic application paths and
configuration names to demonstrate repository checks. Adapt those contracts to
your repository; they are not deployment instructions for a real service.
