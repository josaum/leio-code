import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "benchmark_models.py"
SPEC = importlib.util.spec_from_file_location("benchmark_models", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
benchmark_models = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark_models)

PLUGIN_ROOT = Path(__file__).resolve().parents[1]
GOLDEN = json.loads((PLUGIN_ROOT / "benchmarks" / "context-golden-tasks.json").read_text())


def make_args(**overrides):
    defaults = dict(
        models=list(benchmark_models.DEFAULT_MODELS[:2]),
        agent="codex-model",
        argv=None,
        tasks=PLUGIN_ROOT / "benchmarks" / "context-golden-tasks.json",
        repo=PLUGIN_ROOT,
        worktree_root=Path("/tmp/leio-bench-wt"),
        out=Path("/tmp/leio-bench-test-out"),
        timeout_ms=600_000,
        parallel=False,
        bus=None,
        harness="leio-harness",
        execute=False,
        with_context=False,
        reference_answers=None,
        leio_bin="",
        prompt_variant="baseline",
    )
    defaults.update(overrides)
    import argparse

    return argparse.Namespace(**defaults)


class ExtractLastJsonTests(unittest.TestCase):
    def test_returns_last_object_after_prose_and_fences(self) -> None:
        text = '```json\n{"answer": "wrong"}\n```\nBlah blah\n{"answer": "right", "x": 1}\n'
        self.assertEqual(benchmark_models.extract_last_json(text)["answer"], "right")

    def test_returns_none_without_object(self) -> None:
        self.assertIsNone(benchmark_models.extract_last_json("no json here"))

    def test_returns_none_for_invalid_json_only(self) -> None:
        self.assertIsNone(benchmark_models.extract_last_json("{not json}"))

    def test_nested_object_parses_completely(self) -> None:
        text = 'prefix {"a": {"b": [1, 2]}} suffix'
        self.assertEqual(benchmark_models.extract_last_json(text), {"a": {"b": [1, 2]}})


class ExpectedAnyTests(unittest.TestCase):
    def test_combines_all_expected_lists(self) -> None:
        task = {
            "expected_paths_any": ["src/main.rs"],
            "expected_instruction_paths_any": ["AGENTS.md"],
            "expected_memory_paths_any": ["docs/agent-memory.md"],
            "expected_tests_any": ["npm run check"],
        }
        self.assertEqual(
            benchmark_models.expected_any(task),
            ["src/main.rs", "AGENTS.md", "docs/agent-memory.md", "npm run check"],
        )

    def test_tolerates_missing_keys(self) -> None:
        self.assertEqual(benchmark_models.expected_any({}), [])


class ShortModelTests(unittest.TestCase):
    def test_strips_publisher_and_normalizes(self) -> None:
        self.assertEqual(benchmark_models.short_model("openai/gpt-5.6-luna"), "gpt-5-6-luna")

    def test_truncates_to_32_chars(self) -> None:
        self.assertLessEqual(len(benchmark_models.short_model("x/" + "a" * 50)), 32)


class BuildSpecTests(unittest.TestCase):
    def test_lane_count_is_tasks_times_models(self) -> None:
        args = make_args()
        spec, plan, reference_answers = benchmark_models.build_spec(args, GOLDEN["tasks"])
        self.assertEqual(len(spec["lanes"]), len(GOLDEN["tasks"]) * len(args.models))
        self.assertEqual(len(plan), len(spec["lanes"]))
        self.assertEqual(spec["lanes"][0]["agent"], "codex-model")
        self.assertIn("deepseek/deepseek-v4-flash-0731", spec["goal"])

    def test_spec_uses_camel_case_day_contract(self) -> None:
        spec, _, _ = benchmark_models.build_spec(make_args(), GOLDEN["tasks"][:1])
        for key in ("worktreeRoot", "outputDir", "timeoutMs", "requireFreshCodeView", "agentTemplates"):
            self.assertIn(key, spec)
        lane = spec["lanes"][0]
        for key in ("agentId", "agent", "model", "task", "timeoutMs"):
            self.assertIn(key, lane)

    def test_custom_argv_overrides_agent_key(self) -> None:
        args = make_args(argv=["/usr/bin/env", "my-cli", "--model", "{model}", "{task}"])
        spec, _, _ = benchmark_models.build_spec(args, GOLDEN["tasks"][:1])
        self.assertIn("custom-model", spec["agentTemplates"])
        self.assertEqual(spec["agentTemplates"]["custom-model"]["argv"], args.argv)

    def test_reference_notes_are_loaded_from_a_local_mapping(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            notes = Path(tmp) / "notes.json"
            task = GOLDEN["tasks"][0]
            notes.write_text(json.dumps({task["name"]: "Inspect the build artifact."}))
            spec, _, reference = benchmark_models.build_spec(
                make_args(reference_answers=notes), [task]
            )
            self.assertEqual(reference[task["name"]], "Inspect the build artifact.")
            self.assertIn("Inspect the build artifact.", spec["lanes"][0]["task"])

    def test_reference_notes_reject_nontext_values(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            notes = Path(tmp) / "notes.json"
            notes.write_text('{"task": {"unexpected": true}}')
            with self.assertRaisesRegex(ValueError, "map task names to text"):
                benchmark_models.build_spec(make_args(reference_answers=notes), GOLDEN["tasks"][:1])

    def test_unknown_agent_key_rejected(self) -> None:
        with self.assertRaises(SystemExit):
            benchmark_models.build_spec(make_args(agent="nope"), GOLDEN["tasks"][:1])

    def test_lane_prompt_contains_task_and_verdict_shape(self) -> None:
        spec, _, _ = benchmark_models.build_spec(make_args(), GOLDEN["tasks"][:1])
        self.assertIn(GOLDEN["tasks"][0]["task"], spec["lanes"][0]["task"])
        self.assertIn("evidence_paths", spec["lanes"][0]["task"])


class GradeLaneTests(unittest.TestCase):
    def setUp(self) -> None:
        self.task = GOLDEN["tasks"][0]
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.stdout = Path(self.tmp.name) / "bench-x.stdout.log"
        verdict = {
            "answer": "fix the docker smoke; npm run check gates it",
            "evidence_paths": ["apps-sdk/server.js", "totally/unrelated.rs"],
            "tests": ["npm run check"],
        }
        self.stdout.write_text("thinking...\n" + json.dumps(verdict) + "\n")

    def outcome(self, *, producer="lane:bench-x", path=None, status="passed", error=None):
        return {
            "agentId": "bench-x",
            "status": status,
            "durationMs": 1234,
            "error": error,
            "artifacts": [
                {"path": str(path or self.stdout), "producer": producer, "sha256": "0" * 64, "byteSize": 10}
            ],
        }

    def test_happy_path_scores_json_evidence_and_tests(self) -> None:
        grade = benchmark_models.grade_lane(self.outcome(), self.task, Path(self.tmp.name))
        self.assertTrue(grade["json_valid"])
        self.assertEqual(grade["status"], "passed")
        self.assertEqual(grade["duration_ms"], 1234)
        self.assertIn("apps-sdk/server.js", grade["evidence_hits"])
        self.assertNotIn("totally/unrelated.rs", grade["evidence_hits"])
        self.assertGreater(grade["evidence_ratio"], 0.0)
        self.assertEqual(grade["tests_hit"], ["npm run check"])

    def test_missing_artifact_reports_error(self) -> None:
        outcome = self.outcome()
        outcome["artifacts"] = []
        grade = benchmark_models.grade_lane(outcome, self.task, Path(self.tmp.name))
        self.assertFalse(grade["json_valid"])
        self.assertIn("stdout artifact", str(grade["error"]))

    def test_stdout_without_json_reports_error(self) -> None:
        self.stdout.write_text("no verdict here\n")
        grade = benchmark_models.grade_lane(self.outcome(), self.task, Path(self.tmp.name))
        self.assertFalse(grade["json_valid"])
        self.assertIn("verdict", str(grade["error"]))

    def test_non_object_verdict_reports_error(self) -> None:
        self.stdout.write_text('["a", "list"]\n')
        grade = benchmark_models.grade_lane(self.outcome(), self.task, Path(self.tmp.name))
        self.assertFalse(grade["json_valid"])

    def test_error_passthrough_from_outcome(self) -> None:
        grade = benchmark_models.grade_lane(
            self.outcome(status="failed", error="boom"), self.task, Path(self.tmp.name)
        )
        self.assertEqual(grade["status"], "failed")
        self.assertEqual(grade["error"], "boom")

    def test_fallback_matches_artifact_without_producer(self) -> None:
        outcome = self.outcome(producer="")
        grade = benchmark_models.grade_lane(outcome, self.task, Path(self.tmp.name))
        self.assertTrue(grade["json_valid"])

    def test_fallback_matches_artifact_path_containing_agent_id(self) -> None:
        path = self.stdout.parent / "bench-x.stdout.log"
        path.write_text(self.stdout.read_text())
        outcome = self.outcome(producer="someone-else", path=path)
        grade = benchmark_models.grade_lane(outcome, self.task, Path(self.tmp.name))
        self.assertTrue(grade["json_valid"])

    def test_last_resort_matches_any_stdout_log(self) -> None:
        outcome = self.outcome(producer="someone-else")
        grade = benchmark_models.grade_lane(outcome, self.task, Path(self.tmp.name))
        self.assertTrue(grade["json_valid"])


class FakeHarnessMainTests(unittest.TestCase):
    """End-to-end `--execute` through a fake harness binary (no model spend)."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.stdout = self.root / "lane.stdout.log"
        self.stdout.write_text(json.dumps({
            "answer": "ok",
            "evidence_paths": ["apps-sdk/server.js"],
            "tests": ["npm run check"],
        }))
        day_report = {
            "goal": "sweep",
            "outcomes": [
                {
                    "agentId": "bench-gpt-5-6-luna-apps-sdk-docker-smoke",
                    "runId": "r1",
                    "branch": "b",
                    "worktree": "w",
                    "status": "passed",
                    "durationMs": 500,
                    "error": None,
                    "artifacts": [
                        {"path": str(self.stdout), "producer": "lane:bench-gpt-5-6-luna-apps-sdk-docker-smoke",
                         "sha256": "0" * 64, "byteSize": 20},
                    ],
                },
                {
                    "agentId": "bench-unknown-lane",
                    "runId": "r2",
                    "branch": "b2",
                    "worktree": "w2",
                    "status": "failed",
                    "durationMs": 1,
                    "error": None,
                    "artifacts": [],
                },
            ],
            "passed": 1,
            "failed": 1,
            "leaseStore": str(self.root / "leases.json"),
        }
        self.day_report_path = self.root / "report.json"
        self.day_report_path.write_text(json.dumps(day_report))
        self.fake = self.root / "fake-harness.sh"
        self.fake.write_text("#!/usr/bin/env bash\ncat " + json.dumps(str(self.day_report_path)) + "\n")
        self.fake.chmod(0o755)

    def run_main(self, *extra: str) -> tuple[int, str]:
        import contextlib
        import io
        from unittest import mock

        out = self.root / "sweep"
        argv = [
            "benchmark_models.py",
            "--models", "openai/gpt-5.6-luna",
            "--out", str(out),
            "--harness", str(self.fake),
            *extra,
        ]
        buffer = io.StringIO()
        with mock.patch("sys.argv", argv), contextlib.redirect_stdout(buffer):
            code = benchmark_models.main()
        return code, buffer.getvalue()

    def test_execute_runs_grades_and_writes_reports(self) -> None:
        code, stdout = self.run_main("--execute", "--bus", "127.0.0.1:18815")
        self.assertEqual(code, 0)
        out = self.root / "sweep"
        self.assertIn("day spec:", stdout)
        self.assertIn("```widget", stdout)
        self.assertIn("jai-bench", stdout)

        summary = json.loads((out / "model-sweep-report.json").read_text())
        self.assertEqual(len(summary["models"]), 1)
        model = summary["models"][0]
        self.assertEqual(model["model"], "openai/gpt-5.6-luna")
        self.assertEqual(model["lanes"], len(GOLDEN["tasks"]))
        self.assertEqual(model["graded"], 1)  # synthetic report has 1 outcome
        self.assertEqual(model["missing"], len(GOLDEN["tasks"]) - 1)
        self.assertEqual(model["json_valid"], 1)
        self.assertEqual(model["median_duration_ms"], 500)
        self.assertTrue((out / "model-sweep-report.md").exists())
        unknown = [lane for lane in summary["lanes"] if lane["agentId"] == "bench-unknown-lane"]
        self.assertEqual(unknown[0]["error"], "no plan entry")

    def test_execute_fails_loudly_when_harness_fails(self) -> None:
        self.fake.write_text("#!/usr/bin/env bash\necho boom >&2\nexit 3\n")
        with self.assertRaises(SystemExit):
            self.run_main("--execute")

    def test_dry_run_writes_spec_without_running(self) -> None:
        code, stdout = self.run_main()
        self.assertEqual(code, 0)
        out = self.root / "sweep"
        self.assertIn("dry-run", stdout)
        self.assertTrue((out / "day-spec.json").exists())
        self.assertFalse((out / "model-sweep-report.json").exists())


if __name__ == "__main__":
    unittest.main()
