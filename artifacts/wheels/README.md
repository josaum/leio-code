# Pre-built parser wheels

Runtime wheels consumed by leio-code via `uv run --no-project --with
fca-fast --find-links artifacts/wheels/<platform>` (see `src/fca.rs`).

| Wheel | Version | Platform |
| --- | --- | --- |
| `fca_fast` | 0.2.0 | macos 11+ arm64, manylinux 2.34 aarch64/x86_64 |

Built with maturin (PyO3, abi3 cp38) from the `fca-fast-py` crate in the
upstream workspace; leio-code consumes the pre-built artifacts only and
vendors no parser source. Override the resolution with `LEIO_FCA_WHEEL`
(path or URL) or `LEIO_FCA_FIND_LINKS` (directory or index).
