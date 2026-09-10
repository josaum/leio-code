import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "publish_benchmarks.py"
SPEC = importlib.util.spec_from_file_location("publish_benchmarks", SCRIPT)
assert SPEC and SPEC.loader
publish_benchmarks = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publish_benchmarks)


class PublishBenchmarksTests(unittest.TestCase):
    @staticmethod
    def latency() -> dict:
        return {
            "schema_version": 2,
            "measured_at": "2026-09-09T00:00:00Z",
            "repository": "https://github.com/josaum/leio-code",
            "source": {"revision": "a" * 40, "clean": True, "dirty_path_count": 0},
            "binary": {"version": "leio-code 2.6.2 (aaaaaaaaaaaa, clean)", "sha256": "b" * 64},
            "environment": {"platform": "darwin"},
            "methodology": "test",
            "rows": [
                {
                    "name": "task",
                    "leio_median_ms": 110.0,
                    "rg_median_ms": 11.0,
                    "leio_exit_codes": [0, 0],
                    "rg_exit_codes": [0, 0],
                    "all_exits_zero": True,
                    "repeats": 2,
                    "rg_to_leio_latency_ratio": 0.1,
                    "runs": [
                        {"leio_elapsed_ms": 100.0, "rg_elapsed_ms": 10.0, "leio_exit": 0, "rg_exit": 0},
                        {"leio_elapsed_ms": 120.0, "rg_elapsed_ms": 12.0, "leio_exit": 0, "rg_exit": 0},
                    ],
                }
            ],
        }

    @staticmethod
    def navigation() -> dict:
        run = {
            "elapsed_ms": 1000.0,
            "tool_calls": 11,
            "cursor_restored": True,
            "cursor_survived_provider_restart": True,
        }
        return {
            "schema_version": 2,
            "inspected_source": {"revision": "a" * 40, "clean": True},
            "harness": {
                "revision": "c" * 40,
                "clean": True,
                "entrypoint": "scripts/benchmark_navigation.mjs",
                "entrypoint_sha256": "d" * 64,
                "mcp_wrapper": "mcp/index.js",
                "mcp_wrapper_sha256": "e" * 64,
            },
            "binary": {"version": "leio-code 2.6.2 (aaaaaaaaaaaa, clean)"},
            "rows": [
                {"name": name, "median_ms": 1000.0, "runs": [dict(run)]}
                for name in publish_benchmarks.NAVIGATION_ORDER
            ],
        }

    def test_validate_latency_rejects_hidden_nonzero_exit(self) -> None:
        receipt = self.latency()
        receipt["rows"][0]["runs"][0]["leio_exit"] = 1
        with self.assertRaisesRegex(ValueError, "nonzero exit"):
            publish_benchmarks.validate_latency(receipt)

    def test_validate_navigation_rejects_unpinned_harness(self) -> None:
        receipt = self.navigation()
        del receipt["harness"]["revision"]
        with self.assertRaisesRegex(ValueError, "harness revision"):
            publish_benchmarks.validate_navigation(receipt)

    def test_validate_navigation_rejects_missing_or_duplicate_scenarios(self) -> None:
        missing = self.navigation()
        missing["rows"].pop()
        with self.assertRaisesRegex(ValueError, "missing scenarios"):
            publish_benchmarks.validate_navigation(missing)

        duplicate = self.navigation()
        duplicate["rows"][1]["name"] = duplicate["rows"][0]["name"]
        with self.assertRaisesRegex(ValueError, "duplicate scenario names"):
            publish_benchmarks.validate_navigation(duplicate)

    def test_latency_svg_has_accessible_text_and_both_series(self) -> None:
        public = {
            "latency": {"rows": self.latency()["rows"]},
        }
        svg = publish_benchmarks.render_latency_svg(public)
        self.assertIn('aria-labelledby="title desc"', svg)
        self.assertIn("<title", svg)
        self.assertIn("Every value also appears in the adjacent benchmark table", svg)
        self.assertIn(publish_benchmarks.SERIES_LEIO, svg)
        self.assertIn(publish_benchmarks.SERIES_RG, svg)

    def test_navigation_svg_uses_published_medians(self) -> None:
        svg = publish_benchmarks.render_navigation_svg(self.navigation())
        self.assertIn("1.00 s", svg)
        self.assertIn("3 / 3 scripted runs passed", svg)


if __name__ == "__main__":
    unittest.main()
