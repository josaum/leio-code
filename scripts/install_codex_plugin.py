#!/usr/bin/env python3
"""Install the LEIO Code plugin into another repo-local Codex marketplace."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import zipfile
from pathlib import Path


PLUGIN_NAME = "leio-code"
INCLUDED_PATHS = (
    ".leio-code/config.toml",
    ".codex-plugin",
    ".claude-plugin",
    ".mcp.json",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "build.rs",
    "README.md",
    "LICENSE",
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "THIRD_PARTY.md",
    "GEMINI.md",
    "docs",
    "benchmarks",
    "skills",
    "hooks",
    "mcp",
    "apps-sdk",
    "src",
    "schema",
    "scripts",
    "assets",
    "prompts",
    "agents",
    "artifacts",
    "crates/leio-harness",
    "crates/leio-knowledge-core",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Install the LEIO Code plugin into a repo")
    parser.add_argument(
        "--source",
        type=Path,
        required=True,
        help="Path to a leio-code plugin directory or packaged tar.gz/zip artifact",
    )
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path.cwd(),
        help="Target repository root that will receive .agents/plugins wiring",
    )
    parser.add_argument(
        "--mode",
        choices=("copy", "symlink"),
        default="copy",
        help="Install mode when source is a directory. Archive sources always extract as copy.",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Replace any existing installed plugin directory or symlink",
    )
    return parser.parse_args()


def extract_source(source: Path) -> tuple[Path, tempfile.TemporaryDirectory[str] | None]:
    source = source.expanduser().resolve()
    if source.is_dir():
        return source, None

    tmp = tempfile.TemporaryDirectory(prefix="leio-code-install-")
    temp_root = Path(tmp.name)
    if source.suffix == ".zip":
        with zipfile.ZipFile(source) as archive:
            archive.extractall(temp_root)
    else:
        with tarfile.open(source, "r:*") as archive:
            archive.extractall(temp_root)

    candidates = [path for path in temp_root.iterdir() if path.is_dir()]
    if len(candidates) != 1:
        raise RuntimeError(f"expected one top-level directory in {source}, found {len(candidates)}")
    return candidates[0], tmp


def ensure_marketplace(repo_root: Path) -> Path:
    marketplace_dir = repo_root / ".agents" / "plugins"
    marketplace_dir.mkdir(parents=True, exist_ok=True)
    plugins_dir = repo_root / "plugins"
    plugins_dir.mkdir(parents=True, exist_ok=True)
    marketplace_path = marketplace_dir / "marketplace.json"
    if not marketplace_path.exists():
        marketplace = {
            "name": "local-plugin-marketplace",
            "interface": {"displayName": "Local Plugins"},
            "plugins": [],
        }
        marketplace_path.write_text(json.dumps(marketplace, indent=2) + "\n", encoding="utf-8")
    return marketplace_path


def install_plugin(source_root: Path, repo_root: Path, mode: str, force: bool) -> Path:
    plugins_dir = repo_root / "plugins"
    dest = plugins_dir / PLUGIN_NAME

    if dest.exists() or dest.is_symlink():
        if not force:
            raise RuntimeError(f"{dest} already exists; re-run with --force to replace it")
        if dest.is_symlink() or dest.is_file():
            dest.unlink()
        else:
            shutil.rmtree(dest)

    if mode == "symlink" and source_root.is_dir():
        symlink_target = Path(os.path.relpath(source_root, dest.parent))
        dest.symlink_to(symlink_target, target_is_directory=True)
    else:
        dest.mkdir(parents=True, exist_ok=True)
        for rel in INCLUDED_PATHS:
            src = source_root / rel
            target = dest / rel
            if not src.exists():
                continue
            if src.is_dir():
                shutil.copytree(
                    src,
                    target,
                    dirs_exist_ok=True,
                    symlinks=False,
                    ignore=shutil.ignore_patterns(
                        "__pycache__",
                        ".DS_Store",
                        "*.pyc",
                        "*.pyo",
                        "node_modules",
                        "target",
                        ".venv*",
                        "releases",
                    ),
                )
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(src, target)
    ensure_marketplace_root_compatibility(repo_root, dest)
    return dest


def ensure_marketplace_root_compatibility(repo_root: Path, installed_path: Path) -> Path:
    """Make ./plugins/<name> resolve from either repo root or marketplace root."""
    compat_dir = repo_root / ".agents" / "plugins" / "plugins"
    compat_dir.mkdir(parents=True, exist_ok=True)
    compat_dest = compat_dir / PLUGIN_NAME

    if compat_dest.exists() or compat_dest.is_symlink():
        if compat_dest.is_symlink() or compat_dest.is_file():
            compat_dest.unlink()
        else:
            shutil.rmtree(compat_dest)

    compat_target = Path(os.path.relpath(installed_path, compat_dest.parent))
    compat_dest.symlink_to(compat_target, target_is_directory=True)
    return compat_dest


def prepare_mcp_dependencies(plugin_root: Path) -> None:
    mcp_dir = plugin_root / "mcp"
    package_json = mcp_dir / "package.json"
    if not package_json.is_file():
        return

    lockfile = mcp_dir / "package-lock.json"
    node_modules = mcp_dir / "node_modules"
    if node_modules.is_dir():
        return

    base_cmd = ["npm", "ci", "--omit=dev"] if lockfile.is_file() else ["npm", "install", "--omit=dev"]
    try:
        subprocess.run(base_cmd, cwd=mcp_dir, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except FileNotFoundError as exc:
        raise RuntimeError("npm is required to prepare LEIO Code MCP dependencies") from exc
    except subprocess.CalledProcessError as exc:
        raise RuntimeError("failed to prepare LEIO Code MCP dependencies") from exc


def update_marketplace(marketplace_path: Path) -> None:
    data = json.loads(marketplace_path.read_text(encoding="utf-8"))
    plugins = data.setdefault("plugins", [])
    for plugin in plugins:
        if plugin.get("name") == PLUGIN_NAME:
            plugin["source"] = {"source": "local", "path": f"./plugins/{PLUGIN_NAME}"}
            plugin["policy"] = {"installation": "AVAILABLE", "authentication": "ON_INSTALL"}
            plugin["category"] = "Developer Tools"
            break
    else:
        plugins.append(
            {
                "name": PLUGIN_NAME,
                "source": {"source": "local", "path": f"./plugins/{PLUGIN_NAME}"},
                "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
                "category": "Developer Tools",
            }
        )
    marketplace_path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def main() -> None:
    args = parse_args()
    repo_root = args.repo_root.expanduser().resolve()
    install_mode = args.mode if args.source.expanduser().resolve().is_dir() else "copy"
    source_root, tmp = extract_source(args.source)
    try:
        marketplace_path = ensure_marketplace(repo_root)
        installed_path = install_plugin(source_root, repo_root, install_mode, args.force)
        prepare_mcp_dependencies(installed_path)
        update_marketplace(marketplace_path)
        print(
            json.dumps(
                {
                    "status": "ok",
                    "repo_root": str(repo_root),
                    "installed_path": str(installed_path),
                    "marketplace_path": str(marketplace_path),
                    "mode": install_mode,
                },
                indent=2,
            )
        )
    finally:
        if tmp is not None:
            tmp.cleanup()


if __name__ == "__main__":
    main()
