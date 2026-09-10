import importlib.util
import stat
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "benchmark_nav_graph.py"
SPEC = importlib.util.spec_from_file_location("benchmark_nav_graph", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
benchmark_nav_graph = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark_nav_graph)


class RunJsonTests(unittest.TestCase):
    def fake_binary(self, body: str, *, exit_code: int = 0, stderr: str = "") -> Path:
        directory = Path(self.tmp.name)
        binary = directory / "fake-leio-code"
        binary.write_text(
            "#!/usr/bin/env python3\n"
            "import sys\n"
            f"sys.stderr.write({stderr!r})\n"
            f"print({body!r})\n"
            f"raise SystemExit({exit_code})\n"
        )
        binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
        return binary

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name) / "repo"
        self.repo.mkdir()

    def test_rejects_invalid_json_instead_of_returning_empty_object(self) -> None:
        binary = self.fake_binary("progress text from a broken JSON response")
        with self.assertRaisesRegex(RuntimeError, "invalid JSON"):
            benchmark_nav_graph.run_json(binary, self.repo, "graph", "symbols-in", "src/lib.rs")

    def test_reports_nonzero_exit_and_stderr(self) -> None:
        binary = self.fake_binary("{}", exit_code=7, stderr="backend exploded\n")
        with self.assertRaisesRegex(RuntimeError, "exited 7.*backend exploded"):
            benchmark_nav_graph.run_json(binary, self.repo, "context", "broken query")

    def test_rejects_a_json_object_without_query_entities(self) -> None:
        binary = self.fake_binary('{"summary": "not a query envelope"}')
        with self.assertRaisesRegex(RuntimeError, r"entities\[\]"):
            benchmark_nav_graph.run_json(binary, self.repo, "context", "empty query")

    def test_returns_a_json_object_on_success(self) -> None:
        binary = self.fake_binary('{"entities": []}')
        self.assertEqual(
            benchmark_nav_graph.run_json(binary, self.repo, "context", "healthy query"),
            {"entities": []},
        )


if __name__ == "__main__":
    unittest.main()
