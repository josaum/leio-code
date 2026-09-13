from __future__ import annotations

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
HOOK_PATH = ROOT / ".codex/hooks/leio_codex_hook.py"
SPEC = importlib.util.spec_from_file_location("leio_codex_hook", HOOK_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load hook module from {HOOK_PATH}")
hook = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(hook)


class CodexHookTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name).resolve()

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def test_apply_patch_paths_come_from_tool_input_command(self) -> None:
        payload = {
            "tool_name": "apply_patch",
            "tool_input": {
                "command": "*** Begin Patch\n*** Update File: leio-code/src/main.rs\n*** Add File: .codex/config.toml\n*** End Patch"
            },
        }
        self.assertEqual(
            hook.changed_paths(payload, self.root),
            ["leio-code/src/main.rs", ".codex/config.toml"],
        )

    def test_changed_paths_reject_traversal_deduplicate_and_cap(self) -> None:
        entries = ["*** Update File: ../secret"]
        entries.extend(f"*** Add File: src/file-{index}.rs" for index in range(20))
        entries.append("*** Update File: src/file-0.rs")
        payload = {
            "tool_name": "apply_patch",
            "tool_input": {"command": "\n".join(entries)},
        }
        paths = hook.changed_paths(payload, self.root)
        self.assertEqual(len(paths), 12)
        self.assertNotIn("../secret", paths)
        self.assertEqual(paths[0], "src/file-0.rs")
        self.assertEqual(len(paths), len(set(paths)))

    def test_edit_and_write_paths_must_resolve_inside_repo(self) -> None:
        inside = self.root / "src/main.rs"
        self.assertEqual(
            hook.changed_paths(
                {"tool_name": "Write", "tool_input": {"file_path": str(inside)}},
                self.root,
            ),
            ["src/main.rs"],
        )
        self.assertEqual(
            hook.changed_paths(
                {"tool_name": "Edit", "tool_input": {"file_path": "/tmp/outside"}},
                self.root,
            ),
            [],
        )

    def test_subagent_context_uses_agent_type(self) -> None:
        context = hook.subagent_context({"agent_type": "example_security"})
        self.assertIn("read-only", context)
        self.assertIn("auth", context)
        self.assertIn("leio_code_context", context)

    def test_failed_deploy_bash_selects_only_targeted_doctors(self) -> None:
        payload = {
            "tool_name": "Bash",
            "tool_input": {"command": "deploy/scripts/deploy-target.sh assurant"},
            "tool_response": {"exit_code": 1, "output": "redacted fixture"},
        }
        self.assertEqual(hook.targeted_doctors(payload), ["deploy", "repo-hygiene"])

    def test_failed_rust_and_leio_commands_are_classified_and_capped(self) -> None:
        rust = {
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test --manifest-path leio-code/Cargo.toml"},
            "tool_response": {"exit_code": 101},
        }
        leio = {
            "tool_name": "Bash",
            "tool_input": {"command": "leio-code doctor all --repo .codex"},
            "tool_response": {"exit_code": 1},
        }
        self.assertEqual(hook.targeted_doctors(rust), ["self-contract", "repo-hygiene"])
        self.assertEqual(
            hook.targeted_doctors(leio),
            ["codex-orchestration", "leio-release-coherence"],
        )

    def test_success_and_malformed_payloads_fail_open(self) -> None:
        self.assertIsNone(hook.route_event("PostToolUse", {}, repo_root=self.root))
        self.assertIsNone(
            hook.route_event(
                "PostToolUse",
                {"tool_response": {"exit_code": 0}},
                repo_root=self.root,
            )
        )

    def test_output_is_bounded_supported_hook_shape(self) -> None:
        output = hook.hook_output("SessionStart", "x" * 20_000)
        specific = output["hookSpecificOutput"]
        self.assertEqual(specific["hookEventName"], "SessionStart")
        self.assertLessEqual(len(specific["additionalContext"]), 4_000)

    def test_route_never_echoes_prompt_environment_or_full_tool_output(self) -> None:
        secret = "do-not-echo-this-secret"
        payload = {
            "prompt": secret,
            "env": {"TOKEN": secret},
            "tool_name": "Bash",
            "tool_input": {"command": "docker compose up"},
            "tool_response": {"exit_code": 1, "output": secret},
        }

        def runner(_root: Path, _args: list[str]) -> dict[str, object]:
            return {
                "summary": "deploy doctor found one warning",
                "evidence": [{"path": "deploy/compose.yml", "line": 4}],
                "warnings": ["bounded warning"],
                "raw": secret,
            }

        output = hook.route_event(
            "PostToolUse", payload, repo_root=self.root, runner=runner
        )
        serialized = json.dumps(output)
        self.assertNotIn(secret, serialized)
        self.assertIn("deploy/compose.yml", serialized)

    def test_invalid_json_timeout_and_nonzero_leio_fail_open(self) -> None:
        completed = subprocess.CompletedProcess([], 0, stdout="not json", stderr="")
        with mock.patch.object(hook.subprocess, "run", return_value=completed):
            self.assertIsNone(hook.run_leio(self.root, ["status"]))
        with mock.patch.object(
            hook.subprocess,
            "run",
            side_effect=subprocess.TimeoutExpired("leio-code", 3),
        ):
            self.assertIsNone(hook.run_leio(self.root, ["status"]))
        completed = subprocess.CompletedProcess([], 2, stdout="{}", stderr="failure")
        with mock.patch.object(hook.subprocess, "run", return_value=completed):
            self.assertIsNone(hook.run_leio(self.root, ["status"]))

    def test_atomic_pending_state_coalesces_normalized_paths(self) -> None:
        hook.record_pending_paths(self.root, ["src/a.rs", "src/b.rs"])
        hook.record_pending_paths(self.root, ["src/b.rs", "src/c.rs"])
        state_path = self.root / ".leio-code/codex-hook-pending.json"
        state = json.loads(state_path.read_text())
        self.assertEqual(state["paths"], ["src/a.rs", "src/b.rs", "src/c.rs"])
        self.assertIn("updated_at", state)
        self.assertFalse(state_path.with_suffix(".tmp").exists())

    def test_stop_requests_one_continuation_and_ignores_retry(self) -> None:
        index = self.root / ".leio-code/index.json"
        index.parent.mkdir(parents=True)
        index.write_text("{}")
        hook.record_pending_paths(self.root, ["src/a.rs"])

        stop = hook.route_event("Stop", {}, repo_root=self.root)
        self.assertEqual(stop["decision"], "block")
        self.assertIn("src/a.rs", stop["reason"])
        self.assertIn("focused LEIO refresh/verification", stop["reason"])
        self.assertIsNone(
            hook.route_event("Stop", {"stop_hook_active": True}, repo_root=self.root)
        )
        self.assertIsNone(hook.route_event("Stop", {}, repo_root=self.root))

    def test_subagent_stop_keeps_context_output_without_blocking(self) -> None:
        index = self.root / ".leio-code/index.json"
        index.parent.mkdir(parents=True)
        index.write_text("{}")
        hook.record_pending_paths(self.root, ["src/a.rs"])

        subagent_stop = hook.route_event("SubagentStop", {}, repo_root=self.root)
        self.assertEqual(
            subagent_stop["hookSpecificOutput"]["hookEventName"], "SubagentStop"
        )
        self.assertIn(
            "src/a.rs", subagent_stop["hookSpecificOutput"]["additionalContext"]
        )

    def test_editing_a_notified_path_rearms_the_stop_hint(self) -> None:
        index = self.root / ".leio-code/index.json"
        index.parent.mkdir(parents=True)
        index.write_text("{}")
        hook.record_pending_paths(self.root, ["src/a.rs"])
        first = hook.route_event("Stop", {}, repo_root=self.root)
        self.assertEqual(first["decision"], "block")

        hook.record_pending_paths(self.root, ["src/a.rs"])
        second = hook.route_event("Stop", {}, repo_root=self.root)
        self.assertEqual(second["decision"], "block")

    def test_stop_fails_open_when_notification_cannot_be_persisted(self) -> None:
        index = self.root / ".leio-code/index.json"
        index.parent.mkdir(parents=True)
        index.write_text("{}")
        hook.record_pending_paths(self.root, ["src/a.rs"])

        with mock.patch.object(hook.os, "replace", side_effect=OSError):
            self.assertIsNone(hook.route_event("Stop", {}, repo_root=self.root))


if __name__ == "__main__":
    unittest.main()
