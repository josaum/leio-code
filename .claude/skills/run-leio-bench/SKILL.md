---
name: run-leio-bench
description: Repo-local entry for the model-swept golden-task benchmark (scripts/benchmark_models.py). Use when a task asks to sweep or benchmark models against LEIO golden tasks in this checkout, or to regenerate benchmarks/model-sweep/day-spec.json.
---

# run-leio-bench (repo-local)

The plugin skill `leio-bench` (skills/leio-bench/SKILL.md) is the full guide.
Repo-session quick path:

```bash
python3 scripts/benchmark_models.py                 # dry-run: writes benchmarks/model-sweep/day-spec.json
python3 scripts/benchmark_models.py --execute       # runs the day (spends model tokens)
make coverage                                        # tests + coverage for the runner
```

Gates: clean `git status` (harness refuses dirty worktrees), agent CLIs
authenticated for the chosen OpenRouter slugs, `tests/test_benchmark_models.py`
must stay green when touching the runner.
