#!/usr/bin/env python3
"""Validate benchmark receipts, publish the public bundle, and render SVG charts."""

from __future__ import annotations

import argparse
import html
import json
import math
import statistics
from pathlib import Path
from typing import Any

SURFACE = "#07111e"
GRID = "#365161"
TEXT = "#ffffff"
TEXT_SECONDARY = "#c3c2b7"
TEXT_MUTED = "#898781"
EYEBROW = "#59e3c0"
SERIES_LEIO = "#199e70"
SERIES_RG = "#d95926"
NAVIGATION_ORDER = (
    "Trace context ranking",
    "Follow workflow policy dispatch",
    "Investigate MCP binary selection",
)


def load_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path}: expected a JSON object")
    return value


def rounded_bar(x: float, y: float, width: float, height: float, color: str) -> str:
    radius = min(4.0, height / 2, width / 2)
    right = x + width
    bottom = y + height
    path = (
        f"M{x:.1f} {y:.1f} H{right - radius:.1f} "
        f"Q{right:.1f} {y:.1f} {right:.1f} {y + radius:.1f} "
        f"V{bottom - radius:.1f} Q{right:.1f} {bottom:.1f} "
        f"{right - radius:.1f} {bottom:.1f} H{x:.1f} Z"
    )
    return f'<path d="{path}" fill="{color}"/>'


def _require_clean_source(receipt: dict[str, Any], label: str) -> dict[str, Any]:
    source = receipt.get("source") or receipt.get("inspected_source")
    if not isinstance(source, dict) or not source.get("revision"):
        raise ValueError(f"{label}: missing pinned source revision")
    if source.get("clean") is not True:
        raise ValueError(f"{label}: source tree was not clean")
    return source


def validate_latency(receipt: dict[str, Any]) -> None:
    if receipt.get("schema_version") != 2:
        raise ValueError("latency: expected schema_version 2")
    source = _require_clean_source(receipt, "latency")
    binary = receipt.get("binary")
    if not isinstance(binary, dict) or source["revision"][:12] not in str(binary.get("version", "")):
        raise ValueError("latency: binary version does not match source revision")
    rows = receipt.get("rows")
    if not isinstance(rows, list) or not rows:
        raise ValueError("latency: no rows")
    for row in rows:
        runs = row.get("runs")
        if not isinstance(runs, list) or len(runs) != row.get("repeats"):
            raise ValueError(f"latency: incomplete runs for {row.get('name')}")
        if row.get("all_exits_zero") is not True:
            raise ValueError(f"latency: failed repetition for {row.get('name')}")
        if any(run.get("leio_exit") != 0 or run.get("rg_exit") != 0 for run in runs):
            raise ValueError(f"latency: nonzero exit in {row.get('name')}")
        leio_median = round(statistics.median(run["leio_elapsed_ms"] for run in runs), 1)
        rg_median = round(statistics.median(run["rg_elapsed_ms"] for run in runs), 1)
        if leio_median != row.get("leio_median_ms") or rg_median != row.get("rg_median_ms"):
            raise ValueError(f"latency: median mismatch for {row.get('name')}")
        expected_ratio = round(rg_median / leio_median, 2) if leio_median else None
        if row.get("rg_to_leio_latency_ratio") != expected_ratio:
            raise ValueError(f"latency: ratio mismatch for {row.get('name')}")


def validate_navigation(receipt: dict[str, Any]) -> None:
    if receipt.get("schema_version") != 2:
        raise ValueError("navigation: expected schema_version 2")
    source = _require_clean_source(receipt, "navigation")
    harness = receipt.get("harness")
    required_harness_fields = {
        "revision",
        "entrypoint",
        "entrypoint_sha256",
        "mcp_wrapper",
        "mcp_wrapper_sha256",
    }
    if (
        not isinstance(harness, dict)
        or harness.get("clean") is not True
        or not required_harness_fields.issubset(harness)
    ):
        raise ValueError("navigation: missing clean harness revision or source hashes")
    binary = receipt.get("binary")
    if not isinstance(binary, dict) or source["revision"][:12] not in str(binary.get("version", "")):
        raise ValueError("navigation: binary version does not match inspected source")
    rows = receipt.get("rows")
    if not isinstance(rows, list) or not rows:
        raise ValueError("navigation: no rows")
    row_names = [row.get("name") for row in rows]
    if len(row_names) != len(set(row_names)):
        raise ValueError("navigation: duplicate scenario names")
    missing_scenarios = [name for name in NAVIGATION_ORDER if name not in row_names]
    if missing_scenarios:
        raise ValueError(f"navigation: missing scenarios {missing_scenarios}")
    for row in rows:
        runs = row.get("runs")
        if not isinstance(runs, list) or not runs:
            raise ValueError(f"navigation: no runs for {row.get('name')}")
        median = round(statistics.median(run["elapsed_ms"] for run in runs), 1)
        if median != row.get("median_ms"):
            raise ValueError(f"navigation: median mismatch for {row.get('name')}")
        if any(
            run.get("tool_calls") != 11
            or run.get("cursor_restored") is not True
            or run.get("cursor_survived_provider_restart") is not True
            for run in runs
        ):
            raise ValueError(f"navigation: incomplete assertions for {row.get('name')}")


def validate_retrieval(receipt: dict[str, Any], source_revision: str, binary_version: str) -> None:
    if receipt.get("binary_version") != binary_version:
        raise ValueError("retrieval: binary version does not match latency receipt")
    if source_revision[:12] not in binary_version:
        raise ValueError("retrieval: binary version does not identify source revision")
    results = receipt.get("results")
    if not isinstance(results, list) or len(results) != receipt.get("task_count"):
        raise ValueError("retrieval: task count does not match result rows")
    if not results:
        raise ValueError("retrieval: no task results")
    mean_rr = sum(float(row["reciprocal_rank"]) for row in results) / len(results)
    hit_at_1 = sum(bool(row["hit_at_1"]) for row in results) / len(results)
    hit_at_3 = sum(bool(row["hit_at_3"]) for row in results) / len(results)
    if not math.isclose(mean_rr, float(receipt["mean_reciprocal_rank"])):
        raise ValueError("retrieval: mean reciprocal rank mismatch")
    if not math.isclose(hit_at_1, float(receipt["hit_at_1"])):
        raise ValueError("retrieval: hit_at_1 mismatch")
    if not math.isclose(hit_at_3, float(receipt["hit_at_3"])):
        raise ValueError("retrieval: hit_at_3 mismatch")


def public_results(latency: dict[str, Any], retrieval: dict[str, Any]) -> dict[str, Any]:
    validate_latency(latency)
    source = latency["source"]
    binary = latency["binary"]
    validate_retrieval(retrieval, source["revision"], binary["version"])
    return {
        "schema_version": 2,
        "measured_at": latency["measured_at"],
        "repository": latency["repository"],
        "source": source,
        "binary": {
            "version": binary["version"],
            "sha256": binary["sha256"],
        },
        "environment": latency["environment"],
        "methodology": latency["methodology"],
        "latency": {
            "rows": latency["rows"],
            "limitations": "The arms return different output types. This is subprocess latency, not an agent speedup or equal-workload comparison.",
        },
        "retrieval": {
            key: value
            for key, value in retrieval.items()
            if key not in {"repository", "binary"}
        },
    }


def _nice_axis_max(maximum: float, step: int) -> int:
    return max(step, int(math.ceil(maximum / step) * step))


def render_latency_svg(public: dict[str, Any]) -> str:
    rows = public["latency"]["rows"]
    axis_max = _nice_axis_max(
        max(max(row["leio_median_ms"], row["rg_median_ms"]) for row in rows),
        25,
    )
    plot_x = 355
    plot_width = 720
    chart_top = 170
    group_gap = 100
    lines = [
        '<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="560" viewBox="0 0 1200 560" role="img" aria-labelledby="title desc">',
        '<title id="title">Local CLI query latency</title>',
        '<desc id="desc">Median wall-clock latency for LEIO and ripgrep across three local lookup tasks. Every value also appears in the adjacent benchmark table. Lower is faster; the tools return different outputs.</desc>',
        f'<rect width="1200" height="560" rx="22" fill="{SURFACE}"/>',
        f'<text x="45" y="48" font-family="Arial,sans-serif" font-size="17" font-weight="700" fill="{EYEBROW}">LOCAL CLI LATENCY</text>',
        f'<text x="45" y="100" font-family="Arial,sans-serif" font-size="34" font-weight="700" fill="{TEXT}">Measured, not promised.</text>',
        f'<text x="45" y="132" font-family="Arial,sans-serif" font-size="16" fill="{TEXT_SECONDARY}">Median wall-clock milliseconds · lower is faster · different output types</text>',
        rounded_bar(45, 148, 16, 16, SERIES_LEIO),
        f'<text x="70" y="162" font-family="Arial,sans-serif" font-size="15" fill="{TEXT_SECONDARY}">LEIO</text>',
        rounded_bar(145, 148, 16, 16, SERIES_RG),
        f'<text x="170" y="162" font-family="Arial,sans-serif" font-size="15" fill="{TEXT_SECONDARY}">ripgrep</text>',
    ]
    tick_step = 25
    for tick in range(0, axis_max + 1, tick_step):
        x = plot_x + plot_width * tick / axis_max
        lines.extend(
            [
                f'<line x1="{x:.1f}" y1="{chart_top}" x2="{x:.1f}" y2="470" stroke="{GRID}" stroke-width="1"/>',
                f'<text x="{x:.1f}" y="493" text-anchor="middle" font-family="Arial,sans-serif" font-size="13" fill="{TEXT_MUTED}">{tick}</text>',
            ]
        )
    labels = {
        "symbol-query_dead_code": "Find query_dead_code",
        "symbol-export_code_graph": "Find export_code_graph",
        "context-rdf-namespace": "Context: RDF namespace",
    }
    for index, row in enumerate(rows):
        y = 202 + index * group_gap
        label = html.escape(labels.get(row["name"], row["name"]))
        lines.append(
            f'<text x="45" y="{y + 5}" font-family="Arial,sans-serif" font-size="17" fill="{TEXT_SECONDARY}">{label}</text>'
        )
        for offset, key, color in (
            (18, "leio_median_ms", SERIES_LEIO),
            (50, "rg_median_ms", SERIES_RG),
        ):
            value = float(row[key])
            width = max(2.0, plot_width * value / axis_max)
            bar_y = y + offset
            lines.append(rounded_bar(plot_x, bar_y, width, 20, color))
            lines.append(
                f'<text x="{min(plot_x + width + 10, 1148):.1f}" y="{bar_y + 16:.1f}" font-family="Arial,sans-serif" font-size="15" font-weight="700" fill="{TEXT}">{value:.1f} ms</text>'
            )
    lines.extend(
        [
            f'<line x1="{plot_x}" y1="470" x2="{plot_x + plot_width}" y2="470" stroke="{TEXT_MUTED}" stroke-width="1"/>',
            f'<text x="45" y="535" font-family="Arial,sans-serif" font-size="14" fill="{TEXT_MUTED}">Ten repetitions per task · existing index · complete samples and exit codes in the benchmark receipt</text>',
            '</svg>\n',
        ]
    )
    return "\n".join(lines)


def render_navigation_svg(receipt: dict[str, Any]) -> str:
    validate_navigation(receipt)
    by_name = {row["name"]: row for row in receipt["rows"]}
    rows = [by_name[name] for name in NAVIGATION_ORDER]
    run_count = sum(len(row["runs"]) for row in rows)
    axis_max = _nice_axis_max(max(row["median_ms"] for row in rows), 500)
    plot_x = 355
    plot_width = 720
    chart_top = 165
    lines = [
        '<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="500" viewBox="0 0 1200 500" role="img" aria-labelledby="title desc">',
        '<title id="title">Guided multi-step navigation benchmarks</title>',
        f'<desc id="desc">Median duration for three guided investigations, each using eleven MCP calls and one provider reconnect. All {run_count} scripted runs restored the navigation cursor. Every value also appears in the adjacent benchmark table.</desc>',
        f'<rect width="1200" height="500" rx="22" fill="{SURFACE}"/>',
        f'<text x="45" y="48" font-family="Arial,sans-serif" font-size="17" font-weight="700" fill="{EYEBROW}">FOLLOW THE CODE. KEEP YOUR PLACE.</text>',
        f'<text x="45" y="100" font-family="Arial,sans-serif" font-size="34" font-weight="700" fill="{TEXT}">11 tool calls. One continuous investigation.</text>',
        f'<text x="45" y="132" font-family="Arial,sans-serif" font-size="16" fill="{TEXT_SECONDARY}">Median full-sequence duration · setup excluded · lower is faster</text>',
    ]
    for tick in range(0, axis_max + 1, 500):
        x = plot_x + plot_width * tick / axis_max
        lines.extend(
            [
                f'<line x1="{x:.1f}" y1="{chart_top}" x2="{x:.1f}" y2="405" stroke="{GRID}" stroke-width="1"/>',
                f'<text x="{x:.1f}" y="428" text-anchor="middle" font-family="Arial,sans-serif" font-size="13" fill="{TEXT_MUTED}">{tick / 1000:g} s</text>',
            ]
        )
    for index, row in enumerate(rows):
        y = 195 + index * 72
        label = {
            "Trace context ranking": "Context ranking",
            "Follow workflow policy dispatch": "Workflow policy",
            "Investigate MCP binary selection": "MCP binary selection",
        }[row["name"]]
        value = float(row["median_ms"])
        width = plot_width * value / axis_max
        lines.append(
            f'<text x="45" y="{y + 17}" font-family="Arial,sans-serif" font-size="17" fill="{TEXT_SECONDARY}">{html.escape(label)}</text>'
        )
        lines.append(rounded_bar(plot_x, y, width, 22, SERIES_LEIO))
        lines.append(
            f'<text x="{min(plot_x + width + 10, 1148):.1f}" y="{y + 17}" font-family="Arial,sans-serif" font-size="16" font-weight="700" fill="{TEXT}">{value / 1000:.2f} s</text>'
        )
    lines.extend(
        [
            f'<line x1="{plot_x}" y1="405" x2="{plot_x + plot_width}" y2="405" stroke="{TEXT_MUTED}" stroke-width="1"/>',
            f'<text x="45" y="462" font-family="Arial,sans-serif" font-size="16" fill="{TEXT}">{run_count} / {run_count} scripted runs passed, including cursor restoration after provider restart.</text>',
            f'<text x="45" y="487" font-family="Arial,sans-serif" font-size="13" fill="{TEXT_MUTED}">Guided navigation, not autonomous bug fixing · warm index and graph · complete samples in the benchmark receipt</text>',
            '</svg>\n',
        ]
    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    root = Path(__file__).resolve().parents[1]
    parser.add_argument("--latency", type=Path, required=True)
    parser.add_argument("--retrieval", type=Path, required=True)
    parser.add_argument("--navigation", type=Path, required=True)
    parser.add_argument("--public-output", type=Path, default=root / "benchmarks" / "public-results.json")
    parser.add_argument("--asset-dir", type=Path, default=root / "assets")
    args = parser.parse_args()

    latency = load_json(args.latency)
    retrieval = load_json(args.retrieval)
    navigation = load_json(args.navigation)
    public = public_results(latency, retrieval)
    validate_navigation(navigation)

    args.public_output.parent.mkdir(parents=True, exist_ok=True)
    args.asset_dir.mkdir(parents=True, exist_ok=True)
    args.public_output.write_text(json.dumps(public, indent=2) + "\n", encoding="utf-8")
    (args.asset_dir / "benchmark-latency.svg").write_text(
        render_latency_svg(public), encoding="utf-8"
    )
    (args.asset_dir / "benchmark-navigation.svg").write_text(
        render_navigation_svg(navigation), encoding="utf-8"
    )
    print(
        json.dumps(
            {
                "status": "ok",
                "public_results": str(args.public_output),
                "assets": [
                    str(args.asset_dir / "benchmark-latency.svg"),
                    str(args.asset_dir / "benchmark-navigation.svg"),
                ],
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
