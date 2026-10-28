"""Replace this process with the bundled Leio binary."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path


def code() -> None:
    _register()
    _exec("leio-code")


def harness() -> None:
    _register()
    _exec("leio-harness")


def _register() -> None:
    from leio_code.register import ensure

    ensure()


def _exec(name: str) -> None:
    bindir = Path(__file__).resolve().parent / "bin"
    binary = bindir / (f"{name}.exe" if sys.platform == "win32" else name)
    if not binary.is_file():
        fallback = bindir / name
        if fallback.is_file():
            binary = fallback
        else:
            sys.stderr.write(f"leio-code package is missing {binary}\n")
            raise SystemExit(1)
    if sys.platform == "win32":
        raise SystemExit(subprocess.run([str(binary), *sys.argv[1:]]).returncode)
    os.execv(binary, [str(binary), *sys.argv[1:]])
