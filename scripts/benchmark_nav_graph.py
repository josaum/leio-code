#!/usr/bin/env python3
"""Measure held-out file reachability through LEIO graph navigation."""

from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys
from pathlib import Path
from typing import Any, Mapping


DEFAULT_LIMIT = 5
DEFAULT_TIMEOUT_SECONDS = 120
DEFAULT_SYMBOLS_PER_FILE = 6
DEFAULT_HOP2_SOURCES = 30


def _clip(text: str, limit: int = 1000) -> str:
    rendered = text.strip()
    return rendered if len(rendered) <= limit else rendered[:limit] + "…"


def run_json(
    binary: Path,
    repo: Path,
    *arguments: str,
    timeout: int = DEFAULT_TIMEOUT_SECONDS,
    env: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    """Run one JSON CLI query and reject every malformed response."""
    command = [str(binary), "--json", "--repo", str(repo), *arguments]
    try:
        completed = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=dict(env) if env is not None else None,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"LEIO query timed out after {timeout}s: {' '.join(command)}"
        ) from exc

    stderr = _clip(completed.stderr)
    if completed.returncode != 0:
        detail = f"; stderr={stderr!r}" if stderr else ""
        raise RuntimeError(
            f"LEIO query exited {completed.returncode}: {' '.join(command)}{detail}"
        )

    raw = completed.stdout.strip()
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as exc:
        detail = f"; stderr={stderr!r}" if stderr else ""
        raise RuntimeError(
            f"LEIO query returned invalid JSON: {' '.join(command)}; "
            f"stdout={_clip(raw)!r}{detail}"
        ) from exc
    if not isinstance(payload, dict):
        raise RuntimeError(
            f"LEIO query returned {type(payload).__name__}, expected an object: "
            f"{' '.join(command)}"
        )
    if not isinstance(payload.get("entities"), list):
        raise RuntimeError(
            f"LEIO query returned no entities[] envelope: {' '.join(command)}"
        )
    return payload


def _entities(payload: dict[str, Any], operation: str) -> list[dict[str, Any]]:
    entities = payload.get("entities")
    if not isinstance(entities, list):
        raise RuntimeError(f"{operation} response is missing an entities[] list")
    invalid = [item for item in entities if not isinstance(item, dict)]
    if invalid:
        raise RuntimeError(f"{operation} response contains a non-object entity")
    return entities


def _paths(payload: dict[str, Any], operation: str) -> set[str]:
    found: set[str] = set()

    def walk(value: Any) -> None:
        if isinstance(value, dict):
            path = value.get("path")
            if isinstance(path, str):
                found.add(path)
            for child in value.values():
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)

    _entities(payload, operation)
    walk(payload)
    return found


def _context_files(
    binary: Path,
    repo: Path,
    task: str,
    *,
    limit: int,
    timeout: int,
    env: Mapping[str, str] | None,
) -> list[str]:
    payload = run_json(
        binary,
        repo,
        "context",
        task,
        "--limit",
        str(limit),
        timeout=timeout,
        env=env,
    )
    entities = _entities(payload, "context")
    if not entities:
        raise RuntimeError("context response contains no bundle entity")
    files = entities[0].get("files_to_read")
    if not isinstance(files, list):
        raise RuntimeError("context bundle is missing files_to_read[]")
    paths = [item.get("path") for item in files if isinstance(item, dict)]
    if len(paths) != len(files) or not all(isinstance(path, str) for path in paths):
        raise RuntimeError("context bundle contains a file without a path")
    if not paths:
        raise RuntimeError("context bundle contains no files")
    return paths


def _symbol_urns(
    binary: Path,
    repo: Path,
    file_path: str,
    *,
    symbols_per_file: int,
    timeout: int,
    env: Mapping[str, str] | None,
) -> list[str]:
    payload = run_json(
        binary,
        repo,
        "graph",
        "symbols-in",
        file_path,
        timeout=timeout,
        env=env,
    )
    symbols: list[str] = []
    for entity in _entities(payload, f"symbols-in {file_path}"):
        urn = next(
            (
                entity.get(key)
                for key in ("symbol", "iri", "urn", "id")
                if entity.get(key)
            ),
            None,
        )
        if isinstance(urn, str):
            symbols.append(urn)
        if len(symbols) >= symbols_per_file:
            break
    return symbols


def evaluate(
    binary: Path,
    repo: Path,
    tasks: list[dict[str, Any]],
    *,
    context_limit: int = DEFAULT_LIMIT,
    symbols_per_file: int = DEFAULT_SYMBOLS_PER_FILE,
    hop2_sources: int = DEFAULT_HOP2_SOURCES,
    timeout: int = DEFAULT_TIMEOUT_SECONDS,
    env: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    """Evaluate graph reachability without turning query failures into misses."""
    rows: list[dict[str, Any]] = []
    import_hop_sizes: list[int] = []
    navigated_sizes: list[int] = []
    calls = 0

    for task in tasks:
        relevant = task.get("relevant_paths")
        if not isinstance(relevant, list) or not all(
            isinstance(path, str) for path in relevant
        ):
            raise RuntimeError(f"task has invalid relevant_paths: {task!r}")
        relevant_paths = set(relevant)
        started_calls = calls
        bundle = _context_files(
            binary,
            repo,
            str(task["task"]),
            limit=context_limit,
            timeout=timeout,
            env=env,
        )
        calls += 1
        bundle_set = set(bundle)
        hop1: dict[str, dict[str, Any]] = {}
        for file_path in bundle:
            for query in (
                ("graph", "resolved-imports-in", file_path),
                ("graph", "importers-of", file_path),
            ):
                payload = run_json(
                    binary,
                    repo,
                    *query,
                    timeout=timeout,
                    env=env,
                )
                calls += 1
                for path in _paths(payload, " ".join(query)):
                    if path in bundle_set:
                        continue
                    entry = hop1.setdefault(path, {"sources": set(), "call": False})
                    entry["sources"].add(file_path)
            for urn in _symbol_urns(
                binary,
                repo,
                file_path,
                symbols_per_file=symbols_per_file,
                timeout=timeout,
                env=env,
            ):
                calls += 1
                for direction in ("callers-of", "callees-of"):
                    payload = run_json(
                        binary,
                        repo,
                        "graph",
                        direction,
                        urn,
                        timeout=timeout,
                        env=env,
                    )
                    calls += 1
                    for path in _paths(payload, f"graph {direction} {urn}"):
                        if path in bundle_set:
                            continue
                        entry = hop1.setdefault(
                            path, {"sources": set(), "call": False}
                        )
                        entry["sources"].add(file_path)
                        entry["call"] = True

        hop1_paths = sorted(
            hop1,
            key=lambda path: (
                -len(hop1[path]["sources"]),
                -int(hop1[path]["call"]),
                path,
            ),
        )
        hop2: dict[str, set[str]] = {}
        for file_path in hop1_paths[:hop2_sources]:
            for query in (
                ("graph", "resolved-imports-in", file_path),
                ("graph", "importers-of", file_path),
            ):
                payload = run_json(
                    binary,
                    repo,
                    *query,
                    timeout=timeout,
                    env=env,
                )
                calls += 1
                for path in _paths(payload, " ".join(query)):
                    if path in bundle_set or path in hop1:
                        continue
                    hop2.setdefault(path, set()).add(file_path)

        hop2_paths = sorted(hop2, key=lambda path: (-len(hop2[path]), path))
        navigated = bundle + hop1_paths + hop2_paths
        rank = next(
            (index + 1 for index, path in enumerate(navigated) if path in relevant_paths),
            None,
        )
        rows.append(
            {
                "in_bundle": bool(relevant_paths & bundle_set),
                "hop1": bool(relevant_paths & set(hop1_paths)),
                "hop2": bool(relevant_paths & set(hop2_paths)),
                "nav_rank": rank,
                "nav_size": len(navigated),
                "cli_calls": calls - started_calls,
            }
        )
        import_hop_sizes.append(len(hop1_paths))
        navigated_sizes.append(len(navigated))

    if not rows:
        raise RuntimeError("navigation benchmark has no tasks")

    count = len(rows)

    def fraction(predicate: Any) -> float:
        return round(sum(1 for row in rows if predicate(row)) / count, 4)

    return {
        "tasks": count,
        "protocol": "context(5) -> hop1 imports(both ways)+callers/callees by URN -> hop2 imports",
        "hit_in_bundle_top5": fraction(lambda row: row["in_bundle"]),
        "reachable_by_hop1_only": fraction(
            lambda row: row["hop1"] and not row["in_bundle"]
        ),
        "reachable_by_hop2_only": fraction(
            lambda row: row["hop2"] and not row["hop1"] and not row["in_bundle"]
        ),
        "reachable_bundle_or_hop1": fraction(
            lambda row: row["in_bundle"] or row["hop1"]
        ),
        "reachable_bundle_hop1_or_hop2": fraction(
            lambda row: row["in_bundle"] or row["hop1"] or row["hop2"]
        ),
        "navigated_hit_at_10": fraction(
            lambda row: row["nav_rank"] is not None and row["nav_rank"] <= 10
        ),
        "navigated_hit_at_20": fraction(
            lambda row: row["nav_rank"] is not None and row["nav_rank"] <= 20
        ),
        "navigated_hit_at_40": fraction(
            lambda row: row["nav_rank"] is not None and row["nav_rank"] <= 40
        ),
        "navigated_mrr": round(
            sum(1 / row["nav_rank"] for row in rows if row["nav_rank"] is not None)
            / count,
            4,
        ),
        "median_hop1_neighbors": int(statistics.median(import_hop_sizes)),
        "median_navigated_set": int(statistics.median(navigated_sizes)),
        "mean_cli_calls_per_task": round(
            statistics.mean(row["cli_calls"] for row in rows), 1
        ),
        "rows": rows,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tasks", type=Path)
    parser.add_argument("label")
    parser.add_argument("--binary", type=Path, default=None)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=None)
    parser.add_argument("--context-limit", type=int, default=DEFAULT_LIMIT)
    parser.add_argument("--symbols-per-file", type=int, default=DEFAULT_SYMBOLS_PER_FILE)
    parser.add_argument("--hop2-sources", type=int, default=DEFAULT_HOP2_SOURCES)
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument("--embed-url", default=None)
    parser.add_argument("--embed-model", default=None)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    binary_value = args.binary or os.environ.get("LEIO_CODE_BIN")
    binary = (
        Path(binary_value).expanduser()
        if binary_value
        else Path.home() / ".cargo" / "bin" / "leio-code"
    ).resolve()
    repo = args.repo.expanduser().resolve()
    if not binary.is_file():
        raise SystemExit(f"leio-code binary not found: {binary}")
    if not repo.is_dir():
        raise SystemExit(f"benchmark repository not found: {repo}")

    env = os.environ.copy()
    if args.embed_url is not None:
        env["LEIO_CODE_EMBED_URL"] = args.embed_url
    if args.embed_model is not None:
        env["LEIO_CODE_EMBED_MODEL"] = args.embed_model

    fixture = json.loads(args.tasks.expanduser().read_text(encoding="utf-8"))
    tasks = fixture.get("tasks")
    if not isinstance(tasks, list):
        raise SystemExit("task fixture is missing tasks[]")
    try:
        summary = evaluate(
            binary,
            repo,
            tasks,
            context_limit=args.context_limit,
            symbols_per_file=args.symbols_per_file,
            hop2_sources=args.hop2_sources,
            timeout=args.timeout,
            env=env,
        )
    except RuntimeError as exc:
        print(f"navigation benchmark failed: {exc}", file=sys.stderr)
        return 2

    result = {"label": args.label, **summary}
    rendered = json.dumps(result, indent=2)
    print(rendered)
    if args.output is not None:
        args.output.expanduser().parent.mkdir(parents=True, exist_ok=True)
        args.output.expanduser().write_text(rendered + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
