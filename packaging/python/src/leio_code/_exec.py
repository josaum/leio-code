"""Replace this process with the bundled Leio binary."""

from __future__ import annotations

import os
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
    binary = Path(__file__).resolve().parent / "bin" / name
    if not binary.is_file():
        sys.stderr.write(f"leio-code package is missing {binary}\n")
        raise SystemExit(1)
    os.execv(binary, [str(binary), *sys.argv[1:]])
