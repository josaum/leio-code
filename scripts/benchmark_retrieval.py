#!/usr/bin/env python3
"""Micro-benchmark: LEIO find/context vs ripgrep on golden tasks.

Writes a JSON report to stdout. Markdown summary is assembled by
`docs/BENCHMARKS.md` (checked-in last run) or `--write-md`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import shutil
import statistics
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


DEFAULT_TASKS = [
    {
        "name": "symbol-query_dead_code",
        "leio": ["find", "symbol", "query_dead_code"],
        "rg": ["rg", "-n", "--type", "rust", "fn query_dead_code"],
        "repo": "leio-code",
    },
    {
        "name": "symbol-export_code_graph",
        "leio": ["find", "symbol", "export_code_graph"],
        "rg": ["rg", "-n", "--type", "rust", "fn export_code_graph"],
        "repo": "leio-code",
    },
    {
        "name": "context-rdf-namespace",
        "leio": ["context", "configurable RDF namespace LEIO_CODE_RDF_NAMESPACE"],
        "rg": ["rg", "-n", "LEIO_CODE_RDF_NAMESPACE"],
        "repo": "leio-code",
    },
]


def git(repo: Path, *args: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(repo), *args], text=True
    ).strip()


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def source_identity(repo: Path) -> dict[str, Any]:
    revision = git(repo, "rev-parse", "HEAD")
    dirty_paths = git(repo, "status", "--porcelain=v1", "--untracked-files=all").splitlines()
    return {
        "revision": revision,
        "clean": not dirty_paths,
        "dirty_path_count": len(dirty_paths),
    }


def parse_args() -> argparse.Namespace:
    plugin_root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plugin-root", type=Path, default=plugin_root)
    parser.add_argument("--binary", type=Path, default=Path.home() / ".cargo" / "bin" / "leio-code")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--write-md", type=Path, default=None)
    parser.add_argument(
        "--allow-dirty",
        action="store_true",
        help="Allow measurement against a dirty source tree (reported in the receipt)",
    )
    return parser.parse_args()


def run_timed(cmd: list[str], cwd: Path) -> dict[str, Any]:
    started = time.perf_counter()
    completed = subprocess.run(
        cmd,
        cwd=cwd,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    return {
        "cmd": cmd,
        "exit": completed.returncode,
        "elapsed_ms": elapsed_ms,
        "stdout_bytes": len(completed.stdout.encode()),
        "stderr_bytes": len(completed.stderr.encode()),
    }


def median(values: list[float]) -> float:
    if not values:
        return 0.0
    return float(statistics.median(values))


def summarize_runs(
    name: str,
    leio_runs: list[dict[str, Any]],
    rg_runs: list[dict[str, Any]],
) -> dict[str, Any]:
    """Summarize successful repetitions without hiding an earlier failure."""
    if not leio_runs or len(leio_runs) != len(rg_runs):
        raise ValueError("LEIO and ripgrep require the same nonzero repetition count")
    leio_exit_codes = [int(run["exit"]) for run in leio_runs]
    rg_exit_codes = [int(run["exit"]) for run in rg_runs]
    if any(leio_exit_codes) or any(rg_exit_codes):
        raise RuntimeError(
            f"{name}: LEIO exit codes {', '.join(map(str, leio_exit_codes))}; "
            f"ripgrep exit codes {', '.join(map(str, rg_exit_codes))}"
        )

    leio_median_ms = round(median([run["elapsed_ms"] for run in leio_runs]), 1)
    rg_median_ms = round(median([run["elapsed_ms"] for run in rg_runs]), 1)
    return {
        "name": name,
        "leio_median_ms": leio_median_ms,
        "rg_median_ms": rg_median_ms,
        "leio_exit_codes": leio_exit_codes,
        "rg_exit_codes": rg_exit_codes,
        "all_exits_zero": True,
        "repeats": len(leio_runs),
        "rg_to_leio_latency_ratio": (
            round(rg_median_ms / leio_median_ms, 2) if leio_median_ms else None
        ),
        "runs": [
            {
                "leio_elapsed_ms": round(float(leio_run["elapsed_ms"]), 1),
                "rg_elapsed_ms": round(float(rg_run["elapsed_ms"]), 1),
                "leio_exit": int(leio_run["exit"]),
                "rg_exit": int(rg_run["exit"]),
            }
            for leio_run, rg_run in zip(leio_runs, rg_runs, strict=True)
        ],
    }


def main() -> int:
    args = parse_args()
    repo = args.plugin_root.expanduser().resolve()
    binary_path = args.binary.expanduser().resolve()
    binary = str(binary_path)
    if not binary_path.is_file():
        raise SystemExit(f"leio-code binary not found: {binary}")
    rg = shutil.which("rg")
    if rg is None:
        raise SystemExit("ripgrep (`rg`) is required for the comparison arm")
    if args.repeats < 1 or args.repeats > 100:
        raise SystemExit("--repeats must be between 1 and 100")

    identity = source_identity(repo)
    if not identity["clean"] and not args.allow_dirty:
        raise SystemExit(
            f"source tree is dirty ({identity['dirty_path_count']} paths); commit the benchmark implementation or pass --allow-dirty"
        )
    binary_version = subprocess.check_output([binary, "--version"], text=True).strip()
    if identity["revision"][:12] not in binary_version:
        raise SystemExit(
            f"binary/source revision mismatch: source {identity['revision'][:12]}, binary {binary_version}"
        )

    rows: list[dict[str, Any]] = []
    for task in DEFAULT_TASKS:
        leio_runs = []
        rg_runs = []
        for _ in range(args.repeats):
            leio_runs.append(
                run_timed([binary, "--repo", str(repo), *task["leio"]], repo)
            )
            rg_runs.append(run_timed([*task["rg"], str(repo)], repo))
        rows.append(summarize_runs(task["name"], leio_runs, rg_runs))

    final_identity = source_identity(repo)
    if final_identity != identity:
        raise RuntimeError("source tree changed during the latency benchmark")

    report = {
        "schema_version": 2,
        "measured_at": datetime.now(UTC).isoformat(),
        "repository": "https://github.com/josaum/leio-code",
        "source": identity,
        "binary": {
            "path": str(binary_path),
            "version": binary_version,
            "sha256": file_sha256(binary_path),
        },
        "environment": {
            "platform": sys.platform,
            "arch": platform.machine(),
            "cpu": platform.processor(),
            "python": platform.python_version(),
            "ripgrep": subprocess.check_output([rg, "--version"], text=True)
            .splitlines()[0]
            .strip(),
        },
        "methodology": "Existing index; sequential CLI subprocess wall time. Each repetition records both exit codes and elapsed values; any nonzero exit aborts the benchmark.",
        "rows": rows,
    }
    print(json.dumps(report, indent=2))

    if args.write_md:
        lines = [
            "# LEIO retrieval micro-benchmark",
            "",
            "Generated by `scripts/benchmark_retrieval.py`.",
            "Times are median wall-clock milliseconds on this machine.",
            "This is not a token-to-answer study and does not claim Serena parity.",
            "",
            f"Binary: `{binary}`",
            f"Repo: `{repo}`",
            "",
            "| Task | LEIO median ms | rg median ms | rg / LEIO latency ratio |",
            "| --- | ---: | ---: | ---: |",
        ]
        for row in rows:
            lines.append(
                f"| {row['name']} | {row['leio_median_ms']} | {row['rg_median_ms']} | {row['rg_to_leio_latency_ratio']} |"
            )
        lines.append("")
        args.write_md.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
