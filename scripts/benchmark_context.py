#!/usr/bin/env python3
"""Run golden LEIO context-bundle benchmark tasks."""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path
from typing import Any


def parse_args() -> argparse.Namespace:
    plugin_root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description="Run LEIO context golden benchmark tasks")
    parser.add_argument(
        "--tasks",
        type=Path,
        default=plugin_root / "benchmarks" / "context-golden-tasks.json",
        help="Context benchmark task fixture",
    )
    parser.add_argument(
        "--workspace-root",
        type=Path,
        default=plugin_root.parent,
        help="Workspace root used by tasks whose repo is `workspace`",
    )
    parser.add_argument(
        "--plugin-root",
        type=Path,
        default=plugin_root,
        help="LEIO Code repo root used by tasks whose repo is `leio-code`",
    )
    return parser.parse_args()


def extract_json_object(text: str) -> dict[str, Any]:
    decoder = json.JSONDecoder()
    start = text.find("{")
    while start != -1:
        try:
            parsed, _ = decoder.raw_decode(text[start:])
        except json.JSONDecodeError:
            start = text.find("{", start + 1)
            continue
        if isinstance(parsed, dict):
            return parsed
        start = text.find("{", start + 1)
    raise RuntimeError("no JSON object found in context output")


def repo_for_task(task: dict[str, Any], workspace_root: Path, plugin_root: Path) -> Path:
    repo = task.get("repo", "workspace")
    if repo == "workspace":
        return workspace_root
    if repo == "leio-code":
        return plugin_root
    return Path(str(repo)).expanduser().resolve()


def run_context(task: dict[str, Any], workspace_root: Path, plugin_root: Path) -> dict[str, Any]:
    repo = repo_for_task(task, workspace_root, plugin_root)
    completed = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "--",
            "--json",
            "--repo",
            str(repo),
            "context",
            str(task["task"]),
            "--limit",
            str(task.get("limit", 8)),
        ],
        cwd=plugin_root,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return extract_json_object(completed.stdout)


def context_bundle(envelope: dict[str, Any]) -> dict[str, Any]:
    entities = envelope.get("entities") or []
    if not entities or not isinstance(entities[0], dict):
        raise RuntimeError("context envelope did not include a bundle entity")
    return entities[0]


def command_matches(commands: list[dict[str, Any]], needles: list[str]) -> bool:
    rendered = [str(item.get("command", "")) for item in commands]
    return any(any(needle in command for command in rendered) for needle in needles)


def values_match(items: list[dict[str, Any]], field: str, needles: list[str]) -> bool:
    rendered = [str(item.get(field, "")) for item in items]
    return any(value in needles for value in rendered)


def validate_task(task: dict[str, Any], envelope: dict[str, Any]) -> dict[str, Any]:
    bundle = context_bundle(envelope)
    files = bundle.get("files_to_read") or []
    doctors = bundle.get("doctor_suggestions") or []
    tests = bundle.get("tests_to_run") or []
    zones = bundle.get("context_zones") or []
    instructions = bundle.get("instruction_sources") or bundle.get("agent_instructions") or []
    memory_sources = bundle.get("memory_sources") or bundle.get("memory_banks") or []
    anchors = bundle.get("verification_anchors") or []

    paths = [str(item.get("path", "")) for item in files]
    doctor_kinds = [str(item.get("kind", "")) for item in doctors]
    test_commands = [str(item.get("command", "")) for item in tests]
    zone_names = [str(item.get("name", "")) for item in zones]
    instruction_paths = [str(item.get("path", "")) for item in instructions]
    memory_paths = [str(item.get("path", "")) for item in memory_sources]
    anchor_ids = [str(item.get("anchor", "")) for item in anchors]

    expected_paths = task.get("expected_paths_any") or []
    expected_doctors = task.get("expected_doctor_kinds_any") or []
    expected_tests = task.get("expected_tests_any") or []
    expected_zones = task.get("expected_context_zone_names") or []
    expected_zone_order = task.get("expected_context_zone_order") or []
    expected_instruction_paths = task.get("expected_instruction_paths_any") or []
    expected_instruction_kinds = task.get("expected_instruction_kinds_any") or []
    expected_memory_paths = task.get("expected_memory_paths_any") or []
    expected_memory_kinds = task.get("expected_memory_kinds_any") or []
    expected_anchor_ids = task.get("expected_anchor_ids_any") or []
    expected_anchor_paths = task.get("expected_anchor_paths_any") or []
    failures = []

    if expected_paths and not any(path in paths for path in expected_paths):
        failures.append(f"missing expected path; wanted any of {expected_paths}, got {paths}")
    if expected_doctors and not any(kind in doctor_kinds for kind in expected_doctors):
        failures.append(
            f"missing expected doctor; wanted any of {expected_doctors}, got {doctor_kinds}"
        )
    if expected_tests and not command_matches(tests, expected_tests):
        failures.append(
            f"missing expected test command; wanted any of {expected_tests}, got {test_commands}"
        )
    if expected_zones:
        missing_zones = [zone for zone in expected_zones if zone not in zone_names]
        if missing_zones:
            failures.append(f"missing expected context zones {missing_zones}; got {zone_names}")
    if expected_zone_order and zone_names[: len(expected_zone_order)] != expected_zone_order:
        failures.append(
            f"context zone order changed; wanted prefix {expected_zone_order}, got {zone_names}"
        )
    if expected_instruction_paths and not any(
        path in instruction_paths for path in expected_instruction_paths
    ):
        failures.append(
            f"missing expected instruction source; wanted any of {expected_instruction_paths}, got {instruction_paths}"
        )
    if expected_instruction_kinds and not values_match(
        instructions, "kind", expected_instruction_kinds
    ):
        failures.append(
            f"missing expected instruction kind; wanted any of {expected_instruction_kinds}"
        )
    if expected_memory_paths and not any(path in memory_paths for path in expected_memory_paths):
        failures.append(
            f"missing expected memory source; wanted any of {expected_memory_paths}, got {memory_paths}"
        )
    if expected_memory_kinds and not values_match(memory_sources, "kind", expected_memory_kinds):
        failures.append(f"missing expected memory kind; wanted any of {expected_memory_kinds}")
    if expected_anchor_ids and not any(anchor in anchor_ids for anchor in expected_anchor_ids):
        failures.append(f"missing expected anchor; wanted any of {expected_anchor_ids}, got {anchor_ids}")
    if expected_anchor_paths and not values_match(anchors, "path", expected_anchor_paths):
        failures.append(
            f"missing expected anchor path; wanted any of {expected_anchor_paths}"
        )

    min_files = int(task.get("min_files", 1))
    if len(paths) < min_files:
        failures.append(f"expected at least {min_files} files, got {len(paths)}")

    min_zones = int(task.get("min_context_zones", 0))
    if len(zone_names) < min_zones:
        failures.append(f"expected at least {min_zones} context zones, got {len(zone_names)}")

    min_confidence = float(task.get("min_confidence", 0.5))
    if float(envelope.get("confidence", 0.0)) < min_confidence:
        failures.append(
            f"expected confidence >= {min_confidence}, got {envelope.get('confidence')}"
        )

    if failures:
        raise RuntimeError(f"context benchmark task `{task['name']}` failed: {'; '.join(failures)}")

    return {
        "name": task["name"],
        "summary": envelope.get("summary"),
        "confidence": envelope.get("confidence"),
        "matched_paths": [path for path in paths if path in expected_paths],
        "matched_doctors": [kind for kind in doctor_kinds if kind in expected_doctors],
        "matched_tests": [
            command
            for command in test_commands
            if any(needle in command for needle in expected_tests)
        ],
        "matched_instruction_paths": [
            path for path in instruction_paths if path in expected_instruction_paths
        ],
        "matched_memory_paths": [path for path in memory_paths if path in expected_memory_paths],
        "matched_anchors": [anchor for anchor in anchor_ids if anchor in expected_anchor_ids],
        "context_zones": zone_names,
    }


def main() -> None:
    args = parse_args()
    tasks = json.loads(args.tasks.expanduser().read_text(encoding="utf-8"))
    results = []
    for task in tasks.get("tasks", []):
        envelope = run_context(
            task,
            args.workspace_root.expanduser().resolve(),
            args.plugin_root.expanduser().resolve(),
        )
        results.append(validate_task(task, envelope))

    print(
        json.dumps(
            {
                "status": "ok",
                "task_count": len(results),
                "results": results,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
