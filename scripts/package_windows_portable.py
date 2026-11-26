#!/usr/bin/env python3
"""Assemble the offline Windows x64 distribution from verified release binaries."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import tempfile
import zipfile
from pathlib import Path
from urllib.request import Request, urlopen

from build_packages import ROOT, VERSION, release_binary, stage_server

NODE = "24.22.3"
GIT = "2.56.1"
RUNTIMES = {
    "node": {
        "version": NODE,
        "url": f"https://nodejs.org/dist/v{NODE}/node-v{NODE}-win-x64.zip",
        "sha256": "e612af99f626ab415ea225af6bce5c971feb448cb5cae250bb1c3fa50c5f7d9f",
    },
    "git": {
        "version": GIT,
        "url": f"https://github.com/git-for-windows/git/releases/download/v{GIT}.windows.1/MinGit-{GIT}-64-bit.zip",
        "sha256": "4a92850b6e917da4fd010669b614b55dccf5b0ec333a534cc4be4700dfefdafd",
    },
}

NOTICE = r"""LEIO Code for Windows x64
========================

Extract the entire ZIP. In PowerShell, use .\leio-code.cmd --version,
.\leio-harness.cmd --version, or .\leio-mcp.cmd --registration.
Use .\leio-mcp.cmd --register to register the unpacked server with supported
MCP hosts already configured for your Windows user. Keep the directory in
a stable location after registering. Core indexing, graph and lattice
navigation, the local harness, and stdio MCP work with the bundled runtime.

Node.js and Git for Windows MinGit are separate upstream programs. Their
licenses and notices are included alongside the original runtime files.
Runtime versions, original downloads, and SHA-256 digests are recorded in
runtime-sources.json. Corresponding upstream source and build recipes:

- Node.js source: https://nodejs.org/dist/v{node}/node-v{node}.tar.xz
- Git for Windows source: https://github.com/git-for-windows/git/tree/v{git}.windows.1
- Git for Windows package sources: https://github.com/git-for-windows/build-extra
- Git for Windows dependency versions: runtime/git/etc/package-versions.txt
- Git for Windows SDK and redistribution notices: https://gitforwindows.org/
"""


def verified_download(spec: dict[str, str], dest: Path) -> Path:
    request = Request(spec["url"], headers={"User-Agent": "LEIO-Code-release"})
    with urlopen(request, timeout=60) as response, dest.open("wb") as handle:
        shutil.copyfileobj(response, handle)
    with dest.open("rb") as handle:
        actual = hashlib.file_digest(handle, "sha256").hexdigest()
    if actual != spec["sha256"]:
        raise ValueError(f"Upstream SHA-256 mismatch for {spec['url']}")
    return dest


def package(assets: Path, output: Path) -> Path:
    output.mkdir(parents=True, exist_ok=True)
    destination = output / f"leio-code-{VERSION}-windows-amd64-portable.zip"
    with tempfile.TemporaryDirectory(prefix="leio-windows-") as directory:
        work = Path(directory)
        root = work / "leio-code"
        (root / "bin").mkdir(parents=True)
        runtime = root / "runtime"
        runtime.mkdir()
        for target, asset in (
            ("leio-code.exe", "leio-code-windows-amd64.exe"),
            ("leio-harness.exe", "leio-code-windows-amd64-harness.exe"),
        ):
            shutil.copy2(release_binary(asset, assets), root / "bin" / target)
            launcher = (
                "@echo off\r\n"
                "setlocal DisableDelayedExpansion\r\n"
                'set "PATH=%~dp0runtime\\git\\cmd;%PATH%"\r\n'
                f'"%~dp0bin\\{target}" %*\r\n'
            )
            (root / target.replace(".exe", ".cmd")).write_bytes(launcher.encode("ascii"))

        with zipfile.ZipFile(verified_download(RUNTIMES["node"], work / "node.zip")) as archive:
            prefix = f"node-v{NODE}-win-x64/"
            with archive.open(prefix + "node.exe") as source, (runtime / "node.exe").open("wb") as target:
                shutil.copyfileobj(source, target)
            (runtime / "NODE-LICENSE.txt").write_bytes(archive.read(prefix + "LICENSE"))
        with zipfile.ZipFile(verified_download(RUNTIMES["git"], work / "git.zip")) as archive:
            archive.extractall(runtime / "git")
        git_root = runtime / "git"
        for name in ("cmd/git.exe", "LICENSE.txt", "etc/package-versions.txt"):
            if not (git_root / name).is_file():
                raise ValueError(f"MinGit archive does not provide {name}")

        stage_server(root / "mcp", with_modules=True)
        shutil.copy2(ROOT / "packaging" / "windows" / "launch.mjs", root / "mcp")
        shutil.copy2(ROOT / "packaging" / "register-mcp.js", root)
        (root / "package.json").write_text('{"type":"module"}\n', encoding="utf-8")
        shutil.copytree(ROOT / "skills" / "leio-code", root / "skills" / "leio-code")
        for name in ("LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "THIRD_PARTY.md"):
            shutil.copy2(ROOT / name, root)
        (root / "README.txt").write_text(NOTICE.format(node=NODE, git=GIT), encoding="utf-8")
        (root / "runtime-sources.json").write_text(json.dumps(RUNTIMES, indent=2) + "\n", encoding="utf-8")
        (root / "leio-mcp.cmd").write_bytes(
            b'@echo off\r\nsetlocal DisableDelayedExpansion\r\n'
            b'"%~dp0runtime\\node.exe" "%~dp0mcp\\launch.mjs" %*\r\n'
        )
        with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for file in sorted(root.rglob("*")):
                if file.is_file():
                    archive.write(file, file.relative_to(work).as_posix())
    return destination


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    print(package(args.assets_dir, args.output_dir))


if __name__ == "__main__":
    main()
