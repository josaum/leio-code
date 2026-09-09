#!/usr/bin/env python3
"""Check that current LEIO doctor warning families are accounted for."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path
from typing import Any


REQUIRED_FAMILY_FIELDS = {
    "owner",
    "severity",
    "status",
    "block_later",
    "next_action",
}


def parse_args() -> argparse.Namespace:
    plugin_root = Path(__file__).resolve().parents[1]
    workspace_root = plugin_root.parent
    parser = argparse.ArgumentParser(description="Validate LEIO doctor warning ledger coverage")
    parser.add_argument("--repo", type=Path, default=workspace_root, help="Repository to audit")
    parser.add_argument(
        "--ledger",
        type=Path,
        default=plugin_root / "docs" / "doctor-warning-ledger.json",
        help="Doctor warning ledger JSON",
    )
    parser.add_argument(
        "--doctor-json",
        type=Path,
        help="Optional existing `leio-code --json doctor all` output to validate",
    )
    parser.add_argument(
        "--enforce-counts",
        action="store_true",
        help=(
            "Ratchet mode: exit non-zero if any family's live warning count "
            "exceeds its ledger `last_seen_warning_count` (debt may shrink, never "
            "grow). Implies the standard coverage checks. Use in CI."
        ),
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
    raise RuntimeError("no JSON object found in doctor output")


def run_doctor_all(repo: Path) -> dict[str, Any]:
    plugin_root = Path(__file__).resolve().parents[1]
    completed = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "--",
            "--json",
            "--repo",
            str(repo),
            "doctor",
            "all",
        ],
        cwd=plugin_root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode not in (0, 1):
        raise RuntimeError(
            f"doctor command failed with {completed.returncode}:\n{completed.stderr}\n{completed.stdout}"
        )
    return extract_json_object(completed.stdout)


def warning_family(warning: str) -> str:
    match = re.match(r"^\[([^\]]+)\]\s+", warning)
    if not match:
        return "unclassified"
    return match.group(1)


def count_warning_families(envelope: dict[str, Any]) -> Counter[str]:
    warnings = envelope.get("warnings") or []
    return Counter(warning_family(str(warning)) for warning in warnings)


def sample_family_warnings(
    envelope: dict[str, Any], families: list[str], per_family: int = 2
) -> dict[str, list[str]]:
    samples: dict[str, list[str]] = {family: [] for family in families}
    for warning in envelope.get("warnings") or []:
        family = warning_family(str(warning))
        if family in samples and len(samples[family]) < per_family:
            samples[family].append(str(warning)[:300])
    return samples


def validate_ledger(
    ledger: dict[str, Any],
    counts: Counter[str],
    envelope: dict[str, Any] | None = None,
) -> dict[str, Any]:
    families = ledger.get("families")
    if not isinstance(families, dict):
        raise RuntimeError("doctor ledger must contain a `families` object")

    missing = sorted(family for family in counts if family not in families)
    incomplete = sorted(
        family
        for family, entry in families.items()
        if not isinstance(entry, dict) or REQUIRED_FAMILY_FIELDS - set(entry)
    )
    if missing or incomplete:
        detail = ""
        if missing and envelope is not None:
            samples = sample_family_warnings(envelope, missing)
            detail = "".join(
                f"\n  [{family}] {text}"
                for family, texts in samples.items()
                for text in texts
            )
        raise RuntimeError(
            f"doctor ledger is incomplete: missing_current={missing} "
            f"incomplete_entries={incomplete}{detail}"
        )

    stale = sorted(family for family in families if family not in counts)
    if stale:
        raise RuntimeError(f"doctor ledger contains stale families: {stale}")

    count_drift = {
        family: {
            "ledger_warning_count": int(families[family].get("last_seen_warning_count", 0)),
            "current_warning_count": count,
        }
        for family, count in counts.items()
        if int(families[family].get("last_seen_warning_count", 0)) != count
    }
    return {
        "status": "ok",
        "current_warning_count": sum(counts.values()),
        "current_family_count": len(counts),
        "families": dict(sorted(counts.items())),
        "stale_ledger_families": stale,
        "count_drift": count_drift,
    }


def enforce_counts(ledger: dict[str, Any], counts: Counter[str]) -> dict[str, dict[str, int]]:
    """Families whose live warning count exceeds the ledger baseline.

    The ratchet's teeth: pre-existing debt is tolerated at its recorded level,
    but a family that *grows* (or a brand-new family, surfaced separately by
    :func:`validate_ledger` as ``missing_current``) is a regression. Returns a
    map of regressed families to ``{baseline, current}``; empty means no growth.
    """
    families = ledger.get("families", {})
    grown: dict[str, dict[str, int]] = {}
    for family, current in counts.items():
        baseline = int(families.get(family, {}).get("last_seen_warning_count", 0))
        if current > baseline:
            grown[family] = {"baseline": baseline, "current": current}
    return dict(sorted(grown.items()))


def main() -> None:
    args = parse_args()
    ledger = json.loads(args.ledger.expanduser().read_text(encoding="utf-8"))
    if args.doctor_json:
        envelope = extract_json_object(args.doctor_json.expanduser().read_text(encoding="utf-8"))
    else:
        envelope = run_doctor_all(args.repo.expanduser().resolve())
    counts = count_warning_families(envelope)
    result = validate_ledger(ledger, counts, envelope)
    if args.enforce_counts:
        grown = enforce_counts(ledger, counts)
        result["enforce_counts"] = grown
        if grown:
            print(json.dumps(result, indent=2))
            raise SystemExit(
                f"doctor warning ratchet: {len(grown)} family(ies) grew beyond "
                f"their ledger baseline: {sorted(grown)}"
            )
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
