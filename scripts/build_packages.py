#!/usr/bin/env python3
"""Build the leio-code wheel and npm package from published release binaries.

The installers ship those binaries. They do not compile Rust on the machine
that installs them.
"""

from __future__ import annotations

import hashlib
import base64
import re
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / "packaging" / "dist"
SERVER_FILES = (
    "index.js",
    "compact.js",
    "orientation.js",
    "envelope.js",
    "guide.js",
    "workflow.js",
    "mcp-spec-2025-11-25.js",
    "output-schemas.js",
    "install-root.js",
    "resolve-binary.js",
    "export-paths.js",
    "evidence-contract.js",
    "watch-state.js",
)


def server_modules() -> Path:
    cached = DIST / "server-modules"
    marker = cached / "node_modules" / "zod" / "package.json"
    if marker.is_file():
        return cached / "node_modules"
    if cached.exists():
        shutil.rmtree(cached)
    cached.mkdir(parents=True)
    (cached / "package.json").write_text(
        '{"name":"leio-code-server","private":true,"version":"2.6.5"}\n',
        encoding="utf-8",
    )
    subprocess.run(
        ["npm", "install", "--omit=dev", "--ignore-scripts", "@modelcontextprotocol/sdk@1.28.0", "zod@3.25.76"],
        cwd=cached,
        check=True,
    )
    return cached / "node_modules"


def stage_server(dest: Path, *, with_modules: bool = False) -> None:
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    for name in SERVER_FILES:
        shutil.copy2(ROOT / "mcp" / name, dest / name)
    (dest / "package.json").write_text(
        '{"name":"leio-code-server","private":true,"type":"module","version":"2.6.5"}\n',
        encoding="utf-8",
    )
    shutil.copy2(ROOT / "packaging" / "npm" / "bin" / "leio-mcp.js", dest / "leio-mcp.js")
    if with_modules:
        shutil.copytree(server_modules(), dest / "node_modules")
VERSION = "2.6.5"
TAG = f"leio-code-plugin-v{VERSION}"
REPO = "josaum/leio-code"

PLATFORMS = (
    {
        "code": "leio-code-darwin-arm64",
        "harness": "leio-code-darwin-arm64-harness",
        "tag": "py3-none-macosx_11_0_arm64",
    },
    {
        "code": "leio-code-darwin-amd64",
        "harness": "leio-code-darwin-amd64-harness",
        "tag": "py3-none-macosx_11_0_x86_64",
    },
    {
        "code": "leio-code-linux-amd64",
        "harness": "leio-code-linux-amd64-harness",
        "tag": "py3-none-manylinux_2_34_x86_64",
    },
)


def download(name: str) -> Path:
    dest = DIST / "assets" / name
    if dest.is_file() and dest.stat().st_size > 0:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    url = f"https://github.com/{REPO}/releases/download/{TAG}/{name}"
    print(f"download {url}", file=sys.stderr)
    with urlopen(url) as response:
        dest.write_bytes(response.read())
    dest.chmod(0o755)
    return dest


def linux_tag(binary: Path) -> str:
    versions = [
        tuple(int(part) for part in match.split(b"."))
        for match in re.findall(br"GLIBC_(\d+\.\d+)", binary.read_bytes())
    ]
    if not versions:
        return "py3-none-manylinux_2_34_x86_64"
    major, minor = max(versions)
    return f"py3-none-manylinux_{major}_{minor}_x86_64"


def record_line(path: str, data: bytes) -> str:
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{path},sha256={digest},{len(data)}"


def retag_wheel(source: Path, tag: str) -> Path:
    info = f"leio_code-{VERSION}.dist-info"
    output = DIST / f"leio_code-{VERSION}-{tag}.whl"
    with zipfile.ZipFile(source) as original:
        items = original.infolist()
        files = {item.filename: original.read(item.filename) for item in items}
        modes = {item.filename: item.external_attr >> 16 for item in items}
    wheel_name = f"{info}/WHEEL"
    wheel_text = files[wheel_name].decode()
    wheel_text = re.sub(r"^Tag: .*$", f"Tag: {tag}", wheel_text, flags=re.M)
    wheel_text = wheel_text.replace("Root-Is-Purelib: true", "Root-Is-Purelib: false")
    files[wheel_name] = wheel_text.encode()
    for name in ("leio_code/bin/leio-code", "leio_code/bin/leio-harness"):
        modes[name] = 0o100755
    files["leio_register.pth"] = (
        b"import leio_code.register; leio_code.register.ensure()\n"
    )
    modes["leio_register.pth"] = 0o100644
    record_name = f"{info}/RECORD"
    lines = []
    for name in sorted(files):
        if name == record_name:
            continue
        lines.append(record_line(name, files[name]))
    lines.append(f"{record_name},,")
    files[record_name] = ("\n".join(lines) + "\n").encode()
    with zipfile.ZipFile(output, "w") as rebuilt:
        for name, data in files.items():
            entry = zipfile.ZipInfo(filename=name)
            entry.external_attr = modes.get(name, 0o100644) << 16
            entry.compress_type = zipfile.ZIP_DEFLATED
            rebuilt.writestr(entry, data)
    return output


def build_wheel(platform: dict[str, str]) -> Path:
    code = download(platform["code"])
    harness = download(platform["harness"])
    tag = linux_tag(code) if platform["code"].endswith("linux-amd64") else platform["tag"]
    stage = DIST / "stage" / tag
    if stage.exists():
        shutil.rmtree(stage)
    shutil.copytree(
        ROOT / "packaging" / "python",
        stage,
        ignore=shutil.ignore_patterns("__pycache__", "*.egg-info"),
    )
    bindir = stage / "src" / "leio_code" / "bin"
    bindir.mkdir(parents=True)
    shutil.copy2(code, bindir / "leio-code")
    shutil.copy2(harness, bindir / "leio-harness")
    (bindir / "leio-code").chmod(0o755)
    (bindir / "leio-harness").chmod(0o755)
    mcp_dir = stage / "src" / "leio_code" / "mcp"
    stage_server(mcp_dir, with_modules=True)
    shutil.copy2(ROOT / "packaging" / "register-mcp.js", stage / "src" / "leio_code" / "register-mcp.js")
    wheel_dir = DIST / "wheels-any"
    wheel_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [sys.executable, "-m", "pip", "wheel", "--no-deps", str(stage), "-w", str(wheel_dir)],
        check=True,
    )
    built = next(wheel_dir.glob(f"leio_code-{VERSION}-*.whl"))
    tagged = retag_wheel(built, tag)
    print(tagged, file=sys.stderr)
    return tagged


def pack_npm() -> Path:
    stage = DIST / "npm"
    if stage.exists():
        shutil.rmtree(stage)
    shutil.copytree(
        ROOT / "packaging" / "npm",
        stage,
        ignore=shutil.ignore_patterns("node_modules", "vendor"),
    )
    stage_server(stage / "server")
    shutil.copy2(ROOT / "packaging" / "register-mcp.js", stage / "register-mcp.js")
    install_js = (stage / "install.js").read_text(encoding="utf-8")
    (stage / "install.js").write_text(
        install_js.replace('from "../register-mcp.js"', 'from "./register-mcp.js"'),
        encoding="utf-8",
    )
    subprocess.run(["npm", "pack", "--pack-destination", str(DIST)], cwd=stage, check=True)
    tarball = DIST / f"leio-code-{VERSION}.tgz"
    print(tarball, file=sys.stderr)
    return tarball


def main() -> None:
    DIST.mkdir(parents=True, exist_ok=True)
    for platform in PLATFORMS:
        build_wheel(platform)
    pack_npm()


if __name__ == "__main__":
    main()
