"""Plugin MCP launch must handshake when the host cwd is not the plugin root."""

from __future__ import annotations

import json
import os
import select
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
LAUNCH = ROOT / "scripts" / "launch-stdio-mcp.sh"


class PluginMcpManifestTests(unittest.TestCase):
    def test_claude_plugin_inlines_plugin_root_launch(self) -> None:
        plugin = json.loads((ROOT / ".claude-plugin" / "plugin.json").read_text())
        servers = plugin["mcpServers"]
        self.assertIsInstance(servers, dict)
        args = servers["leio-code"]["args"]
        joined = " ".join(args)
        self.assertIn("${CLAUDE_PLUGIN_ROOT}", joined)
        self.assertIn("launch-stdio-mcp.sh", joined)

    def test_project_mcp_json_keeps_a_repo_relative_server(self) -> None:
        project = json.loads((ROOT / ".mcp.json").read_text())
        server = project["mcpServers"]["leio-code"]
        self.assertEqual(server["command"], "bash")
        self.assertTrue(
            any("launch-stdio-mcp.sh" in arg for arg in server["args"]),
            server["args"],
        )

    def test_launch_script_resolves_plugin_root(self) -> None:
        src = LAUNCH.read_text()
        self.assertIn("CLAUDE_PLUGIN_ROOT", src)
        self.assertIn("mcp/index.js", src)

    def test_launch_script_pins_cargo_installed_binary(self) -> None:
        src = LAUNCH.read_text()
        self.assertIn("LEIO_CODE_BIN", src)
        self.assertIn("${HOME}/.cargo/bin/leio-code", src)

    def test_desktop_manifest_uses_bundled_node_entry(self) -> None:
        manifest = json.loads((ROOT / "manifest.json").read_text())
        cfg = manifest["server"]["mcp_config"]
        self.assertEqual(cfg["command"], "node")
        joined = " ".join(cfg["args"])
        self.assertIn("${__dirname}/mcp/index.js", joined)
        self.assertIn("LEIO_CODE_BIN", cfg.get("env", {}))


class PluginMcpLaunchProcessTests(unittest.TestCase):
    def test_relative_launch_from_foreign_cwd_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            completed = subprocess.run(
                ["bash", "./scripts/launch-stdio-mcp.sh"],
                cwd=tmp,
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertNotEqual(completed.returncode, 0)

    def test_absolute_launch_from_foreign_cwd_handshakes(self) -> None:
        env = os.environ.copy()
        env["CLAUDE_PLUGIN_ROOT"] = str(ROOT)
        proc = subprocess.Popen(
            ["bash", str(LAUNCH)],
            cwd=tempfile.gettempdir(),
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            proc.stdin.write(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": "2025-11-25",
                            "capabilities": {},
                            "clientInfo": {
                                "name": "leio-plugin-launch-test",
                                "version": "0",
                            },
                        },
                    }
                ).encode()
                + b"\n"
            )
            proc.stdin.flush()
            message = _read_ndjson(proc.stdout, 12.0)
            self.assertIsNotNone(message, "stdio MCP did not answer initialize")
            self.assertEqual(message.get("id"), 1)
            result = message.get("result") or {}
            self.assertIn(
                result.get("protocolVersion"),
                ("2025-11-25", "2026-07-28"),
                message,
            )
            self.assertEqual(result.get("serverInfo", {}).get("name"), "leio-code")
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=3)
            for stream in (proc.stdin, proc.stdout, proc.stderr):
                if stream is not None:
                    stream.close()


def _read_ndjson(stream, seconds: float) -> dict | None:
    deadline = time.monotonic() + seconds
    buf = b""
    fd = stream.fileno()
    while time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        ready, _, _ = select.select([fd], [], [], remaining)
        if not ready:
            return None
        chunk = os.read(fd, 4096)
        if not chunk:
            return None
        buf += chunk
        if b"\n" not in buf:
            continue
        line, _rest = buf.split(b"\n", 1)
        line = line.strip()
        if not line:
            buf = _rest
            continue
        return json.loads(line)
    return None


if __name__ == "__main__":
    unittest.main()
