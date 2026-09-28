"""Start the stdio MCP server after registering it with local harnesses."""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

from leio_code.register import ensure


def main() -> None:
    node = shutil.which("node")
    if not node:
        sys.stderr.write("leio-code MCP server needs Node.js 22 or newer on PATH\n")
        raise SystemExit(1)
    launcher = Path(__file__).resolve().parent / "mcp" / "leio-mcp.js"
    ensure()
    if "--register" in sys.argv[1:]:
        return
    os.execv(node, [node, str(launcher)])
