#!/usr/bin/env python3
"""Verify LEIO Code plugin release artifacts.

The release check is intentionally artifact-first: it validates the manifest
digests, inspects archive payloads, and installs one archive into a temporary
repo so stale or incomplete packages are caught before publication.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path
from typing import Iterable


REQUIRED_ARCHIVE_PATHS = (
    ".leio-code/config.toml",
    ".codex-plugin/plugin.json",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "README.md",
    "LICENSE",
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "THIRD_PARTY.md",
    "benchmarks/context-golden-tasks.json",
    "docs/agent-memory.md",
    "docs/doctor-warning-ledger.json",
    "skills/leio-code/SKILL.md",
    "mcp/index.js",
    "apps-sdk/server.js",
    "src/context.rs",
    "src/doctors/self_contract.rs",
    "scripts/package_codex_plugin.py",
    "scripts/install_codex_plugin.py",
)
# #release-verify-contract keeps the packaged context benchmark, memory, and doctor ledger auditable.

FORBIDDEN_PARTS = {"node_modules", "target", "__pycache__", ".venv", "releases"}


def parse_args() -> argparse.Namespace:
    plugin_root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description="Verify LEIO Code release artifacts")
    parser.add_argument(
        "--plugin-root",
        type=Path,
        default=plugin_root,
        help="Path to the leio-code plugin root",
    )
    parser.add_argument(
        "--manifest",
        type=Path,
        help="Release manifest JSON. Defaults to releases/<name>-plugin-v<version>.manifest.json",
    )
    parser.add_argument(
        "--skip-install-smoke",
        action="store_true",
        help="Skip installing the tarball into a temporary repo",
    )
    return parser.parse_args()


def sha256_hex(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while True:
            chunk = handle.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def default_manifest_path(plugin_root: Path) -> Path:
    plugin_json = json.loads((plugin_root / ".codex-plugin" / "plugin.json").read_text(encoding="utf-8"))
    name = plugin_json["name"]
    version = plugin_json["version"]
    return plugin_root / "releases" / f"{name}-plugin-v{version}.manifest.json"


def archive_members(path: Path) -> set[str]:
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            return {member for member in archive.namelist() if not member.endswith("/")}

    with tarfile.open(path, "r:*") as archive:
        return {member.name for member in archive.getmembers() if member.isfile()}


def verify_archive_payload(
    artifact_path: Path,
    plugin_name: str,
    required_paths: Iterable[str] = REQUIRED_ARCHIVE_PATHS,
) -> dict[str, object]:
    members = archive_members(artifact_path)
    prefixed_required = {f"{plugin_name}/{path}" for path in required_paths}
    missing = sorted(prefixed_required - members)
    forbidden = sorted(
        member
        for member in members
        if any(part in FORBIDDEN_PARTS for part in Path(member).parts)
    )
    if missing or forbidden:
        raise RuntimeError(
            f"{artifact_path.name} payload check failed: missing={missing} forbidden={forbidden}"
        )
    return {
        "path": str(artifact_path),
        "member_count": len(members),
        "required_paths": sorted(prefixed_required),
    }


def verify_manifest(manifest_path: Path, install_smoke: bool = True) -> dict[str, object]:
    manifest_path = manifest_path.expanduser().resolve()
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    plugin_name = manifest["plugin_name"]
    artifacts = manifest.get("artifacts") or []
    if not artifacts:
        raise RuntimeError(f"{manifest_path} does not list any artifacts")

    checked = []
    tarball_for_install: Path | None = None
    for artifact in artifacts:
        artifact_path = Path(artifact["path"]).expanduser().resolve()
        if not artifact_path.is_file():
            raise RuntimeError(f"missing release artifact: {artifact_path}")
        expected_size = int(artifact["size_bytes"])
        expected_sha = str(artifact["sha256"])
        actual_size = artifact_path.stat().st_size
        actual_sha = sha256_hex(artifact_path)
        if actual_size != expected_size:
            raise RuntimeError(
                f"{artifact_path.name} size mismatch: expected {expected_size}, got {actual_size}"
            )
        if actual_sha != expected_sha:
            raise RuntimeError(
                f"{artifact_path.name} sha256 mismatch: expected {expected_sha}, got {actual_sha}"
            )
        payload = verify_archive_payload(artifact_path, plugin_name)
        checked.append(
            {
                "path": str(artifact_path),
                "format": artifact.get("format"),
                "sha256": actual_sha,
                "size_bytes": actual_size,
                "payload": payload,
            }
        )
        if artifact.get("format") == "tar.gz":
            tarball_for_install = artifact_path

    install_result = None
    if install_smoke:
        if tarball_for_install is None:
            raise RuntimeError("release manifest does not include a tar.gz artifact for install smoke")
        install_result = install_smoke_test(tarball_for_install, plugin_name)

    return {
        "status": "ok",
        "manifest": str(manifest_path),
        "plugin_name": plugin_name,
        "version": manifest.get("version"),
        "artifacts": checked,
        "install_smoke": install_result,
    }


def install_smoke_test(artifact_path: Path, plugin_name: str) -> dict[str, object]:
    plugin_root = Path(__file__).resolve().parents[1]
    installer = plugin_root / "scripts" / "install_codex_plugin.py"
    with tempfile.TemporaryDirectory(prefix="leio-code-release-install-") as tmp:
        repo_root = Path(tmp) / "repo"
        repo_root.mkdir()
        subprocess.run(
            [
                sys.executable,
                str(installer),
                "--source",
                str(artifact_path),
                "--repo-root",
                str(repo_root),
                "--force",
            ],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        installed = repo_root / "plugins" / plugin_name
        compatibility_link = repo_root / ".agents" / "plugins" / "plugins" / plugin_name
        marketplace = repo_root / ".agents" / "plugins" / "marketplace.json"
        required_installed = [
            installed / ".leio-code" / "config.toml",
            installed / "apps-sdk" / "server.js",
            installed / "mcp" / "index.js",
            installed / "mcp" / "node_modules",
            installed / "src" / "context.rs",
        ]
        missing = [str(path) for path in required_installed if not path.exists()]
        if missing:
            raise RuntimeError(f"install smoke missing expected paths: {missing}")
        if not compatibility_link.is_symlink() or compatibility_link.resolve() != installed.resolve():
            raise RuntimeError(
                f"install smoke compatibility link is invalid: {compatibility_link}"
            )
        marketplace_data = json.loads(marketplace.read_text(encoding="utf-8"))
        plugin_entries = [
            plugin
            for plugin in marketplace_data.get("plugins", [])
            if plugin.get("name") == plugin_name
        ]
        if not plugin_entries:
            raise RuntimeError("install smoke marketplace does not reference leio-code")
        return {
            "repo_root": str(repo_root),
            "installed_path": str(installed),
            "compatibility_link": str(compatibility_link),
            "marketplace_path": str(marketplace),
            "mcp_dependencies_prepared": (installed / "mcp" / "node_modules").is_dir(),
        }


def main() -> None:
    args = parse_args()
    plugin_root = args.plugin_root.expanduser().resolve()
    manifest_path = args.manifest or default_manifest_path(plugin_root)
    result = verify_manifest(manifest_path, install_smoke=not args.skip_install_smoke)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
