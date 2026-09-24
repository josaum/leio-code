# Repository-owned doctors

LEIO ships reusable check mechanisms. Application policy, paths, tenant rules,
release surfaces and their regression fixtures belong in the repository they
check. Adding another product-specific Rust module to `src/doctors` is a boundary
violation. Repository packs are versioned alongside their implementation.

## Declarative checks

Place tracked manifests in `.leio-code/doctors/*.toml`. The existing versioned
local-pack schema supports bounded static checks without executing repository
code. Discovery, named doctor calls and `all` / `baseline` / `ci` use these packs.
Malformed packs produce a failing configuration result, never a clean skip.

## Rust checks

A tracked `.leio-code/native-doctors.json` declares schema version 1, a pack name,
and doctor names, descriptions and suite membership. Optional `covered_by`
names an owning composite, avoiding duplicate execution when both are selected.
The catalog contains no executable command. Capabilities can inspect it safely.

The repository owns a Rust executable. A pack can link the LEIO library and call
`doctors::native::serve(registry())`, or implement the versioned JSON protocol
without linking the engine. LEIO starts it with `--request PATH`; the bounded
request carries `protocol: 1`, a request ID, the absolute root, the index snapshot
and selected doctor names. The response repeats protocol/request identity and
returns an exact map of doctor names to standard query envelopes.

Build and review the executable, then explicitly register it for the selected
repository with:

```sh
leio-code --repo /absolute/repo trust-doctor-pack --binary /absolute/reviewed-binary
```

This installs a content-addressed executable in the user's private state directory
and binds its hash, catalog hash and canonical repository root. Repository files
cannot grant their own trust. Catalog or installed-binary changes fail closed.
The original build path is not used during execution. This is trust in reviewed
code, not an OS sandbox: trusted checks can perform the operations their code
implements. Hosted Apps SDK disables native execution even if the host has local
trust records. It can continue running declarative checks.

The runner validates response identity, doctor names and envelopes; limits input
to 128 MiB and output to 8 MiB; imposes a 60-second process deadline; and owns a
Unix process group. Crash, malformed output, missing trust or timeout yields a
failed doctor result. Suites use batches of at most eight doctors with at most
four child processes; a failed batch cannot discard completed batches. Native
execution currently requires a Unix host.

## Ownership after migration

- Example operational, deployment, tenant, OCR/layout integration and agent-role
  contracts: `example-workspace/tools/leio-doctors`.
- Generic parser, shared Arrow pin, documented binding and parser CI contracts:
  sovereign `josaum/parsers-rs/.leio-code/rust-doctors`.
- LEIO's own product/release checks: `tools/leio-self-doctors` in this repository.
- Reference remains a separate project. No Reference policy or source is moved into
  either Example or parsers-rs by this migration.

Generic orphan detection takes its production surfaces and dynamic entrypoint
exceptions from `[doctors.orphan_files]`, never product names compiled into LEIO.
Repositories may list explicitly disabled generic checks in `[doctors] disabled`.

Build scripts with Rust toolchain defaults are listed in `[doctors] rust_build_scripts`;
the engine contains no product-specific script path. Rebuild and renew trust after
editing native doctor source; registration executes the reviewed binary snapshot.
