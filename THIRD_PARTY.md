# Third-party materials

Original LEIO Code code and documentation use `MIT OR Apache-2.0`.
Dependencies are not relicensed by this project.

- `Cargo.lock` records the Rust dependency versions. Their published package
  manifests and license files provide the corresponding terms and attribution.
- `mcp/package-lock.json` and `apps-sdk/package-lock.json` record Node dependency
  versions. Their package license files remain with installed dependencies.
- `vendor/manifest.json` currently records no vendored crates.
- Optional external wheels and services are separate distributions. Consult
  their own notices before redistribution; the core stdio installation does
  not require them.

A binary or bundle distributor must preserve the applicable dependency notices
alongside the root license files. The public source installer builds locally
from the locked Rust dependencies and installs the locked Node dependencies;
it does not substitute the project's license for their individual licenses.
