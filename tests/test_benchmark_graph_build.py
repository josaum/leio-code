import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "benchmark_graph_build.py"
SPEC = importlib.util.spec_from_file_location("benchmark_graph_build", SCRIPT)
BENCHMARK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCHMARK)


class GraphBuildBenchmarkTests(unittest.TestCase):
    def test_timeout_is_failure_without_reading_incomplete_artifacts(self):
        with patch.object(BENCHMARK.subprocess, "run", side_effect=subprocess.TimeoutExpired("leio", 30)):
            result = BENCHMARK.run_build(Path("/bin/leio"), Path("/repo"), Path("/missing"), 30)
        self.assertEqual(result["status"], "timeout")
        self.assertNotIn("graph_sha256", result)

    def test_execution_failure_preserves_exit_and_diagnostics(self):
        failed = subprocess.CompletedProcess([], 7, stdout="", stderr="graph build failed")
        with patch.object(BENCHMARK.subprocess, "run", return_value=failed):
            result = BENCHMARK.run_build(Path("/bin/leio"), Path("/repo"), Path("/missing"), 30)
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["exit_code"], 7)
        self.assertEqual(result["stderr"], "graph build failed")

    def test_success_records_exact_counts_and_artifact_hashes(self):
        keys = (
            "parsed_file_count", "symbol_count", "occurrence_count",
            "import_edge_count", "call_edge_count", "resolved_call_edge_count",
        )
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            manifest = {key: number for number, key in enumerate(keys)}
            manifest["source_fingerprint"] = "source-fingerprint"
            (output / "manifest.json").write_text(json.dumps(manifest))
            (output / "graph.nq").write_text("graph bytes")
            (output / "query-cache.json").write_text("cache bytes")
            succeeded = subprocess.CompletedProcess([], 0, stdout=json.dumps({"timing_ms": 17, "warnings": []}), stderr="")
            with patch.object(BENCHMARK.subprocess, "run", return_value=succeeded) as run:
                result = BENCHMARK.run_build(Path("/bin/leio"), Path("/repo"), output, 30)
            self.assertEqual(result["status"], "passed")
            self.assertEqual(result["counts"], {key: manifest[key] for key in keys})
            self.assertEqual(result["source_fingerprint"], "source-fingerprint")
            self.assertEqual(result["graph_sha256"], BENCHMARK.digest(output / "graph.nq"))
            self.assertEqual(result["cache_sha256"], BENCHMARK.digest(output / "query-cache.json"))
            self.assertEqual(run.call_args.args[0][-2:], ["--output-dir", str(output)])
            self.assertEqual(run.call_args.kwargs["timeout"], 30)


if __name__ == "__main__":
    unittest.main()
