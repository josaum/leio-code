# Vendored Crate Provenance

The rules that make a **copy-vendored first-party crate** honest about where it came from.

Enforced by the `vendored-crate-provenance` doctor (`src/doctors/vendored_crate_provenance.rs`), which runs under `leio-code audit --strict`.

## Why this exists

leio-code is a separate repository from `example-workspace`, and `example-workspace` has no root `Cargo.toml` — it is ~20 independent cargo workspaces. A relative path dependency across the two would depend on a sibling checkout layout, so leio-code keeps hand-maintained copies under `vendor/` instead.

That choice is deliberate and worth keeping: path deps build offline, need no registry, and add no external tooling. What it lacked was a contract. **`fca-fast-core`** once fell a minor release behind upstream — the Clippy fixes it needed already existed there — and nothing reported it.

## The analogue: Python wheels

The workspace already solved the equivalent problem on the Python side. `artifacts/manifest.json` records, per built wheel, its `name`, `version`, `platform`, `sha256`, and `source_crate`; `scripts/ops/check-package-versions.py` holds versions to a single table across every manifest surface; and the `fast-wheelhouse-contract` doctor enforces the whole thing under `audit --strict`.

This is the crate-side analogue of that pattern. It does **not** introduce a registry, a publish step, or a new external dependency — the vendored source stays exactly where it is.

## The record

`vendor/manifest.json`, checked in, one entry per vendored crate:

```json
{
  "name": "fca-fast-core",
  "version": "0.1.0",
  "path": "vendor/fca-fast-core",
  "policy": "pinned-divergent",
  "content_sha256": "…",
  "upstream": {
    "repo": "example-workspace",
    "path": "office-parsers-rs/fca-fast-core",
    "commit": "9887a1838",
    "branch": "main"
  },
  "note": "…"
}
```

### Policies

| Policy | Meaning |
|---|---|
| `pinned-exact` | The copy is meant to be byte-identical to the named upstream release. |
| `pinned-divergent` | The copy deliberately differs — held back, or narrowly backported. A `note` explaining why is **required**. |

### Content hash

`sha256` over every regular file under the crate directory (excluding any `target` directory), sorted by forward-slash relative path, feeding `path || 0x00 || bytes || 0x00` per file.

Regenerate after an intentional update:

```bash
python3 - <<'PY'
import hashlib, os
root = "vendor/fca-fast-core"
files = []
for dirpath, dirnames, filenames in os.walk(root):
    dirnames[:] = [d for d in dirnames if d != "target"]
    for fn in filenames:
        ap = os.path.join(dirpath, fn)
        files.append((os.path.relpath(ap, root).replace(os.sep, "/"), ap))
files.sort(key=lambda x: x[0].encode())
h = hashlib.sha256()
for rel, ap in files:
    h.update(rel.encode()); h.update(b"\0")
    h.update(open(ap, "rb").read()); h.update(b"\0")
print(h.hexdigest())
PY
```

## What the doctor checks

1. Every directory under `vendor/` containing a `Cargo.toml` has a manifest entry. An unrecorded vendored crate is a warning.
2. `policy` is one of the two valid values.
3. `upstream.repo`, `upstream.path` and `upstream.commit` are all present and non-empty.
4. `pinned-divergent` carries a `note`.
5. The recorded `version` matches the vendored `Cargo.toml`'s `[package] version`.
6. The recomputed content hash matches `content_sha256`.

Check 6 is the load-bearing one: **a vendored crate cannot be edited without saying where it now comes from.** Any content change breaks the hash and fails the doctor until the manifest is updated, which forces the editor to state the new upstream commit.

The doctor validates the *record*, not the sibling repository. It runs against one `repo_root` and never requires another checkout to be present — so it works in CI, in a container, and on a machine that has only this repo.

## Updating a vendored crate

1. Copy the new upstream source into `vendor/<crate>/`.
2. Regenerate `content_sha256` (snippet above).
3. Update `version`, `upstream.commit`, and `note` in `vendor/manifest.json`.
4. `cargo test && cargo clippy --all-targets -- -D warnings`.
5. `leio-code doctor vendored-crate-provenance --repo .`

## Keeping a copy a pin, not a fork

A vendored copy stays pinnable only if local-only behavior lives outside the crate: when you need behavior the upstream crate should not have, put it in the consumer — not in the vendored copy.

## Profiles

Registered for `PROFILE_LEIO_CODE` (where the first-party copies live) and `PROFILE_EXAMPLE`. Deliberately **not** `PROFILE_GENERIC`: an unrelated repository's `cargo vendor` output is third-party and not ours to record.
