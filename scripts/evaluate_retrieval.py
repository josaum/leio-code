#!/usr/bin/env python3
"""Measure first-relevant rank on labeled repository tasks; no model calls."""
import argparse
import json
import subprocess
from pathlib import Path


def metrics(paths, relevant):
    ranks = [i + 1 for i, path in enumerate(paths) if path in relevant]
    rank = min(ranks) if ranks else None
    return {"first_relevant_rank": rank, "reciprocal_rank": 1 / rank if rank else 0,
            "hit_at_1": rank == 1, "hit_at_3": rank is not None and rank <= 3}


def evaluate(binary, repo, tasks):
    results = []
    for task in tasks:
        for candidate in task['relevant_paths']:
            if not (repo / candidate).is_file():
                raise ValueError(f"Missing labeled file: {candidate}")
        run = subprocess.run([str(binary), '--json', '--repo', str(repo), 'context', task['task'], '--limit', '5'],
                             capture_output=True, text=True, check=True, timeout=120)
        envelope = json.loads(run.stdout)
        paths = [row['path'] for row in envelope['entities'][0]['files_to_read']]
        results.append({**task, 'paths': paths, **metrics(paths, task['relevant_paths'])})
    return {'repository': str(repo), 'binary': str(binary),
            'binary_version': subprocess.check_output([str(binary), '--version'], text=True).strip(),
            'task_count': len(results),
            'mean_reciprocal_rank': sum(r['reciprocal_rank'] for r in results) / len(results),
            'hit_at_1': sum(r['hit_at_1'] for r in results) / len(results),
            'hit_at_3': sum(r['hit_at_3'] for r in results) / len(results),
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
