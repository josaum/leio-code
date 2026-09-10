#!/usr/bin/env python3
"""Measure first-relevant rank on labeled repository tasks; no model calls."""
import argparse
from collections import Counter
import json
import subprocess
from pathlib import Path


DEFAULT_TIMEOUT_SECONDS = 120
DIAGNOSTIC_LIMIT = 1000
SEMANTIC_SOURCES = {"none", "precomputed", "on_demand"}


def metrics(paths, relevant):
    ranks = [i + 1 for i, path in enumerate(paths) if path in relevant]
    rank = min(ranks) if ranks else None
    return {"first_relevant_rank": rank, "reciprocal_rank": 1 / rank if rank else 0,
            "hit_at_1": rank == 1, "hit_at_3": rank is not None and rank <= 3}


def _clip(value, limit=DIAGNOSTIC_LIMIT):
    if isinstance(value, bytes):
        value = value.decode(errors="replace")
    rendered = (value or "").strip()
    return rendered if len(rendered) <= limit else rendered[:limit] + "…"


def _context_error(message, command, stdout="", stderr=""):
    return RuntimeError(
        f"{message}: {' '.join(command)}; "
        f"stdout={_clip(stdout)!r}; stderr={_clip(stderr)!r}"
    )


def run_context(binary, repo, task, *, limit=5, timeout=DEFAULT_TIMEOUT_SECONDS):
    command = [
        str(binary), '--json', '--repo', str(repo), 'context', task,
        '--limit', str(limit),
    ]
    try:
        completed = subprocess.run(
            command,
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        raise _context_error(
            f"context query timed out after {timeout}s",
            command,
            exc.stdout if exc.stdout is not None else "",
            exc.stderr if exc.stderr is not None else "",
        ) from exc

    if completed.returncode != 0:
        raise _context_error(
            f"context query exited {completed.returncode}",
            command,
            completed.stdout,
            completed.stderr,
        )
    try:
        envelope = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise _context_error(
            "context query returned invalid JSON",
            command,
            completed.stdout,
            completed.stderr,
        ) from exc

    entities = envelope.get('entities') if isinstance(envelope, dict) else None
    if not entities or not isinstance(entities[0], dict):
        raise _context_error(
            "context response is missing entities[0].files_to_read",
            command,
            completed.stdout,
            completed.stderr,
        )
    rows = entities[0].get('files_to_read')
    if not isinstance(rows, list):
        raise _context_error(
            "context response is missing entities[0].files_to_read",
            command,
            completed.stdout,
            completed.stderr,
        )
    if any(
        not isinstance(row, dict) or not isinstance(row.get('path'), str)
        for row in rows
    ):
        raise _context_error(
            "context response has malformed files_to_read rows",
            command,
            completed.stdout,
            completed.stderr,
        )

    meta = envelope.get('meta')
    source = meta.get('semantic_source') if isinstance(meta, dict) else None
    if source is None:
        source = 'unavailable'
    elif source not in SEMANTIC_SOURCES:
        raise _context_error(
            f"context response has invalid semantic_source {source!r}",
            command,
            completed.stdout,
            completed.stderr,
        )
    return [row['path'] for row in rows], source


def evaluate(binary, repo, tasks):
    results = []
    for task in tasks:
        for candidate in task['relevant_paths']:
            if not (repo / candidate).is_file():
                raise ValueError(f"Missing labeled file: {candidate}")
        paths, semantic_source = run_context(binary, repo, task['task'])
        results.append({
            **task,
            'paths': paths,
            'semantic_source': semantic_source,
            **metrics(paths, task['relevant_paths']),
        })
    semantic_source_counts = dict(Counter(
        result['semantic_source'] for result in results
    ))
    return {'repository': str(repo), 'binary': str(binary),
            'binary_version': subprocess.check_output([str(binary), '--version'], text=True).strip(),
            'task_count': len(results),
            'mean_reciprocal_rank': sum(r['reciprocal_rank'] for r in results) / len(results),
            'hit_at_1': sum(r['hit_at_1'] for r in results) / len(results),
            'hit_at_3': sum(r['hit_at_3'] for r in results) / len(results),
            'semantic_source_counts': semantic_source_counts,
            'limitations': 'Small labeled suite; relevance labels are not exhaustive. No architecture or call-edge accuracy claim.',
            'results': results}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--tasks', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    report = evaluate(args.binary.expanduser().resolve(), args.repo.resolve(), json.loads(args.tasks.read_text())['tasks'])
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({k:v for k,v in report.items() if k != 'results'}, indent=2))


if __name__ == '__main__':
    main()
