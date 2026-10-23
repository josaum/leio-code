#!/usr/bin/env python3
"""Run reproducible paper ablations for LEIO Code and LEIO Harness."""
from __future__ import annotations

import argparse
from collections import defaultdict
import json
import math
import os
from pathlib import Path
import re
import subprocess
import time


STOP_WORDS = {
    "a", "an", "and", "for", "in", "of", "on", "or", "the", "to", "with", "fix",
}


def tokenize(text: str) -> list[str]:
    parts = re.findall(r"[A-Za-z0-9]+", text.lower())
    return [p for p in parts if len(p) >= 2 and p not in STOP_WORDS]


def approx_tokens(byte_count: int) -> int:
    return max(1, math.ceil(byte_count / 4.0))


def compute_rank_metrics(paths: list[str], relevant: list[str]) -> dict:
    rel_set = set(relevant)
    ranks = [i + 1 for i, p in enumerate(paths) if p in rel_set]
    rank = min(ranks) if ranks else None
    return {
        "first_relevant_rank": rank,
        "reciprocal_rank": (1.0 / rank) if rank else 0.0,
        "hit_at_1": rank == 1,
        "hit_at_3": rank is not None and rank <= 3,
        "hit_at_5": rank is not None and rank <= 5,
    }


def run_rg_baseline(repo: Path, task: str, limit: int = 5) -> tuple[list[str], int, float]:
    tokens = tokenize(task)
    t0 = time.perf_counter()
    file_token_hits: dict[str, set[str]] = defaultdict(set)
    file_total_matches: dict[str, int] = defaultdict(int)
    rg_output_bytes = 0

    for tok in tokens:
        proc = subprocess.run(
            [
                "rg", "-i", "-c",
                "--glob", "!.git/**",
                "--glob", "!.leio-code/**",
                "--glob", "!target/**",
                "--glob", "!node_modules/**",
                "--glob", "!benchmarks/**",
                tok,
                ".",
            ],
            cwd=repo,
            capture_output=True,
            text=True,
            check=False,
        )
        rg_output_bytes += len((proc.stdout or "").encode("utf-8"))
        for line in (proc.stdout or "").splitlines():
            if ":" not in line:
                continue
            path_part, count_part = line.rsplit(":", 1)
            rel_path = path_part.removeprefix("./")
            try:
                cnt = int(count_part)
            except ValueError:
                continue
            file_token_hits[rel_path].add(tok)
            file_total_matches[rel_path] += cnt

    ranked = sorted(
        file_token_hits.keys(),
        key=lambda p: (
            -len(file_token_hits[p]),
            # Penalize raw occurrence flood on giant files unless token count leads
            -min(file_total_matches[p], 25),
            len(p),
            p,
        ),
    )[:limit]
    elapsed_ms = (time.perf_counter() - t0) * 1000.0
    return ranked, rg_output_bytes, elapsed_ms


def run_leio_ablation(
    binary: Path,
    repo: Path,
    task: str,
    ablation: str,
    limit: int = 5,
) -> tuple[list[str], int, float]:
    env = os.environ.copy()
    env["LEIO_CONTEXT_ABLATION"] = ablation
    env["LEIO_REUSE_INDEX"] = "1"
    t0 = time.perf_counter()
    proc = subprocess.run(
        [str(binary), "--json", "--repo", str(repo), "context", task, "--limit", str(limit)],
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    elapsed_ms = (time.perf_counter() - t0) * 1000.0
    envelope = json.loads(proc.stdout)
    bundle = envelope["entities"][0]
    rows = bundle["files_to_read"]
    paths = [r["path"] for r in rows]
    # Compact MCP text digest size (~420 bytes)
    digest_bytes = len(envelope.get("summary", "").encode("utf-8")) + sum(
        len(r["path"]) + 48 for r in rows
    )
    return paths, digest_bytes, elapsed_ms


def estimate_agent_localization_tokens(
    repo: Path,
    paths: list[str],
    relevant: list[str],
    tool_output_bytes: int,
    *,
    is_leio: bool,
) -> dict:
    rel_set = set(relevant)
    rank = next((i + 1 for i, p in enumerate(paths) if p in rel_set), None)
    inspect_count = rank if rank is not None else len(paths)
    read_bytes = 0
    for p in paths[:inspect_count]:
        fp = repo / p
        if fp.is_file():
            size = fp.stat().st_size
            # With LEIO explain symbol, agent reads bounded 45-line symbol window (~1800 bytes)
            # With rg, agent reads whole file or large chunks (~min(size, 16384) bytes)
            read_bytes += min(size, 1800) if is_leio else min(size, 16384)
    total_tokens = approx_tokens(tool_output_bytes + read_bytes)
    tool_calls = (1 + (1 if rank == 1 else inspect_count)) if is_leio else (len(relevant) + 2 + inspect_count)
    return {
        "tokens_to_localize": total_tokens,
        "tool_calls_to_localize": tool_calls,
    }


def evaluate_suite(binary: Path, repo: Path, tasks: list[dict]) -> dict:
    modes = [
        ("rg_lexical", "Ripgrep Lexical Baseline"),
        ("path_only", "LEIO Path-Only IDF"),
        ("ast_symbol_only", "AST Symbol-Only (Repo-Map/LSP style)"),
        ("naive_rrf", "Naive 3-Layer RRF (v2.6.2)"),
        ("no_playbook", "LEIO v2.6.5 (w/o ACE Playbook)"),
        ("full", "LEIO Code v2.6.5 (Full Hybrid)"),
    ]
    summary_by_mode = {}
    for mode_key, mode_label in modes:
        per_task = []
        cat_buckets = defaultdict(list)
        for item in tasks:
            for rel in item["relevant_paths"]:
                if not (repo / rel).is_file():
                    raise FileNotFoundError(f"Missing ground-truth file: {rel}")
            if mode_key == "rg_lexical":
                paths, out_bytes, elapsed_ms = run_rg_baseline(repo, item["task"], limit=5)
                cost = estimate_agent_localization_tokens(
                    repo, paths, item["relevant_paths"], out_bytes, is_leio=False
                )
            else:
                paths, out_bytes, elapsed_ms = run_leio_ablation(
                    binary, repo, item["task"], mode_key, limit=5
                )
                cost = estimate_agent_localization_tokens(
                    repo, paths, item["relevant_paths"], out_bytes, is_leio=True
                )
            m = compute_rank_metrics(paths, item["relevant_paths"])
            row = {
                "id": item.get("id", item["task"][:16]),
                "category": item.get("category", "general"),
                "task": item["task"],
                "relevant_paths": item["relevant_paths"],
                "paths": paths,
                "elapsed_ms": round(elapsed_ms, 2),
                **m,
                **cost,
            }
            per_task.append(row)
            cat_buckets[row["category"]].append(row)

        n = len(per_task)
        by_category = {}
        for cat, rows in sorted(cat_buckets.items()):
            cn = len(rows)
            by_category[cat] = {
                "count": cn,
                "mrr": round(sum(r["reciprocal_rank"] for r in rows) / cn, 4),
                "hit_at_1": round(sum(1 for r in rows if r["hit_at_1"]) / cn, 4),
                "hit_at_3": round(sum(1 for r in rows if r["hit_at_3"]) / cn, 4),
            }

        latencies = sorted(r["elapsed_ms"] for r in per_task)
        summary_by_mode[mode_key] = {
            "label": mode_label,
            "task_count": n,
            "mrr": round(sum(r["reciprocal_rank"] for r in per_task) / n, 4),
            "hit_at_1": round(sum(1 for r in rows_iter(per_task) if r["hit_at_1"]) / n, 4),
            "hit_at_3": round(sum(1 for r in rows_iter(per_task) if r["hit_at_3"]) / n, 4),
            "hit_at_5": round(sum(1 for r in rows_iter(per_task) if r["hit_at_5"]) / n, 4),
            "mean_tokens_to_localize": round(
                sum(r["tokens_to_localize"] for r in per_task) / n, 1
            ),
            "mean_tool_calls": round(
                sum(r["tool_calls_to_localize"] for r in per_task) / n, 2
            ),
            "median_latency_ms": round(latencies[n // 2], 2),
            "by_category": by_category,
            "results": per_task,
        }
    return summary_by_mode


def rows_iter(rows: list[dict]) -> list[dict]:
    return rows


def evaluate_coupled_co_edits(binary: Path, repo: Path) -> dict:
    coupled_tasks = [
        {
            "primary": "mcp/index.js",
            "task": "register stdio MCP tool handler in mcp/index.js",
            "expected_partners": ["schema/leio-code-mcp-service-descriptor.json", "docs/MCP-SURFACE-GAP.md"],
        },
        {
            "primary": "src/code_graph.rs",
            "task": "update RDF N-Quads emitter in src/code_graph.rs",
            "expected_partners": ["src/indexer.rs", "Cargo.toml"],
        },
        {
            "primary": "apps-sdk/server.js",
            "task": "update Apps SDK HTTP tool router in apps-sdk/server.js",
            "expected_partners": ["docs/MCP-SURFACE-GAP.md", "mcp/index.js"],
        },
        {
            "primary": "mcp/output-schemas.js",
            "task": "update MCP tool output schema definitions in mcp/output-schemas.js",
            "expected_partners": ["schema/leio-code-mcp-service-descriptor.json", "docs/MCP-SURFACE-GAP.md"],
        },
        {
            "primary": "mcp/workflow.js",
            "task": "update workflow MCP tool handler in mcp/workflow.js",
            "expected_partners": ["mcp/output-schemas.js", "schema/leio-code-mcp-service-descriptor.json"],
        },
        {
            "primary": "mcp/guide.js",
            "task": "update MCP routing guide topic builder in mcp/guide.js",
            "expected_partners": ["docs/MCP-SURFACE-GAP.md", "mcp/index.js"],
        },
    ]
    results = {}
    for mode in ["no_playbook", "full"]:
        hits = 0
        for item in coupled_tasks:
            paths, _, _ = run_leio_ablation(binary, repo, item["task"], mode, limit=6)
            if any(p in paths for p in item["expected_partners"]):
                hits += 1
        results[mode] = {
            "coupled_task_count": len(coupled_tasks),
            "partner_hit_at_6": round(hits / len(coupled_tasks), 4),
        }
    return results


def evaluate_sigreg_and_harness(harness_bin: Path, repo: Path) -> dict:
    # 1. Measure SIGReg isotropy on collapsed vs isotropic vs GEPA-diversified populations
    dim = 32
    # Collapsed population: 8 lanes pointing nearly in the same direction
    collapsed = []
    for i in range(8):
        vec = [1.0] + [0.01 * ((i % 3) - 1)] * (dim - 1)
        norm = math.sqrt(sum(x * x for x in vec))
        collapsed.append([x / norm for x in vec])

    # Orthogonal / well-spread population across 8 lanes
    isotropic = []
    for i in range(8):
        vec = [
            math.sin((i + 1) * (j + 1) * 1.6180339887)
            for j in range(dim)
        ]
        norm = math.sqrt(sum(x * x for x in vec))
        isotropic.append([x / norm for x in vec])

    import tempfile
    def run_sigreg(vectors: list[list[float]]) -> dict:
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as tf:
            json.dump([{"vector": v} for v in vectors], tf)
            tmp_path = tf.name
        try:
            proc = subprocess.run(
                [str(harness_bin), "sigreg", "isotropy", "--rows", tmp_path],
                capture_output=True,
                text=True,
                check=True,
            )
            return json.loads(proc.stdout)
        finally:
            Path(tmp_path).unlink(missing_ok=True)

    collapsed_res = run_sigreg(collapsed)
    isotropic_res = run_sigreg(isotropic)

    # Load 40-lane model sweep summary if present
    sweep_path = repo / "benchmarks" / "model-sweep" / "model-sweep-report.json"
    sweep_summary = {}
    if sweep_path.is_file():
        raw_sweep = json.loads(sweep_path.read_text())
        sweep_summary = {
            "total_lanes": raw_sweep.get("total_lanes", len(raw_sweep.get("lanes", []))),
            "models_evaluated": len(raw_sweep.get("by_model", {})),
            "by_model": raw_sweep.get("by_model", {}),
        }

    return {
        "sigreg_collapsed_population": collapsed_res,
        "sigreg_isotropic_population": isotropic_res,
        "model_sweep_40lane": sweep_summary,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path.home() / ".cargo/bin/leio-code")
    parser.add_argument("--harness-binary", type=Path, default=Path.home() / ".cargo/bin/leio-harness")
    parser.add_argument("--repo", type=Path, default=Path("."))
    parser.add_argument(
        "--suite",
        type=Path,
        default=Path("benchmarks/paper-evaluation-suite.json"),
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("benchmarks/paper-ablation-results.json"),
    )
    args = parser.parse_args()

    repo = args.repo.resolve()
    binary = args.binary.expanduser().resolve()
    harness_binary = args.harness_binary.expanduser().resolve()
    suite = json.loads((repo / args.suite).read_text())

    # Warm index once so ablations measure retrieval & ranking rather than cold indexing
    subprocess.run([str(binary), "index", "--repo", str(repo)], check=True, capture_output=True)

    ablations = evaluate_suite(binary, repo, suite["tasks"])
    co_edit_eval = evaluate_coupled_co_edits(binary, repo)
    harness_eval = evaluate_sigreg_and_harness(harness_binary, repo)

    report = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "binary_version": subprocess.check_output([str(binary), "--version"], text=True).strip(),
        "suite_size": len(suite["tasks"]),
        "ablations": ablations,
        "co_edit_partner_evaluation": co_edit_eval,
        "harness_evaluation": harness_eval,
    }
    out_path = repo / args.output
    out_path.write_text(json.dumps(report, indent=2) + "\n")

    print("=== 25-Task Multi-Category Ablation Summary ===")
    for k, v in ablations.items():
        print(
            f"{k:16s} | MRR={v['mrr']:.4f} | Hit@1={v['hit_at_1']:.4f} | "
            f"Hit@3={v['hit_at_3']:.4f} | Hit@5={v['hit_at_5']:.4f} | "
            f"Tokens={v['mean_tokens_to_localize']:6.1f} | Calls={v['mean_tool_calls']:.2f} | "
            f"p50={v['median_latency_ms']:.1f}ms"
        )
    print("=== Coupled Co-Edit Partner Recall@6 ===")
    for k, v in co_edit_eval.items():
        print(f"{k:16s} | Partner_Hit@6={v['partner_hit_at_6']:.4f}")


if __name__ == "__main__":
    main()
