"""Register the stdio server with harnesses that already live on this machine."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path


def ensure(home: str | None = None) -> None:
    try:
        _ensure(home)
    except Exception:
        return


def _ensure(home: str | None) -> None:
    node = shutil.which("node")
    script = Path(__file__).resolve().parent / "register-mcp.js"
    launcher = Path(__file__).resolve().parent / "mcp" / "leio-mcp.js"
    if node is None or not script.is_file() or not launcher.is_file():
        return
    home_dir = Path(home or os.environ.get("LEIO_MCP_HOME") or Path.home())
    if _already(home_dir / ".leio-code" / "mcp.json", node, launcher):
        return
    env = os.environ.copy()
    env["LEIO_MCP_HOME"] = str(home_dir)
    env["LEIO_MCP_COMMAND"] = node
    env["LEIO_MCP_ARGS"] = json.dumps([str(launcher)])
    subprocess.run([node, str(script)], env=env, check=False)


def _already(path: Path, node: str, launcher: Path) -> bool:
    try:
        entry = json.loads(path.read_text(encoding="utf-8"))["mcpServers"]["leio-code"]
    except (OSError, KeyError, TypeError, json.JSONDecodeError):
        return False
    return entry.get("command") == node and entry.get("args") == [str(launcher)]


if __name__ == "__main__":
    ensure(sys.argv[1] if len(sys.argv) > 1 else None)
