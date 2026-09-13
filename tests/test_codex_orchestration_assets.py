from __future__ import annotations

import json
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]

EXPECTED_ROLES = {
    "example_explorer": ("gpt-5.6-terra", "low", "read-only"),
    "example_worker": ("gpt-5.6-terra", "medium", "workspace-write"),
    "example_verifier": ("gpt-5.6-luna", "medium", "workspace-write"),
    "example_reviewer": ("gpt-5.6-sol", "high", "read-only"),
    "example_security": ("gpt-5.6-sol", "xhigh", "read-only"),
}


class CodexOrchestrationAssetsTest(unittest.TestCase):
    def test_project_config_pins_alpha_v2_bounded_runtime(self) -> None:
        config = tomllib.loads((ROOT / ".codex/config.toml").read_text())

        self.assertEqual(config["model"], "gpt-5.6-sol")
        self.assertEqual(config["model_reasoning_effort"], "medium")
        self.assertEqual(config["plan_mode_reasoning_effort"], "high")
        self.assertEqual(config["sandbox_mode"], "workspace-write")
        self.assertTrue(config["features"]["multi_agent"])
        self.assertTrue(config["features"]["hooks"])
        self.assertTrue(config["features"]["multi_agent_v2"]["enabled"])
        self.assertEqual(
            config["features"]["multi_agent_v2"][
                "max_concurrent_threads_per_session"
            ],
            4,
        )
        self.assertNotIn("max_threads", config["agents"])
        self.assertEqual(config["agents"]["max_depth"], 1)
        self.assertEqual(config["agents"]["job_max_runtime_seconds"], 1200)
        self.assertIs(config["agents"]["interrupt_message"], True)
        self.assertNotIn("mcp_servers", config)

    def test_each_role_has_an_explicit_model_effort_and_sandbox(self) -> None:
        for name, expected in EXPECTED_ROLES.items():
            with self.subTest(role=name):
                data = tomllib.loads(
                    (ROOT / f".codex/agents/{name}.toml").read_text()
                )
                self.assertEqual(data["name"], name)
                self.assertEqual(
                    (
                        data["model"],
                        data["model_reasoning_effort"],
                        data["sandbox_mode"],
                    ),
                    expected,
                )
                self.assertTrue(data["description"].strip())
                self.assertTrue(data["developer_instructions"].strip())

    def test_codex_assets_are_not_ignored(self) -> None:
        patterns = {
            line.split("#", 1)[0].strip()
            for line in (ROOT / ".gitignore").read_text().splitlines()
        }
        self.assertFalse(
            patterns.intersection({".codex", ".codex/", "/.codex", "/.codex/"})
        )

    def test_root_instructions_define_roles_and_writer_ownership(self) -> None:
        instructions = (ROOT / "AGENTS.md").read_text()
        for role in EXPECTED_ROLES:
            self.assertIn(role, instructions)
        self.assertIn(
            "never assign overlapping file ownership to concurrent writers",
            instructions,
        )

    def test_hook_manifest_uses_only_supported_portable_events(self) -> None:
        manifest = json.loads((ROOT / ".codex/hooks.json").read_text())
        hooks = manifest["hooks"]
        self.assertEqual(
            set(hooks),
            {
                "SessionStart",
                "SubagentStart",
                "PreToolUse",
                "PostToolUse",
                "SubagentStop",
                "Stop",
            },
        )
        for registrations in hooks.values():
            for registration in registrations:
                for action in registration["hooks"]:
                    command = action["command"]
                    self.assertIn(
                        'repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0',
                        command,
                    )
                    self.assertIn(
                        'python3 "$repo_root/.codex/hooks/leio_codex_hook.py"',
                        command,
                    )
                    self.assertNotIn("/Users/", command)
                    self.assertLessEqual(action["timeout"], 10)

    def test_hook_commands_fail_open_outside_git_repository(self) -> None:
        manifest = json.loads((ROOT / ".codex/hooks.json").read_text())
        payload = json.dumps({"cwd": "/tmp/not-a-project"})

        with tempfile.TemporaryDirectory() as tempdir:
            for event, registrations in manifest["hooks"].items():
                for registration in registrations:
                    for action in registration["hooks"]:
                        with self.subTest(event=event):
                            completed = subprocess.run(
                                action["command"],
                                cwd=tempdir,
                                input=payload,
                                capture_output=True,
                                text=True,
                                shell=True,
                                check=False,
                            )
                            self.assertEqual(completed.returncode, 0)
                            self.assertEqual(completed.stdout, "")
                            self.assertEqual(completed.stderr, "")

    def test_makefile_exposes_durable_orchestration_gate(self) -> None:
        makefile = (ROOT / "Makefile").read_text()
        self.assertIn("leio-code-codex-orchestration-verify:", makefile)
        self.assertIn("doctor codex-orchestration", makefile)
        self.assertIn("doctor leio-release-coherence", makefile)


if __name__ == "__main__":
    unittest.main()
