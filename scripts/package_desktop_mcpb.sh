#!/usr/bin/env bash
# Pack a Claude Desktop MCP Bundle (.mcpb). Does not include the Rust crate.
set -euo pipefail
ROOT="$(cd -- "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(python3 -c "import json; print(json.load(open('${ROOT}/manifest.json'))['version'])")"
OUT="${1:-${ROOT}/releases/leio-code-${VERSION}.mcpb}"
ASSETS="${2:-}"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/leio-mcpb.XXXXXX")"
cleanup() { rm -rf "${STAGE}"; }
trap cleanup EXIT

mkdir -p "${STAGE}/mcp" "$(dirname "${OUT}")"
cp "${ROOT}/LICENSE" "${ROOT}/LICENSE-MIT" "${ROOT}/LICENSE-APACHE" "${ROOT}/THIRD_PARTY.md" "${STAGE}/"
cp "${ROOT}/manifest.json" "${STAGE}/manifest.json"
cp "${ROOT}/assets/icon.png" "${STAGE}/icon.png"
cp "${ROOT}/mcp/package.json" "${ROOT}/mcp/package-lock.json" "${STAGE}/mcp/"
find "${ROOT}/mcp" -maxdepth 1 -name '*.js' ! -name '*.test.js' -exec cp {} "${STAGE}/mcp/" \;
cp "${ROOT}/mcp/desktop-launch.mjs" "${STAGE}/mcp/"
if [[ -n "${ASSETS}" ]]; then
  python3 - "${ROOT}" "${ASSETS}" "${STAGE}" <<'PY'
import shutil
import sys
import zipfile
from pathlib import Path

root, assets, stage = map(Path, sys.argv[1:])
sys.path.insert(0, str(root / "scripts"))
from build_packages import PLATFORMS, VERSION, release_binary

for platform, files in zip(("darwin-arm64", "darwin-x64", "linux-x64", "win32-x64"), PLATFORMS, strict=True):
    vendor = stage / "mcp" / "vendor" / platform
    vendor.mkdir(parents=True)
    suffix = ".exe" if platform == "win32-x64" else ""
    shutil.copy2(release_binary(files["code"], assets), vendor / f"leio-code{suffix}")
    shutil.copy2(release_binary(files["harness"], assets), vendor / f"leio-harness{suffix}")
    if not suffix:
        for executable in vendor.iterdir():
            executable.chmod(0o755)
with zipfile.ZipFile(assets / f"leio-code-{VERSION}-windows-amd64-portable.zip") as archive:
    archive.extractall(stage / "portable", [n for n in archive.namelist() if n.startswith("leio-code/runtime/git/")])
runtime = stage / "mcp" / "runtime" / "win32-x64"
runtime.mkdir(parents=True)
shutil.move(stage / "portable" / "leio-code" / "runtime" / "git", runtime / "git")
shutil.rmtree(stage / "portable")
PY
fi
# Install a fresh flat node_modules in the stage — never copy the repo's
# pnpm-layout tree: pnpm's relative symlinks do not survive the .mcpb zip
# round-trip, and a half-dangling @modelcontextprotocol/sdk breaks the
# packaged server at import time (ERR_MODULE_NOT_FOUND).
(cd "${STAGE}/mcp" && npm ci --omit=dev --ignore-scripts --no-audit --no-fund >/dev/null)

npx --yes @anthropic-ai/mcpb@2.1.2 validate "${STAGE}/manifest.json"
npx --yes @anthropic-ai/mcpb@2.1.2 pack "${STAGE}" "${OUT}"
ls -lh "${OUT}"
echo "${OUT}"
