import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "benchmark_context.py"
SPEC = importlib.util.spec_from_file_location("benchmark_context", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
benchmark_context = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark_context)


class ContextBenchmarkTests(unittest.TestCase):
    def test_validate_task_accepts_expected_context_surface(self) -> None:
        task = {
            "name": "sample",
            "expected_paths_any": ["apps-sdk/server.js"],
            "expected_doctor_kinds_any": ["self-contract"],
            "expected_tests_any": ["npm run check"],
            "min_files": 1,
            "min_confidence": 0.8,
            "min_context_zones": 5,
            "expected_context_zone_order": [
                "instructions",
                "memory",
                "anchors",
                "ranked_files",
                "graph_followups",
                "verification",
                "risks",
            ],
            "expected_instruction_paths_any": ["AGENTS.md"],
            "expected_instruction_kinds_any": ["agent_instructions"],
            "expected_memory_paths_any": ["docs/agent-memory.md"],
            "expected_memory_kinds_any": ["memory_bank"],
            "expected_anchor_ids_any": ["#release-verify-contract"],
            "expected_anchor_paths_any": ["scripts/verify_release_artifact.py"],
        }
        envelope = {
            "confidence": 0.86,
            "summary": "ok",
            "entities": [
                {
                    "context_zones": [
                        {"name": "instructions"},
                        {"name": "memory"},
                        {"name": "anchors"},
                        {"name": "ranked_files"},
                        {"name": "graph_followups"},
                        {"name": "verification"},
                        {"name": "risks"},
                    ],
                    "instruction_sources": [
                        {"path": "AGENTS.md", "kind": "agent_instructions"}
                    ],
                    "memory_sources": [
                        {"path": "docs/agent-memory.md", "kind": "memory_bank"}
                    ],
                    "verification_anchors": [
                        {
                            "anchor": "#release-verify-contract",
                            "path": "scripts/verify_release_artifact.py",
                        }
                    ],
                    "files_to_read": [{"path": "apps-sdk/server.js"}],
                    "doctor_suggestions": [{"kind": "self-contract"}],
                    "tests_to_run": [{"command": "cd apps-sdk && npm run check && npm test"}],
                }
            ],
        }

        result = benchmark_context.validate_task(task, envelope)

        self.assertEqual(result["name"], "sample")
        self.assertEqual(result["matched_paths"], ["apps-sdk/server.js"])
        self.assertEqual(result["matched_instruction_paths"], ["AGENTS.md"])
        self.assertEqual(result["matched_memory_paths"], ["docs/agent-memory.md"])
        self.assertEqual(result["matched_anchors"], ["#release-verify-contract"])

    def test_validate_task_rejects_missing_expected_doctor(self) -> None:
        task = {
            "name": "missing-doctor",
            "expected_doctor_kinds_any": ["self-contract"],
        }
        envelope = {
            "confidence": 0.86,
            "entities": [
                {
                    "files_to_read": [{"path": "apps-sdk/server.js"}],
                    "doctor_suggestions": [],
                    "tests_to_run": [],
                }
            ],
        }

        with self.assertRaisesRegex(RuntimeError, "missing expected doctor"):
            benchmark_context.validate_task(task, envelope)


if __name__ == "__main__":
    unittest.main()
