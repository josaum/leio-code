#!/usr/bin/env python3
"""Build portable LEIO Code plugin release artifacts.

This packages a self-contained copy of the plugin so it can be installed into
another repository or machine without depending on the full Example monorepo
layout.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import stat
import shutil
import tarfile
import tempfile
import zipfile
from pathlib import Path


INCLUDED_PATHS = (
    ".leio-code/config.toml",
    ".codex-plugin",
    ".claude-plugin",
    ".mcp.json",
    "Cargo.toml",
    "Cargo.lock",
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
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description="Package the LEIO Code plugin")
    parser.add_argument(
        "--plugin-root",
        type=Path,
        default=root,
        help="Path to the leio-code plugin root",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=root / "releases",
        help="Directory that will receive the release artifacts",
    )
    parser.add_argument(
        "--format",
        choices=("both", "tar.gz", "zip"),
        default="both",
        help="Artifact format to generate",
    )
    parser.add_argument(
        "--clean",
        action="store_true",
        help="Remove existing matching artifacts before building new ones",
    )
    return parser.parse_args()


def read_plugin_meta(plugin_root: Path) -> tuple[str, str]:
    manifest_path = plugin_root / ".codex-plugin" / "plugin.json"
    data = json.loads(manifest_path.read_text(encoding="utf-8"))
    return str(data["name"]), str(data["version"])


def sha256_hex(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while True:
            chunk = handle.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def copy_payload(plugin_root: Path, staging_root: Path, plugin_name: str) -> Path:
    dest_root = staging_root / plugin_name
    dest_root.mkdir(parents=True, exist_ok=True)
    for rel in INCLUDED_PATHS:
        src = plugin_root / rel
        dest = dest_root / rel
        if not src.exists():
            continue
        if src.is_dir():
            shutil.copytree(
                src,
                dest,
                dirs_exist_ok=True,
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
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dest)
    return dest_root


def normalized_tar_info(tar_info: tarfile.TarInfo) -> tarfile.TarInfo:
    tar_info.uid = 0
    tar_info.gid = 0
    tar_info.uname = ""
    tar_info.gname = ""
    tar_info.mtime = 0
    return tar_info


def build_tar_gz(staging_root: Path, artifact_path: Path, top_level: str) -> None:
    payload_root = staging_root / top_level
    with artifact_path.open("wb") as raw:
        with gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for path in [payload_root, *sorted(payload_root.rglob("*"))]:
                    archive.add(
                        path,
                        arcname=str(path.relative_to(staging_root)),
                        recursive=False,
                        filter=normalized_tar_info,
                    )


def build_zip(staging_root: Path, artifact_path: Path, top_level: str) -> None:
    with zipfile.ZipFile(artifact_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for path in sorted((staging_root / top_level).rglob("*")):
            if path.is_dir():
                continue
            zip_info = zipfile.ZipInfo(
                filename=str(path.relative_to(staging_root)),
                date_time=(1980, 1, 1, 0, 0, 0),
            )
            zip_info.compress_type = zipfile.ZIP_DEFLATED
            mode = stat.S_IMODE(path.stat().st_mode)
            zip_info.external_attr = mode << 16
            archive.writestr(zip_info, path.read_bytes())


def main() -> None:
    args = parse_args()
    plugin_root = args.plugin_root.expanduser().resolve()
    output_dir = args.output_dir.expanduser().resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    plugin_name, version = read_plugin_meta(plugin_root)
    base_name = f"{plugin_name}-plugin-v{version}"

    if args.clean:
        for existing in output_dir.glob(f"{base_name}*"):
            if existing.is_file():
                existing.unlink()

    with tempfile.TemporaryDirectory(prefix=f"{plugin_name}-release-") as tmp:
        staging_root = Path(tmp)
        copy_payload(plugin_root, staging_root, plugin_name)

        artifacts: list[dict[str, str | int]] = []
        if args.format in ("both", "tar.gz"):
            tar_path = output_dir / f"{base_name}.tar.gz"
            build_tar_gz(staging_root, tar_path, plugin_name)
            artifacts.append(
                {
                    "path": str(tar_path),
                    "format": "tar.gz",
                    "sha256": sha256_hex(tar_path),
                    "size_bytes": tar_path.stat().st_size,
                }
            )
        if args.format in ("both", "zip"):
            zip_path = output_dir / f"{base_name}.zip"
            build_zip(staging_root, zip_path, plugin_name)
            artifacts.append(
                {
                    "path": str(zip_path),
                    "format": "zip",
                    "sha256": sha256_hex(zip_path),
                    "size_bytes": zip_path.stat().st_size,
                }
            )

    manifest = {
        "plugin_name": plugin_name,
        "version": version,
        "artifact_base_name": base_name,
        "plugin_root": str(plugin_root),
        "included_paths": list(INCLUDED_PATHS),
        "artifacts": artifacts,
    }
    manifest_path = output_dir / f"{base_name}.manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
