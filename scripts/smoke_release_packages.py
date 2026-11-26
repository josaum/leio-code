#!/usr/bin/env python3
"""Install the wheel, npm tarball and desktop bundle on their target OS."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(args: list[str | Path], cwd: Path, env: dict[str, str]) -> str:
    result = subprocess.run(
        [str(arg) for arg in args], cwd=cwd, env=env, timeout=180,
        capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    if result.returncode:
        raise RuntimeError(f"{args[0]} exited {result.returncode}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def one_asset(directory: Path, pattern: str) -> Path:
    matches = sorted(directory.glob(pattern))
    if len(matches) != 1:
        raise ValueError(f"Expected one {pattern} asset, got {matches}")
    return matches[0]


def smoke(assets: Path, version: str, revision: str, wheel_tag: str) -> None:
    node = Path(shutil.which("node") or "node").resolve(strict=True)
    npm = (node.parent / "node_modules" / "npm" / "bin" / "npm-cli.js"
           if os.name == "nt" else Path(shutil.which("npm") or "npm").resolve(strict=True))
    windows = os.name == "nt"
    suffix = ".exe" if windows else ""
    verify_stdio = ROOT / "scripts" / "verify-stdio.mjs"
    with tempfile.TemporaryDirectory(prefix="LEIO packages café ") as directory:
        work = Path(directory)
        env = os.environ.copy()
        for key in ("LEIO_CODE_BIN", "LEIO_HARNESS_BIN", "LEIO_PLUGIN_ROOT",
                    "CLAUDE_PLUGIN_ROOT", "CODEX_PLUGIN_ROOT", "LEIO_CODE_REPO_ROOT"):
            env.pop(key, None)
        env.update(LEIO_MCP_SKIP_REGISTER="1", LEIO_CODE_RELEASE_DIR=str(assets),
                   LEIO_MCP_HOME=str(work / "registration"), LEIO_DISABLE_EVENTS="1")

        def versions(code: list[str | Path], harness: list[str | Path]) -> None:
            assert f"{version} ({revision[:12]}, clean)" in run([*code, "--version"], work, env)
            assert version in run([*harness, "--version"], work, env)

        venv = work / "venv"
        run([sys.executable, "-m", "venv", venv], work, env)
        bin_dir = venv / ("Scripts" if windows else "bin")
        python = bin_dir / f"python{suffix}"
        wheel = one_asset(assets, f"leio_code-{version}-py3-none-{wheel_tag}.whl")
        run([python, "-m", "pip", "install", "--no-index", "--no-deps", wheel], work, env)
        versions([bin_dir / f"leio-code{suffix}"], [bin_dir / f"leio-harness{suffix}"])
        wheel_entry = Path(run([python, "-c",
            "import pathlib,leio_code; print(pathlib.Path(leio_code.__file__).parent/'mcp'/'leio-mcp.js')"
        ], work, env).strip())
        run([node, verify_stdio, wheel_entry], work, env)
        print(f"Installed {wheel.name} and verified CLI, harness and offline stdio")

        npm_package = one_asset(assets, f"leio-code-{version}.tgz")
        npm_command = [node, npm] if windows else [npm]
        run([*npm_command, "install", "--no-audit", "--no-fund", npm_package], work, env)
        npm_bin = work / "node_modules" / "leio-code" / "bin"
        versions([node, npm_bin / "leio-code.js"], [node, npm_bin / "leio-harness.js"])
        run([node, verify_stdio, npm_bin / "leio-mcp.js"], work, env)
        print(f"Installed {npm_package.name} from the staged release assets and verified stdio")

        desktop = one_asset(assets, f"leio-code-plugin-v{version}.mcpb")
        desktop_root = work / "desktop"
        with zipfile.ZipFile(desktop) as archive:
            archive.extractall(desktop_root)
        run([node, verify_stdio, desktop_root / "mcp" / "desktop-launch.mjs"], work, env)
        print(f"Verified {desktop.name} can start with the packaged platform binaries")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets-dir", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--wheel-tag", required=True)
    args = parser.parse_args()
    smoke(args.assets_dir.resolve(), args.version, args.revision, args.wheel_tag)
