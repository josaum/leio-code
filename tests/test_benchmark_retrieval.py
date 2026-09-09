import importlib.util
import statistics
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "benchmark_retrieval.py"
SPEC = importlib.util.spec_from_file_location("benchmark_retrieval", SCRIPT)
assert SPEC and SPEC.loader
benchmark_retrieval = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark_retrieval)


class BenchmarkRetrievalTests(unittest.TestCase):
    def test_summarize_runs_keeps_every_exit_and_honest_ratio_name(self) -> None:
        leio_runs = [
            {"exit": 0, "elapsed_ms": 100.0},
            {"exit": 0, "elapsed_ms": 120.0},
        ]
        rg_runs = [
            {"exit": 0, "elapsed_ms": 10.0},
            {"exit": 0, "elapsed_ms": 12.0},
        ]

        row = benchmark_retrieval.summarize_runs("task", leio_runs, rg_runs)

        self.assertEqual(row["leio_exit_codes"], [0, 0])
        self.assertEqual(row["rg_exit_codes"], [0, 0])
        self.assertTrue(row["all_exits_zero"])
        self.assertEqual(row["rg_to_leio_latency_ratio"], 0.1)
        self.assertNotIn("speedup_vs_rg", row)

    def test_published_median_is_reproducible_from_published_runs(self) -> None:
        # Regression: the median was taken over full-precision timings while the
        # receipt published each repetition rounded to 0.1 ms, so
        # publish_benchmarks -- which recomputes the median from the published
        # runs and demands an exact match -- rejected honest receipts.
        leio_runs = [{"exit": 0, "elapsed_ms": value} for value in (78.64, 78.14)]
        rg_runs = [{"exit": 0, "elapsed_ms": value} for value in (9.64, 9.14)]

        row = benchmark_retrieval.summarize_runs("task", leio_runs, rg_runs)

        for arm in ("leio", "rg"):
            published = [run[f"{arm}_elapsed_ms"] for run in row["runs"]]
            self.assertEqual(
                round(statistics.median(published), 1),
                row[f"{arm}_median_ms"],
                f"{arm} median must follow from the published repetitions",
            )
        self.assertEqual(
            row["rg_to_leio_latency_ratio"],
            round(row["rg_median_ms"] / row["leio_median_ms"], 2),
        )

    def test_summarize_runs_rejects_empty_or_mismatched_repetitions(self) -> None:
        with self.assertRaisesRegex(ValueError, "same nonzero repetition count"):
            benchmark_retrieval.summarize_runs("task", [], [])
        with self.assertRaisesRegex(ValueError, "same nonzero repetition count"):
            benchmark_retrieval.summarize_runs(
                "task",
                [{"exit": 0, "elapsed_ms": 100.0}],
                [],
            )

    def test_summarize_runs_rejects_any_failed_repetition(self) -> None:
        leio_runs = [
            {"exit": 7, "elapsed_ms": 100.0, "cmd": ["leio-code"]},
            {"exit": 0, "elapsed_ms": 110.0, "cmd": ["leio-code"]},
        ]
        rg_runs = [
            {"exit": 0, "elapsed_ms": 10.0, "cmd": ["rg"]},
            {"exit": 0, "elapsed_ms": 11.0, "cmd": ["rg"]},
        ]

        with self.assertRaisesRegex(RuntimeError, "task.*LEIO exit codes.*7, 0"):
            benchmark_retrieval.summarize_runs("task", leio_runs, rg_runs)

    def test_main_returns_nonzero_when_any_timed_command_fails(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            binary = root / "leio-code"
            binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            binary.chmod(0o755)
            argv = [
                "benchmark_retrieval.py",
                "--plugin-root",
                str(root),
                "--binary",
                str(binary),
                "--repeats",
                "2",
            ]
            runs = iter(
                [
                    {"exit": 1, "elapsed_ms": 100.0, "cmd": ["leio-code"]},
                    {"exit": 0, "elapsed_ms": 10.0, "cmd": ["rg"]},
                    {"exit": 0, "elapsed_ms": 101.0, "cmd": ["leio-code"]},
                    {"exit": 0, "elapsed_ms": 11.0, "cmd": ["rg"]},
                ]
            )
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(benchmark_retrieval.shutil, "which", return_value="/usr/bin/rg"),
                mock.patch.object(
                    benchmark_retrieval,
                    "source_identity",
                    return_value={"revision": "a" * 40, "clean": True, "dirty_path_count": 0},
                ),
                mock.patch.object(
                    benchmark_retrieval.subprocess,
                    "check_output",
                    side_effect=["leio-code 2.6.2 (aaaaaaaaaaaa, clean)\n", "ripgrep 14.0.0\n"],
                ),
                mock.patch.object(benchmark_retrieval, "file_sha256", return_value="b" * 64),
                mock.patch.object(benchmark_retrieval, "DEFAULT_TASKS", [{"name": "task", "leio": ["find"], "rg": ["rg"]}]),
                mock.patch.object(benchmark_retrieval, "run_timed", side_effect=lambda *_: next(runs)),
            ):
                with self.assertRaisesRegex(RuntimeError, "LEIO exit codes.*1, 0"):
                    benchmark_retrieval.main()


if __name__ == "__main__":
    unittest.main()
