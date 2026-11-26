#!/usr/bin/env python3
"""Exercise the released Windows ZIP with only the bundled runtime on PATH."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import tempfile
import zipfile
from pathlib import Path
from typing import Mapping, Sequence


def run(argv: Sequence[str | Path], *, cwd: Path, env: Mapping[str, str] | None = None) -> str:
    result = subprocess.run(
        [str(arg) for arg in argv], cwd=cwd, env=env, timeout=150,
        capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    if result.returncode:
        raise RuntimeError(f"{argv[0]} exited {result.returncode}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def audit_executable(executable: Path, dumpbin: Path, system32: Path) -> list[str]:
    output = run([dumpbin, "/DEPENDENTS", executable], cwd=executable.parent)
    names = sorted(set(re.findall(r"^\s+([\w.-]+\.dll)\s*$", output, re.MULTILINE | re.IGNORECASE)))
    if not names:
        raise ValueError(f"No PE imports reported for {executable.name}")
    for name in names:
        key = name.casefold()
        if re.match(r"(?:vcruntime|msvcp|msvcr|concrt|vcomp)\d", key):
            raise ValueError(f"{executable.name} requires separately installed {name}")
        if not key.startswith(("api-ms-win-", "ext-ms-win-")) and not (system32 / name).is_file():
            raise ValueError(f"{executable.name} requires non-system {name}")
    print(f"{executable.name} uses {len(names)} Windows system DLLs")
    return names


def smoke(archive: Path, dumpbin: Path, version: str, revision: str) -> None:
    if os.name != "nt":
        raise RuntimeError("Windows portable smoke must run natively on Windows")
    system = Path(os.environ["SystemRoot"])
    system32 = system / "System32"
    with tempfile.TemporaryDirectory(prefix="LEIO release café ") as directory:
        workspace = Path(directory)
        home = workspace / "empty home"
        home.mkdir()
        with zipfile.ZipFile(archive) as package:
            package.extractall(workspace)
        root = workspace / "leio-code"
        node = root / "runtime" / "node.exe"
        git = root / "runtime" / "git" / "cmd" / "git.exe"
        code = root / "bin" / "leio-code.exe"
        harness = root / "bin" / "leio-harness.exe"
        for binary in (code, harness, node):
            audit_executable(binary, dumpbin, system32)
        isolated = {
            "SystemRoot": str(system), "WINDIR": str(system),
            "ComSpec": str(system32 / "cmd.exe"), "PATH": str(system32),
            "TEMP": str(workspace), "TMP": str(workspace),
            "USERPROFILE": str(home), "HOME": str(home),
            "APPDATA": str(home), "LOCALAPPDATA": str(home),
            "LEIO_MCP_HOME": str(home), "GIT_CONFIG_NOSYSTEM": "1",
            "LEIO_DISABLE_EVENTS": "1",
        }

        def bundled(command: str, *args: str) -> str:
            return run([system32 / "cmd.exe", "/d", "/c", command, *args],
                       cwd=root, env=isolated)

        assert f"{version} ({revision[:12]}, clean)" in bundled("leio-code.cmd", "--version")
        assert version in bundled("leio-harness.cmd", "--version")
        registration = json.loads(bundled("leio-mcp.cmd", "--registration"))
        spec = registration["mcpServers"]["leio-code"]
        assert Path(spec["command"]) == node
        launcher = root / "mcp" / "launch.mjs"
        assert list(map(Path, spec["args"])) == [launcher]
        desktop_config = home / "AppData" / "Roaming" / "Claude" / "claude_desktop_config.json"
        desktop_config.parent.mkdir(parents=True)
        desktop_config.write_text('{"mcpServers":{"keep":{"command":"unchanged"}}}', encoding="utf-8")
        bundled("leio-mcp.cmd", "--register")
        updated = json.loads(desktop_config.read_text(encoding="utf-8"))
        assert updated["mcpServers"] == {"leio-code": spec, "keep": {"command": "unchanged"}}
        assert (home / ".leio-code" / "skills" / "leio-code" / "SKILL.md").is_file()
        fixture = workspace / "source project"
        fixture.mkdir()
        (fixture / "module.py").write_text("def caller():\n    return callee()\n\ndef callee():\n    return 1\n", encoding="utf-8")
        with_git = {**isolated, "PATH": os.pathsep.join((str(git.parent), isolated["PATH"]))}
        run([git, "init", "-q"], cwd=fixture, env=with_git)
        run([git, "add", "module.py"], cwd=fixture, env=with_git)
        run([git, "-c", "user.name=Release smoke", "-c", "user.email=smoke@example.invalid",
             "commit", "-qm", "fixture"], cwd=fixture, env=with_git)
        run([code, "--repo", fixture, "index"], cwd=workspace, env=with_git)
        run([code, "--repo", fixture, "export", "formal-context"], cwd=workspace, env=with_git)
        assert (fixture / ".leio-code" / "exports" / "formal-context-v1" / "lattice.json").is_file()

        plan = fixture / "plan.json"
        plan.write_text(json.dumps({
            "request": "Exercise native harness process execution",
            "constraints": [], "questions": [], "acceptance": ["build.txt"],
            "steps": [{
                "name": "build", "kind": "build",
                "argv": [str(node), "-e", "require('node:fs').writeFileSync('build.txt', 'created')"],
                "timeout_ms": 10000, "retry_safe": False, "artifacts": ["build.txt"],
            }],
        }), encoding="utf-8")
        state = [harness, "workflow", "--dir", fixture / "run", "--action"]

        def workflow(action: str, *args: str | Path) -> dict:
            return json.loads(run([*state, action, *args], cwd=workspace, env=with_git))

        digest = workflow("init", "--input", plan, "--repo", fixture)["digest"]
        evidence = fixture / "evidence.json"
        evidence.write_text("{}", encoding="utf-8")
        workflow("evidence", "--input", evidence)
        for action in ("confirm", "approve"):
            workflow(action, "--approval", digest)
        complete = workflow("execute", "--approval", digest)
        assert complete["state"] == "completed", complete
        assert (fixture / "build.txt").read_text(encoding="utf-8") == "created"

        stdio_check = Path(__file__).with_name("verify-stdio.mjs").resolve()
        run([node, stdio_check, launcher], cwd=workspace, env=isolated)
        print("Windows x64 portable ZIP validated native CLI, bundled Git and Node, "
              "lattice export, harness execution, MCP registration and stdio navigation.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--dumpbin", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--revision", required=True)
    args = parser.parse_args()
    smoke(args.archive, args.dumpbin, args.version, args.revision)
